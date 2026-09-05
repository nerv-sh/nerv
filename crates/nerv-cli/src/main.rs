//! `nerv` — command-line entry point.
//!
//! Five user-facing commands (PLAN.md §9):
//! - `nerv init zsh`     — emit shell hook for `eval` or `>> ~/.zshrc`
//! - `nerv doctor`       — environment diagnostics
//! - `nerv start|stop`   — daemon lifecycle
//! - `nerv spec list`    — list bundled specs
//! - `nerv uninstall`    — trace-zero removal (docs/uninstall-spec.md)
//!
//! Intentionally absent in v1.0: `telemetry`, `update`, `spec install`,
//! `spec update`, `spec dev`, `feedback`, `config`. Single command surface
//! is part of the trust contract (PLAN.md §9 / §0 GO 조건 ①).
//!
//! Internal (hidden) commands:
//! - `nerv _complete`    — IPC bridge for ZLE widget (M0-1 PoC)

use clap::{Parser, Subcommand};
use nerv_engine::{Generator, Response, SpecRegistry, Subcommand as SpecNode, Suggestion, paths};

#[derive(Parser, Debug)]
#[command(
    name = "nerv",
    version,
    about = "Inline shell autocomplete for macOS zsh — without the AWS login.",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Print the shell integration script. Use with `eval "$(nerv init zsh)"`
    /// or append to `~/.zshrc` via the marker block (uninstall-spec.md §3).
    Init {
        #[arg(value_enum)]
        shell: Shell,
        /// Internal: emit the raw shell hook (called from inside the
        /// marker block). Users normally don't pass this.
        #[arg(long, hide = true)]
        shell_script: bool,
    },
    /// Diagnose the local Nerv installation. Returns non-zero exit code
    /// if any error-state condition is present (docs/error-states.md §5).
    Doctor,
    /// Start the `nervd` background daemon.
    Start,
    /// Stop the `nervd` background daemon.
    Stop,
    /// Spec-related subcommands.
    Spec {
        #[command(subcommand)]
        cmd: SpecCmd,
    },
    /// Remove every trace of Nerv: marker blocks, daemon, caches, configs.
    ///
    /// See docs/uninstall-spec.md for the full acceptance contract.
    Uninstall {
        /// Preserve `~/.config/nerv/` while removing everything else.
        #[arg(long)]
        keep_config: bool,
        /// Suppress stdout output (logs are always written).
        #[arg(long)]
        quiet: bool,
    },
    /// Internal: IPC bridge for the ZLE widget. Not user-facing.
    #[command(name = "_complete", hide = true)]
    InternalComplete {
        /// The input line (LBUFFER from zsh).
        line: String,
        /// Cursor byte offset within line.
        cursor: usize,
    },
    /// Internal: record an accepted suggestion for frecency ranking.
    /// Called fire-and-forget by the widget on Tab/Enter accept.
    #[command(name = "_record", hide = true)]
    InternalRecord {
        /// Top-level binary name (e.g. `git`).
        spec: String,
        /// The insertion string the user committed.
        insertion: String,
    },
}

#[derive(Subcommand, Debug)]
enum SpecCmd {
    /// List all bundled specs with their tier and (limited) flag.
    List,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy)]
enum Shell {
    Zsh,
    /// bash reaches autocomplete only through the PTY shim (no ZLE).
    Bash,
    /// fish reaches autocomplete only through the PTY shim.
    Fish,
}

fn main() -> anyhow::Result<()> {
    // Rust's runtime ignores SIGPIPE by default, which turns
    // `nerv spec list | head` into a panic. Restore Unix default
    // so the CLI exits silently when its stdout closes mid-write.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    // Per-keystroke hot path: the ZLE widget forks `nerv _complete <line>
    // <cursor>` on every character. Dispatch it before init_tracing and the
    // full clap tree build — neither buys anything for the bridge. A shape
    // mismatch falls through to clap so error messages stay identical.
    {
        let args: Vec<String> = std::env::args().collect();
        if args.len() == 4 && args[1] == "_complete" {
            if let Ok(cursor) = args[3].parse::<usize>() {
                return cmd_internal_complete(&args[2], cursor);
            }
        }
    }

    init_tracing();

    let cli = Cli::parse();
    match cli.command {
        Command::Init {
            shell,
            shell_script,
        } => cmd_init(shell, shell_script),
        Command::Doctor => cmd_doctor(),
        Command::Start => cmd_start(),
        Command::Stop => cmd_stop(),
        Command::Spec { cmd } => match cmd {
            SpecCmd::List => cmd_spec_list(),
        },
        Command::Uninstall { keep_config, quiet } => cmd_uninstall(keep_config, quiet),
        Command::InternalComplete { line, cursor } => cmd_internal_complete(&line, cursor),
        Command::InternalRecord { spec, insertion } => cmd_internal_record(&spec, &insertion),
    }
}

fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_env("NERV_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

// ---------- command stubs (M0–M1) ----------

fn cmd_init(shell: Shell, shell_script: bool) -> anyhow::Result<()> {
    match shell {
        Shell::Zsh => {
            if shell_script {
                // Inner hook — called from inside the marker block at zsh
                // startup. Validate environment + warn on conflicts here,
                // since this is the moment we actually have ZSH_VERSION set.
                if let Err(msg) = check_zsh_env_compat() {
                    // E3 (error-states §3.3): zsh < 5.8 or no zsh. Exit 0
                    // with empty stdout so `eval` doesn't break the user's
                    // shell, but write the warning to stderr.
                    eprintln!("{msg}");
                    return Ok(());
                }
                warn_widget_conflicts(); // E4 — non-blocking notices

                let bin = std::env::current_exe()?.to_string_lossy().into_owned();
                println!("export NERV_BIN={bin:?}");
                // NERV_PTY=1 opts the user into the figterm-style PTY
                // shim (PLAN §5.8). Emit the PTY bootstrap script
                // INSTEAD OF the ZLE widget; the widget itself
                // self-skips when NERV_PTY is set, but emitting both
                // wastes bytes and confuses `nerv doctor`. The PTY
                // path is mutually exclusive with the widget by
                // CLAUDE.md §4 invariant.
                let pty_mode = std::env::var_os("NERV_PTY").is_some();
                if pty_mode {
                    if let Some(pty_bin) = resolve_pty_bin_for_init(&bin) {
                        println!("export NERV_PTY_BIN={pty_bin:?}");
                    }
                }
                print!("{}", init_snippet_for_zsh(pty_mode));
                Ok(())
            } else {
                // Outer: install/refresh the ~/.zshrc marker block
                // (idempotent — first-5-min §0.5-C), then emit it on
                // stdout too, so `eval "$(nerv init zsh)"` both persists
                // the hook AND activates it in the current session. The
                // inner shell_script invocation does the env validation
                // each session, where ZSH_VERSION etc. are actually set.
                let bin = std::env::current_exe()?.to_string_lossy().into_owned();
                let block = nerv_shell::init_block(
                    &bin,
                    env!("CARGO_PKG_VERSION"),
                    &installed_at_rfc3339(),
                    "zsh",
                );
                if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
                    apply_init_block(&home, "zsh", &block);
                }
                print!("{block}");
                Ok(())
            }
        }
        Shell::Bash => cmd_init_pty_only(shell_script, PtyShell::Bash),
        Shell::Fish => cmd_init_pty_only(shell_script, PtyShell::Fish),
    }
}

/// The rc file `nerv init <shell>` manages, relative to `$HOME`. Must
/// stay inside `SHELL_INIT_FILES` — doctor and uninstall only scan that
/// list, so writing anywhere else would orphan the block.
fn rc_file_for_shell(shell_name: &str) -> Option<&'static str> {
    match shell_name {
        "zsh" => Some(".zshrc"),
        "bash" => Some(".bashrc"),
        "fish" => Some(".config/fish/config.fish"),
        _ => None,
    }
}

/// Install/refresh the marker block in the shell's rc file, idempotently
/// (first-5-min §0.5-C): absent → append; same version + binary → leave
/// the file untouched (silent — eval-in-rc users hit this every shell
/// startup); anything else → strip all blocks, append one fresh copy.
/// Failures degrade to a grey warning: stdout still carries the block,
/// so `eval "$(nerv init <shell>)"` keeps working this session even when
/// the rc isn't writable.
fn apply_init_block(home: &std::path::Path, shell_name: &str, block: &str) {
    use nerv_shell::UpsertAction;
    let Some(rel) = rc_file_for_shell(shell_name) else {
        return;
    };
    let rc = home.join(rel);
    let existing = std::fs::read_to_string(&rc).unwrap_or_default();
    let up = nerv_shell::upsert_block(&existing, block);
    if up.action == UpsertAction::Current {
        return;
    }
    if let Err(e) = write_rc_atomic(&rc, &up.content) {
        eprintln!("[nerv] cannot write ~/{rel} ({e}) — append the printed block manually");
        return;
    }
    match up.action {
        UpsertAction::Installed => eprintln!("nerv: installed init block into ~/{rel}"),
        UpsertAction::Updated { from } => eprintln!(
            "nerv: updated existing init block (v{} → v{})",
            from.as_deref().unwrap_or("unknown"),
            env!("CARGO_PKG_VERSION"),
        ),
        UpsertAction::Current => unreachable!("returned above"),
    }
}

/// Atomic rc write: temp file in the same directory + rename, so a shell
/// mid-way through sourcing the file keeps reading the old inode and
/// never sees a half-written rc (uninstall-spec §4 step 3 semantics).
/// Preserves the original file's permissions across the inode swap.
fn write_rc_atomic(rc: &std::path::Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = rc.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file_name = rc
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| std::io::Error::other("rc path has no file name"))?;
    let tmp = rc.with_file_name(format!("{file_name}.nerv-tmp"));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
    }
    if let Ok(meta) = std::fs::metadata(rc) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, rc)
}

/// Shells that reach autocomplete only through the PTY shim (no ZLE).
#[derive(Clone, Copy)]
enum PtyShell {
    Bash,
    Fish,
}

impl PtyShell {
    fn name(self) -> &'static str {
        match self {
            PtyShell::Bash => "bash",
            PtyShell::Fish => "fish",
        }
    }

    /// The `export`/`set` line that pins NERV_PTY_BIN, in the shell's own
    /// syntax. fish uses `set -gx`, POSIX shells use `export`.
    fn export_pty_bin(self, pty_bin: &str) -> String {
        match self {
            PtyShell::Bash => format!("export NERV_PTY_BIN={pty_bin:?}"),
            PtyShell::Fish => format!("set -gx NERV_PTY_BIN {pty_bin:?}"),
        }
    }

    fn snippet(self) -> &'static str {
        match self {
            PtyShell::Bash => include_str!("../../../shell-integrations/bash/_nerv-pty.bash"),
            PtyShell::Fish => include_str!("../../../shell-integrations/fish/_nerv-pty.fish"),
        }
    }
}

