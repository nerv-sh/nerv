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
}

fn main() -> anyhow::Result<()> {
    init_tracing();
    // Rust's runtime ignores SIGPIPE by default, which turns
    // `nerv spec list | head` into a panic. Restore Unix default
    // so the CLI exits silently when its stdout closes mid-write.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

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
                // Outer block: just emit the ~/.zshrc marker. The inner
                // shell_script invocation does the env validation each
                // session, where ZSH_VERSION etc. are actually set.
                let bin = std::env::current_exe()?.to_string_lossy().into_owned();
                let block = nerv_shell::init_block(
                    &bin,
                    env!("CARGO_PKG_VERSION"),
                    "TODO-RFC3339-timestamp",
                );
                print!("{block}");
                Ok(())
            }
        }
    }
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
    r
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

/// Marker block in ~/.zshrc.
fn check_shell_hook(r: &mut DoctorReport) {
    let Some(home) = std::env::var_os("HOME") else {
        r.push(DoctorLevel::Err, "shell hook", "HOME unset".into(), None);
        return;
    };
    let zshrc = std::path::PathBuf::from(home).join(".zshrc");
    if !zshrc.exists() {
        r.push(
            DoctorLevel::Warn,
            "shell hook",
            "~/.zshrc not found".into(),
            Some("run: nerv init zsh >> ~/.zshrc".into()),
        );
        return;
    }
    let content = match std::fs::read_to_string(&zshrc) {
        Ok(c) => c,
        Err(e) => {
            r.push(
                DoctorLevel::Err,
                "shell hook",
                format!("read failed: {e}"),
                None,
            );
            return;
        }
    };
    let count = nerv_shell::count_blocks(&content);
    match count {
        0 => r.push(
            DoctorLevel::Warn,
            "shell hook",
            "no nerv marker block in ~/.zshrc".into(),
            Some("run: nerv init zsh >> ~/.zshrc".into()),
        ),
        1 => r.push(
            DoctorLevel::Ok,
            "shell hook",
            "~/.zshrc marker block 1개 (멱등 OK)".into(),
            None,
        ),
        n => r.push(
            DoctorLevel::Warn,
            "shell hook",
            format!("{n} marker blocks (should be 1)"),
            Some("run: nerv uninstall && nerv init zsh >> ~/.zshrc".into()),
        ),
    }
}

/// E1: nervd running.
fn check_daemon(r: &mut DoctorReport) {
    let Some(pid_path) = paths::pid_path() else {
        r.push(DoctorLevel::Err, "daemon", "HOME unset".into(), None);
        return;
    };
    let Some(pid) = read_pid(&pid_path) else {
        r.push(
            DoctorLevel::Err,
            "daemon",
            "nervd not running".into(),
            Some("run: nerv start".into()),
        );
        return;
    };
    if process_alive(pid) {
        r.push(
            DoctorLevel::Ok,
            "daemon",
            format!("nervd running (pid {pid})"),
            None,
        );
    } else {
        r.push(
            DoctorLevel::Err,
            "daemon",
            format!("stale PID file (pid {pid} not alive)"),
            Some("run: nerv start".into()),
        );
    }
}

/// E2 + E5: specs loaded + parse errors.
fn check_specs(r: &mut DoctorReport) {
    let specs_dir = match std::env::var_os("NERV_SPECS_DIR")
        .map(std::path::PathBuf::from)
        .or_else(paths::specs_dir)
    {
        Some(d) => d,
        None => {
            r.push(DoctorLevel::Err, "specs", "HOME unset".into(), None);
            return;
        }
    };
    if !specs_dir.exists() {
        r.push(
            DoctorLevel::Warn,
            "specs",
            format!("dir missing: {}", specs_dir.display()),
            Some("populate via build-specs or homebrew install".into()),
        );
        return;
    }
    // Doctor eager-scans (load_dir) so it can report parse errors up
    // front, rather than waiting for a user keystroke to surface E2.
    let (registry, errs) = SpecRegistry::load_dir(&specs_dir);
    let count = registry.len();
    let err_count = errs.len();
    if err_count == 0 && count == 0 {
        r.push(
            DoctorLevel::Warn,
            "specs",
            "0 specs loaded".into(),
            Some(format!("populate {}", specs_dir.display())),
        );
        return;
    }
    if err_count > 0 {
        r.push(
            DoctorLevel::Err,
            "spec health",
            format!("{err_count} disabled: {}", errs[0]),
            Some("run: brew reinstall nerv".into()),
        );
    }
    r.push(DoctorLevel::Ok, "specs", format!("{count} loaded"), None);
}

