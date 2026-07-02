mod ansi;
#[cfg(target_os = "linux")]
mod cleanup;
pub mod cli;
mod engine_client;
mod event_handler;
mod ghost;
pub mod history;
pub mod input;
pub mod interceptor;
pub mod ipc;
pub mod logger;
mod message;
mod popup;
pub mod pty;
pub mod term;
pub mod update;

use std::env;
#[cfg(unix)]
use std::ffi::{CString, OsStr};
use std::sync::{LazyLock, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result, anyhow};
use bytes::BytesMut;
use cfg_if::cfg_if;
use clap::Parser;
use cli::Cli;
use flume::{Receiver, Sender};
use nerv_log::{LogArgs, initialize_logging};
use nerv_os::{Context, Env};
use nerv_proto::local::{self, EnvironmentVariable, TerminalCursorCoordinates};
use nerv_proto::remote::Hostbound;
use nerv_proto::remote_hooks::{hook_to_message, new_edit_buffer_hook};
use nerv_settings::state;
use nerv_term::Term;
use nerv_term::ansi::Processor;
use nerv_term::event::EventListener;
use nerv_term::term::{ShellState, SizeInfo, TextBuffer};
use nerv_util::env_var::{NERV_LOG_LEVEL, NERV_PTY_SESSION_ID, NERV_SHELL, NERV_TERM};
use nerv_util::process_info::{Pid, PidExt};
use nerv_util::{PRODUCT_NAME, PTY_BINARY_NAME, Terminal as FigTerminal, directories};
#[cfg(unix)]
use nix::unistd::execvp;
use portable_pty::PtySize;
use tokio::io::{self, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio::{runtime, select};
use tracing::{debug, error, info, trace, warn};

use crate::event_handler::EventHandler;
use crate::input::{InputEvent, KeyCode, KeyCodeEncodeModes, KeyboardEncoding, Modifiers};
use crate::interceptor::KeyInterceptor;
use crate::ipc::{spawn_figterm_ipc, spawn_remote_ipc};
use crate::message::{process_figterm_message, process_remote_message};
#[cfg(unix)]
use crate::pty::unix::open_pty;
#[cfg(windows)]
use crate::pty::win::open_pty;
use crate::pty::{AsyncMasterPtyExt, CommandBuilder};
use crate::term::{SystemTerminal, Terminal};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const BUFFER_SIZE: usize = 16384;

static INSERT_ON_NEW_CMD: Mutex<Option<(String, bool, bool)>> = Mutex::new(None);
static INSERTION_LOCKED_AT: RwLock<Option<SystemTime>> = RwLock::new(None);
static EXPECTED_BUFFER: Mutex<String> = Mutex::new(String::new());

static SHELL_ENVIRONMENT_VARIABLES: Mutex<Vec<EnvironmentVariable>> = Mutex::new(Vec::new());
static SHELL_ALIAS: Mutex<Option<String>> = Mutex::new(None);

static USER_ENABLED_SHELLS: LazyLock<Vec<String>> = LazyLock::new(|| {
    nerv_settings::state::get("user.enabled-shells")
        .ok()
        .flatten()
        .unwrap_or_default()
});

static HOSTNAME: LazyLock<Option<String>> = LazyLock::new(sysinfo::System::host_name);

pub enum MainLoopEvent {
    Insert {
        insert: Vec<u8>,
        unlock: bool,
        bracketed: bool,
        execute: bool,
    },
    UnlockInterception,
    SetImmediateMode(bool),
    PromptSSH {
        uuid: String,
        remote_host: String,
    },
    SetCsiU,
    UnsetCsiU,
}

fn shell_state_to_context(shell_state: &ShellState) -> local::ShellContext {
    let terminal = FigTerminal::parent_terminal(&Context::new()).map(|s| s.to_string());

    local::ShellContext {
        pid: shell_state.local_context.pid,
        ttys: shell_state.local_context.tty.clone(),
        process_name: shell_state.local_context.shell.clone(),
        shell_path: shell_state
            .local_context
            .shell_path
            .clone()
            .map(|path| path.display().to_string()),
        wsl_distro: shell_state.local_context.wsl_distro.clone(),
        current_working_directory: shell_state
            .local_context
            .current_working_directory
            .clone()
            .map(|cwd| cwd.display().to_string()),
        session_id: shell_state.local_context.session_id.clone(),
        terminal,
        hostname: shell_state
            .local_context
            .username
            .as_deref()
            .and_then(|username| {
                HOSTNAME
                    .as_deref()
                    .map(|hostname| format!("{username}@{hostname}"))
            }),
        environment_variables: SHELL_ENVIRONMENT_VARIABLES.lock().unwrap().clone(),
        qterm_version: Some(env!("CARGO_PKG_VERSION").into()),
        preexec: Some(shell_state.preexec),
        osc_lock: Some(shell_state.osc_lock),
        alias: SHELL_ALIAS.lock().unwrap().clone(),
    }
}

#[allow(clippy::needless_return)]
fn get_cursor_coordinates(terminal: &dyn Terminal) -> Option<TerminalCursorCoordinates> {
    cfg_if! {
        if #[cfg(target_os = "windows")] {
            use term::cast;

            let coordinate = terminal.get_cursor_coordinate().ok()?;
            let screen_size = terminal.get_screen_size().ok()?;
            return Some(TerminalCursorCoordinates {
                x: cast(coordinate.cols).ok()?,
                y: cast(coordinate.rows).ok()?,
                xpixel: cast(screen_size.xpixel).ok()?,
                ypixel: cast(screen_size.ypixel).ok()?,
            });
        } else {
            let _terminal = terminal;
            return None;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn _should_install_remote_ssh_integration(
    uuid: String,
    remote_host: String,
    main_loop_tx: Sender<MainLoopEvent>,
    remote_receiver: Receiver<nerv_proto::remote::Clientbound>,
    remote_sender: Sender<Hostbound>,
    term: &Term<EventHandler>,
    pty_master: &mut Box<dyn crate::pty::AsyncMasterPty + Send + Sync>,
    key_interceptor: &mut KeyInterceptor,
) -> Option<bool> {
    use nerv_proto::remote::clientbound;

    let remote_install_setting =
        nerv_settings::settings::get_string_or("ssh.remote-prompt", "ask".into());
    if remote_install_setting == "never" {
        return Some(false);
    }

    let key = format!("ssh.remote-prompt.disable-host.{remote_host}");
    let disable_host = nerv_settings::state::get_bool_or(key, false);
    if disable_host {
        return Some(false);
    }

    let prompt_timeout: u64 =
        nerv_settings::settings::get_int_or("ssh.remote-prompt.timeout", 2000)
            .try_into()
            .unwrap_or(2000);

    // Wait for child ssh session to connect to local desktop instance.
    let got_child_connection =
        tokio::time::timeout(tokio::time::Duration::from_millis(prompt_timeout), async {
            loop {
                if let Ok(msg) = remote_receiver.recv_async().await {
                    if let Some(clientbound::Packet::NotifyChildSessionStarted(
                        clientbound::NotifyChildSessionStarted { parent_id },
                    )) = msg.packet
                    {
                        if parent_id == uuid {
                            return true;
                        }
                    } else {
                        process_remote_message(
                            msg,
                            main_loop_tx.clone(),
                            remote_sender.clone(),
                            term,
                            pty_master,
                            key_interceptor,
                        )
                        .await
                        .ok();
                    }
                }
            }
        })
        .await
        .is_ok();

    if got_child_connection {
        return Some(false);
    }

    if remote_install_setting == "always" {
        return Some(true);
    }

    None
}

fn can_send_edit_buffer<T>(term: &Term<T>) -> bool
where
    T: EventListener,
{
    let shell_enabled = ["bash", "zsh", "fish", "nu", "dash"]
        .into_iter()
        .chain(USER_ENABLED_SHELLS.iter().map(|s| s.as_str()))
        .any(|s| {
            let shell_raw = term.shell_state().get_context().shell.as_deref();
            // we actually want to work with a nested figterm :)
            let shell = match shell_raw.and_then(|s| s.strip_suffix(" (figterm)")) {
                Some(s) => Some(s),
                None => shell_raw,
            };

            shell == Some(s)
        });
    let preexec = term.shell_state().preexec;

    let mut handle = INSERTION_LOCKED_AT.write().unwrap();
    let insertion_locked = match handle.as_ref() {
        Some(at) => {
            let lock_expired = at.elapsed().unwrap_or(Duration::ZERO) > Duration::from_millis(16);
            let should_unlock = lock_expired
                || term.get_current_buffer().is_none_or(|buff| {
                    &buff.buffer == (&EXPECTED_BUFFER.lock().unwrap() as &String)
                });
            if should_unlock {
                handle.take();
                if lock_expired {
                    trace!("insertion lock released because lock expired");
                } else {
                    trace!("insertion lock released because buffer looks like how we expect");
                }
                false
            } else {
                true
            }
        }
        None => false,
    };
    drop(handle);

    trace!(%shell_enabled, %preexec, %insertion_locked, "can_send_edit_buffer");

    shell_enabled && !insertion_locked && !preexec
}

const NERV_DISABLE_AUTOCOMPLETE: &str = "NERV_DISABLE_AUTOCOMPLETE";

fn autocomplete_enabled(env: &Env) -> bool {
    env.get_os(NERV_DISABLE_AUTOCOMPLETE)
        .is_none_or(|s| s.is_empty())
}

static AUTOCOMPLETE_ENABLED: LazyLock<bool> = LazyLock::new(|| autocomplete_enabled(&Env::new()));

async fn send_edit_buffer<T>(
    term: &Term<T>,
    sender: &Sender<Hostbound>,
    cursor_coordinates: Option<TerminalCursorCoordinates>,
) -> Result<()>
where
    T: EventListener,
{
    if !*AUTOCOMPLETE_ENABLED {
        return Ok(());
    }

    match term.get_current_buffer() {
        Some(edit_buffer) => {
            if let Some(cursor_idx) = edit_buffer.cursor_idx.and_then(|i| i.try_into().ok()) {
                debug!("edit_buffer: {edit_buffer:?}");
                trace!("buffer bytes: {:02X?}", edit_buffer.buffer.as_bytes());
                trace!(
                    "buffer chars: {:?}",
                    edit_buffer.buffer.chars().collect::<Vec<_>>()
                );

                let context = shell_state_to_context(term.shell_state());

                let edit_buffer_hook = new_edit_buffer_hook(
                    Some(context),
                    edit_buffer.buffer,
                    cursor_idx,
                    0,
                    cursor_coordinates,
                );
                let message = hook_to_message(edit_buffer_hook);

                trace!("Sending: {message:?}");

                sender.send_async(message).await?;
            }
            Ok(())
        }
        None => Err(anyhow!("No edit buffer to send")),
    }
}

/// Live inline-completion overlay state for PTY mode (Phase 3a + 3b):
/// the ghost remainder drawn on the prompt line, the popup list below it,
/// and the bookkeeping needed to paint and erase them without corrupting
/// the shell's own output.
#[derive(Default)]
struct Overlay {
    /// Ghost remainder currently shown (Right-arrow accepts it).
    ghost: Option<String>,
    /// Popup list, when there are ≥2 suggestions.
    popup: Option<popup::Popup>,
    /// Buffer the current popup was built for — used to keep the
    /// selection stable across re-queries while the line is unchanged.
    buffer: String,
    /// High-water mark of rows reserved below the cursor this prompt
    /// (monotonic so we never re-scroll and walk the prompt up the
    /// screen). Reset when a command runs.
    reserved: usize,
    /// Rows the popup last painted, for erase-before-redraw.
    drawn: usize,
}

/// Re-query `nervd` for the line under the cursor and update `overlay`'s
/// ghost + popup (without drawing). Selection is preserved while the
/// buffer is unchanged; a changed buffer rebuilds the popup from the top.
/// Clears the overlay model when there's nothing to show.
async fn refresh_suggestions<T>(term: &Term<T>, overlay: &mut Overlay, max_vis: usize)
where
    T: EventListener,
{
    if !*AUTOCOMPLETE_ENABLED {
        overlay.ghost = None;
        overlay.popup = None;
        return;
    }

    let Some(TextBuffer { buffer, cursor_idx }) = term.get_current_buffer() else {
        overlay.ghost = None;
        overlay.popup = None;
        return;
    };
    // `cursor_idx` is a byte offset; only preview at end-of-line.
    let at_eol = cursor_idx == Some(buffer.len());
    if !at_eol {
        overlay.ghost = None;
        overlay.popup = None;
        return;
    }
    let cursor = buffer.len();

    let cwd = term
        .shell_state()
        .local_context
        .current_working_directory
        .as_ref()
        .map(|p| p.display().to_string());

    let suggestions = engine_client::complete(&buffer, cursor, cwd).await;
    if suggestions.is_empty() {
        overlay.ghost = None;
        overlay.popup = None;
        overlay.buffer = buffer;
        return;
    }

    // Rebuild the popup only when the line changed; otherwise keep the
    // user's current selection.
    if buffer != overlay.buffer || overlay.popup.is_none() {
        let items = suggestions
            .iter()
            .map(|s| popup::PopupItem {
                display: s.display.clone(),
                insertion: s.insertion.clone(),
            })
            .collect();
        overlay.popup = popup::Popup::new(items, max_vis);
    }
    overlay.buffer = buffer.clone();

    // Ghost mirrors the active row (selected popup item, else the top).
    let active = overlay
        .popup
        .as_ref()
        .map(|p| p.selected_item().insertion.clone())
        .unwrap_or_else(|| suggestions[0].insertion.clone());
    overlay.ghost = ghost::compute_ghost(&buffer, &active);
}

/// Paint the current overlay model: reserve rows as needed, erase the
/// previous popup, draw the popup and ghost, and leave the cursor where
/// it started.
async fn draw_overlay(stdout: &mut io::Stdout, overlay: &mut Overlay, cols: usize) {
    let need = overlay.popup.as_ref().map(|p| p.rows()).unwrap_or(0);
    if need > overlay.reserved {
        let _ = stdout
            .write_all(popup::reserve_seq(need - overlay.reserved).as_bytes())
            .await;
        overlay.reserved = need;
    }
    if overlay.drawn > 0 {
        let _ = stdout
            .write_all(popup::clear_seq(overlay.drawn).as_bytes())
            .await;
    }
    if let Some(p) = &overlay.popup {
        let _ = stdout.write_all(p.render_seq(cols).as_bytes()).await;
        overlay.drawn = p.rows();
    } else {
        overlay.drawn = 0;
    }
    match &overlay.ghost {
        Some(g) => {
            let _ = stdout.write_all(ghost::render_seq(g).as_bytes()).await;
        }
        None => {
            let _ = stdout.write_all(ghost::clear_seq().as_bytes()).await;
        }
    }
    let _ = stdout.flush().await;
}

/// Erase the overlay from the screen and reset its drawing bookkeeping
/// (called when a command runs — the screen scrolls past anyway).
async fn reset_overlay(stdout: &mut io::Stdout, overlay: &mut Overlay) {
    if overlay.drawn > 0 {
        let _ = stdout
            .write_all(popup::clear_seq(overlay.drawn).as_bytes())
            .await;
    }
    let _ = stdout.write_all(ghost::clear_seq().as_bytes()).await;
    let _ = stdout.flush().await;
    *overlay = Overlay::default();
}

fn get_parent_shell() -> Result<String> {
    match env::var(NERV_SHELL).ok().filter(|s| !s.is_empty()) {
        Some(v) => Ok(v),
        None => match env::var("SHELL").ok().filter(|s| !s.is_empty()) {
            Some(shell) => Ok(shell),
            None => {
                anyhow::bail!("No NERV_SHELL or SHELL found");
            }
        },
    }
}

fn build_shell_command(command: Option<&[String]>) -> Result<CommandBuilder> {
    let mut builder = match command {
        Some(command) => {
            let mut iter = command.iter().map(|s| s.as_str());

            let mut builder = CommandBuilder::new(iter.next().unwrap());
            for arg in iter {
                builder.arg(arg);
            }
            builder
        }
        None => {
            let parent_shell = get_parent_shell()?;
            let mut builder = CommandBuilder::new(parent_shell);

            if env::var("NERV_IS_LOGIN_SHELL").ok().as_deref() == Some("1") {
                builder.arg("--login");
            }

            if let Some(execution_string) = env::var("NERV_EXECUTION_STRING")
                .ok()
                .filter(|s| !s.is_empty())
            {
                builder.args(["-c", &execution_string]);
            }

            if let Some(extra_args) = env::var("NERV_SHELL_EXTRA_ARGS")
                .ok()
                .filter(|s| !s.is_empty())
            {
                builder.args(
                    extra_args
                        .split_whitespace()
                        .filter(|arg| arg != &"--login"),
                );
            }

            builder
        }
    };

    builder.env(NERV_TERM, env!("CARGO_PKG_VERSION"));
    if env::var_os("TMUX").is_some() {
        builder.env("NERV_TERM_TMUX", env!("CARGO_PKG_VERSION"));
    }

    // Clean up environment and launch shell.
    builder.env_remove(NERV_SHELL);
    builder.env_remove("NERV_IS_LOGIN_SHELL");
    builder.env_remove("NERV_START_TEXT");
    builder.env_remove("NERV_SHELL_EXTRA_ARGS");
    builder.env_remove("NERV_EXECUTION_STRING");

    if let Ok(dir) = std::env::current_dir() {
        builder.cwd(dir);
    }

    Ok(builder)
}

#[cfg(unix)]
fn launch_shell(command: Option<&[String]>) -> Result<()> {
    let cmd = build_shell_command(command)?.as_command()?;
    let mut args: Vec<&OsStr> = std::vec![cmd.get_program()];
    args.extend(cmd.get_args());

    let cargs: Vec<_> = args
        .into_iter()
        .map(|arg| {
            CString::new(arg.to_string_lossy().as_ref()).expect("Failed to convert arg to CString")
        })
        .collect();
    for (key, val) in cmd.get_envs() {
        unsafe {
            match val {
                Some(value) => env::set_var(key, value),
                None => {
                    env::remove_var(key);
                }
            }
        }
    }

    execvp(&cargs[0], &cargs).expect("Failed to execvp");
    unreachable!()
}

fn figterm_main(command: Option<&[String]>) -> Result<()> {
    nerv_settings::settings::init_global().ok();
    // Telemetry stripped (PRD v0.6 §0.2).

    let context = Context::new();

    let session_id = match std::env::var("MOCK_NERV_PTY_SESSION_ID") {
        Ok(id) => id,
        Err(_) => uuid::Uuid::new_v4().simple().to_string(),
    };

    unsafe {
        std::env::set_var(NERV_PTY_SESSION_ID, &session_id);
    }

    let parent_id = nerv_os::Env::new().nerv_parent().ok();

    let mut terminal = SystemTerminal::new_from_stdio()?;
    let screen_size = terminal.get_screen_size()?;

    // Clamp to ≥1×1. A 0-row/0-col winsize (detached or not-yet-sized
    // terminal) makes the shadow terminal's grid panic on its
    // visible-lines assertion, so no zero dimension may reach it.
    let pty_size = PtySize {
        rows: (screen_size.rows as u16).max(1),
        cols: (screen_size.cols as u16).max(1),
        pixel_width: screen_size.xpixel as u16,
        pixel_height: screen_size.ypixel as u16,
    };

    let pty = open_pty(&pty_size).context("Failed to open pty")?;
    let command = build_shell_command(command)?;

    let pty_name = pty.slave.get_name().unwrap_or_else(|| session_id.clone());

    let _log_guard = match initialize_logging(LogArgs {
        log_level: None,
        log_to_stdout: false,
        log_file_path: Some(
            directories::logs_dir()?.join(format!("{PTY_BINARY_NAME}{pty_name}.log")),
        ),
        delete_old_log_file: true,
    }) {
        Ok(logger_guard) => Some(logger_guard),
        Err(err) => {
            if !nerv_settings::state::get_bool_or("pty.suppress_log_error", false) {
                // let id = capture_anyhow(&err);
                eprintln!("Fig failed to init logger: {err:?}");
            }
            None
        }
    };

    logger::stdio_debug_log(format!("pty name: {pty_name}"));
    logger::stdio_debug_log("Forking child shell process");

    #[cfg(unix)]
    {
        let pid = nix::unistd::getpid();
        logger::stdio_debug_log(format!("Parent pid: {pid}"));
    }

    let mut child = pty.slave.spawn_command(command)?;
    info!("Shell: {:?}", child.process_id());
    if let Some(pid) = child.process_id() {
        logger::stdio_debug_log(format!("Child pid: {pid}"));
    }

    let (child_tx, mut child_rx) = oneshot::channel();
    std::thread::spawn(move || child_tx.send(child.wait()));

    info!("Pid: {}", Pid::current());
    info!("Pty name: {pty_name}");

    let runtime = runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name_fn(|| {
            static ATOMIC_ID: std::sync::atomic::AtomicUsize =
                std::sync::atomic::AtomicUsize::new(0);
            let id = ATOMIC_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            format!("{PTY_BINARY_NAME}-runtime-worker-{id}")
        })
        .build()?;

    let runtime_result = runtime.block_on(async {
        update::check_for_update(&context);

        terminal.set_raw_mode()?;

        let (main_loop_tx, main_loop_rx) = flume::bounded::<MainLoopEvent>(16);

        let history_sender = history::spawn_history_task().await;

        // Spawn thread to handle figterm ipc
        let incoming_receiver = spawn_figterm_ipc(&session_id).await?;

        // Spawn thread to handle remote ipc
        let (remote_sender, remote_receiver, stop_ipc_tx) = spawn_remote_ipc(
            session_id.clone(),
            parent_id,
            main_loop_tx.clone()
        ).await?;

        let mut stdout = io::stdout();
        let mut master = pty.master.get_async_master_pty()?;

        let mut processor = Processor::new();
        let size = SizeInfo::new(pty_size.rows as usize, pty_size.cols as usize);
        let event_sender = EventHandler::new(remote_sender.clone(), history_sender.clone(), main_loop_tx.clone());
        let mut term = nerv_term::Term::new(size, event_sender, 1, session_id.clone());

        #[cfg(target_os = "windows")]
        term.set_windows_delay_end_prompt(true);

        let mut write_buffer: Vec<u8> = vec![0; BUFFER_SIZE];

        let mut key_interceptor = KeyInterceptor::new();
        key_interceptor.load_key_intercepts()?;

        let mut edit_buffer_interval = tokio::time::interval(Duration::from_millis(16));

        let mut first_time = true;

        let input_rx = terminal.read_input()?;

        let key_code_encode_mode = KeyCodeEncodeModes {
            #[cfg(unix)]
            encoding: KeyboardEncoding::Xterm,
            #[cfg(windows)]
            encoding: KeyboardEncoding::Win32,
            application_cursor_keys: false,
            newline_mode: false,
        };

        if let Ok(shell) = get_parent_shell() {
            let path = std::path::Path::new(&shell);
            let name = path.file_name().and_then(|name| name.to_str()).unwrap_or(shell.as_str());
            let title_osc = format!("\x1b]0;{name}\x07");
            if let Err(err) = stdout.write(title_osc.as_bytes()).await {
                error!("Failed to write title osc: {err}");
            }
        }

        let mut csi_u_set = false;

        // Phase 3a/3b: live inline-completion overlay (ghost + popup).
        let mut overlay = Overlay::default();

        let result: Result<()> = 'select_loop: loop {
            if first_time && term.shell_state().has_seen_prompt {
                trace!("Has seen prompt and first time");
                let initial_command = env::var("NERV_START_TEXT").ok().filter(|s| !s.is_empty());
                if let Some(mut initial_command) = initial_command {
                    debug!("Sending initial text: {initial_command}");
                    initial_command.push('\n');
                    if let Err(err) = master.write_all(initial_command.as_bytes()).await {
                        error!("Failed to write initial command: {err}");
                    }
                }
                first_time = false;
            }

            let select_result: Result<()> = select! {
                biased;
                res = main_loop_rx.recv_async() => {
                    match res {
                        Ok(event) => {
                            match event {
                                MainLoopEvent::Insert { insert, unlock, bracketed, execute } => {
                                    use bstr::ByteSlice;
                                    if bracketed {
                                        if term.mode().contains(nerv_term::term::TermMode::BRACKETED_PASTE) {
                                            master.write_all(b"\x1b[200~").await?;
                                            master.write_all(&insert.replace(b"\x1b", "")).await?;
                                            master.write_all(b"\x1b[201~").await?;
                                        } else {
                                            master.write_all(&insert.replace("\r\n", "\r").replace("\n", "\r")).await?;
                                        }
                                    } else {
                                        master.write_all(&insert).await?;
                                    }

                                    if execute {
                                        master.write_all(b"\r").await?; 
                                    }

                                    if unlock {
                                        key_interceptor.reset();
                                    }
                                },
                                MainLoopEvent::UnlockInterception => {
                                    key_interceptor.reset();
                                },
                                MainLoopEvent::SetImmediateMode(mode) => {
                                    if let Err(err) = terminal.set_immediate_mode(mode) {
                                        error!(%err, "Failed to set immediate mode");
                                    }
                                },
                                MainLoopEvent::SetCsiU => {
                                    // Send CSI > 1 u
                                    stdout.write_all(b"\x1b[>1u").await?;
                                    stdout.flush().await?;
                                    csi_u_set = true;
                                },
                                MainLoopEvent::UnsetCsiU => {
                                    // Send CSI < u
                                    stdout.write_all(b"\x1b[<u").await?;
                                    stdout.flush().await?;
                                    csi_u_set = false;
                                },
                                MainLoopEvent::PromptSSH { uuid: _, remote_host: _ } => {
                                    // let should_install = should_install_remote_ssh_integration(
                                    //     uuid,
                                    //     remote_host.clone(),
                                    //     main_loop_tx.clone(),
                                    //     remote_receiver.clone(),
                                    //     remote_sender.clone(),
                                    //     &term,
                                    //     &mut master,
                                    //     &mut key_interceptor,
                                    // ).await;

                                    // let should_install = match should_install {
                                    //     Some(val) => val,
                                    //     None => {
                                    //         prompt_remote_integration_install(
                                    //             remote_host,
                                    //             console_term.clone(),
                                    //             console_term_key_tx.clone(),
                                    //             &mut terminal,
                                    //             input_rx.clone(),
                                    //         ).await.unwrap_or(false)
                                    //     }
                                    // };

                                    // if should_install {
                                    //     let installation_command = "curl -fSsL https://fig.io/install-minimal.sh | bash; exec $SHELL\n";
                                    //     master.write_all(installation_command.as_bytes()).await?;
                                    // }
                                }
                            }
                        }
                        Err(err) => warn!("Failed to recv: {err}"),
                    };
                    Ok(())
                }
                res = input_rx.recv_async() => {
                    let mut input_res = Ok(());
                    match res {
                        Ok(events) => {
                            let mut write_buffer = BytesMut::new();
                            for event in events {
                                match event {
                                    Ok((raw, InputEvent::Key(event))) => {
                                        // Do not do most stuff during not preexec since that means a command is running
                                        let preexec = term.shell_state().preexec;

                                        debug!(?event, ?raw, %preexec,  "Got key event");

                                        // Phase 3a/3b: drive the inline overlay before the
                                        // key reaches the shell. Navigation, accept and
                                        // dismiss are consumed; everything else falls through.
                                        if !preexec
                                            && (overlay.popup.is_some() || overlay.ghost.is_some())
                                        {
                                            let cols = terminal
                                                .get_screen_size()
                                                .map(|s| s.cols.max(1))
                                                .unwrap_or(80);

                                            // Tab / Down → next, Shift-Tab / Up → prev,
                                            // PageDown / PageUp → jump by one window
                                            // (clamped, no wrap). (down, by_page) pairs.
                                            let nav = match (event.key, event.modifiers) {
                                                (KeyCode::DownArrow, _) => Some((true, false)),
                                                (KeyCode::Tab, m) if !m.contains(Modifiers::SHIFT) => {
                                                    Some((true, false))
                                                }
                                                (KeyCode::UpArrow, _) => Some((false, false)),
                                                (KeyCode::Tab, m) if m.contains(Modifiers::SHIFT) => {
                                                    Some((false, false))
                                                }
                                                (KeyCode::PageDown, _) => Some((true, true)),
                                                (KeyCode::PageUp, _) => Some((false, true)),
                                                _ => None,
                                            };
                                            if let (Some((down, by_page)), Some(p)) =
                                                (nav, overlay.popup.as_mut())
                                            {
                                                match (down, by_page) {
                                                    (true, false) => p.next(),
                                                    (false, false) => p.prev(),
                                                    (true, true) => p.page_next(),
                                                    (false, true) => p.page_prev(),
                                                }
                                                let ins = p.selected_item().insertion.clone();
                                                overlay.ghost =
                                                    ghost::compute_ghost(&overlay.buffer, &ins);
                                                draw_overlay(&mut stdout, &mut overlay, cols).await;
                                                continue;
                                            }

                                            // Right-arrow at end-of-line accepts the ghost.
                                            if event.key == KeyCode::RightArrow
                                                && event.modifiers == Modifiers::NONE
                                            {
                                                if let Some(rem) = overlay.ghost.take() {
                                                    // Frecency: record the accepted insertion so
                                                    // the next request can boost it (mirrors the
                                                    // M0 ZLE widget's `nerv _record`). The spec is
                                                    // the first word; the insertion is the popup
                                                    // selection, or the completed current token.
                                                    let spec = overlay
                                                        .buffer
                                                        .split_whitespace()
                                                        .next()
                                                        .unwrap_or("")
                                                        .to_string();
                                                    let insertion = match &overlay.popup {
                                                        Some(p) => p.selected_item().insertion.clone(),
                                                        None => {
                                                            let tok = overlay
                                                                .buffer
                                                                .rsplit(char::is_whitespace)
                                                                .next()
                                                                .unwrap_or("");
                                                            format!("{tok}{rem}")
                                                        }
                                                    };
                                                    if !spec.is_empty() && !insertion.is_empty() {
                                                        tokio::spawn(engine_client::record_accept(
                                                            spec, insertion,
                                                        ));
                                                    }
                                                    write_buffer.extend(rem.as_bytes());
                                                    continue;
                                                }
                                            }

                                            // Escape dismisses the overlay.
                                            if event.key == KeyCode::Escape {
                                                reset_overlay(&mut stdout, &mut overlay).await;
                                                key_interceptor.reset();
                                                continue;
                                            }
                                        }

                                        // if we are in CSI u mode we try to encode first, otherwise we try to send the raw bytes first
                                        let raw = if csi_u_set {
                                            event.key.encode(event.modifiers, key_code_encode_mode, true)
                                                .ok()
                                                .map(|s| s.into_bytes().into()).or(raw)
                                        } else {
                                            raw.or_else(|| {
                                                event.key.encode(event.modifiers, key_code_encode_mode, true)
                                                    .ok()
                                                    .map(|s| s.into_bytes().into())
                                            })
                                        };

                                        let handled_action = if !preexec {
                                            if let Some(action) = key_interceptor.intercept_key(&event) {
                                                debug!(?action, "Intercepted action");
                                                let s = raw.clone()
                                                    .and_then(|b| String::from_utf8(b.to_vec()).ok())
                                                    .unwrap_or_default();
                                                let context = shell_state_to_context(term.shell_state());
                                                let hook = nerv_proto::remote_hooks::new_intercepted_key_hook(context, action, s);
                                                remote_sender.send(hook_to_message(hook)).unwrap();

                                                if event.key == KeyCode::Escape {
                                                    key_interceptor.reset();
                                                }
                                                true
                                            } else {
                                                false
                                            }
                                        } else {
                                            false
                                        };

                                        if !handled_action {
                                            if let Some(bytes) = raw {
                                                if (event.key == KeyCode::Char('c') || event.key == KeyCode::Char('d'))
                                                    && event.modifiers == Modifiers::CTRL {
                                                    key_interceptor.reset();
                                                }
                                                write_buffer.extend(&bytes);
                                            }
                                        }
                                    }
                                    Ok((_, InputEvent::Resized)) => {
                                        terminal.flush()?;

                                        let size = terminal.get_screen_size()?;
                                        // Clamp to ≥1×1 (see open-pty note) so a
                                        // degenerate resize can't panic the grid.
                                        let rows = (size.rows as u16).max(1);
                                        let cols = (size.cols as u16).max(1);
                                        let pty_size = PtySize {
                                            rows,
                                            cols,
                                            pixel_width: size.xpixel as u16,
                                            pixel_height: size.ypixel as u16,
                                        };

                                        master.resize(pty_size)?;
                                        let window_size =
                                            SizeInfo::new(rows as usize, cols as usize);
                                        debug!("Window size changed: {window_size:?}");
                                        term.resize(window_size);
                                    }
                                    Ok((None, InputEvent::Paste(string))) => {
                                        // Pass through bracketed pastes.
                                        if term.mode().contains(nerv_term::term::TermMode::BRACKETED_PASTE) {
                                            write_buffer.extend(b"\x1b[200~");
                                            write_buffer.extend(string.replace('\x1b', "").as_bytes());
                                            write_buffer.extend(b"\x1b[201~");
                                        } else {
                                            write_buffer.extend(string.replace("\r\n", "\r").replace('\n', "\r").as_bytes());
                                        }
                                    }
                                    Ok((raw, _)) => {
                                        if let Some(raw) = raw {
                                            info!("Fallback write");
                                            write_buffer.extend(&raw);
                                        } else {
                                            info!("Unhandled input event with no raw pass-through data");
                                        }
                                    }
                                    Err(err) => {
                                        error!("Failed receiving input from stdin: {err}");
                                        input_res = Err(err);
                                        break;
                                    }
                                };
                            }
                            master.write_all(&write_buffer).await?;
                        }
                        Err(err) => {
                            warn!("Failed recv: {err}");
                        }
                    };
                    input_res
                }
                res = master.read(&mut write_buffer) => {
                    #[cfg(feature = "profiling_early_exit")]
                    break 'select_loop Ok(());
                    match res {
                        Ok(0) => {
                            trace!("EOF from master");
                            break 'select_loop Ok(());
                        },
                        Ok(size) => {
                            trace!("Read {size} bytes from master");

                            let old_delayed_count = term.get_delayed_events_count();
                            for byte in &write_buffer[..size] {
                                processor.advance(&mut term, *byte);
                            }

                            let delayed_count = term.get_delayed_events_count();

                            // We have delayed events and did not receive delayed events. Flush all
                            // delayed events now.
                            if delayed_count > 0 && delayed_count == old_delayed_count {
                                term.flush_delayed_events();
                            }

                            stdout.write_all(&write_buffer[..size]).await?;
                            stdout.flush().await?;

                            if write_buffer.capacity() == write_buffer.len() {
                                write_buffer.reserve(write_buffer.len());
                            }

                            if can_send_edit_buffer(&term) {
                                let cursor_coordinates = get_cursor_coordinates(&terminal);
                                if let Err(err) = send_edit_buffer(&term, &remote_sender, cursor_coordinates).await {
                                    warn!("Failed to send edit buffer: {err}");
                                }
                                // Phase 3a/3b: refresh ghost + popup from nervd
                                // and repaint. Popup window mirrors the M0
                                // widget: LINES-6, clamped to [3,10].
                                let (cols, max_vis) = match terminal.get_screen_size() {
                                    Ok(s) => (s.cols.max(1), s.rows.saturating_sub(6).clamp(3, 10)),
                                    Err(_) => (80, 5),
                                };
                                refresh_suggestions(&term, &mut overlay, max_vis).await;
                                draw_overlay(&mut stdout, &mut overlay, cols).await;
                            } else if overlay.drawn > 0 || overlay.ghost.is_some() {
                                // A command started running (preexec) — clear.
                                reset_overlay(&mut stdout, &mut overlay).await;
                            }

                            Ok(())
                        }
                        Err(err) => {
                            error!("Failed to read from master: {err}");
                            break 'select_loop Ok(());
                        }
                    }
                }
                msg = remote_receiver.recv_async() => {
                    match msg {
                        Ok(message) => {
                            trace!("Received message from socket: {message:?}");
                            process_remote_message(
                                message,
                                main_loop_tx.clone(),
                                remote_sender.clone(),
                                &term,
                                &mut master,
                                &mut key_interceptor
                            ).await?;
                        }
                        Err(err) => {
                            error!("Failed to receive message from socket: {err}");
                        }
                    }
                    Ok(())
                }
                msg = incoming_receiver.recv_async() => {
                    match msg {
                        Ok((message, sender)) => {
                            debug!("Received message from figterm listener: {message:?}");
                            process_figterm_message(
                                message,
                                main_loop_tx.clone(),
                                sender.clone(),
                                &term,
                                &history_sender,
                                &mut master,
                                &mut key_interceptor,
                                &session_id,
                            ).await?;
                        }
                        Err(err) => {
                            error!("Failed to receive message from socket: {err}");
                        }
                    }
                    Ok(())
                }
                // Check if to send the edit buffer because of timeout
                _ = edit_buffer_interval.tick() => {
                    let send_eb = INSERTION_LOCKED_AT.read().unwrap().is_some();
                    if send_eb && can_send_edit_buffer(&term) {
                        let cursor_coordinates = get_cursor_coordinates(&terminal);
                        if let Err(err) = send_edit_buffer(&term, &remote_sender, cursor_coordinates).await {
                            warn!(%err, "Failed to send edit buffer");
                        }
                    }
                    Ok(())
                }
                _ = &mut child_rx => {
                    trace!("Shell process exited");
                    break 'select_loop Ok(());
                }
            };

            if let Err(err) = select_result {
                error!("Error in select loop: {err}");
                break 'select_loop Err(err);
            }
        };

        let _ = stop_ipc_tx.send(());
        // Telemetry stripped (PRD v0.6 §0.2).

        result
    });

    // Reading from stdin is a blocking task on a separate thread:
    // https://github.com/tokio-rs/tokio/issues/2466
    // We must explicitly shutdown the runtime to exit.
    // This can cause resource leaks if we aren't careful about tasks we spawn.
    runtime.shutdown_background();

    // attempt cleanup
    #[cfg(target_os = "linux")]
    cleanup::cleanup()?;

    runtime_result
}

fn main() {
    let cli = Cli::parse();
    let command = cli.command.as_deref();

    logger::stdio_debug_log(format!("{NERV_LOG_LEVEL}={}", nerv_log::get_log_level()));

    if !state::get_bool_or("pty.enabled", true) {
        println!("[NOTE] qterm is disabled. Autocomplete will not work.");
        logger::stdio_debug_log("qterm is disabled. `qterm.enabled` == false");
        return;
    }

    match figterm_main(command) {
        Ok(()) => {
            info!("Exiting");
        }
        Err(err) => {
            error!("Error in async runtime: {err}");
            println!("{PRODUCT_NAME} had an Error!: {err:?}");
            // capture_anyhow(&err);

            // Fallback to normal shell
            #[cfg(unix)]
            if let Err(err) = launch_shell(command) {
                // capture_anyhow(&err);
                logger::stdio_debug_log(err.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autocomplete_enabled_test() {
        assert!(autocomplete_enabled(&Env::new_fake()));
        assert!(autocomplete_enabled(&Env::from_slice(&[(
            NERV_DISABLE_AUTOCOMPLETE,
            ""
        )])));
        assert!(!autocomplete_enabled(&Env::from_slice(&[(
            NERV_DISABLE_AUTOCOMPLETE,
            "1"
        )])));
        assert!(!autocomplete_enabled(&Env::from_slice(&[(
            NERV_DISABLE_AUTOCOMPLETE,
            "1"
        )])));
    }
}
