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
use nerv_engine::{
    FrecencyStore, MatchMode, MatchingConfig, Request, Response, SpecRegistry, Suggestion,
    complete_in, manifest, paths,
};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, error, info, warn};

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
    // env override → populated user cache → bundled set shipped next to
    // the binary (brew `share/nerv/specs` or tarball `specs/`) → user
    // path. A fresh `brew install` user completes against the bundle
    // without ever running build-specs.
    // Primary = env override → populated user cache → bundled set; the
    // user overlay `~/.config/nerv/specs/` (if the dir exists) is layered
    // on top — a stem there replaces the bundled file wholesale. The E5
    // schema gate below looks at the primary dir only.
    let layers = paths::resolve_spec_layers().expect("HOME present (just checked)");

    // Lazy registry: no upfront disk scan. Specs are read on first
    // lookup and cached. Startup stays O(1) even with 700+ specs.
    let registry = Arc::new(SpecRegistry::at_dirs(&layers.dirs()));
    info!(layers = ?layers, "spec registry initialized (lazy)");

    // E5: reject a spec cache built for a different schema version
    // (error-states.md §3.5). On mismatch the daemon stays up but every
    // Complete returns empty with this reason, which the CLI bridge turns
    // into the grey ZLE hint. A missing manifest is tolerated.
    let schema_block: Arc<Option<String>> =
        Arc::new(match manifest::check_schema(&layers.primary) {
            manifest::SchemaStatus::Mismatch { found } => {
                let reason = format!(
                    "spec schema mismatch — daemon expects v{}, found v{found}",
                    manifest::SUPPORTED_SCHEMA_VERSION
                );
                error!(
                    "{reason}. Run: brew reinstall nerv (or: nerv doctor). \
                 Autocomplete disabled until resolved."
                );
                Some(reason)
            }
            _ => None,
        });

    // Frecency: per-spec usage history that nudges repeat picks to
    // the top of suggestion lists. Persisted as a TSV next to specs.
    // NERV_FRECENCY_FILE=- disables loading (tests / sandboxed
    // benchmarks that don't want the user's real history bleeding in).
    let frecency_path = std::env::var_os("NERV_FRECENCY_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| cache_dir.join("frecency.tsv"));
    let frecency = if frecency_path == std::path::PathBuf::from("-") {
        Arc::new(FrecencyStore::empty())
    } else {
        Arc::new(FrecencyStore::load(&frecency_path))
    };
    info!(
        path = %frecency_path.display(),
        entries = frecency.len(),
        "frecency store loaded"
    );

    // User matching mode — defaults to prefix; opt-in fuzzy via
    // `~/.config/nerv/nerv.toml` (PLAN §5.1). Loaded once at boot;
    // edits require a daemon restart.
    let matching = MatchingConfig::load_default();
    info!(mode = ?matching.mode, "matching config loaded");

    // Best-effort cleanup of any stale socket from a previous run.
    let _ = tokio::fs::remove_file(&sock_path).await;

    info!(socket = %sock_path.display(), "nervd starting (M0 stub)");

    // M0-1 stub: bind a UDS, echo each line back as a Pong response.
    // Real listener uses `interprocess::local_socket` for cross-platform
    // compatibility (M1).
    use tokio::net::UnixListener;
    let listener = UnixListener::bind(&sock_path)
        .with_context(|| format!("cannot bind UDS at {}", sock_path.display()))?;

    // PID file is written AFTER the socket is bound: it is the readiness
    // signal `nerv start` polls before returning (and releasing its
    // start lock). Writing it earlier reopens the double-spawn race —
    // a second `nerv start` would probe between pid-write and bind,
    // find no listener, and spawn a rival daemon whose stale-socket
    // cleanup steals this one's listener.
    write_pid_file(&pid_path).await?;

    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            res = listener.accept() => {
                match res {
                    Ok((stream, _addr)) => {
                        let registry = registry.clone();
                        let frecency = frecency.clone();
                        let schema_block = schema_block.clone();
                        tokio::spawn(handle_connection(
                            stream,
                            registry,
                            frecency,
                            matching.mode,
                            schema_block,
                        ));
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

async fn handle_connection(
    stream: tokio::net::UnixStream,
    registry: Arc<SpecRegistry>,
    frecency: Arc<FrecencyStore>,
    mode: MatchMode,
    schema_block: Arc<Option<String>>,
) {
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
                pid: std::process::id(),
            },
            Ok(Request::Complete { line, cursor, cwd }) => match schema_block.as_ref() {
                // E5: schema mismatch disables all completion; the reason
                // string is what the CLI bridge sniffs for the ZLE hint.
                Some(reason) => Response::Empty {
                    reason: Some(reason.clone()),
                },
                // complete_in is synchronous and can block for hundreds of
                // ms (cold spec parse, generator subprocess). Run it on the
                // blocking pool so one slow completion doesn't stall every
                // other connection on the 2-thread runtime.
                None => {
                    let registry = registry.clone();
                    let frecency = frecency.clone();
                    tokio::task::spawn_blocking(move || {
                        engine_complete(&registry, &frecency, &line, cursor, cwd.as_deref(), mode)
                    })
                    .await
                    .unwrap_or_else(|e| Response::Error {
                        message: format!("completion task failed: {e}"),
                    })
                }
            },
            Ok(Request::DoctorAutorun) => Response::Empty {
                reason: Some("doctor-autorun-stub".to_string()),
            },
            Ok(Request::RecordAccept { spec, insertion }) => {
                // flush_if_dirty rewrites the TSV on disk — keep the file
                // IO off the async workers alongside the in-memory record.
                let frecency = frecency.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    frecency.record(&spec, &insertion);
                    frecency.flush_if_dirty();
                })
                .await;
                Response::Empty {
                    reason: Some("recorded".to_string()),
                }
            }
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

/// Max suggestions transported to the widget per keystroke. The ZLE
/// popup only shows a ~10-row sliding window, but the widget receives
/// *every* row over the socket, splits it into a zsh array, and scans
/// it O(N) to size the box. At the 16 914 `brew install` formulae that
/// scan alone is ~270ms per keystroke — the observed stutter. Capping
/// after ranking is safe because `complete_in` already prefix-filters,
/// so the cap never drops a row the user is typing toward; it only
/// truncates broad empty/one-char browsing, where frecency has already
/// floated any previously-picked entry into the kept head. 500 keeps
/// the widget's split+measure ~6ms while covering typical prefixed
/// result sets (e.g. `brew install a` = 442) with no truncation at all.
const MAX_SUGGESTIONS: usize = 500;

/// Dispatch a Complete request through the real engine pipeline.
/// After the engine returns, apply a frecency boost so suggestions
/// the user has accepted before float to the top of the list.
fn engine_complete(
    registry: &SpecRegistry,
    frecency: &FrecencyStore,
    line: &str,
    cursor: usize,
    cwd: Option<&str>,
    mode: MatchMode,
) -> Response {
    let cwd_path = cwd.map(std::path::Path::new);
    let mut result = complete_in(line, cursor, registry, cwd_path, mode);
    if result.items.is_empty() {
        return Response::Empty {
            reason: result.reason,
        };
    }
    // Extract the binary name once — frecency keys are per-spec.
    if let Some(spec_name) = line.split_whitespace().next() {
        let mut scored: Vec<(f64, Suggestion)> = result
            .items
            .drain(..)
            .map(|s| (frecency.score(spec_name, &s.insertion), s))
            .collect();
        rank_completions(&mut scored);
        result.items = scored.into_iter().map(|(_, s)| s).collect();
    }
    // Bound the transported list — see MAX_SUGGESTIONS. Ranking already
    // ran, so this drops only the least-relevant tail.
    result.items.truncate(MAX_SUGGESTIONS);
    Response::Suggestions {
        items: result.items,
    }
}

/// Order scored completion items for display. `.`/`..` are universal path
/// primitives, not picks to be ranked — pin them to the very top (`.`
/// before `..`) ahead of any frecency boost, so `open .` never buries
/// them under a frecency-boosted `.DS_Store`. Everything else sorts by
/// score DESC; the stable sort preserves the engine's incoming order
/// (priority / exact-case) for score ties.
fn rank_completions(scored: &mut [(f64, Suggestion)]) {
    let dotnav_rank = |disp: &str| match disp {
        "./" => 0u8,
        "../" => 1,
        _ => 2,
    };
    scored.sort_by(|a, b| {
        dotnav_rank(a.1.display.as_str())
            .cmp(&dotnav_rank(b.1.display.as_str()))
            .then_with(|| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal))
    });
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
    // SIGTERM only. SIGINT is intentionally NOT handled — `nerv start`
    // historically inherited the shell's process group, so a Ctrl-C
    // in the user's interactive zsh delivered SIGINT to the daemon
    // too and quietly killed inline completion until the next
    // explicit `nerv start`. Ignoring SIGINT here is a belt to
    // `cmd_start`'s `process_group(0)` suspenders — either alone
    // would fix the regression, both makes it stay fixed.
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM");
    let _ = term.recv().await;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sugg(display: &str) -> Suggestion {
        Suggestion {
            insertion: display.to_string(),
            display: display.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn dotnav_pins_above_frecency_boost() {
        // Regression: `open .` must lead with `./` then `../`, even when
        // a real dotfile (`.DS_Store`) carries a strong frecency boost.
        // The filepaths_at unit test passed while this was broken because
        // the reorder happens here, after the engine returns.
        let mut scored = vec![
            (0.0, sugg("../")),
            (0.0, sugg("./")),
            (9.0, sugg(".DS_Store")), // frecency-boosted real entry
            (0.0, sugg(".gitignore")),
        ];
        rank_completions(&mut scored);
        let order: Vec<&str> = scored.iter().map(|(_, s)| s.display.as_str()).collect();
        assert_eq!(order[0], "./", "current dir must lead");
        assert_eq!(order[1], "../", "parent dir second");
        // Boosted dotfile still beats the unboosted one — below dotnav.
        assert_eq!(order[2], ".DS_Store");
        assert_eq!(order[3], ".gitignore");
    }

    #[test]
    fn transport_cap_keeps_ranked_head_drops_tail() {
        // A huge candidate set (brew's 16k formulae) must not ship whole:
        // the widget scans every row O(N) to size the popup, stuttering
        // past a few hundred. Cap AFTER ranking so a frecency-boosted
        // entry that sorts alphabetically late still survives into the
        // kept head, and only the least-relevant tail is dropped.
        let mut scored: Vec<(f64, Suggestion)> = (0..MAX_SUGGESTIONS + 50)
            .map(|i| (0.0, sugg(&format!("pkg{i:05}"))))
            .collect();
        scored.push((9.0, sugg("zzz-frecency-boosted")));
        rank_completions(&mut scored);
        let mut items: Vec<Suggestion> = scored.into_iter().map(|(_, s)| s).collect();
        items.truncate(MAX_SUGGESTIONS);
        assert_eq!(items.len(), MAX_SUGGESTIONS, "list capped for transport");
        assert_eq!(
            items[0].display, "zzz-frecency-boosted",
            "frecency survivor kept at head despite late alpha order"
        );
    }

    #[test]
    fn non_dotnav_still_sorts_by_frecency() {
        // No `.`/`..` present: pure frecency DESC, stable for ties.
        let mut scored = vec![
            (0.0, sugg("status")),
            (5.0, sugg("checkout")),
            (0.0, sugg("commit")),
        ];
        rank_completions(&mut scored);
        let order: Vec<&str> = scored.iter().map(|(_, s)| s.display.as_str()).collect();
        assert_eq!(order[0], "checkout"); // boosted floats up
        assert_eq!(order[1], "status"); // ties keep incoming order
        assert_eq!(order[2], "commit");
    }
}