fn cmd_start() -> anyhow::Result<()> {
    use anyhow::Context;
    use std::fs;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    let pid_path = paths::pid_path().context("HOME unset")?;
    let cache_dir = paths::cache_dir().context("HOME unset")?;
    let log_path = paths::daemon_log_path().context("HOME unset")?;

    if let Some(pid) = read_pid(&pid_path) {
        if process_alive(pid) {
            println!("nervd already running (pid {pid})");
            return Ok(());
        }
        let _ = fs::remove_file(&pid_path);
    }

    fs::create_dir_all(&cache_dir).with_context(|| format!("mkdir {}", cache_dir.display()))?;
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
    let Some(pid) = read_pid(&pid_path) else {
        println!("nervd not running");
        return Ok(());
    };
    if !process_alive(pid) {
        let _ = fs::remove_file(&pid_path);
        println!("nervd not running (stale PID file removed)");
        return Ok(());
    }

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
    use std::path::PathBuf;

    let specs_dir = std::env::var_os("NERV_SPECS_DIR")
        .map(PathBuf::from)
        .or_else(paths::specs_dir)
        .ok_or_else(|| anyhow::anyhow!("HOME unset and NERV_SPECS_DIR not set"))?;

    if !specs_dir.exists() {
        println!("(no specs at {})", specs_dir.display());
        return Ok(());
    }

    let registry = SpecRegistry::at_dir(&specs_dir);
    let names = registry.dir_listing();
    if names.is_empty() {
        println!("(no specs found in {})", specs_dir.display());
        return Ok(());
    }

    println!("{:<18} {:>5} {:>5}  TIER", "NAME", "SUBS", "OPTS");
    for name in names {
        let Some(spec) = registry.lookup(&name) else {
            eprintln!("  ⚠ {name}: load error (run nerv doctor)");
            continue;
        };
        let (subs, opts) = count_tree(spec.as_ref());
        let tier = compute_tier(spec.as_ref());
        println!("{name:<18} {subs:>5} {opts:>5}  {tier}");
    }
    Ok(())
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
        Some(h) => strip_zsh_hooks(h, &mut log)?,
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
    let Some(pid) = read_pid(&pid_path) else {
        log.ok("daemon", "not running".into());
        return true;
    };
    if !process_alive(pid) {
        log.ok("daemon", format!("stale pid {pid} ignored"));
        return true;
    }

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
fn strip_zsh_hooks(
    home: &std::path::Path,
    log: &mut UninstallLog,
) -> anyhow::Result<Option<std::path::PathBuf>> {
    use std::fs;

    let init_files = [".zshrc", ".zshenv", ".zprofile", ".zlogin"];
    let mut first_backup: Option<std::path::PathBuf> = None;
    let mut total_blocks_removed = 0usize;

    for name in init_files {
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
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let sock_path = paths::socket_path().ok_or_else(|| anyhow::anyhow!("HOME unset"))?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()?;

    rt.block_on(async {
        let stream = UnixStream::connect(&sock_path).await?;
        let (read_half, mut write_half) = stream.into_split();

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
        let mut json = serde_json::to_string(&req)?;
        json.push('\n');
        write_half.write_all(json.as_bytes()).await?;

        let mut reader = BufReader::new(read_half);
        let mut resp_line = String::new();
        reader.read_line(&mut resp_line).await?;

        let resp: Response = serde_json::from_str(resp_line.trim())?;
        if let Response::Suggestions { items } = resp {
            for s in &items {
                print_suggestion(s);
            }
        }
        // Any other response type → no output → no popup in zsh.
        Ok(())
    })
}

fn print_suggestion(s: &Suggestion) {
    // Wire format: insertion \t display \t description \t icon
    // Icon is empty string when None. Widget renders icon as a
    // prefix glyph to the display column.
    let desc = s.description.as_deref().unwrap_or("");
    let icon = s.icon.as_deref().unwrap_or("");
    println!("{}\t{}\t{}\t{}", s.insertion, s.display, desc, icon);
}

fn cmd_internal_record(spec: &str, insertion: &str) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let sock_path = paths::socket_path().ok_or_else(|| anyhow::anyhow!("HOME unset"))?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()?;

    rt.block_on(async {
        let stream = UnixStream::connect(&sock_path).await?;
        let (read_half, mut write_half) = stream.into_split();
        let req = nerv_engine::Request::RecordAccept {
            spec: spec.to_string(),
            insertion: insertion.to_string(),
        };
        let mut json = serde_json::to_string(&req)?;
        json.push('\n');
        write_half.write_all(json.as_bytes()).await?;
        // Drain the single-line response so the daemon can close
        // the conn cleanly. We don't act on the body.
        let mut reader = BufReader::new(read_half);
        let mut resp_line = String::new();
        let _ = reader.read_line(&mut resp_line).await;
        anyhow::Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn strip_zsh_hooks_removes_block_and_creates_backup() {
        // Isolated HOME so we don't touch the real ~/.zshrc.
        let tmp = std::env::temp_dir().join(format!("nerv-strip-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let zshrc = tmp.join(".zshrc");
        let original = format!(
            "alias ll='ls -la'\n{}# trailing user comment\n",
            nerv_shell::init_block("/opt/homebrew/bin/nerv", "0.1.0", "ts")
        );
        std::fs::write(&zshrc, &original).unwrap();

        let mut log = UninstallLog::new(true);
        let backup = strip_zsh_hooks(&tmp, &mut log).unwrap();
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
    fn strip_zsh_hooks_no_op_when_no_blocks() {
        let tmp = std::env::temp_dir().join(format!("nerv-strip-noop-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let zshrc = tmp.join(".zshrc");
        let body = "alias x=ls\n";
        std::fs::write(&zshrc, body).unwrap();
        let mut log = UninstallLog::new(true);
        let backup = strip_zsh_hooks(&tmp, &mut log).unwrap();
        assert!(backup.is_none(), "no backup when nothing to strip");
        assert_eq!(std::fs::read_to_string(&zshrc).unwrap(), body);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn strip_zsh_hooks_handles_missing_home_files() {
        let tmp = std::env::temp_dir().join(format!("nerv-strip-empty-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let mut log = UninstallLog::new(true);
        let backup = strip_zsh_hooks(&tmp, &mut log).unwrap();
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

    /// `strip_zsh_hooks` cycles through .zshrc, .zshenv, .zprofile,
    /// .zlogin in that order and reports the first backup path. When
    /// only .zshenv has a marker block, the backup path returned must
    /// point at .zshenv.
    #[test]
    fn strip_zsh_hooks_first_backup_picks_first_modified_file() {
        let tmp = std::env::temp_dir().join(format!("nerv-strip-first-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let plain_rc = "alias ll=ls\n";
        std::fs::write(tmp.join(".zshrc"), plain_rc).unwrap();
        let env_with_block = format!(
            "{}export FOO=bar\n",
            nerv_shell::init_block("/opt/homebrew/bin/nerv", "0.1.0", "ts"),
        );
        std::fs::write(tmp.join(".zshenv"), &env_with_block).unwrap();
        let mut log = UninstallLog::new(true);
        let backup = strip_zsh_hooks(&tmp, &mut log).unwrap().expect("backup");
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
    fn strip_zsh_hooks_counts_multiple_blocks_per_file() {
        let tmp = std::env::temp_dir().join(format!("nerv-strip-multi-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let blk = nerv_shell::init_block("/opt/homebrew/bin/nerv", "0.1.0", "ts");
        let zshrc = format!("alias a=1\n{blk}alias b=2\n{blk}alias c=3\n");
        std::fs::write(tmp.join(".zshrc"), &zshrc).unwrap();
        let mut log = UninstallLog::new(true);
        let _ = strip_zsh_hooks(&tmp, &mut log).unwrap();
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
    fn strip_zsh_hooks_cleans_up_temp_file() {
        let tmp = std::env::temp_dir().join(format!("nerv-strip-tmp-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let body = format!(
            "alias x=ls\n{}\n",
            nerv_shell::init_block("/opt/homebrew/bin/nerv", "0.1.0", "ts"),
        );
        std::fs::write(tmp.join(".zshrc"), &body).unwrap();
        let mut log = UninstallLog::new(true);
        let _ = strip_zsh_hooks(&tmp, &mut log).unwrap();
        let leftover = tmp.join(".nerv-tmp");
        assert!(
            !leftover.exists(),
            "atomic temp file should be renamed away"
        );
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