/// bash/fish integration. Unlike zsh there is no ZLE widget path — these
/// shells get inline autocomplete only via the PTY shim (PLAN §6.2), so
/// the inner hook emits the PTY bootstrap when `NERV_PTY=1` and otherwise
/// prints a one-line note (the sourced output stays empty so shell
/// startup is unaffected).
fn cmd_init_pty_only(shell_script: bool, shell: PtyShell) -> anyhow::Result<()> {
    let bin = std::env::current_exe()?.to_string_lossy().into_owned();
    if !shell_script {
        // Outer rc block; same markers as zsh so the uninstaller strips
        // every shell's block identically. Installed/refreshed in the
        // shell's rc (idempotent) and echoed for the eval form.
        let block = nerv_shell::init_block(
            &bin,
            env!("CARGO_PKG_VERSION"),
            &installed_at_rfc3339(),
            shell.name(),
        );
        if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
            apply_init_block(&home, shell.name(), &block);
        }
        print!("{block}");
        return Ok(());
    }

    if std::env::var_os("NERV_PTY").is_none() {
        // E-tone note (error-states §4): one grey line, no apology.
        eprintln!(
            "[nerv] {0} autocomplete requires NERV_PTY=1 — export NERV_PTY=1 before launching {0}.",
            shell.name()
        );
        return Ok(());
    }
    if let Some(pty_bin) = resolve_pty_bin_for_init(&bin) {
        println!("{}", shell.export_pty_bin(&pty_bin));
    }
    print!("{}", shell.snippet());
    Ok(())
}

/// Returns the zsh integration snippet to emit for the inner hook.
/// Two flavors: the default ZLE widget (`_nerv.zsh`) and the
/// PTY-shim bootstrap (`_nerv-pty.zsh`). The PTY flavor activates
/// only when `NERV_PTY=1` was set in the parent environment.
fn init_snippet_for_zsh(pty_mode: bool) -> &'static str {
    if pty_mode {
        include_str!("../../../shell-integrations/zsh/_nerv-pty.zsh")
    } else {
        include_str!("../../../shell-integrations/zsh/_nerv.zsh")
    }
}

/// Locate the `nerv-pty` binary that ships next to the `nerv` CLI.
/// `bin` is the path to the running `nerv` executable. We look for a
/// sibling named `nerv-pty` (Homebrew + cargo install both place
/// them in the same dir). When the sibling exists we export the
/// absolute path so the shim doesn't depend on PATH — important
/// inside an interactive shell with a freshly-stripped PATH.
fn resolve_pty_bin_for_init(bin: &str) -> Option<String> {
    let parent = std::path::Path::new(bin).parent()?;
    let candidate = parent.join("nerv-pty");
    if candidate.exists() {
        Some(candidate.to_string_lossy().into_owned())
    } else {
        None
    }
}

/// E3 (error-states.md §3.3): require zsh ≥ 5.8. Returns an error
/// message ready for stderr if the environment is wrong.
///
/// `$ZSH_VERSION` is a zsh-internal special parameter — NOT exported
/// to child processes by default. So when the user does
/// `eval "$(nerv init zsh)"`, we get an empty value even though
/// we're being sourced from a perfectly fine zsh. Fall back to
/// asking zsh directly via `$SHELL --version`.
fn check_zsh_env_compat() -> Result<(), String> {
    let ver = std::env::var("ZSH_VERSION")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(detect_zsh_version_via_shell)
        .unwrap_or_default();
    if ver.is_empty() {
        return Err("[nerv] zsh required (no ZSH_VERSION env var detected,\n\
                    and $SHELL did not point to a usable zsh).\n\
                    nerv init zsh must be sourced from inside an interactive zsh session,\n\
                    or run: ZSH_VERSION=\"$ZSH_VERSION\" eval \"$(nerv init zsh)\""
            .into());
    }
    let (major, minor) = parse_zsh_version(&ver);
    if major > 5 || (major == 5 && minor >= 8) {
        return Ok(());
    }
    Err(format!(
        "[nerv] zsh 5.8+ required (current: {ver}) — please upgrade.\n\
         Suggested: brew install zsh && chsh -s $(brew --prefix)/bin/zsh\n\
         Autocomplete will be disabled until zsh is upgraded."
    ))
}

