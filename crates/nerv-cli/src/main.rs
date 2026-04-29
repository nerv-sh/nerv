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

use clap::{Parser, Subcommand};
use nerv_engine::paths;

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
    }
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_env("NERV_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

// ---------- command stubs (M0–M1) ----------

fn cmd_init(shell: Shell, shell_script: bool) -> anyhow::Result<()> {
    // M0-1 will produce the actual ZLE widget script. For now, emit
    // the marker-wrapped block via nerv-shell to validate the contract.
    match shell {
        Shell::Zsh => {
            if shell_script {
                // Inner script: the actual ZLE widget. Stub for now.
                println!("# nerv zsh shell-script — populated in M0-1");
                Ok(())
            } else {
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

fn cmd_doctor() -> anyhow::Result<()> {
    // docs/error-states.md §5 — output format is fixed.
    println!("nerv doctor");
    println!();
    if let Some(d) = paths::cache_dir() {
        println!("  ⌛ pending implementation (M1 7–12주차)");
        println!("     cache dir: {}", d.display());
    } else {
        println!("  ✗ HOME unset — cannot resolve nerv paths");
    }
    Ok(())
}

fn cmd_start() -> anyhow::Result<()> {
    // M0-1: spawn nervd. After §3.6 lands, this also triggers the
    // automatic doctor self-check on success.
    anyhow::bail!("nerv start: not yet implemented (M0-1)")
}

fn cmd_stop() -> anyhow::Result<()> {
    anyhow::bail!("nerv stop: not yet implemented (M0-1)")
}

fn cmd_spec_list() -> anyhow::Result<()> {
    // Reads specs-prebuilt/manifest.json (built by `build/spec-transpile`).
    println!("(no specs yet — run M0-2 to populate specs-prebuilt/)");
    Ok(())
}

fn cmd_uninstall(keep_config: bool, quiet: bool) -> anyhow::Result<()> {
    // docs/uninstall-spec.md §4 — 8-step procedure, atomic .zshrc edits.
    let _ = (keep_config, quiet);
    anyhow::bail!("nerv uninstall: not yet implemented (M1 13–16주차)")
}
