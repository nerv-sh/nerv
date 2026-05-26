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
use nerv_engine::{Request, Response, SpecRegistry, complete_in, paths};
use std::sync::Arc;
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
    let specs_dir = std::env::var_os("NERV_SPECS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| paths::specs_dir().expect("HOME present (just checked)"));

    // Lazy registry: no upfront disk scan. Specs are read on first
    // lookup and cached. Startup stays O(1) even with 700+ specs.
    let registry = Arc::new(SpecRegistry::at_dir(&specs_dir));
    info!(
        specs_dir = %specs_dir.display(),
        "spec registry initialized (lazy)"
    );

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
                        let registry = registry.clone();
                        tokio::spawn(handle_connection(stream, registry));
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

async fn handle_connection(stream: tokio::net::UnixStream, registry: Arc<SpecRegistry>) {
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
            Ok(Request::Complete { line, cursor, cwd }) => {
                engine_complete(&registry, &line, cursor, cwd.as_deref())
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

/// Dispatch a Complete request through the real engine pipeline.
fn engine_complete(
    registry: &SpecRegistry,
    line: &str,
    cursor: usize,
    cwd: Option<&str>,
) -> Response {
    let cwd_path = cwd.map(std::path::Path::new);
    let result = complete_in(line, cursor, registry, cwd_path);
    if result.items.is_empty() {
        return Response::Empty {
            reason: result.reason,
        };
    }
    Response::Suggestions {
        items: result.items,
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
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM");
    let mut int = signal(SignalKind::interrupt()).expect("install SIGINT");
    tokio::select! {
        _ = term.recv() => {},
        _ = int.recv() => {},
    }
}

// E2 (docs/error-states.md §3.2) reporting moves entirely into
// `nerv doctor`, which scans the spec dir eagerly and prints
// parse errors in the diagnostic table. The lazy daemon now
// emits a generic "no spec for X" reason — sufficient for the
// widget's E1 hint path; precise per-spec error attribution is
// `nerv doctor`'s responsibility.

fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_env("NERV_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