/// Fallback for the `eval "$(nerv init zsh)"` case where $ZSH_VERSION
/// isn't exported. If $SHELL ends in `zsh`, exec it with `--version`
/// and parse the output. `zsh --version` prints: `zsh 5.9 (arm-...)`
fn detect_zsh_version_via_shell() -> Option<String> {
    let shell = std::env::var("SHELL").ok()?;
    if !shell.ends_with("/zsh") && shell != "zsh" {
        return None;
    }
    let out = std::process::Command::new(&shell)
        .arg("--version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace().nth(1).map(|s| s.to_string())
}

/// E4 (error-states.md §3.4): scan env for known widget-conflict
/// markers and emit one stderr line per detection. Non-blocking —
/// the hook installs normally; user can decide to coexist.
fn warn_widget_conflicts() {
    let candidates: &[(&str, &str)] = &[
        ("ZSH_AUTOSUGGEST_USE_ASYNC", "zsh-autosuggestions"),
        ("_FZF_COMPLETION_DIR", "fzf completion"),
        ("FZF_DEFAULT_OPTS", "fzf (general)"),
        ("STARSHIP_SHELL", "starship prompt (no conflict, info only)"),
    ];
    for (env_var, tool) in candidates {
        if std::env::var_os(env_var).is_some() {
            eprintln!(
                "[nerv] detected {tool} — Nerv runs alongside but key bindings may conflict.\n\
                 See: https://nerv.sh/docs/conflicts"
            );
        }
    }
}

fn cmd_doctor() -> anyhow::Result<()> {
    let report = build_doctor_report();
    println!("nerv doctor");
    println!();
    for entry in &report.entries {
        let prefix = match entry.level {
            DoctorLevel::Ok => "✓",
            DoctorLevel::Warn => "⚠",
            DoctorLevel::Err => "✗",
        };
        println!("  {prefix} {:<22} {}", entry.label, entry.detail);
        if let Some(hint) = &entry.hint {
            println!("                           → {hint}");
        }
    }
    println!();
    let (ok, warn, err) = report.counts();
    println!("Result: {ok} OK, {warn} warning, {err} error");
    if err > 0 {
        std::process::exit(1);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum DoctorLevel {
    Ok,
    Warn,
    Err,
}

#[derive(Debug, Clone)]
struct DoctorEntry {
    level: DoctorLevel,
    label: String,
    detail: String,
    hint: Option<String>,
}

#[derive(Debug, Default)]
struct DoctorReport {
    entries: Vec<DoctorEntry>,
}

impl DoctorReport {
    fn push(&mut self, level: DoctorLevel, label: &str, detail: String, hint: Option<String>) {
        self.entries.push(DoctorEntry {
            level,
            label: label.into(),
            detail,
            hint,
        });
    }

    fn counts(&self) -> (usize, usize, usize) {
        let mut o = 0;
        let mut w = 0;
        let mut e = 0;
        for entry in &self.entries {
            match entry.level {
                DoctorLevel::Ok => o += 1,
                DoctorLevel::Warn => w += 1,
                DoctorLevel::Err => e += 1,
            }
        }
        (o, w, e)
    }
}

fn build_doctor_report() -> DoctorReport {
    let mut r = DoctorReport::default();
    check_zsh_version(&mut r);
    check_shell_hook(&mut r);
    check_daemon(&mut r);
    check_specs(&mut r);
    check_spec_misses(&mut r);
    check_schema_version(&mut r);
    check_pty_mode(&mut r);
    r
}

/// Commands the daemon completed empty for want of a spec. Advisory
/// (never red): it is the pointer toward writing an overlay spec, not a
/// fault. Silent when nothing has been recorded — a fresh install shows
/// no row at all.
fn check_spec_misses(r: &mut DoctorReport) {
    let Some(path) = paths::misses_path() else {
        return;
    };
    check_spec_misses_in(r, &path);
}

/// Path-injected half of [`check_spec_misses`], so tests exercise the
/// row without touching the real cache dir.
fn check_spec_misses_in(r: &mut DoctorReport, path: &std::path::Path) {
    let top = nerv_engine::misses::MissCounter::load(path).top_n(5);
    if top.is_empty() {
        return;
    }
    let detail = top
        .iter()
        .map(|(name, count)| format!("{name} {count}"))
        .collect::<Vec<_>>()
        .join(", ");
    let hint = paths::user_specs_dir().map(|d| format!("add a spec in {}", d.display()));
    r.push(DoctorLevel::Ok, "spec misses", detail, hint);
}

/// E5: spec cache schema version vs the daemon's supported version
/// (error-states.md §3.5). A mismatch is a blocking (red) row.
fn check_schema_version(r: &mut DoctorReport) {
    // env override → populated user cache → bundled (brew share/ or
    // tarball specs/) → user path. Same chain the daemon reads.
    let Some(layers) = paths::resolve_spec_layers() else {
        return;
    };
    match nerv_engine::manifest::check_schema(&layers.primary) {
        nerv_engine::manifest::SchemaStatus::Ok => r.push(
            DoctorLevel::Ok,
            "spec schema",
            format!("v{}", nerv_engine::manifest::SUPPORTED_SCHEMA_VERSION),
            None,
        ),
        // Missing manifest is non-fatal (pre-manifest install); stay quiet
        // rather than nag — E2/specs row already covers an empty cache.
        nerv_engine::manifest::SchemaStatus::Missing => {}
        nerv_engine::manifest::SchemaStatus::Mismatch { found } => r.push(
            DoctorLevel::Err,
            "spec schema",
            format!(
                "mismatch — daemon expects v{}, found v{found}",
                nerv_engine::manifest::SUPPORTED_SCHEMA_VERSION
            ),
            Some("run: brew reinstall nerv".into()),
        ),
    }
}

/// PLAN §5.8 PTY shim is M1 opt-in via `NERV_PTY=1`. Doctor
/// reports the active state so users can confirm their env flag
/// took effect AND that the `nerv-pty` binary the shim execs is
/// actually present. Silent when NERV_PTY isn't set — the default
/// ZLE-widget path doesn't need this row.
fn check_pty_mode(r: &mut DoctorReport) {
    if std::env::var_os("NERV_PTY").is_none() {
        return;
    }
    // Inside the PTY shim already? Re-entry detector — the wrapper
    // exports NERV_PTY_SESSION_ID before re-execing zsh under
    // itself, so non-empty means doctor is running INSIDE the
    // shim's child shell (expected).
    let inside_shim = std::env::var_os("NERV_PTY_SESSION_ID").is_some();

    // Resolve nerv-pty: prefer NERV_PTY_BIN (set by the init hook),
    // else look for a sibling of the running nerv binary, else
    // fall back to PATH.
    let from_env = std::env::var_os("NERV_PTY_BIN").map(std::path::PathBuf::from);
    let from_sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.parent().map(|d| {
                d.join(if cfg!(windows) {
                    "nerv-pty.exe"
                } else {
                    "nerv-pty"
                })
            })
        })
        .filter(|p| p.exists());
    let from_path = which("nerv-pty").ok();

    match from_env
        .filter(|p| p.exists())
        .or(from_sibling)
        .or(from_path)
    {
        Some(p) => {
            let detail = if inside_shim {
                format!("active, binary at {} (running inside shim)", p.display())
            } else {
                format!("opt-in set, binary at {}", p.display())
            };
            r.push(DoctorLevel::Ok, "pty shim", detail, None);
        }
        None => {
            r.push(
                DoctorLevel::Err,
                "pty shim",
                "NERV_PTY=1 but nerv-pty binary not found".into(),
                Some("unset NERV_PTY or install nerv-pty (brew reinstall nerv)".into()),
            );
        }
    }
}

/// Minimal PATH lookup. Returns the first matching executable.
fn which(name: &str) -> Result<std::path::PathBuf, ()> {
    let path = std::env::var_os("PATH").ok_or(())?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(())
}

/// E3: zsh version ≥ 5.8.
fn check_zsh_version(r: &mut DoctorReport) {
    use std::process::Command;
    let out = match Command::new("zsh").arg("--version").output() {
        Ok(o) if o.status.success() => o,
        _ => {
            r.push(
                DoctorLevel::Err,
                "zsh version",
                "zsh not found on PATH".into(),
                Some("install zsh (5.8+)".into()),
            );
            return;
        }
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let version = text.split_whitespace().nth(1).unwrap_or("?").to_string();
    let (major, minor) = parse_zsh_version(&version);
    if major > 5 || (major == 5 && minor >= 8) {
        r.push(
            DoctorLevel::Ok,
            "zsh version",
            format!("{version} (>= 5.8)"),
            None,
        );
    } else {
        r.push(
            DoctorLevel::Err,
            "zsh version",
            format!("{version} (< 5.8)"),
            Some("upgrade: brew upgrade zsh".into()),
        );
    }
}

fn parse_zsh_version(s: &str) -> (u32, u32) {
    let mut it = s.split('.');
    let major = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let minor = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    (major, minor)
}

/// Every shell init file `nerv init {zsh,bash,fish}` may write its marker
/// block into. Single source of truth for both the doctor hook check and the
/// uninstall strip — they must agree on where a hook can live, or one covers a
/// shell the other misses. Paths are relative to `$HOME`; fish's is nested.
const SHELL_INIT_FILES: [&str; 8] = [
    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".zlogin",
    ".bashrc",
    ".bash_profile",
    ".profile",
    ".config/fish/config.fish",
];

/// Marker block presence across every shell's init files (zsh/bash/fish).
fn check_shell_hook(r: &mut DoctorReport) {
    let Some(home) = std::env::var_os("HOME") else {
        r.push(DoctorLevel::Err, "shell hook", "HOME unset".into(), None);
        return;
    };
    let home = std::path::PathBuf::from(home);
    // A block per file is fine (a dual-shell user installs into each); only
    // >1 block in the *same* file is the not-idempotent case worth flagging.
    let mut found: Vec<&str> = Vec::new();
    let mut dup: Option<(&str, usize)> = None;
    for name in SHELL_INIT_FILES {
        let Ok(content) = std::fs::read_to_string(home.join(name)) else {
            continue;
        };
        let count = nerv_shell::count_blocks(&content);
        if count == 0 {
            continue;
        }
        found.push(name);
        if count > 1 && dup.is_none() {
            dup = Some((name, count));
        }
    }
    if let Some((file, n)) = dup {
        r.push(
            DoctorLevel::Warn,
            "shell hook",
            format!("{n} marker blocks in ~/{file} (should be 1)"),
            Some("run: nerv uninstall && nerv init <shell>".into()),
        );
        return;
    }
    if found.is_empty() {
        r.push(
            DoctorLevel::Warn,
            "shell hook",
            "no nerv marker block found".into(),
            Some("run: nerv init <zsh|bash|fish> >> <rc>".into()),
        );
        return;
    }
    let files = found
        .iter()
        .map(|f| format!("~/{f}"))
        .collect::<Vec<_>>()
        .join(", ");
    r.push(
        DoctorLevel::Ok,
        "shell hook",
        format!("marker block in {files} (멱등 OK)"),
        None,
    );
}

/// E1: nervd running.
///
/// Ground truth is whether the daemon answers on its UDS socket — that is
/// exactly the E1 trigger the ZLE widget sees (`connect(2)` succeeds + a
/// `Ping` is answered), per error-states.md §3.1. The PID file is a
/// secondary artifact: a daemon started outside `nerv start` (or one whose
/// PID file was cleaned up while it kept serving) is alive without one, so a
/// PID-file-only check reports a false "not running" and sends the user to
/// spawn a duplicate. Probe the socket first; use the PID file only for the
/// diagnostic detail and to distinguish a stale PID from a clean absence.
fn check_daemon(r: &mut DoctorReport) {
    let pid = paths::pid_path().and_then(|p| read_pid(&p));
    let responds = paths::socket_path()
        .map(|s| daemon_responds_at(&s))
        .unwrap_or(false);
    if responds {
        let detail = match pid {
            Some(pid) => format!("nervd running (pid {pid})"),
            None => "nervd running".into(),
        };
        r.push(DoctorLevel::Ok, "daemon", detail, None);
        return;
    }
    // Socket silent — fall back to the PID file for a precise message.
    match pid {
        Some(pid) if process_alive(pid) => r.push(
            DoctorLevel::Err,
            "daemon",
            format!("nervd process alive (pid {pid}) but socket unresponsive"),
            Some("run: nerv stop && nerv start".into()),
        ),
        Some(pid) => r.push(
            DoctorLevel::Err,
            "daemon",
            format!("stale PID file (pid {pid} not alive)"),
            Some("run: nerv start".into()),
        ),
        None => r.push(
            DoctorLevel::Err,
            "daemon",
            "nervd not running".into(),
            Some("run: nerv start".into()),
        ),
    }
}

/// Synchronous liveness probe: connect to the daemon's UDS socket at `sock`
/// and send a `Ping`, returning true iff it answers with a `pong`. Short
/// timeouts keep `nerv doctor` snappy when the socket file is present but
/// nothing is listening.
fn daemon_responds_at(sock: &std::path::Path) -> bool {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let Ok(mut stream) = UnixStream::connect(sock) else {
        return false;
    };
    let timeout = Some(std::time::Duration::from_millis(500));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);
    if stream.write_all(b"{\"method\":\"ping\"}\n").is_err() {
        return false;
    }
    let mut buf = [0u8; 128];
    match stream.read(&mut buf) {
        Ok(n) if n > 0 => std::str::from_utf8(&buf[..n])
            .map(|s| s.contains("pong"))
            .unwrap_or(false),
        _ => false,
    }
}

/// Ask the daemon for its PID over the socket: send a `Ping` and parse the
/// `pid` out of the `pong` reply. Returns None if nothing answers, the reply
/// isn't a pong, or it carries no usable pid (an older daemon predating the
/// `pid` field decodes it as 0 via `#[serde(default)]` — those are stoppable
/// only through the PID file). Lets `nerv stop` / `uninstall` terminate a live
/// daemon whose PID file is missing or stale.
fn daemon_pid_via_socket(sock: &std::path::Path) -> Option<u32> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(sock).ok()?;
    let timeout = Some(std::time::Duration::from_millis(500));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);
    stream.write_all(b"{\"method\":\"ping\"}\n").ok()?;
    let mut buf = [0u8; 256];
    let n = stream.read(&mut buf).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&buf[..n]).ok()?;
    if v.get("kind")?.as_str()? != "pong" {
        return None;
    }
    match v.get("pid")?.as_u64()? {
        0 => None, // older daemon (serde default) — no usable pid
        p => Some(p as u32),
    }
}

/// E2 + E5: specs loaded + parse errors.
fn check_specs(r: &mut DoctorReport) {
    // Same layered chain the daemon reads (overlay → env/user cache →
    // bundled), so doctor reports the specs completions actually use.
    let Some(layers) = paths::resolve_spec_layers() else {
        r.push(DoctorLevel::Err, "specs", "HOME unset".into(), None);
        return;
    };
    check_specs_in(r, &layers);
}

/// Doctor rows for a concrete layer list (`resolve_spec_layers` shape:
/// primary last, optional user overlay first). Split out so the rows are
/// unit-testable against tempdirs without touching HOME.
fn check_specs_in(r: &mut DoctorReport, layers: &paths::SpecLayers) {
    let primary = &layers.primary;
    if !primary.exists() {
        r.push(
            DoctorLevel::Warn,
            "specs",
            format!("dir missing: {}", primary.display()),
            Some("reinstall nerv (brew reinstall nerv) or run build-specs".into()),
        );
        return;
    }
    // Doctor eager-scans (load_dirs) so it can report parse errors up
    // front, rather than waiting for a user keystroke to surface E2.
    let (registry, errs) = SpecRegistry::load_dirs(&layers.dirs());
    let count = registry.len();
    // The overlay (if any) gets its own row: a broken user file must read
    // as "your file", not as a corrupt install. Every error names its file,
    // so split on which layer the file lives in.
    let overlay = layers.overlay.as_deref();
    let (overlay_errs, primary_errs): (Vec<_>, Vec<_>) = errs
        .iter()
        .partition(|e| overlay.is_some_and(|o| std::path::Path::new(e.path()).starts_with(o)));
    if errs.is_empty() && count == 0 {
        r.push(
            DoctorLevel::Warn,
            "specs",
            "0 specs loaded".into(),
            Some(format!("populate {}", primary.display())),
        );
        return;
    }
    if !primary_errs.is_empty() {
        r.push(
            DoctorLevel::Err,
            "spec health",
            format!("{} disabled: {}", primary_errs.len(), primary_errs[0]),
            Some("run: brew reinstall nerv".into()),
        );
    }
    r.push(DoctorLevel::Ok, "specs", format!("{count} loaded"), None);
    if let Some(o) = overlay {
        let served = registry.stems_served_from(o).len();
        if overlay_errs.is_empty() {
            r.push(
                DoctorLevel::Ok,
                "user specs",
                format!("{served} in {}", o.display()),
                None,
            );
        } else {
            r.push(
                DoctorLevel::Err,
                "user specs",
                format!("{} disabled: {}", overlay_errs.len(), overlay_errs[0]),
                Some(format!("fix or remove that file in {}", o.display())),
            );
        }
    }
}

