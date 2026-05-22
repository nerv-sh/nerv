//! `nervd` — Nerv's background daemon.
//!
//! Listens on a Unix domain socket, decodes [`nerv_engine::Request`]
//! messages, and replies with [`nerv_engine::Response`] over JSON-RPC.
//!
//! v1.0 contract:
//! - Single socket at `~/Library/Caches/nerv/nervd.sock` (macOS).
//! - PID file at `~/Library/Caches/nerv/nervd.pid` (uninstall-spec.md §2).
//! - SIGTERM → graceful shutdown within 5 s; SIGKILL accepted as fallback.
//! - Static spec-only matching. No JS runtime, no network.
//!
//! M0-1 PoC: just an echo server. Real matching arrives in M1 0–6주차.

use anyhow::Context;
use nerv_engine::{paths, Request, Response, Suggestion, SuggestionKind};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, info, warn};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cache_dir = paths::cache_dir().context("cannot resolve nerv cache dir ($HOME unset?)")?;
    tokio::fs::create_dir_all(&cache_dir).await?;

    // Allow override via env for testing (e2e tests use a temp socket).
    let sock_path = std::env::var_os("NERV_SOCK")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| paths::socket_path().expect("HOME present (just checked)"));
    let pid_path = std::env::var_os("NERV_PID")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| paths::pid_path().expect("HOME present (just checked)"));

    write_pid_file(&pid_path).await?;

    // Best-effort cleanup of any stale socket from a previous run.
    let _ = tokio::fs::remove_file(&sock_path).await;

    info!(socket = %sock_path.display(), "nervd starting (M0 stub)");

    // M0-1 stub: bind a UDS, echo each line back as a Pong response.
    // Real listener uses `interprocess::local_socket` for cross-platform
    // compatibility (M1).
    use tokio::net::UnixListener;
    let listener = UnixListener::bind(&sock_path)
        .with_context(|| format!("cannot bind UDS at {}", sock_path.display()))?;

    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            res = listener.accept() => {
                match res {
                    Ok((stream, _addr)) => {
                        tokio::spawn(handle_connection(stream));
                    }
                    Err(e) => warn!(?e, "accept error"),
                }
            }
            _ = &mut shutdown => {
                info!("shutdown signal received");
                break;
            }
        }
    }

    let _ = tokio::fs::remove_file(&sock_path).await;
    let _ = tokio::fs::remove_file(&pid_path).await;
    Ok(())
}

async fn handle_connection(stream: tokio::net::UnixStream) {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        debug!(req = trimmed, "received");
        let resp = match serde_json::from_str::<Request>(trimmed) {
            Ok(Request::Ping) => Response::Pong {
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            Ok(Request::Complete { line, cursor }) => {
                // M0-1 stub: Fig-like behavior with hardcoded git subcommands.
                // Real spec-tree matching arrives in M0-2 / M1.
                stub_complete(&line, cursor)
            }
            Ok(Request::DoctorAutorun) => Response::Empty {
                reason: Some("doctor-autorun-stub".to_string()),
            },
            Err(e) => Response::Error {
                message: format!("invalid request: {e}"),
            },
        };
        if let Ok(s) = serde_json::to_string(&resp) {
            if write_half.write_all(s.as_bytes()).await.is_err()
                || write_half.write_all(b"\n").await.is_err()
                || write_half.flush().await.is_err()
            {
                break;
            }
        }
    }
}

async fn write_pid_file(path: &std::path::Path) -> anyhow::Result<()> {
    use std::process;
    let s = format!("{}\n", process::id());
    tokio::fs::write(path, s)
        .await
        .with_context(|| format!("cannot write PID file at {}", path.display()))?;
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM");
    let mut int = signal(SignalKind::interrupt()).expect("install SIGINT");
    tokio::select! {
        _ = term.recv() => {},
        _ = int.recv() => {},
    }
}

/// M0-1 stub: Fig-like contextual completion for git subcommands.
///
/// Only returns suggestions at the subcommand position (first arg after the
/// command name). Filters by prefix when the user is mid-typing. Returns
/// empty once a subcommand is already present.
fn stub_complete(line: &str, cursor: usize) -> Response {
    let input = &line[..cursor.min(line.len())];
    let tokens: Vec<&str> = input.split_whitespace().collect();
    let trailing_space = input.ends_with(' ');

    // Determine what the user is completing:
    // - 0 tokens: empty → no suggestions
    // - 1 token, trailing space: "git " → show all subcommands
    // - 1 token, no space: "git" → still typing command name → no suggestions
    // - 2 tokens, no space: "git co" → filter subcommands by prefix "co"
    // - 2 tokens, trailing space: "git commit " → past subcommand → no suggestions (stub)
    // - 3+ tokens: deep in args → no suggestions (stub)

    let all_subs = [
        ("commit", "Record changes to the repository"),
        ("clone", "Clone a repository into a new directory"),
        ("checkout", "Switch branches or restore files"),
        ("push", "Update remote refs along with objects"),
        ("pull", "Fetch and integrate with another repo"),
        ("branch", "List, create, or delete branches"),
        ("merge", "Join two or more development histories"),
        ("rebase", "Reapply commits on top of another base"),
        ("status", "Show the working tree status"),
        ("log", "Show commit logs"),
        ("diff", "Show changes between commits"),
        ("add", "Add file contents to the index"),
        ("stash", "Stash changes in a dirty working directory"),
        ("fetch", "Download objects and refs from a remote"),
        ("reset", "Reset current HEAD to a specified state"),
    ];

    let prefix = match (tokens.len(), trailing_space) {
        (1, true) => "",                // "git " → show all
        (2, false) => tokens[1],        // "git co" → filter by "co"
        _ => return Response::Suggestions { items: vec![] },
    };

    let items: Vec<Suggestion> = all_subs
        .iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .take(5)
        .map(|(name, desc)| Suggestion {
            insertion: name.to_string(),
            display: name.to_string(),
            description: Some(desc.to_string()),
            kind: SuggestionKind::Subcommand,
        })
        .collect();

    Response::Suggestions { items }
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_env("NERV_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