fn cmd_start() -> anyhow::Result<()> {
    use anyhow::Context;
    use std::fs;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    let pid_path = paths::pid_path().context("HOME unset")?;
    let cache_dir = paths::cache_dir().context("HOME unset")?;
    let log_path = paths::daemon_log_path().context("HOME unset")?;

    // A daemon may already be serving on the socket without a readable PID
    // file (started outside this path, or the file was removed while it kept
    // running). Spawning anyway rebinds the socket and orphans the live
    // daemon, so probe the socket first — it's the real readiness signal
    // (same as `nerv doctor` / the ZLE widget), independent of the PID file.
    if let Some(sock) = paths::socket_path() {
        if daemon_responds_at(&sock) {
            match read_pid(&pid_path) {
                Some(pid) => println!("nervd already running (pid {pid})"),
                None => println!("nervd already running"),
            }
            return Ok(());
        }
    }

    if let Some(pid) = read_pid(&pid_path) {
        if process_alive(pid) {
            println!("nervd already running (pid {pid})");
            return Ok(());
        }
        let _ = fs::remove_file(&pid_path);
    }

    fs::create_dir_all(&cache_dir).with_context(|| format!("mkdir {}", cache_dir.display()))?;

    // Serialize the probe→spawn critical section across processes.
    // Every new shell autostarts `nerv start` in the background, so two
    // terminals opened together race: both probe before either daemon
    // has bound the socket, both spawn, and the second daemon's
    // stale-socket cleanup steals the first's listener — leaving an
    // orphaned nervd no PID file points at (a trace uninstall can't
    // see). An exclusive flock makes the loser wait; its re-probe below
    // then finds the winner's socket and no-ops. The lock file lives in
    // the cache dir, so uninstall's cache sweep removes it. flock is
    // advisory and best-effort: on failure we fall through to the old
    // racy-but-rare behavior rather than blocking startup.
    let _start_lock = fs::File::create(cache_dir.join("nervd.start.lock"))
        .inspect(|f| {
            #[cfg(unix)]
            {
                use std::os::unix::io::AsRawFd;
                unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
            }
        })
        .ok();
    // Double-check under the lock: the starter we waited on may have
    // just brought the daemon up.
    if let Some(sock) = paths::socket_path() {
        if daemon_responds_at(&sock) {
            match read_pid(&pid_path) {
                Some(pid) => println!("nervd already running (pid {pid})"),
                None => println!("nervd already running"),
            }
            return Ok(());
        }
    }
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open {}", log_path.display()))?;
    let log_err = log.try_clone()?;

    let bin = resolve_nervd_path()?;
    let mut cmd = Command::new(&bin);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    // Put the daemon in its own process group so a Ctrl-C in the
    // launching shell (or any later interactive shell that shares the
    // pgrp) doesn't deliver SIGINT to the daemon too. Without this
    // the daemon inherited the shell's pgrp and silently died the
    // first time a user hit Ctrl-C — popup stopped appearing until
    // the user re-ran `nerv start`.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn()
        .with_context(|| format!("spawn {}", bin.display()))?;

    // Wait briefly for the daemon to write its PID file (signals readiness).
    for _ in 0..30 {
        if pid_path.exists() {
            let pid = read_pid(&pid_path).unwrap_or(0);
            println!("nervd started (pid {pid})");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!(
        "nervd did not start within 3s (check {})",
        log_path.display()
    )
}

fn cmd_stop() -> anyhow::Result<()> {
    use anyhow::Context;
    use std::fs;
    use std::time::Duration;

    let pid_path = paths::pid_path().context("HOME unset")?;
    let file_pid = read_pid(&pid_path);

    // Resolve the daemon PID: prefer the PID file, but fall back to asking
    // the daemon over its socket. A live daemon whose PID file was removed
    // (or that was started outside `nerv start`) is still stoppable.
    let pid = match file_pid {
        Some(pid) if process_alive(pid) => pid,
        _ => match paths::socket_path().and_then(|s| daemon_pid_via_socket(&s)) {
            Some(pid) => pid,
            None => {
                if file_pid.is_some() {
                    let _ = fs::remove_file(&pid_path);
                    println!("nervd not running (stale PID file removed)");
                } else {
                    println!("nervd not running");
                }
                return Ok(());
            }
        },
    };

    #[cfg(unix)]
    {
        // SAFETY: SIGTERM to a known PID, no UB.
        let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            anyhow::bail!("kill(SIGTERM, {pid}): {err}");
        }
    }
    #[cfg(not(unix))]
    {
        anyhow::bail!("nerv stop: only Unix supported in v1.0");
    }

    for _ in 0..50 {
        if !process_alive(pid) {
            println!("nervd stopped (pid {pid})");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!("nervd did not stop within 5s after SIGTERM");
}

/// Find the `nervd` binary path: $NERV_DAEMON_BIN override, or the
/// sibling of the current `nerv` executable.
fn resolve_nervd_path() -> anyhow::Result<std::path::PathBuf> {
    use anyhow::Context;
    if let Some(p) = std::env::var_os("NERV_DAEMON_BIN") {
        return Ok(p.into());
    }
    let exe = std::env::current_exe().context("locate nerv binary")?;
    let dir = exe
        .parent()
        .context("nerv binary has no parent directory")?;
    let candidate = dir.join("nervd");
    if candidate.exists() {
        return Ok(candidate);
    }
    anyhow::bail!(
        "nervd binary not found alongside nerv (looked at {}). \
         set $NERV_DAEMON_BIN to override.",
        candidate.display()
    );
}

fn read_pid(path: &std::path::Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    // kill(pid, 0) returns 0 iff the process exists and we can signal
    // it; errno=ESRCH means no such process.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    false
}

fn cmd_spec_list() -> anyhow::Result<()> {
    let layers = paths::resolve_spec_layers()
        .ok_or_else(|| anyhow::anyhow!("HOME unset and NERV_SPECS_DIR not set"))?;
    for line in spec_list_lines(&layers) {
        println!("{line}");
    }
    Ok(())
}

/// The `nerv spec list` table for a concrete layer list (primary last,
/// optional user overlay first): union of every layer, one row per stem,
/// overlay-served stems marked `*` after TIER with a legend at the end.
/// Rows that fail to load are skipped with a stderr note (doctor has the
/// detail). Pure over `layers` so it is unit-testable.
fn spec_list_lines(layers: &paths::SpecLayers) -> Vec<String> {
    let primary = &layers.primary;
    if !primary.exists() {
        return vec![format!("(no specs at {})", primary.display())];
    }
    let registry = SpecRegistry::at_dirs(&layers.dirs());
    let names = registry.dir_listing();
    if names.is_empty() {
        return vec![format!("(no specs found in {})", primary.display())];
    }
    let overlay = layers.overlay.as_deref();
    let from_overlay: std::collections::HashSet<String> = overlay
        .map(|o| registry.stems_served_from(o).into_iter().collect())
        .unwrap_or_default();

    let mut lines = vec![format!("{:<18} {:>5} {:>5}  TIER", "NAME", "SUBS", "OPTS")];
    for name in names {
        let Some(spec) = registry.lookup(&name) else {
            eprintln!("  ⚠ {name}: load error (run nerv doctor)");
            continue;
        };
        let (subs, opts) = count_tree(spec.as_ref());
        let tier = compute_tier(spec.as_ref());
        let mark = if from_overlay.contains(&name) {
            "*"
        } else {
            ""
        };
        lines.push(format!("{name:<18} {subs:>5} {opts:>5}  {tier}{mark}"));
    }
    if let Some(o) = overlay {
        lines.push(String::new());
        lines.push(format!("* = served from {} (user overlay)", o.display()));
    }
    lines
}

/// Recursive count of subcommands + options across the whole spec tree.
/// `subs` excludes the root itself.
fn count_tree(node: &SpecNode) -> (usize, usize) {
    let mut subs = 0;
    let mut opts = node.options.len();
    for child in &node.subcommands {
        subs += 1;
        let (cs, co) = count_tree(child);
        subs += cs;
        opts += co;
    }
    (subs, opts)
}

/// Classify a spec by the heaviest generator it contains. Static
/// specs are Tier A. Static + Template generator (shell command) is
/// Tier B (limited). Anything requiring a JS closure (Custom, or
/// Script with post-process) is Tier C — M1 rquickjs only.
fn compute_tier(node: &SpecNode) -> &'static str {
    let mut tier = 'A';
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        for opt in &n.options {
            for arg in &opt.args {
                tier = upgrade_tier(tier, arg.generators.iter());
            }
        }
        for arg in &n.args {
            tier = upgrade_tier(tier, arg.generators.iter());
        }
        for child in &n.subcommands {
            stack.push(child);
        }
    }
    match tier {
        'A' => "A",
        'B' => "B (limited)",
        _ => "C (M1)",
    }
}

fn upgrade_tier<'a>(current: char, gens: impl IntoIterator<Item = &'a Generator>) -> char {
    let mut t = current;
    for g in gens {
        let g_tier = match g {
            Generator::Template { .. } => 'B',
            Generator::Script {
                has_post_process: false,
                ..
            } => 'B',
            // PackageJsonScripts + Filepaths are well-known Tier C
            // recovered to Tier B by a native Rust path (see complete.rs).
            Generator::PackageJsonScripts => 'B',
            Generator::Filepaths { .. } => 'B',
            Generator::ZoxideQuery => 'B',
            Generator::SshHosts => 'B',
            Generator::MakefileTargets => 'B',
            Generator::ManPages => 'B',
            Generator::PackageJsonDeps => 'B',
            Generator::KubectlResources => 'B',
            Generator::CargoTargets { .. } => 'B',
            Generator::ScriptWithJsonPath { .. } => 'B',
            Generator::AwsList { .. } => 'B',
            Generator::Script { .. } | Generator::Custom { .. } => 'C',
        };
        if (g_tier == 'B' && t == 'A') || g_tier == 'C' {
            t = g_tier;
        }
    }
    t
}

fn cmd_uninstall(keep_config: bool, quiet: bool) -> anyhow::Result<()> {
    use anyhow::Context;
    use std::fs;
    use std::path::PathBuf;

    let mut log = UninstallLog::new(quiet);

    // Step 1-2: stop daemon (best-effort, idempotent)
    let stopped = stop_daemon_for_uninstall(&mut log);
    let _ = stopped;

    // Step 3: shell-hook removal (atomic, with timestamped backup)
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let backup_path = match &home {
        Some(h) => strip_shell_hooks(h, &mut log)?,
        None => {
            log.warn("shell hook", "HOME unset");
            None
        }
    };

    // Step 4: cache dir
    if let Some(dir) = paths::cache_dir() {
        match fs::remove_dir_all(&dir) {
            Ok(()) => log.ok("cache", format!("removed {}", dir.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                log.ok("cache", "already absent".into());
            }
            Err(e) => log.warn("cache", &format!("{e}")),
        }
    }

    // Step 5: log dir
    if let Some(dir) = paths::log_dir() {
        match fs::remove_dir_all(&dir) {
            Ok(()) => log.ok("logs", format!("removed {}", dir.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                log.ok("logs", "already absent".into());
            }
            Err(e) => log.warn("logs", &format!("{e}")),
        }
    }

    // Step 6: config dir
    if keep_config {
        log.ok("config", "preserved (--keep-config)".into());
    } else if let Some(dir) = paths::config_dir() {
        match fs::remove_dir_all(&dir) {
            Ok(()) => log.ok("config", format!("removed {}", dir.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                log.ok("config", "already absent".into());
            }
            Err(e) => log.warn("config", &format!("{e}")),
        }
    }

    // Step 8: summary
    log.finish(backup_path.as_deref());
    if log.warnings() > 0 {
        std::process::exit(1);
    }
    let _ = home; // hush unused on no-HOME branch
    Ok::<(), anyhow::Error>(()).context("uninstall")
}

/// Stop the daemon as part of uninstall — SIGTERM with timeout, then
/// SIGKILL fallback. Logs to the provided UninstallLog.
fn stop_daemon_for_uninstall(log: &mut UninstallLog) -> bool {
    use std::time::Duration;

    let Some(pid_path) = paths::pid_path() else {
        log.warn("daemon", "HOME unset");
        return false;
    };
    // Prefer the PID file; fall back to the socket so a live daemon with a
    // missing/stale PID file is still stopped — uninstall must leave zero
    // trace (uninstall-spec.md §4), and a surviving daemon is a trace.
    let file_pid = read_pid(&pid_path);
    let pid = match file_pid {
        Some(pid) if process_alive(pid) => pid,
        _ => match paths::socket_path().and_then(|s| daemon_pid_via_socket(&s)) {
            Some(pid) => pid,
            None => {
                match file_pid {
                    Some(pid) => log.ok("daemon", format!("stale pid {pid} ignored")),
                    None => log.ok("daemon", "not running".into()),
                }
                return true;
            }
        },
    };

    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        for _ in 0..50 {
            if !process_alive(pid) {
                log.ok("daemon", format!("stopped (pid {pid})"));
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        // SIGKILL fallback
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        for _ in 0..10 {
            if !process_alive(pid) {
                log.ok("daemon", format!("killed (pid {pid})"));
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        log.warn("daemon", &format!("pid {pid} still alive after SIGKILL"));
        false
    }
    #[cfg(not(unix))]
    {
        log.warn("daemon", "non-unix not supported");
        false
    }
}

/// Step 3 of uninstall-spec.md: scan zsh init files, strip marker
/// blocks, write atomically, leave a timestamped backup behind.
/// Returns the backup path of the first file actually modified.
fn strip_shell_hooks(
    home: &std::path::Path,
    log: &mut UninstallLog,
) -> anyhow::Result<Option<std::path::PathBuf>> {
    use std::fs;

    let mut first_backup: Option<std::path::PathBuf> = None;
    let mut total_blocks_removed = 0usize;

    // Scan every shell's init file — see [`SHELL_INIT_FILES`]. `nerv init
    // {zsh,bash,fish}` all emit the same marker block, so a leftover bash/fish
    // hook would run `eval "$(nerv …)"` on shell start after the binary is gone
    // → command-not-found on every new shell (uninstall-spec §3a / §114).
    for name in SHELL_INIT_FILES {
        let path = home.join(name);
        if !path.exists() {
            continue;
        }
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                log.warn("shell hook", &format!("read {}: {e}", path.display()));
                continue;
            }
        };
        let count = nerv_shell::count_blocks(&content);
        if count == 0 {
            continue;
        }
        let stripped = nerv_shell::strip_blocks(&content);
        let backup = path.with_extension(format!("nerv-backup-{}", chrono_like_timestamp()));
        if let Err(e) = fs::copy(&path, &backup) {
            log.warn("shell hook", &format!("backup {}: {e}", backup.display()));
            continue;
        }
        // Atomic: write to temp file in same dir, then rename.
        let tmp = path.with_extension("nerv-tmp");
        if let Err(e) = fs::write(&tmp, &stripped) {
            log.warn("shell hook", &format!("temp write: {e}"));
            let _ = fs::remove_file(&tmp);
            continue;
        }
        if let Err(e) = fs::rename(&tmp, &path) {
            log.warn("shell hook", &format!("rename: {e}"));
            let _ = fs::remove_file(&tmp);
            continue;
        }
        total_blocks_removed += count;
        if first_backup.is_none() {
            first_backup = Some(backup);
        }
    }

    if total_blocks_removed > 0 {
        log.ok(
            "shell hook",
            format!("removed {total_blocks_removed} block(s)"),
        );
    } else {
        log.ok("shell hook", "no marker blocks found".into());
    }
    Ok(first_backup)
}

/// Minimal timestamp generator: YYYY-MM-DDTHH-MM-SS (filesystem-safe).
/// Uses SystemTime to avoid pulling chrono in.
/// RFC 3339 / ISO-8601 UTC timestamp for the marker block's
/// `# Installed:` line (e.g. `2026-06-07T08:30:00Z`). Falls back to
/// `"unknown"` if formatting somehow fails.
fn installed_at_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string())
}

fn chrono_like_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Format as unix-seconds — readable, monotonic, sortable.
    secs.to_string()
}

struct UninstallLog {
    quiet: bool,
    ok_count: usize,
    warn_count: usize,
}

impl UninstallLog {
    fn new(quiet: bool) -> Self {
        Self {
            quiet,
            ok_count: 0,
            warn_count: 0,
        }
    }
    fn ok(&mut self, label: &str, detail: String) {
        self.ok_count += 1;
        if !self.quiet {
            println!("  ✓ {label:<14} {detail}");
        }
    }
    fn warn(&mut self, label: &str, detail: &str) {
        self.warn_count += 1;
        if !self.quiet {
            eprintln!("  ⚠ {label:<14} {detail}");
        }
    }
    fn warnings(&self) -> usize {
        self.warn_count
    }
    fn finish(&self, backup: Option<&std::path::Path>) {
        if self.quiet {
            return;
        }
        if self.warn_count == 0 {
            match backup {
                Some(p) => println!("nerv removed. backup: {}", p.display()),
                None => println!("nerv removed."),
            }
        } else {
            let total = self.ok_count + self.warn_count;
            let ok = self.ok_count;
            let warn = self.warn_count;
            println!("nerv: {ok}/{total} steps OK, {warn} warning — see above");
        }
    }
}

// ---------- internal: _complete (M0-1 IPC bridge) ----------

fn cmd_internal_complete(line: &str, cursor: usize) -> anyhow::Result<()> {
    let req = nerv_engine::Request::Complete {
        line: line.to_string(),
        cursor,
        // Capture the client's cwd so filesystem-aware generators
        // (package.json scripts, etc.) see the user's working dir,
        // not the daemon's. Falls back to None on error so the
        // daemon picks its own cwd as a last resort.
        cwd: std::env::current_dir()
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string())),
    };

    match nerv_engine::ipc_client::query_sync(&req)? {
        Response::Suggestions { items } => {
            for s in &items {
                print_suggestion(s);
            }
        }
        // E5: a schema-mismatch reason exits 3 (distinct from the daemon-
        // down failure) so the ZLE widget can show its one-line hint.
        Response::Empty { reason: Some(r) } if r.starts_with("spec schema mismatch") => {
            eprintln!("[nerv] {r}");
            std::process::exit(3);
        }
        // Any other response → no output → no popup in zsh.
        _ => {}
    }
    Ok(())
}

/// The popup footer shows a single clamped line, so shipping a full
/// multi-hundred-char description (aws service blurbs run 500–2000 chars)
/// is pure IPC + zsh-scan overhead — with ~600 aws subcommands it turned
/// a `aws ` completion into a 376 KB response that the widget then
/// re-scanned on every keystroke. Collapse tabs/newlines (they'd corrupt
/// the tab-separated wire format) and cap the length; the widget clamps
/// to the box width anyway.
fn wire_desc(desc: &str) -> String {
    const MAX: usize = 200;
    let cleaned = wire_field(desc);
    if cleaned.chars().count() <= MAX {
        cleaned
    } else {
        let mut out: String = cleaned.chars().take(MAX).collect();
        out.push('…');
        out
    }
}

/// Collapse tab/newline/CR to a space. The wire format is tab-separated
/// with one suggestion per line, so a stray tab or newline in ANY field
/// (a generator that echoes `git remote -v`'s `origin\t<url>`, a spec
/// with a multi-line description, …) shifts every field after it and
/// tears the popup box. Sanitising every field at the wire boundary makes
/// the format robust no matter what a generator returns.
fn wire_field(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

fn print_suggestion(s: &Suggestion) {
    // Wire format: insertion \t display \t description \t icon
    // Icon is empty string when None. Widget renders icon as a
    // prefix glyph to the display column.
    let insertion = wire_field(&s.insertion);
    let display = wire_field(&s.display);
    let desc = wire_desc(s.description.as_deref().unwrap_or(""));
    let icon = wire_field(s.icon.as_deref().unwrap_or(""));
    println!("{insertion}\t{display}\t{desc}\t{icon}");
}

fn cmd_internal_record(spec: &str, insertion: &str) -> anyhow::Result<()> {
    let req = nerv_engine::Request::RecordAccept {
        spec: spec.to_string(),
        insertion: insertion.to_string(),
    };
    // We don't act on the ack body, just let the daemon record + close.
    nerv_engine::ipc_client::query_sync(&req)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_init_block_installs_then_noops_then_updates() {
        // The `eval "$(nerv init zsh)"` contract (first-5-min §0.5-C):
        // fresh rc → block appended; re-run same version → file
        // byte-identical (mtime-stable no-op); version bump → single
        // block replaced in place, user lines intact.
        let tmp = std::env::temp_dir().join(format!("nerv-apply-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let rc = tmp.join(".zshrc");
        std::fs::write(&rc, "alias ll='ls -la'\n").unwrap();

        let v1 = nerv_shell::init_block("/opt/homebrew/bin/nerv", "0.9.0", "t1", "zsh");
        apply_init_block(&tmp, "zsh", &v1);
        let after_install = std::fs::read_to_string(&rc).unwrap();
        assert_eq!(nerv_shell::count_blocks(&after_install), 1);
        assert!(after_install.starts_with("alias ll='ls -la'\n"));

        // Same version + bin, new timestamp → must not rewrite the file.
        let v1_again = nerv_shell::init_block("/opt/homebrew/bin/nerv", "0.9.0", "t2", "zsh");
        apply_init_block(&tmp, "zsh", &v1_again);
        assert_eq!(std::fs::read_to_string(&rc).unwrap(), after_install);

        // Version bump → exactly one block, new version, user line kept.
        let v2 = nerv_shell::init_block("/opt/homebrew/bin/nerv", "1.0.0", "t3", "zsh");
        apply_init_block(&tmp, "zsh", &v2);
        let after_update = std::fs::read_to_string(&rc).unwrap();
        assert_eq!(nerv_shell::count_blocks(&after_update), 1);
        assert!(after_update.contains("Version: 1.0.0"));
        assert!(!after_update.contains("Version: 0.9.0"));
        assert!(after_update.contains("alias ll='ls -la'"));
        // No temp residue from the atomic write.
        assert!(!tmp.join(".zshrc.nerv-tmp").exists());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn apply_init_block_creates_fish_config_dir() {
        // fish's rc is nested (.config/fish/config.fish); apply must
        // create the parent chain on a pristine home.
        let tmp = std::env::temp_dir().join(format!("nerv-applyfish-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let blk = nerv_shell::init_block("/opt/homebrew/bin/nerv", "1.0.0", "t", "fish");
        apply_init_block(&tmp, "fish", &blk);
        let rc = tmp.join(".config/fish/config.fish");
        let content = std::fs::read_to_string(&rc).unwrap();
        assert_eq!(nerv_shell::count_blocks(&content), 1);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn rc_file_for_shell_stays_inside_doctor_scan_list() {
        // Contract: init may only write where doctor/uninstall scan,
        // or the block would be orphaned.
        for shell in ["zsh", "bash", "fish"] {
            let rel = rc_file_for_shell(shell).expect("known shell");
            assert!(
                SHELL_INIT_FILES.contains(&rel),
                "{rel} not in SHELL_INIT_FILES"
            );
        }
        assert_eq!(rc_file_for_shell("tcsh"), None);
    }

    #[test]
    fn installed_at_rfc3339_is_well_formed() {
        let ts = installed_at_rfc3339();
        // RFC 3339 UTC: contains the date/time separator and a zone.
        assert!(ts.contains('T'), "missing T separator: {ts}");
        assert!(ts.ends_with('Z') || ts.contains('+'), "missing zone: {ts}");
        assert!(ts.starts_with("20"), "implausible year: {ts}");
    }

    #[test]
    fn chrono_like_timestamp_is_numeric_and_growing() {
        let t1 = chrono_like_timestamp();
        std::thread::sleep(std::time::Duration::from_secs(1));
        let t2 = chrono_like_timestamp();
        assert!(
            t1.parse::<u64>().is_ok(),
            "timestamp not parseable as u64: {t1}"
        );
        let n1: u64 = t1.parse().unwrap();
        let n2: u64 = t2.parse().unwrap();
        assert!(n2 >= n1, "{n2} should be >= {n1}");
    }

    #[test]
    fn strip_shell_hooks_removes_block_and_creates_backup() {
        // Isolated HOME so we don't touch the real ~/.zshrc.
        let tmp = std::env::temp_dir().join(format!("nerv-strip-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let zshrc = tmp.join(".zshrc");
        let original = format!(
            "alias ll='ls -la'\n{}# trailing user comment\n",
            nerv_shell::init_block("/opt/homebrew/bin/nerv", "0.1.0", "ts", "zsh")
        );
        std::fs::write(&zshrc, &original).unwrap();

        let mut log = UninstallLog::new(true);
        let backup = strip_shell_hooks(&tmp, &mut log).unwrap();
        assert!(backup.is_some(), "expected backup PathBuf");
        let backup_path = backup.unwrap();
        assert!(backup_path.exists(), "backup file should exist");
        assert_eq!(
            std::fs::read_to_string(&backup_path).unwrap(),
            original,
            "backup must hold the pre-strip content verbatim"
        );

        // Post-strip .zshrc has no marker block but keeps user code.
        let after = std::fs::read_to_string(&zshrc).unwrap();
        assert_eq!(nerv_shell::count_blocks(&after), 0);
        assert!(after.contains("alias ll='ls -la'"));
        assert!(after.contains("# trailing user comment"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn strip_shell_hooks_covers_bash_and_fish() {
        // `nerv init` supports bash + fish; uninstall must strip their marker
        // blocks too, or the leftover `eval "$(nerv …)"` breaks every new
        // shell once the binary is gone (uninstall-spec §3a / §114).
        let tmp = std::env::temp_dir().join(format!("nerv-strip-bf-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join(".config/fish")).unwrap();
        let bashrc = tmp.join(".bashrc");
        let fishcfg = tmp.join(".config/fish/config.fish");
        std::fs::write(
            &bashrc,
            format!(
                "alias ll=ls\n{}",
                nerv_shell::init_block("/opt/homebrew/bin/nerv", "0.1.0", "ts", "bash")
            ),
        )
        .unwrap();
        std::fs::write(
            &fishcfg,
            format!(
                "set -gx FOO 1\n{}",
                nerv_shell::init_block("/opt/homebrew/bin/nerv", "0.1.0", "ts", "fish")
            ),
        )
        .unwrap();

        let mut log = UninstallLog::new(true);
        strip_shell_hooks(&tmp, &mut log).unwrap();

        let bash_after = std::fs::read_to_string(&bashrc).unwrap();
        assert_eq!(
            nerv_shell::count_blocks(&bash_after),
            0,
            "bash hook block must be stripped"
        );
        assert!(bash_after.contains("alias ll=ls"));
        let fish_after = std::fs::read_to_string(&fishcfg).unwrap();
        assert_eq!(
            nerv_shell::count_blocks(&fish_after),
            0,
            "fish hook block must be stripped"
        );
        assert!(fish_after.contains("set -gx FOO 1"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn strip_shell_hooks_no_op_when_no_blocks() {
        let tmp = std::env::temp_dir().join(format!("nerv-strip-noop-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let zshrc = tmp.join(".zshrc");
        let body = "alias x=ls\n";
        std::fs::write(&zshrc, body).unwrap();
        let mut log = UninstallLog::new(true);
        let backup = strip_shell_hooks(&tmp, &mut log).unwrap();
        assert!(backup.is_none(), "no backup when nothing to strip");
        assert_eq!(std::fs::read_to_string(&zshrc).unwrap(), body);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn strip_shell_hooks_handles_missing_home_files() {
        let tmp = std::env::temp_dir().join(format!("nerv-strip-empty-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let mut log = UninstallLog::new(true);
        let backup = strip_shell_hooks(&tmp, &mut log).unwrap();
        assert!(backup.is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `parse_zsh_version` accepts the `zsh --version`-style output —
    /// digits separated by dots, ignoring suffixes. E3 (zsh < 5.8
    /// hint) gates entirely on this parser.
    #[test]
    fn parse_zsh_version_handles_common_shapes() {
        assert_eq!(parse_zsh_version("5.8"), (5, 8));
        assert_eq!(parse_zsh_version("5.9.1"), (5, 9));
        assert_eq!(parse_zsh_version("5.8.2-1"), (5, 8));
        assert_eq!(parse_zsh_version("4.3.11"), (4, 3));
    }

    /// Garbage / empty input falls back to (0, 0) — the doctor table
    /// then prints "unknown" and the E3 gate stays open. Don't panic.
    #[test]
    fn parse_zsh_version_falls_back_on_garbage() {
        assert_eq!(parse_zsh_version(""), (0, 0));
        assert_eq!(parse_zsh_version("five.eight"), (0, 0));
        assert_eq!(parse_zsh_version("nope"), (0, 0));
        // Leading non-numeric major still degrades cleanly.
        assert_eq!(parse_zsh_version("v5.8"), (0, 8));
    }

    /// `read_pid` parses the PID file written by `nerv start`.
    /// Whitespace and trailing newlines are trimmed before parsing.
    #[test]
    fn read_pid_parses_clean_file() {
        let tmp = std::env::temp_dir().join(format!("nerv-pid-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("nervd.pid");
        std::fs::write(&path, "12345\n").unwrap();
        assert_eq!(read_pid(&path), Some(12345));
        // No trailing whitespace.
        std::fs::write(&path, "67890").unwrap();
        assert_eq!(read_pid(&path), Some(67890));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Missing PID file / garbage contents must return None, never
    /// panic. The uninstall path uses None to mean "daemon not
    /// running".
    #[test]
    fn read_pid_returns_none_on_garbage_or_missing() {
        let tmp = std::env::temp_dir().join(format!("nerv-pid-bad-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("nervd.pid");
        // Missing entirely.
        assert_eq!(read_pid(&tmp.join("nope")), None);
        // Non-numeric content.
        std::fs::write(&path, "garbage\n").unwrap();
        assert_eq!(read_pid(&path), None);
        // Empty.
        std::fs::write(&path, "").unwrap();
        assert_eq!(read_pid(&path), None);
        // Negative — u32::parse rejects.
        std::fs::write(&path, "-1\n").unwrap();
        assert_eq!(read_pid(&path), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `process_alive(0)` on unix: kill(0,0) sends to the current
    /// process group; succeeds when called from the test binary
    /// itself. Verify the wrapper returns true for the test process
    /// own PID, and false for an obviously-dead PID.
    #[cfg(unix)]
    #[test]
    fn process_alive_self_and_dead() {
        let self_pid = std::process::id();
        assert!(process_alive(self_pid), "test process should be alive");
        // A PID just above current is statistically unlikely to map
        // to a live process on a sleep-light CI host. Skip the
        // assertion if it happens to be alive (no false-positive).
        let probe = u32::MAX - 1;
        // u32::MAX - 1 is rejected by some kernels as invalid; expect
        // false either way (the kill syscall returns -1 / ESRCH).
        assert!(!process_alive(probe));
    }

    /// A missing socket file means the daemon is not listening — probe
    /// returns false without erroring (the common "not started" case).
    #[cfg(unix)]
    #[test]
    fn daemon_responds_at_false_when_no_socket() {
        let sock =
            std::path::PathBuf::from(format!("/tmp/nerv-doctor-none-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        assert!(!daemon_responds_at(&sock));
    }

    /// A live daemon answers a `Ping` with a `pong` — regression for the
    /// PID-file-only false negative (daemon serving without a PID file).
    #[cfg(unix)]
    #[test]
    fn daemon_responds_at_true_on_pong() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;
        let sock =
            std::path::PathBuf::from(format!("/tmp/nerv-doctor-pong-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 64];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"{\"kind\":\"pong\",\"version\":\"0.1.0\"}\n");
            }
        });
        assert!(daemon_responds_at(&sock));
        let _ = handle.join();
        let _ = std::fs::remove_file(&sock);
    }

    /// A socket that accepts but answers with something other than a
    /// `pong` (e.g. a foreign process) must read as "not the daemon".
    #[cfg(unix)]
    #[test]
    fn daemon_responds_at_false_on_non_pong() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;
        let sock = std::path::PathBuf::from(format!(
            "/tmp/nerv-doctor-garbage-{}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 64];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"nope\n");
            }
        });
        assert!(!daemon_responds_at(&sock));
        let _ = handle.join();
        let _ = std::fs::remove_file(&sock);
    }

    /// A one-shot `pong` server for the `daemon_pid_via_socket` tests:
    /// accepts one connection, ignores the request, replies with `reply`.
    #[cfg(unix)]
    fn pong_server(tag: &str, reply: &'static str) -> std::path::PathBuf {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;
        let sock = std::path::PathBuf::from(format!(
            "/tmp/nerv-pidsock-{tag}-{}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 64];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        sock
    }

    /// `daemon_pid_via_socket` extracts the daemon PID from a `pong` so
    /// `nerv stop` / `uninstall` can signal a daemon with no PID file.
    #[cfg(unix)]
    #[test]
    fn daemon_pid_via_socket_extracts_pid() {
        let sock = pong_server(
            "pid",
            "{\"kind\":\"pong\",\"version\":\"0.1.0\",\"pid\":4242}\n",
        );
        assert_eq!(daemon_pid_via_socket(&sock), Some(4242));
        let _ = std::fs::remove_file(&sock);
    }

    /// A pong from an older daemon (no `pid`, decoded as 0) or with an
    /// explicit 0 yields None — not a usable target to signal.
    #[cfg(unix)]
    #[test]
    fn daemon_pid_via_socket_none_without_usable_pid() {
        let no_pid = pong_server("nopid", "{\"kind\":\"pong\",\"version\":\"0.1.0\"}\n");
        assert_eq!(daemon_pid_via_socket(&no_pid), None);
        let _ = std::fs::remove_file(&no_pid);

        let zero = pong_server(
            "zero",
            "{\"kind\":\"pong\",\"version\":\"0.1.0\",\"pid\":0}\n",
        );
        assert_eq!(daemon_pid_via_socket(&zero), None);
        let _ = std::fs::remove_file(&zero);

        let missing = std::path::PathBuf::from(format!(
            "/tmp/nerv-pidsock-missing-{}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&missing);
        assert_eq!(daemon_pid_via_socket(&missing), None);
    }

    /// `strip_shell_hooks` cycles through .zshrc, .zshenv, .zprofile,
    /// .zlogin in that order and reports the first backup path. When
    /// only .zshenv has a marker block, the backup path returned must
    /// point at .zshenv.
    #[test]
    fn strip_shell_hooks_first_backup_picks_first_modified_file() {
        let tmp = std::env::temp_dir().join(format!("nerv-strip-first-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let plain_rc = "alias ll=ls\n";
        std::fs::write(tmp.join(".zshrc"), plain_rc).unwrap();
        let env_with_block = format!(
            "{}export FOO=bar\n",
            nerv_shell::init_block("/opt/homebrew/bin/nerv", "0.1.0", "ts", "zsh"),
        );
        std::fs::write(tmp.join(".zshenv"), &env_with_block).unwrap();
        let mut log = UninstallLog::new(true);
        let backup = strip_shell_hooks(&tmp, &mut log).unwrap().expect("backup");
        assert!(
            backup
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".zshenv"),
            "first backup should be the .zshenv file, got {backup:?}",
        );
        assert_eq!(
            std::fs::read_to_string(tmp.join(".zshrc")).unwrap(),
            plain_rc,
            ".zshrc must stay untouched",
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Multiple marker blocks within a single init file are all
    /// stripped + counted. Catches a regression where the
    /// total_blocks_removed counter would only see the first match.
    #[test]
    fn strip_shell_hooks_counts_multiple_blocks_per_file() {
        let tmp = std::env::temp_dir().join(format!("nerv-strip-multi-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let blk = nerv_shell::init_block("/opt/homebrew/bin/nerv", "0.1.0", "ts", "zsh");
        let zshrc = format!("alias a=1\n{blk}alias b=2\n{blk}alias c=3\n");
        std::fs::write(tmp.join(".zshrc"), &zshrc).unwrap();
        let mut log = UninstallLog::new(true);
        let _ = strip_shell_hooks(&tmp, &mut log).unwrap();
        let after = std::fs::read_to_string(tmp.join(".zshrc")).unwrap();
        assert_eq!(nerv_shell::count_blocks(&after), 0);
        assert!(after.contains("alias a=1"));
        assert!(after.contains("alias b=2"));
        assert!(after.contains("alias c=3"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The atomic-write path writes a `.nerv-tmp` sibling and renames
    /// over the original. Confirm no `.nerv-tmp` leftover is left
    /// behind on the happy path.
    #[test]
    fn strip_shell_hooks_cleans_up_temp_file() {
        let tmp = std::env::temp_dir().join(format!("nerv-strip-tmp-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let body = format!(
            "alias x=ls\n{}\n",
            nerv_shell::init_block("/opt/homebrew/bin/nerv", "0.1.0", "ts", "zsh"),
        );
        std::fs::write(tmp.join(".zshrc"), &body).unwrap();
        let mut log = UninstallLog::new(true);
        let _ = strip_shell_hooks(&tmp, &mut log).unwrap();
        let leftover = tmp.join(".nerv-tmp");
        assert!(
            !leftover.exists(),
            "atomic temp file should be renamed away"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `which("...")` mirrors PATH lookup for the doctor's PTY check.
    /// Empty PATH returns Err; missing binary returns Err; existing
    /// file returns the resolved path.
    #[test]
    fn which_finds_existing_path_entry() {
        let tmp = std::env::temp_dir().join(format!("nerv-which-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let exe = tmp.join("nerv-pty");
        std::fs::write(&exe, b"#!/bin/sh\n").unwrap();
        // Preserve current PATH and prepend our tempdir; restore on
        // exit so other tests don't see the override.
        let original = std::env::var_os("PATH");
        let new_path = match &original {
            Some(p) => {
                let mut v = std::ffi::OsString::from(&tmp);
                v.push(":");
                v.push(p);
                v
            }
            None => std::ffi::OsString::from(&tmp),
        };
        unsafe { std::env::set_var("PATH", &new_path) };
        let got = which("nerv-pty");
        let missing = which("nerv-no-such-binary-xyz-9876");
        match original {
            Some(p) => unsafe { std::env::set_var("PATH", p) },
            None => unsafe { std::env::remove_var("PATH") },
        }
        assert_eq!(got.unwrap(), exe);
        assert!(missing.is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `init_snippet_for_zsh(false)` returns the ZLE widget body —
    /// recognisable by its `__NERV_LOADED` re-entry guard. The PTY
    /// flavor's bootstrap uses `NERV_PTY_SESSION_ID` instead.
    #[test]
    fn init_snippet_for_zsh_default_is_zle_widget() {
        let body = init_snippet_for_zsh(false);
        assert!(body.contains("__NERV_LOADED"));
        assert!(!body.contains("NERV_PTY_BIN"));
    }

    /// `init_snippet_for_zsh(true)` returns the PTY bootstrap —
    /// recognisable by its NERV_PTY_SESSION_ID re-entry guard and
    /// the exec of NERV_PTY_BIN.
    #[test]
    fn init_snippet_for_zsh_pty_mode_is_pty_bootstrap() {
        let body = init_snippet_for_zsh(true);
        assert!(body.contains("NERV_PTY_SESSION_ID"));
        assert!(body.contains("NERV_PTY_BIN"));
        // Mutual exclusion: PTY snippet must NOT define the ZLE
        // widget global.
        assert!(!body.contains("__NERV_LOADED"));
    }

    /// The bash bootstrap is PTY-only: it must carry the OSC 697 marker
    /// emitter, re-exec nerv-pty, and self-skip when NERV_PTY is unset.
    #[test]
    fn bash_pty_snippet_is_pty_bootstrap() {
        let body = PtyShell::Bash.snippet();
        // OSC 697 prompt markers + session correlation.
        assert!(body.contains("697"));
        assert!(body.contains("NERV_PTY_SESSION_ID"));
        // bash prompt hook (not zsh's add-zsh-hook precmd).
        assert!(body.contains("PROMPT_COMMAND"));
        // Shell=bash is mandatory — the shadow term gates edit-buffer
        // reads on a recognized shell, so dropping it kills the ghost.
        assert!(body.contains("Shell=bash"));
        // Hands off to the shim and self-skips without opt-in.
        assert!(body.contains("exec \"$__NERV_PTY_BIN\""));
        assert!(body.contains("NERV_PTY"));
        // Must NOT define the zsh ZLE widget global.
        assert!(!body.contains("__NERV_LOADED"));
    }

    /// fish bootstrap: PTY-only, fish-syntax, must carry the mandatory
    /// `Shell=fish` marker and re-exec nerv-pty.
    #[test]
    fn fish_pty_snippet_is_pty_bootstrap() {
        let body = PtyShell::Fish.snippet();
        assert!(body.contains("697"));
        assert!(body.contains("NERV_PTY_SESSION_ID"));
        // fish event hook + prompt wrap (not bash's PROMPT_COMMAND).
        assert!(body.contains("--on-event fish_prompt"));
        assert!(body.contains("function fish_prompt"));
        // Same gating requirement as bash.
        assert!(body.contains("Shell=fish"));
        // fish-syntax re-exec.
        assert!(body.contains("exec $__nerv_pty_bin -- $SHELL"));
        assert!(!body.contains("__NERV_LOADED"));
    }

    /// fish pins NERV_PTY_BIN with `set -gx`, bash with `export`.
    #[test]
    fn pty_shell_export_uses_native_syntax() {
        assert!(
            PtyShell::Bash
                .export_pty_bin("/x/nerv-pty")
                .starts_with("export NERV_PTY_BIN=")
        );
        assert!(
            PtyShell::Fish
                .export_pty_bin("/x/nerv-pty")
                .starts_with("set -gx NERV_PTY_BIN ")
        );
    }

    /// `resolve_pty_bin_for_init` returns Some(path) when a sibling
    /// `nerv-pty` exists next to the given `nerv` binary; None
    /// otherwise. Used by the inner init hook to export
    /// NERV_PTY_BIN so the shim doesn't depend on PATH.
    #[test]
    fn resolve_pty_bin_for_init_finds_sibling() {
        let tmp = std::env::temp_dir().join(format!("nerv-pty-init-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let nerv = tmp.join("nerv");
        let pty = tmp.join("nerv-pty");
        std::fs::write(&nerv, b"#!/bin/sh\n").unwrap();
        // No sibling yet.
        assert_eq!(
            resolve_pty_bin_for_init(nerv.to_str().unwrap()),
            None,
            "should be None when sibling missing",
        );
        std::fs::write(&pty, b"#!/bin/sh\n").unwrap();
        let got = resolve_pty_bin_for_init(nerv.to_str().unwrap()).unwrap();
        assert_eq!(std::path::PathBuf::from(got), pty);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A recorded miss tally becomes one advisory row, most-missed
    /// first, capped at five names.
    #[test]
    fn doctor_reports_top_spec_misses() {
        let path = std::env::temp_dir().join(format!("nerv-misses-doc-{}.tsv", std::process::id()));
        let counter = nerv_engine::misses::MissCounter::load(&path);
        for _ in 0..12 {
            counter.record("zeph");
        }
        for _ in 0..9 {
            counter.record("aic2");
        }
        counter.flush_if_dirty();

        let mut r = DoctorReport::default();
        check_spec_misses_in(&mut r, &path);
        assert_eq!(
            doctor_labels(&r),
            [("spec misses".to_string(), "Ok".into())]
        );
        assert_eq!(r.entries[0].detail, "zeph 12, aic2 9");
        let _ = std::fs::remove_file(&path);
    }

    /// No recorded misses → no row (a fresh install's doctor output is
    /// unchanged).
    #[test]
    fn doctor_omits_spec_misses_row_when_nothing_recorded() {
        let path =
            std::env::temp_dir().join(format!("nerv-misses-none-{}.tsv", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut r = DoctorReport::default();
        check_spec_misses_in(&mut r, &path);
        assert!(r.entries.is_empty());
    }

    /// Two-layer fixture: (overlay, primary) under a fresh temp dir.
    fn overlay_layers(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("nerv-layers-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let overlay = root.join("overlay");
        let primary = root.join("primary");
        std::fs::create_dir_all(&overlay).unwrap();
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::write(
            primary.join("git.json"),
            r#"{"name":"git","subcommands":[{"name":"status"}]}"#,
        )
        .unwrap();
        std::fs::write(primary.join("echo.json"), r#"{"name":"echo"}"#).unwrap();
        (overlay, primary)
    }

    fn layers_of(
        overlay: Option<std::path::PathBuf>,
        primary: std::path::PathBuf,
    ) -> paths::SpecLayers {
        paths::SpecLayers { overlay, primary }
    }

    fn doctor_labels(r: &DoctorReport) -> Vec<(String, String)> {
        r.entries
            .iter()
            .map(|e| (e.label.clone(), format!("{:?}", e.level)))
            .collect()
    }

    /// Overlay present and healthy → a green `user specs` row counting only
    /// the stems it actually serves; `specs` counts the union.
    #[test]
    fn doctor_reports_user_specs_row_for_healthy_overlay() {
        let (overlay, primary) = overlay_layers("healthy");
        std::fs::write(overlay.join("claude.json"), r#"{"name":"claude"}"#).unwrap();
        std::fs::write(overlay.join("echo.json"), r#"{"name":"echo"}"#).unwrap();
        let mut r = DoctorReport::default();
        check_specs_in(&mut r, &layers_of(Some(overlay.clone()), primary));
        let rows = doctor_labels(&r);
        assert_eq!(
            rows,
            [
                ("specs".to_string(), "Ok".to_string()),
                ("user specs".into(), "Ok".into())
            ]
        );
        assert_eq!(
            r.entries[0].detail, "3 loaded",
            "union: git + echo(overlay) + claude"
        );
        assert_eq!(r.entries[1].detail, format!("2 in {}", overlay.display()));
    }

    /// A broken overlay file is *the user's* problem: red `user specs` row
    /// with a fix-the-file hint, while the primary `specs` row stays green
    /// (no "reinstall nerv" advice for a typo in ~/.config).
    #[test]
    fn doctor_isolates_broken_overlay_file_to_user_specs_row() {
        let (overlay, primary) = overlay_layers("broken");
        std::fs::write(overlay.join("claude.json"), "{ not json").unwrap();
        let mut r = DoctorReport::default();
        check_specs_in(&mut r, &layers_of(Some(overlay.clone()), primary));
        let rows = doctor_labels(&r);
        assert_eq!(
            rows,
            [
                ("specs".to_string(), "Ok".to_string()),
                ("user specs".into(), "Err".into())
            ]
        );
        assert!(
            r.entries[1].detail.starts_with("1 disabled: "),
            "{}",
            r.entries[1].detail
        );
        assert_eq!(
            r.entries[1].hint.as_deref(),
            Some(format!("fix or remove that file in {}", overlay.display()).as_str())
        );
    }

    /// No overlay layer → no `user specs` row at all (fresh install output
    /// is byte-for-byte the pre-overlay doctor).
    #[test]
    fn doctor_omits_user_specs_row_without_overlay() {
        let (_overlay, primary) = overlay_layers("none");
        let mut r = DoctorReport::default();
        check_specs_in(&mut r, &layers_of(None, primary));
        assert_eq!(doctor_labels(&r), [("specs".to_string(), "Ok".to_string())]);
        assert_eq!(r.entries[0].detail, "2 loaded");
    }

    /// `spec list` shows the union once per stem, marks overlay-served rows
    /// with `*`, and closes with the legend.
    #[test]
    fn spec_list_marks_overlay_rows_and_adds_legend() {
        let (overlay, primary) = overlay_layers("list");
        std::fs::write(overlay.join("claude.json"), r#"{"name":"claude"}"#).unwrap();
        std::fs::write(
            overlay.join("echo.json"),
            r#"{"name":"echo","options":[{"names":["-z"]}]}"#,
        )
        .unwrap();
        let lines = spec_list_lines(&layers_of(Some(overlay.clone()), primary));
        let names: Vec<&str> = lines[1..4]
            .iter()
            .map(|l| l.split_whitespace().next().unwrap())
            .collect();
        assert_eq!(
            names,
            ["claude", "echo", "git"],
            "union, sorted, no duplicate echo"
        );
        assert!(
            lines[1].ends_with('*'),
            "claude is overlay-served: {}",
            lines[1]
        );
        assert!(
            lines[2].ends_with('*'),
            "echo overridden by overlay: {}",
            lines[2]
        );
        assert!(!lines[3].ends_with('*'), "git is bundled: {}", lines[3]);
        assert!(
            lines[2].contains("    0     1"),
            "overlay echo (1 option) wins: {}",
            lines[2]
        );
        assert_eq!(
            lines.last().unwrap(),
            &format!("* = served from {} (user overlay)", overlay.display())
        );
    }

    /// Without an overlay the table has no marks and no legend — unchanged
    /// output for every existing user.
    #[test]
    fn spec_list_without_overlay_has_no_marks_or_legend() {
        let (_overlay, primary) = overlay_layers("plain");
        let lines = spec_list_lines(&layers_of(None, primary));
        assert_eq!(lines.len(), 3, "header + 2 rows, no legend: {lines:?}");
        assert!(lines.iter().all(|l| !l.ends_with('*')));
    }

    /// `count_tree` walks the spec tree and returns (subs, opts). The
    /// root itself is not counted; only descendants. Empty spec → 0/0.
    #[test]
    fn count_tree_empty_returns_zero_zero() {
        let root = SpecNode {
            name: "x".into(),
            ..Default::default()
        };
        assert_eq!(count_tree(&root), (0, 0));
    }

    /// Two levels of nesting + a few options per node — the totals
    /// sum across the whole tree, root excluded.
    #[test]
    fn count_tree_sums_across_descendants() {
        use nerv_engine::Opt;
        let leaf = SpecNode {
            name: "leaf".into(),
            options: vec![Opt {
                names: vec!["-x".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let mid = SpecNode {
            name: "mid".into(),
            options: vec![
                Opt {
                    names: vec!["-a".into()],
                    ..Default::default()
                },
                Opt {
                    names: vec!["-b".into()],
                    ..Default::default()
                },
            ],
            subcommands: vec![leaf],
            ..Default::default()
        };
        let root = SpecNode {
            name: "root".into(),
            subcommands: vec![mid],
            ..Default::default()
        };
        // Subs: mid + leaf = 2. Opts: root 0 + mid 2 + leaf 1 = 3.
        assert_eq!(count_tree(&root), (2, 3));
    }

    /// `upgrade_tier` is monotone: once C, never downgrades; B stays
    /// when only A-level generators follow. Custom + Script-with-pp
    /// are the only Tier C upgrades.
    #[test]
    fn upgrade_tier_classifies_generators() {
        use nerv_engine::Generator;
        // No generators → stays A.
        assert_eq!(upgrade_tier('A', std::iter::empty()), 'A');
        // Template → B.
        let g = [Generator::Template {
            script: vec!["echo".into()],
        }];
        assert_eq!(upgrade_tier('A', g.iter()), 'B');
        // Custom (no source) → C.
        let g = [Generator::Custom {
            description_hint: None,
            source: None,
        }];
        assert_eq!(upgrade_tier('A', g.iter()), 'C');
        // Script with post-process → C, never downgrades.
        let g = [
            Generator::Script {
                script: vec!["echo".into()],
                has_post_process: true,
            },
            Generator::Template {
                script: vec!["echo".into()],
            },
        ];
        assert_eq!(upgrade_tier('A', g.iter()), 'C');
        // Already C → stays C even with A-only follow-ups.
        assert_eq!(upgrade_tier('C', std::iter::empty()), 'C');
    }

    /// `compute_tier` returns the human-readable label
    /// `nerv spec list` prints in its TIER column.
    #[test]
    fn compute_tier_labels_are_stable() {
        use nerv_engine::{Arg, Generator};
        // Empty spec → A.
        let root = SpecNode {
            name: "x".into(),
            ..Default::default()
        };
        assert_eq!(compute_tier(&root), "A");
        // Spec with Template arg → B (limited).
        let root = SpecNode {
            name: "x".into(),
            args: vec![Arg {
                generators: vec![Generator::Template {
                    script: vec!["echo".into()],
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(compute_tier(&root), "B (limited)");
        // Spec with Custom arg → C (M1).
        let root = SpecNode {
            name: "x".into(),
            args: vec![Arg {
                generators: vec![Generator::Custom {
                    description_hint: None,
                    source: None,
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(compute_tier(&root), "C (M1)");
    }
}
