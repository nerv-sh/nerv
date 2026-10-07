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
use nerv_engine::complete::CommandNames;
use nerv_engine::misses::MissCounter;
use nerv_engine::{
    Config, FrecencyStore, HistoryStore, MatchMode, Request, Response, SpecRegistry, Suggestion,
    complete_in, manifest, no_spec_binary, paths, wire,
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
    // `~/.config/nerv/nerv.toml`, read once at boot — matching mode
    // (PLAN §5.1) and derivation switch. Edits need a daemon restart.
    let config = Config::load_default();
    info!(mode = ?config.matching.mode, derive = config.derived.enabled, "config loaded");
    let layers =
        paths::resolve_spec_layers(config.derived.enabled).expect("HOME present (just checked)");

    // Lazy registry: no upfront disk scan. Specs are read on first
    // lookup and cached. Startup stays O(1) even with 700+ specs. With
    // a derived layer present, a command with no spec anywhere gets one
    // from its own `--help` on the background populator thread.
    let registry = Arc::new(SpecRegistry::for_layers(&layers));
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
        .unwrap_or_else(|| cache_dir.join(paths::FRECENCY_NAME));
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

    // Spec-miss tally: which commands completed empty for want of a
    // spec. Local diagnostics only — `nerv doctor` reads it back so the
    // user knows which overlay spec is worth writing. Same `-` sentinel
    // as frecency for test isolation.
    let misses_path = std::env::var_os("NERV_MISSES_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| cache_dir.join(paths::MISSES_NAME));
    let misses = if misses_path == std::path::PathBuf::from("-") {
        Arc::new(MissCounter::empty())
    } else {
        Arc::new(MissCounter::load(&misses_path))
    };
    info!(
        path = %misses_path.display(),
        entries = misses.len(),
        "spec-miss counter loaded"
    );

    // Executed-command history behind history-ranked suggestions. Same
    // `-` sentinel as frecency; `paths::history_path` honours
    // NERV_HISTORY_FILE so the CLI and the daemon agree on the file.
    let history_path = paths::history_path().expect("HOME present (just checked)");
    let history = if history_path == std::path::PathBuf::from("-") {
        Arc::new(HistoryStore::empty())
    } else {
        Arc::new(HistoryStore::load(&history_path))
    };
    info!(
        path = %history_path.display(),
        rows = history.len(),
        "command history loaded"
    );

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

    // Command names offered while the first token is still being typed.
    // Spec stems are listed lazily on the first completion; the PATH
    // walk is kicked off here so it is warm by the time anyone types,
    // without boot waiting on it.
    let names = Arc::new(NameCache::from_env());
    names.prewarm();

    let shared = Shared {
        registry,
        frecency,
        misses: misses.clone(),
        names,
        history,
        mode: config.matching.mode,
        schema_block,
    };

    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            res = listener.accept() => {
                match res {
                    Ok((stream, _addr)) => {
                        tokio::spawn(handle_connection(stream, shared.clone()));
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

    // Last chance to persist the tally: `flush_if_dirty` is throttled
    // (MIN_FLUSH_INTERVAL) so the counts from the final window are still
    // in memory here.
    misses.flush_now();

    let _ = tokio::fs::remove_file(&sock_path).await;
    let _ = tokio::fs::remove_file(&pid_path).await;
    Ok(())
}

/// Daemon state every connection reads. Cloning is a handful of `Arc`
/// bumps.
#[derive(Clone)]
struct Shared {
    registry: Arc<SpecRegistry>,
    frecency: Arc<FrecencyStore>,
    misses: Arc<MissCounter>,
    names: Arc<NameCache>,
    history: Arc<HistoryStore>,
    mode: MatchMode,
    schema_block: Arc<Option<String>>,
}

async fn handle_connection(stream: tokio::net::UnixStream, shared: Shared) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    loop {
        // Bounded line read. `BufReader::lines()` is unbounded, and a
        // shell-name registration rides one long line — a buggy client
        // must not grow the daemon's heap without limit. Bytes past
        // MAX_REQUEST_LINE are drained to the newline and the line is
        // answered with an Error before parsing — a client blocked on
        // its reply must not wait forever — and the connection stays
        // usable. Bytes are decoded once per line: a multi-byte name
        // split across two reads would decode as U+FFFD per chunk.
        let mut line: Vec<u8> = Vec::new();
        let mut oversized = false;
        let mut eof = false;
        loop {
            let available = match reader.fill_buf().await {
                Ok(buf) => buf,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return,
            };
            if available.is_empty() {
                eof = true;
                break;
            }
            if let Some(pos) = available.iter().position(|&b| b == b'\n') {
                if !oversized {
                    if line.len() + pos > MAX_REQUEST_LINE {
                        oversized = true;
                        keep_head(&mut line, &available[..pos]);
                    } else {
                        line.extend_from_slice(&available[..pos]);
                    }
                }
                reader.consume(pos + 1);
                break;
            }
            if !oversized {
                if line.len() + available.len() > MAX_REQUEST_LINE {
                    oversized = true;
                    keep_head(&mut line, available);
                } else {
                    line.extend_from_slice(available);
                }
            }
            let len = available.len();
            reader.consume(len);
        }
        if oversized && eof {
            break;
        }
        if !oversized && line.is_empty() {
            if eof {
                break;
            }
            continue;
        }
        let line = String::from_utf8_lossy(&line);
        let trimmed = line.trim();
        if !oversized && trimmed.is_empty() {
            continue;
        }
        // An oversized text request (a huge pasted command) keeps only its
        // head; that is enough to answer it in its own protocol. A JSON
        // error here would read to the widget as "old daemon" and switch
        // its socket off for good.
        if oversized && wire::is_text_request(&line) {
            warn!("text request exceeds {MAX_REQUEST_LINE} bytes");
            if write_lines(&mut write_half, wire::text_error_reply(&line))
                .await
                .is_err()
            {
                break;
            }
            continue;
        }
        // A line opening with a text verb comes from zsh's socket path
        // (`nerv_engine::wire`); everything else is JSON, or garbage that
        // the JSON path answers with an Error.
        // Only the line break goes: the last field (a record's `prev`)
        // may end in a space the fork path keeps too.
        let raw = line.trim_end_matches(['\r', '\n']);
        if !oversized && wire::is_text_request(raw) {
            let text = wire::parse_text_request(raw);
            // Never log a record line: it is command text.
            debug!(
                verb = raw.split(wire::US).next().unwrap_or(""),
                "received text"
            );
            let lines = match text {
                Ok(text) => {
                    let request = match &text {
                        wire::TextRequest::Complete { request, .. }
                        | wire::TextRequest::Predict { request, .. }
                        | wire::TextRequest::Record(request) => request.clone(),
                    };
                    let resp = dispatch(Ok(request), &shared).await;
                    wire::text_reply(&text, resp)
                }
                Err(e) => {
                    warn!(%e, "bad text request");
                    wire::text_error_reply(raw)
                }
            };
            if write_lines(&mut write_half, lines).await.is_err() {
                break;
            }
            continue;
        }
        // The line itself stays out of the log: a record carries the command
        // run, a complete the line being typed.
        debug!(bytes = trimmed.len(), "received");
        let request = if oversized {
            Err(format!("request line exceeds {MAX_REQUEST_LINE} bytes"))
        } else {
            serde_json::from_str::<Request>(trimmed).map_err(|e| format!("invalid request: {e}"))
        };
        let resp = dispatch(request, &shared).await;
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

/// How much of an oversized line is kept: enough for a text request's
/// verb and seq.
const OVERSIZED_HEAD: usize = 64;

/// Keep the head of an oversized line, drop the rest.
fn keep_head(line: &mut Vec<u8>, more: &[u8]) {
    let room = OVERSIZED_HEAD.saturating_sub(line.len());
    line.extend_from_slice(&more[..room.min(more.len())]);
    line.truncate(OVERSIZED_HEAD);
}

/// Write text-protocol reply lines in one go; nothing for an empty list.
async fn write_lines(
    write_half: &mut tokio::net::unix::OwnedWriteHalf,
    lines: Vec<String>,
) -> std::io::Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    let mut out = String::new();
    for l in lines {
        out.push_str(&l);
        out.push('\n');
    }
    write_half.write_all(out.as_bytes()).await?;
    write_half.flush().await
}

/// Answer one decoded request — the same for JSON and text requests.
async fn dispatch(request: Result<Request, String>, shared: &Shared) -> Response {
    let Shared {
        registry,
        frecency,
        misses,
        names,
        history,
        mode,
        schema_block,
    } = shared.clone();
    match request {
        Ok(Request::Ping) => Response::Pong {
            version: env!("CARGO_PKG_VERSION").to_string(),
            pid: std::process::id(),
        },
        Ok(Request::Complete {
            line,
            cursor,
            cwd,
            prev,
            typed,
        }) => match schema_block.as_ref() {
            // E5: schema mismatch disables all completion; the reason
            // string is what the CLI bridge sniffs for the ZLE hint.
            Some(reason) => Response::empty(reason.clone()),
            // complete_in is synchronous and can block for hundreds of
            // ms (cold spec parse, generator subprocess). Run it on the
            // blocking pool so one slow completion doesn't stall every
            // other connection on the 2-thread runtime.
            None => {
                let registry = registry.clone();
                let frecency = frecency.clone();
                let misses = misses.clone();
                let names = names.clone();
                let history = history.clone();
                tokio::task::spawn_blocking(move || {
                    // Building the name list means stat-ing every
                    // spec layer, cloning ~700 stems and folding the
                    // frecency table. The engine calls this only for
                    // the first token or a command word with no spec
                    // — a small minority of keystrokes.
                    let cmd_names = || names.names(&registry, &frecency);
                    let mut resp = engine_complete(
                        &registry,
                        Ranking {
                            frecency: &frecency,
                            history: Some(&history),
                            prev: prev.as_deref().unwrap_or(""),
                        },
                        Some(&cmd_names),
                        &line,
                        cursor,
                        cwd.as_deref(),
                        mode,
                    );
                    attach_ghost(
                        &mut resp,
                        &history,
                        typed.as_deref().unwrap_or(&line),
                        cwd.as_deref().unwrap_or(""),
                        prev.as_deref().unwrap_or(""),
                    );
                    // A "no spec for X" empty is the only response the
                    // tally cares about — and only once it is settled.
                    // The first keystroke on a cold stem returns empty
                    // while the spec is still parsing or being derived
                    // from `--help`; counting that would list commands
                    // that complete fine one key later. The flush is
                    // throttled inside the counter.
                    if let Response::Empty {
                        reason: Some(r), ..
                    } = &resp
                    {
                        if let Some(binary) = no_spec_binary(r) {
                            if !registry.is_loading(binary) {
                                misses.record(binary);
                                misses.flush_if_dirty();
                            }
                        }
                    }
                    resp
                })
                .await
                .unwrap_or_else(|e| Response::Error {
                    message: format!("completion task failed: {e}"),
                })
            }
        },
        Ok(Request::DoctorAutorun) => Response::empty("doctor-autorun-stub".to_string()),
        Ok(Request::RecordAccept { spec, insertion }) => {
            // flush_if_dirty rewrites the TSV on disk — keep the file
            // IO off the async workers alongside the in-memory record.
            let frecency = frecency.clone();
            let _ = tokio::task::spawn_blocking(move || {
                frecency.record(&spec, &insertion);
                frecency.flush_if_dirty();
            })
            .await;
            Response::empty("recorded".to_string())
        }
        Ok(Request::RegisterShellNames { names: incoming }) => {
            // Memory-only by contract: the names are dotfile content
            // (docs/error-states.md §3.6.3 — local only, no
            // telemetry), so registration itself touches no file. The
            // prune below rewrites the MISS tally, which is ordinary
            // daemon-owned state — the names never reach it.
            if !incoming.is_empty() {
                let shell_snapshot = {
                    let mut shell = shell_lock(&names.shell);
                    *shell = shell_names_union(&shell, &incoming);
                    shell.clone()
                };
                // A function·alias the shell completes itself must
                // not keep a miss row: doctor would advise an overlay
                // spec for it, which is always wrong advice. This is
                // the only place the tally can be pruned — the names
                // exist nowhere else, and they arrive here. File IO
                // stays off the async workers, like every tally write.
                let misses = misses.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let set: std::collections::HashSet<String> =
                        shell_snapshot.iter().cloned().collect();
                    let dropped = misses.prune(&set);
                    if dropped > 0 {
                        info!(dropped, "pruned shell-name rows from the miss tally");
                    }
                })
                .await;
            }
            Response::empty("registered".to_string())
        }
        Ok(Request::RecordCommand {
            command,
            expanded,
            cwd,
            exit,
            prev,
        }) => {
            // The append is file IO — off the async workers, like
            // every other cache write.
            let history = history.clone();
            let entry = nerv_engine::history::Entry {
                ts: nerv_engine::history::now_unix(),
                exit,
                cwd,
                prev,
                command,
                expanded,
            };
            let res = tokio::task::spawn_blocking(move || history.record(entry)).await;
            match res {
                Ok(Ok(_)) => Response::empty("recorded".to_string()),
                Ok(Err(e)) => {
                    warn!(%e, "history write failed");
                    Response::Error {
                        message: format!("history write failed: {e}"),
                    }
                }
                Err(e) => Response::Error {
                    message: format!("history task failed: {e}"),
                },
            }
        }
        // No `$history` fallback exists for a prediction, so "none" is
        // simply an absent ghost.
        Ok(Request::Predict { prev, cwd }) => Response::Empty {
            reason: None,
            unspecced: false,
            ghost: history.predict(&prev, &cwd),
        },
        Ok(Request::ImportHistory { path }) => {
            let history = history.clone();
            let res = tokio::task::spawn_blocking(move || {
                history.import_zsh_history(std::path::Path::new(&path))
            })
            .await;
            match res {
                Ok(Ok(n)) => {
                    info!(imported = n, "zsh history imported");
                    Response::empty(format!("imported {n}"))
                }
                Ok(Err(e)) => Response::Error {
                    message: format!("history import failed: {e}"),
                },
                Err(e) => Response::Error {
                    message: format!("history task failed: {e}"),
                },
            }
        }
        Err(message) => Response::Error { message },
    }
}

/// The command-name list handed to the engine while the first token is
/// being typed, kept off the keystroke path.
///
/// Spec stems are a `read_dir` per layer, so they are re-listed only
/// when a layer directory's mtime moves — a derived spec landing, the
/// user dropping in an overlay. The frecent list is read straight from
/// the in-memory frecency table every time, so a command accepted a
/// second ago ranks immediately.
#[derive(Default)]
struct NameCache {
    stems: std::sync::Mutex<StemSnapshot>,
    path: Arc<PathCache>,
    /// Shell function·alias names registered by zsh sessions (plan
    /// slice 02). Memory-only — the names are dotfile content and must
    /// reach no file. Sessions union; the cap drops the oldest.
    shell: SharedShellNames,
}

/// The shell-name store behind `NameCache::shell`: an `Arc` under a
/// mutex, so a registration swaps the whole list in place while
/// concurrent keystrokes keep serving the previous `Arc` untouched.
type SharedShellNames = std::sync::Mutex<Arc<Vec<String>>>;

/// Cap on the union of registered shell names (plan slice 02). A zsh
/// session caps itself at 2,000 before sending; the daemon accepts
/// several sessions, so the union cap is the next power of two up.
const SHELL_NAMES_CAP: usize = 4096;

/// Request-line length cap (plan slice 02). A shell-name registration
/// rides one long line — thousands of names at ~20 bytes each — so the
/// cap sits a power of ten above real traffic; anything longer is
/// garbage or hostile and is discarded before parsing.
const MAX_REQUEST_LINE: usize = 256 * 1024;

/// Union of the sessions' shell names, oldest first: dedup, cap at
/// `SHELL_NAMES_CAP`, overflow drops the oldest. A name the incoming
/// registration repeats moves to the newest end — a shell that just
/// re-registered it is the freshest owner, and must not lose it to the
/// cap as if it were stale. Free-standing so the cap contract is
/// unit-testable without a daemon.
fn shell_names_union(existing: &[String], incoming: &[String]) -> Arc<Vec<String>> {
    let fresh: std::collections::HashSet<&str> = incoming.iter().map(String::as_str).collect();
    let mut seen = std::collections::HashSet::with_capacity(existing.len() + incoming.len());
    let mut union: Vec<String> = Vec::with_capacity(existing.len() + incoming.len());
    let kept = existing.iter().filter(|n| !fresh.contains(n.as_str()));
    for name in kept.chain(incoming.iter()) {
        if seen.insert(name.as_str()) {
            union.push(name.clone());
        }
    }
    if union.len() > SHELL_NAMES_CAP {
        union.drain(..union.len() - SHELL_NAMES_CAP);
    }
    Arc::new(union)
}

/// A poisoned mutex would take every later keystroke down with it; the
/// shell list is a plain cache, so recovering the inner value is safe.
fn shell_lock(shell: &SharedShellNames) -> std::sync::MutexGuard<'_, Arc<Vec<String>>> {
    shell.lock().unwrap_or_else(|poisoned| {
        shell.clear_poison();
        poisoned.into_inner()
    })
}

/// Executable names on `PATH`, the last source offered for a command
/// name. A full scan walks thousands of files, so it never runs on the
/// keystroke path: a request stats the `PATH` directories (tens of
/// them), serves whatever the last completed scan produced — an empty
/// list before the first one lands — and schedules a rescan in the
/// background when a stamp moved.
///
/// `NERV_PATH_SCAN=0` leaves the source empty — the switch the Rust
/// daemon test harnesses set so command-name rows don't vary with the
/// developer's real `PATH`.
#[derive(Default)]
struct PathCache {
    dirs: Vec<std::path::PathBuf>,
    snap: std::sync::Mutex<PathSnapshot>,
    scanning: std::sync::atomic::AtomicBool,
}

/// `PATH` directories to scan, or none when the scan is switched off.
/// Split out from the environment read so the switch itself is testable
/// — four daemon harnesses rely on it holding.
fn dirs_from(scan: Option<&str>, path: &str) -> Vec<std::path::PathBuf> {
    if scan == Some("0") {
        return vec![];
    }
    nerv_engine::complete::path_dirs(path)
}

#[derive(Default)]
struct PathSnapshot {
    names: Arc<Vec<String>>,
    stamp: Option<Vec<Option<std::time::SystemTime>>>,
}

#[derive(Default)]
struct StemSnapshot {
    names: Arc<Vec<String>>,
    /// One mtime per spec layer, in `layer_dirs` order. `None` marks a
    /// layer that does not exist yet (a missing overlay is normal); its
    /// later creation still shows up as a change.
    stamp: Option<Vec<Option<std::time::SystemTime>>>,
}

impl NameCache {
    /// Read `PATH` once, from the daemon's own environment.
    /// `Request::Complete` does not carry the client's `PATH`, so a
    /// shell that exports a new one is invisible here until the daemon
    /// restarts — the same bound `nerv.toml` already has.
    fn from_env() -> Self {
        let dirs = dirs_from(
            std::env::var("NERV_PATH_SCAN").ok().as_deref(),
            &std::env::var("PATH").unwrap_or_default(),
        );
        Self {
            path: Arc::new(PathCache {
                dirs,
                ..PathCache::default()
            }),
            ..Self::default()
        }
    }

    /// Kick the first `PATH` scan so the source is warm by the time the
    /// user types, without making boot wait for it.
    fn prewarm(&self) {
        self.path.schedule_rescan();
    }

    fn names(&self, registry: &SpecRegistry, frecency: &FrecencyStore) -> CommandNames {
        CommandNames::from_shared(
            frecency.spec_names(),
            self.stems(registry),
            self.path.names(),
            self.shell_names(),
        )
    }

    /// The registered shell names, as a shared list. Locking is only
    /// ever an `Arc` clone — registrations swap, they never mutate.
    fn shell_names(&self) -> Arc<Vec<String>> {
        shell_lock(&self.shell).clone()
    }

    /// Spec stems, re-listed only when a layer's mtime moved. The
    /// `read_dir` runs *outside* the lock: the moment a derived spec
    /// lands, every concurrent keystroke would otherwise queue behind
    /// one directory walk.
    fn stems(&self, registry: &SpecRegistry) -> Arc<Vec<String>> {
        let stamp: Vec<Option<std::time::SystemTime>> = registry
            .layer_dirs()
            .iter()
            .map(|dir| std::fs::metadata(dir).and_then(|m| m.modified()).ok())
            .collect();
        {
            let snap = self.lock();
            if snap.stamp.as_ref() == Some(&stamp) {
                return snap.names.clone();
            }
        }
        let names = Arc::new(registry.dir_listing());
        let mut snap = self.lock();
        snap.names = names.clone();
        snap.stamp = Some(stamp);
        names
    }

    /// A panic while the snapshot was held would otherwise poison the
    /// mutex for the rest of the process, sending *every* later
    /// keystroke back to `read_dir`. The snapshot is a plain cache, so
    /// taking the inner value back is always safe.
    fn lock(&self) -> std::sync::MutexGuard<'_, StemSnapshot> {
        self.stems.lock().unwrap_or_else(|poisoned| {
            self.stems.clear_poison();
            poisoned.into_inner()
        })
    }
}

impl PathCache {
    /// Names from the last completed scan. Touches the disk only to
    /// stat each `PATH` directory; a moved stamp schedules a rescan
    /// instead of running one here.
    fn names(self: &Arc<Self>) -> Arc<Vec<String>> {
        if self.dirs.is_empty() {
            return Arc::default();
        }
        let stamp = self.stamps();
        let (names, fresh) = {
            let snap = self.lock();
            (snap.names.clone(), snap.stamp.as_ref() == Some(&stamp))
        };
        if !fresh {
            self.schedule_rescan();
        }
        names
    }

    fn schedule_rescan(self: &Arc<Self>) {
        use std::sync::atomic::Ordering;
        if self.dirs.is_empty() || self.scanning.swap(true, Ordering::SeqCst) {
            return;
        }
        let cache = self.clone();
        std::thread::spawn(move || {
            // Clearing the flag on drop, not after the call: a panic in
            // the walk would otherwise leave it set for the daemon's
            // lifetime and freeze the name list silently. Same guard the
            // generator cache uses for its in-flight set.
            let _guard = ScanGuard(&cache);
            cache.rescan();
        });
    }

    /// The walk itself. The stamp is taken *before* it, so a directory
    /// that changes mid-scan leaves the snapshot looking stale and the
    /// next request schedules another pass.
    fn rescan(&self) {
        let stamp = self.stamps();
        let names = Arc::new(nerv_engine::complete::executables_in(&self.dirs));
        let mut snap = self.lock();
        snap.names = names;
        snap.stamp = Some(stamp);
    }

    fn stamps(&self) -> Vec<Option<std::time::SystemTime>> {
        self.dirs
            .iter()
            .map(|dir| std::fs::metadata(dir).and_then(|m| m.modified()).ok())
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PathSnapshot> {
        self.snap.lock().unwrap_or_else(|poisoned| {
            self.snap.clear_poison();
            poisoned.into_inner()
        })
    }
}

/// Releases `PathCache::scanning` however the scan thread ends.
struct ScanGuard<'a>(&'a PathCache);

impl Drop for ScanGuard<'_> {
    fn drop(&mut self) {
        self.0
            .scanning
            .store(false, std::sync::atomic::Ordering::SeqCst);
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

/// What orders a completion reply: popup picks, and the command history
/// when there is one.
#[derive(Clone, Copy)]
struct Ranking<'a> {
    frecency: &'a FrecencyStore,
    history: Option<&'a HistoryStore>,
    /// The command run before this one — the sequence signal.
    prev: &'a str,
}

/// Dispatch a Complete request through the real engine pipeline.
/// After the engine returns, rank the rows by what the user picks and
/// runs ([`rank_by_frecency`]).
fn engine_complete(
    registry: &SpecRegistry,
    ranking: Ranking<'_>,
    names: Option<&dyn Fn() -> CommandNames>,
    line: &str,
    cursor: usize,
    cwd: Option<&str>,
    mode: MatchMode,
) -> Response {
    let cwd_path = cwd.map(std::path::Path::new);
    let mut result = complete_in(line, cursor, registry, cwd_path, mode, names);
    let before = before_cursor(line, cursor);
    let signals = ranking
        .history
        .map(|h| h.token_signals(&completed_words(before), cwd.unwrap_or(""), ranking.prev));
    let mut history_words = std::collections::HashSet::new();
    if let Some(sig) = signals
        .as_deref()
        .filter(|_| wants_history_rows(&result.items, result.reason.as_deref()))
    {
        let extra = history_rows(
            sig,
            &result.items,
            partial_word(before),
            cwd_path,
            result.path_arg,
        );
        history_words = extra.iter().map(|s| s.insertion.clone()).collect();
        result.items.extend(extra);
    }
    if result.items.is_empty() {
        return Response::Empty {
            reason: result.reason,
            unspecced: result.unspecced,
            ghost: None,
        };
    }
    // Extract the binary name once — frecency keys are per-spec.
    if let Some(spec_name) = line.split_whitespace().next() {
        result.items = rank_by_frecency(
            std::mem::take(&mut result.items),
            spec_name,
            ranking.frecency,
            signals.as_deref(),
            MAX_SUGGESTIONS,
        );
    }
    // History rows follow the spec's own, as in Fig: they are words the
    // spec does not know, so a spec row that ranks lower is still the
    // likelier pick. Within the spec's rows, folders that exist here lead
    // (`cd ` → subfolders before `~` and `-`): a constant collects picks
    // and runs from every directory, a folder only from this one, so a
    // folder never entered would otherwise sit under the constants.
    // Stable, so each group keeps its ranked order (`./`, `../` first).
    // Cached key: one stat per row, not one per comparison.
    result.items.sort_by_cached_key(|s| {
        let history = history_words.contains(&s.insertion);
        let folder_here = !history
            && s.insertion.ends_with('/')
            && cwd_path.is_some_and(|d| d.join(&s.insertion).is_dir());
        (history, !folder_here)
    });
    // Ranking already truncated to the transport cap (MAX_SUGGESTIONS).
    debug_assert!(result.items.len() <= MAX_SUGGESTIONS);
    Response::Suggestions {
        items: result.items,
        token_complete: result.token_complete,
        unspecced: result.unspecced,
        ghost: None,
    }
}

/// Put the history ghost for `typed` on a completion reply. An empty
/// history leaves `ghost` unset, so the widget keeps zsh's `$history`
/// fallback — a fresh install, or one still importing, is not left with
/// no ghost at all. Otherwise the field is always set, `""` for "no
/// match": the widget must not paint `$history` over a ranked "nothing".
fn attach_ghost(resp: &mut Response, history: &HistoryStore, typed: &str, cwd: &str, prev: &str) {
    if history.is_empty() {
        return;
    }
    let found = history.ghost(typed, cwd, prev).unwrap_or_default();
    if let Response::Suggestions { ghost, .. } | Response::Empty { ghost, .. } = resp {
        *ghost = Some(found);
    }
}

/// `line` up to `cursor`. The widget counts the cursor in characters
/// (`${#send_line}`), so a byte slice would split `ls 한글` mid-character
/// and panic; past the end it is the whole line.
fn before_cursor(line: &str, cursor: usize) -> &str {
    line.char_indices()
        .nth(cursor)
        .map_or(line, |(byte, _)| &line[..byte])
}

/// The word being typed at the cursor: `ch` of `git ch`, empty right
/// after a space or a separator.
fn partial_word(before_cursor: &str) -> &str {
    if before_cursor.ends_with(char::is_whitespace) {
        return "";
    }
    let word = before_cursor
        .rsplit(char::is_whitespace)
        .next()
        .unwrap_or("");
    word.rsplit([';', '|', '&']).next().unwrap_or(word)
}

/// Whether history rows may join this reply. Three replies are contracts
/// that extra rows would break:
/// - a command-word correction comes back as the only row, and the
///   widget recognises it by that (`(( ${#rlines} == 1 ))` in _nerv.zsh);
/// - "no spec for X" is what the miss tally counts and what sends the
///   widget to zsh's own completion;
/// - rows the source ranked itself (zoxide, command names) keep that
///   order. A history row would score above them, and after `z` it is a
///   partial query (`z nerv`) that zoxide's rows, full paths, never match:
///   picking it re-runs the fuzzy jump the full paths are there to avoid.
fn wants_history_rows(items: &[Suggestion], reason: Option<&str>) -> bool {
    !items.iter().any(|s| s.replace.is_some() || s.source_ranked)
        && reason.is_none_or(|r| no_spec_binary(r).is_none())
}

/// How many words the history may add as rows of their own.
const HISTORY_ROWS: usize = 5;

/// Words that followed this line in the history but that the spec does
/// not offer — `feature-x` after `git checkout` when no branch generator
/// ran, a host typed after `ssh` that `~/.ssh/config` does not list. Only words that extend what is being typed,
/// best frecency first, at most [`HISTORY_ROWS`]. Their rows read
/// `history`, are ranked with the rest and then placed after the spec's
/// rows ([`engine_complete`]). A word that only ever came in failed runs
/// (`yarn web:deployㅔ`, `yarn w\eb:deploy`), or only once in the imported
/// zsh history, is a typo, not a pick.
///
/// A relative path (`src/x.rs`) names a file in the directory it was
/// typed in: it is offered only where it was typed or where it exists,
/// or it would outrank the files that are really here. A word written as
/// a folder (`build/`) is offered only while that folder exists.
fn history_rows(
    signals: &nerv_engine::history::TokenSignals,
    items: &[Suggestion],
    partial: &str,
    cwd: Option<&std::path::Path>,
    path_arg: bool,
) -> Vec<Suggestion> {
    use nerv_engine::history::token_key;
    let have: std::collections::HashSet<&str> =
        items.iter().map(|s| token_key(&s.insertion)).collect();
    let now = nerv_engine::history::now_unix();
    let mut words: Vec<(f64, &nerv_engine::history::TokenStat)> = signals
        .tokens
        .iter()
        .filter(|(key, t)| {
            !have.contains(key.as_str())
                // A success nerv saw, or — among rows imported from the zsh
                // history, which carry no exit status (nor a directory, so
                // they are `count - in_dirs`) — a word typed more than once:
                // a one-off there is as likely a typo.
                && (t.ok > 0 || t.count.saturating_sub(t.in_dirs) >= 2)
                && t.word.starts_with(partial)
                && t.word != partial
                // Quoting (a backslash too), expansions, control operators
                // and redirections: the index splits on whitespace only, so
                // these are pieces of a larger word (`'quoted`, `My\` of
                // `My\ Folder/`, `&&`, `>`, `main;make`).
                && !t.word.contains(['\'', '"', '`', '\\', '$', ';', '|', '&', '<', '>', '(', ')'])
        })
        .map(|(_, t)| (nerv_engine::history::frecency(t.count, t.last, now), t))
        .collect();
    words.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.word.cmp(&b.1.word)));
    words
        .into_iter()
        // The disk is asked last and lazily: a stat for the few best
        // words, not for every word the history holds.
        .filter(|(_, t)| applies_here(t, cwd, path_arg))
        .take(HISTORY_ROWS)
        .map(|(_, t)| Suggestion {
            insertion: t.word.clone(),
            display: t.word.clone(),
            description: Some("history".to_string()),
            icon: Some(nerv_engine::complete::HISTORY_ICON.to_string()),
            kind: nerv_engine::SuggestionKind::Argument,
            ..Suggestion::default()
        })
        .collect()
}

/// A word that names a path relative to the directory it was typed in:
/// it has a `/`, and does not start at `/` or `~`.
fn is_relative_path(word: &str) -> bool {
    word.contains('/') && !word.starts_with(['/', '~'])
}

/// Whether a history word still means something in `cwd`.
///
/// A word that is a path must name something now: the source of an `mv`
/// or `rm -r` that went through is gone, even where it was typed. Two
/// things say "path":
/// - the spec, for the argument at the cursor (`path_arg`): every word
///   there is one, whatever it looks like (`mv old-name`);
/// - a trailing `/` (`build/`, `/tmp/out/`), which must be a folder.
///   Anywhere else `feature/x` or `org/image` may be a branch or an
///   image, and is not checked.
///
/// Never checked: an option (`-v`), `~/…` and a glob (the shell expands
/// them, this does not), a word with a `:` (a URL, `host:backup/`).
///
/// Any other relative path applies where it was typed or where it exists.
fn applies_here(
    t: &nerv_engine::history::TokenStat,
    cwd: Option<&std::path::Path>,
    path_arg: bool,
) -> bool {
    let word = t.word.as_str();
    let local = !word.starts_with(['~', '-']) && !word.contains([':', '*', '?', '[', '{']);
    match cwd {
        Some(dir) if local && word.ends_with('/') => dir.join(word).is_dir(),
        Some(dir) if local && path_arg => dir.join(word).exists(),
        _ => t.here > 0 || !is_relative_path(word) || cwd.is_some_and(|d| d.join(word).exists()),
    }
}

/// The finished words of the command being completed: `["git"]` for
/// `git ch`, `["git", "checkout"]` for `git checkout `. Only the last
/// segment of a compound line counts — after `&&`, `||`, `;` or `|`,
/// spaced or not (`cd x; git ch` → `["git"]`) — the part the engine
/// completes. Words split on whitespace, like the history index they are
/// looked up in; a separator inside quotes also splits, which at worst
/// loses the history signal for that line.
fn completed_words(before_cursor: &str) -> Vec<&str> {
    let segment = before_cursor
        .rfind([';', '|', '&'])
        .map_or(before_cursor, |i| &before_cursor[i + 1..]);
    let mut words: Vec<&str> = segment.split_whitespace().collect();
    if !segment.ends_with(char::is_whitespace) {
        words.pop();
    }
    words
}

/// Score each item for display, then order them via [`rank_completions`]
/// (docs/history-suggestions.md §5):
///
/// score = 0.4 · frecency + 0.3 · directory + 0.5 · sequence + 0.25 · head sequence
///
/// frecency is the item's popup accepts ([`FrecencyStore::score`]) plus
/// the runs of recorded commands that had this word here — typing
/// `git status` by hand counts as much as picking it — normalised by the
/// best in the list. directory is the share of those runs made in the
/// current directory. sequence is how often those runs directly followed
/// the previous command (head sequence: its first two words), divided by
/// the best in the list. Source-ranked rows (zoxide) score zero so the
/// stable sort keeps the engine's order for them. The list is truncated
/// to `cap` after sorting (dropping the least-relevant tail).
fn rank_by_frecency(
    items: Vec<Suggestion>,
    spec_name: &str,
    frecency: &FrecencyStore,
    signals: Option<&nerv_engine::history::TokenSignals>,
    cap: usize,
) -> Vec<Suggestion> {
    use nerv_engine::history::{W_DIR, W_FRECENCY, W_SEQ, W_SEQ_HEAD};
    let now = nerv_engine::history::now_unix();
    // A sequence seen once is not a habit — the same floor the empty-prompt
    // prediction uses; one `git add` → `git stash` must not top the popup.
    let habit = |n: u32| {
        if n >= nerv_engine::history::PREDICT_MIN {
            n as f64
        } else {
            0.0
        }
    };
    // (frecency, directory share, after prev, after head) per row.
    let raw: Vec<(f64, f64, f64, f64)> = items
        .iter()
        .map(|s| {
            if s.source_ranked {
                return (0.0, 0.0, 0.0, 0.0);
            }
            let accepts = frecency.score(spec_name, &s.insertion);
            let Some(t) = signals.and_then(|sig| sig.get(&s.insertion)) else {
                return (accepts, 0.0, 0.0, 0.0);
            };
            let typed = nerv_engine::history::frecency(t.count, t.last, now);
            let dir = if t.in_dirs == 0 {
                0.0
            } else {
                t.here as f64 / t.in_dirs as f64
            };
            (
                accepts + typed,
                dir,
                habit(t.after_prev),
                habit(t.after_head),
            )
        })
        .collect();
    let max = |f: fn(&(f64, f64, f64, f64)) -> f64| raw.iter().map(f).fold(0.0, f64::max);
    let (frec_max, prev_max, head_max) = (max(|r| r.0), max(|r| r.2), max(|r| r.3));
    let norm = |v: f64, m: f64| if m > 0.0 { v / m } else { 0.0 };
    let mut scored: Vec<(f64, Suggestion)> = items
        .into_iter()
        .zip(raw)
        .map(|(s, (frec, dir, after_prev, after_head))| {
            let score = W_FRECENCY * norm(frec, frec_max)
                + W_DIR * dir
                + W_SEQ * norm(after_prev, prev_max)
                + W_SEQ_HEAD * norm(after_head, head_max);
            (score, s)
        })
        .collect();
    rank_completions(&mut scored);
    scored.truncate(cap);
    scored.into_iter().map(|(_, s)| s).collect()
}

/// Order scored completion items for display. `.`/`..` are universal path
/// primitives, not picks to be ranked — pin them to the very top (`.`
/// before `..`) ahead of any frecency boost, so `open .` never buries
/// them under a frecency-boosted `.DS_Store`. Everything else sorts by
/// score DESC; the stable sort preserves the engine's incoming order
/// (priority / exact-case) for score ties. **That stability is
/// load-bearing**: `rank_by_frecency` gives `source_ranked` rows (zoxide)
/// score 0.0 and relies on the tie keeping the engine's order — a future
/// secondary tie-break here (e.g. alpha) would silently re-alphabetize
/// them and re-introduce the `z tak-bro` ranking regression.
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

    /// Directory mtime is the whole guard keeping `read_dir` off the
    /// keystroke path: an unchanged layer must be served from the
    /// snapshot, and a spec landing in it (a `--help` derivation, a
    /// user overlay) must show up on the next keystroke.
    #[test]
    fn name_cache_relists_stems_only_when_a_layer_changes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("foo.json"), "{}").expect("write spec");
        let registry = SpecRegistry::at_dir(tmp.path());
        let frecency = FrecencyStore::empty();
        let cache = NameCache::default();

        assert_eq!(*cache.stems(&registry), vec!["foo".to_string()]);

        // Add a spec but rewind the directory mtime: the snapshot is
        // keyed on that stamp, so the new file must stay invisible.
        let before = std::fs::metadata(tmp.path())
            .and_then(|m| m.modified())
            .expect("dir mtime");
        std::fs::write(tmp.path().join("bar.json"), "{}").expect("write spec");
        let dir = std::fs::File::open(tmp.path()).expect("open dir");
        dir.set_times(std::fs::FileTimes::new().set_modified(before))
            .expect("rewind dir mtime");
        assert_eq!(
            *cache.stems(&registry),
            vec!["foo".to_string()],
            "an unchanged stamp must be served from the snapshot"
        );

        // A real mtime move re-lists.
        dir.set_times(
            std::fs::FileTimes::new().set_modified(before + std::time::Duration::from_secs(1)),
        )
        .expect("advance dir mtime");
        assert_eq!(
            *cache.stems(&registry),
            vec!["bar".to_string(), "foo".to_string()]
        );

        // The full list layers frecency on top, most-used first.
        frecency.record("zeph", "x");
        let names = cache.names(&registry, &frecency);
        assert!(names.is_complete_name("zeph") && names.is_complete_name("bar"));
    }

    /// Sessions union into the shell-name list, deduped, capped at 4096
    /// with the oldest dropped (plan slice 02). Free-standing so the cap
    /// contract is testable without a daemon.
    #[test]
    fn shell_names_union_caps_at_4096_dropping_oldest() {
        let existing: Vec<String> = (0..4000).map(|i| format!("fn{i}")).collect();
        let incoming: Vec<String> = (0..1000).map(|i| format!("new{i}")).collect();
        let union = shell_names_union(&existing, &incoming);
        assert_eq!(union.len(), 4096, "cap must hold across sessions");
        // Oldest dropped: fn0 is gone, the newest of both sides survive.
        assert!(!union.iter().any(|n| n == "fn0"));
        assert!(union.iter().any(|n| n == "fn3999"));
        assert!(union.iter().any(|n| n == "new999"));
        // Dedup across sessions.
        let dup = shell_names_union(&["g".to_string()], &["g".to_string(), "h".to_string()]);
        assert_eq!(*dup, vec!["g".to_string(), "h".to_string()]);
        // A re-registered name moves to the newest end, so the cap drops
        // names nobody re-sent before it.
        let moved = shell_names_union(
            &["a".to_string(), "b".to_string()],
            &["a".to_string(), "c".to_string()],
        );
        assert_eq!(
            *moved,
            vec!["b".to_string(), "a".to_string(), "c".to_string()]
        );
        let mut old: Vec<String> = vec!["keep".to_string()];
        old.extend((0..4095).map(|i| format!("fn{i}")));
        let capped = shell_names_union(&old, &["keep".to_string(), "x".to_string()]);
        assert!(
            capped.iter().any(|n| n == "keep"),
            "re-sent name must survive the cap"
        );
        assert!(!capped.iter().any(|n| n == "fn0"));
        // Nothing registered → nothing stored.
        assert!(shell_names_union(&[], &[]).is_empty());
    }

    fn exe(dir: &std::path::Path, name: &str) {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").expect("write");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    fn path_cache(dir: &std::path::Path) -> Arc<PathCache> {
        Arc::new(PathCache {
            dirs: vec![dir.to_path_buf()],
            ..PathCache::default()
        })
    }

    /// A full `PATH` walk is thousands of files, so a request must
    /// never run one: before the first scan lands the source is simply
    /// empty, and completion answers from the other two.
    #[test]
    fn path_names_are_empty_until_a_scan_completes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        exe(tmp.path(), "zeph");
        let cache = path_cache(tmp.path());
        assert!(
            cache.names().is_empty(),
            "asking for names must not walk PATH inline"
        );
        cache.rescan();
        assert_eq!(*cache.names(), vec!["zeph".to_string()]);
    }

    /// A newly installed binary has to show up without a daemon
    /// restart; the directory mtime is what notices.
    #[test]
    fn path_names_refresh_in_the_background_when_a_dir_changes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        exe(tmp.path(), "zeph");
        let cache = path_cache(tmp.path());
        cache.rescan();
        assert_eq!(*cache.names(), vec!["zeph".to_string()]);

        exe(tmp.path(), "aicommit2");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            // The first call schedules; later ones observe the result.
            let names = cache.names();
            if names.len() == 2 {
                assert_eq!(*names, vec!["aicommit2".to_string(), "zeph".to_string()]);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "background rescan never landed: {names:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// The isolation switch the Rust daemon harnesses set, so a scan of
    /// the developer's real PATH can't leak into their assertions.
    #[test]
    fn path_scan_switch_decides_which_dirs_are_walked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().to_string();
        assert!(dirs_from(Some("0"), &dir).is_empty());
        assert_eq!(dirs_from(None, &dir), vec![tmp.path().to_path_buf()]);
        // Only the exact "0" disables it — an unset-looking value must
        // not silently turn completion's last source off.
        assert_eq!(dirs_from(Some("1"), &dir), vec![tmp.path().to_path_buf()]);
    }

    /// An empty dir list is what the switch produces, and it must leave
    /// the source quiet rather than scanning anything.
    #[test]
    fn a_pathless_cache_stays_empty() {
        let cache: Arc<PathCache> = Arc::new(PathCache::default());
        cache.schedule_rescan();
        assert!(cache.names().is_empty());
    }

    /// PATH is the third source of the list the engine matches against;
    /// dropping it on the floor here would be invisible to every other
    /// test in this file.
    #[test]
    fn name_list_carries_the_path_source() {
        let specs = tempfile::tempdir().expect("tempdir");
        std::fs::write(specs.path().join("git.json"), "{}").expect("write spec");
        let bin = tempfile::tempdir().expect("tempdir");
        exe(bin.path(), "zeph");

        let cache = NameCache {
            path: path_cache(bin.path()),
            ..NameCache::default()
        };
        cache.path.rescan();
        let names = cache.names(&SpecRegistry::at_dir(specs.path()), &FrecencyStore::empty());
        assert!(names.is_complete_name("zeph"), "PATH name missing");
        assert!(names.is_complete_name("git"), "spec stem missing");
    }

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
        let mut items: Vec<Suggestion> = (0..MAX_SUGGESTIONS + 50)
            .map(|i| sugg(&format!("pkg{i:05}")))
            .collect();
        items.push(sugg("zzz-frecency-boosted"));
        let frecency = FrecencyStore::empty();
        for _ in 0..2 {
            frecency.record("brew", "zzz-frecency-boosted");
        }
        let items = rank_by_frecency(items, "brew", &frecency, None, MAX_SUGGESTIONS);
        assert_eq!(items.len(), MAX_SUGGESTIONS, "list capped for transport");
        assert_eq!(
            items[0].display, "zzz-frecency-boosted",
            "frecency survivor kept at head despite late alpha order"
        );
    }

    fn run(history: &HistoryStore, command: &str, cwd: &str) {
        history
            .record(nerv_engine::history::Entry {
                ts: nerv_engine::history::now_unix(),
                exit: 0,
                cwd: cwd.into(),
                prev: String::new(),
                command: command.into(),
                expanded: String::new(),
            })
            .unwrap();
    }

    fn order(items: &[Suggestion]) -> Vec<&str> {
        items.iter().map(|s| s.display.as_str()).collect()
    }

    #[test]
    fn typed_commands_raise_popup_rows_without_any_accept() {
        let history = HistoryStore::empty();
        run(&history, "git status", "/r");
        let items = vec![sugg("checkout"), sugg("commit"), sugg("status")];
        let signals = history.token_signals(&completed_words("git "), "/r", "");
        let ranked = rank_by_frecency(
            items,
            "git",
            &FrecencyStore::empty(),
            Some(&*signals),
            MAX_SUGGESTIONS,
        );
        assert_eq!(order(&ranked), ["status", "checkout", "commit"]);
    }

    #[test]
    fn directory_affinity_flips_popup_order() {
        let history = HistoryStore::empty();
        for _ in 0..3 {
            run(&history, "npm run dev", "/a");
            run(&history, "npm run build", "/b");
        }
        let items = || vec![sugg("build"), sugg("dev"), sugg("test")];
        let words = completed_words("npm run ");
        let in_a = rank_by_frecency(
            items(),
            "npm",
            &FrecencyStore::empty(),
            Some(&*history.token_signals(&words, "/a", "")),
            MAX_SUGGESTIONS,
        );
        let in_b = rank_by_frecency(
            items(),
            "npm",
            &FrecencyStore::empty(),
            Some(&*history.token_signals(&words, "/b", "")),
            MAX_SUGGESTIONS,
        );
        assert_eq!(order(&in_a)[0], "dev");
        assert_eq!(order(&in_b)[0], "build");
    }

    fn run_after(history: &HistoryStore, command: &str, prev: &str) {
        history
            .record(nerv_engine::history::Entry {
                ts: nerv_engine::history::now_unix(),
                exit: 0,
                cwd: "/".into(),
                prev: prev.into(),
                command: command.into(),
                expanded: String::new(),
            })
            .unwrap();
    }

    #[test]
    fn what_followed_the_previous_command_tops_the_popup() {
        let history = HistoryStore::empty();
        for _ in 0..4 {
            run(&history, "git push", "/");
        }
        for _ in 0..2 {
            run_after(&history, "git commit -m wip", "git add .");
        }
        let items = || vec![sugg("commit"), sugg("push"), sugg("status")];
        let words = completed_words("git ");
        let rank = |prev: &str| {
            rank_by_frecency(
                items(),
                "git",
                &FrecencyStore::empty(),
                Some(&*history.token_signals(&words, "/", prev)),
                MAX_SUGGESTIONS,
            )
        };
        assert_eq!(order(&rank(""))[0], "push");
        assert_eq!(order(&rank("git add ."))[0], "commit");
        assert_eq!(order(&rank("git add src/x.rs"))[0], "commit");
    }

    #[test]
    fn history_rows_fill_in_words_the_spec_lacks() {
        let history = HistoryStore::empty();
        for w in [
            "feature-x",
            "feature-x",
            "fix-y",
            "main",
            "'quoted arg'",
            "f && make",
            "fix;make",
            "f > out.patch",
            "fo\\ bar",
        ] {
            run(&history, &format!("git checkout {w}"), "/");
        }
        let sig = history.token_signals(&completed_words("git checkout f"), "/", "");
        let rows = history_rows(
            &sig,
            &[sugg("main")],
            partial_word("git checkout f"),
            None,
            false,
        );
        let words: Vec<&str> = rows.iter().map(|s| s.insertion.as_str()).collect();
        // Extends `f`, best first; `main` is already a spec row.
        assert_eq!(words, ["feature-x", "fix-y"]);
        assert_eq!(rows[0].description.as_deref(), Some("history"));
        assert_eq!(
            rows[0].icon.as_deref(),
            Some(nerv_engine::complete::HISTORY_ICON)
        );
        let all = history_rows(&sig, &[], "", None, false);
        // Quoted words, operators and redirections are never offered back.
        let bad = |w: &str| w.contains(['\'', ';', '&', '>', '|', '\\']);
        assert!(all.iter().all(|s| !bad(&s.insertion)), "{all:?}");
        assert!(all.len() <= HISTORY_ROWS);
    }

    #[test]
    fn history_rows_leave_corrections_and_no_spec_replies_alone() {
        let fix = Suggestion {
            replace: Some(nerv_engine::ReplaceSpan { start: 0, end: 4 }),
            ..sugg("expo")
        };
        assert!(!wants_history_rows(&[fix], None));
        let no_spec = format!("{}nosuchbin", nerv_engine::complete::NO_SPEC_REASON_PREFIX);
        assert!(!wants_history_rows(&[], Some(&no_spec)));
        assert!(wants_history_rows(&[sugg("main")], None));
        assert!(wants_history_rows(&[], None));
        // zoxide's rows (and command names) keep the source's order.
        let zoxide = Suggestion {
            source_ranked: true,
            ..sugg("/Users/me/nerv-sh")
        };
        assert!(!wants_history_rows(&[zoxide], None));
    }

    /// A relative path typed in another directory is not offered where it
    /// does not exist; typed here, existing here, or absolute, it is.
    #[test]
    fn history_rows_offer_relative_paths_only_where_they_apply() {
        let dir = std::env::temp_dir().join(format!("nerv-hist-rows-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("here")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/real.rs"), "").unwrap();
        let other = dir.join("other").to_string_lossy().into_owned();
        let here = dir.to_string_lossy().into_owned();
        let history = HistoryStore::empty();
        run(&history, "vim src/elsewhere.rs", &other);
        run(&history, "vim src/real.rs", &other);
        run(&history, "vim src/typed-here.rs", &here);
        run(&history, "vim /etc/hosts", &other);
        let sig = history.token_signals(&["vim"], &here, "");
        let rows = history_rows(&sig, &[], "", Some(&dir), false);
        let mut words: Vec<&str> = rows.iter().map(|s| s.insertion.as_str()).collect();
        words.sort_unstable();
        assert_eq!(words, ["/etc/hosts", "src/real.rs", "src/typed-here.rs"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression (2026-10-07): `mv kno` listed folders that had been moved
    /// away. A folder typed here as the source of an `mv` or `rm` is gone
    /// once the command succeeds, yet "typed here" kept offering it.
    #[test]
    fn history_rows_drop_folders_that_are_gone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("knowledge-kept")).unwrap();
        let here = tmp.path().to_string_lossy().into_owned();
        let history = HistoryStore::empty();
        run(&history, "mv knowledge-moved/ elsewhere/", &here);
        run(&history, "mv knowledge-kept/ elsewhere/", &here);
        let sig = history.token_signals(&["mv"], &here, "");
        let rows = history_rows(&sig, &[], "kno", Some(tmp.path()), false);
        let words: Vec<&str> = rows.iter().map(|s| s.insertion.as_str()).collect();
        assert_eq!(words, ["knowledge-kept/"]);
    }

    /// Only a local folder is checked against the disk: a branch, a remote
    /// path and a URL name nothing here and are still offered.
    #[test]
    fn history_rows_keep_words_that_are_not_local_folders() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let here = tmp.path().to_string_lossy().into_owned();
        let history = HistoryStore::empty();
        run(&history, "sync feature/x", &here);
        run(&history, "sync host:backup/", &here);
        run(&history, "sync https://example.com/", &here);
        run(&history, "sync ~/gone/", &here);
        let sig = history.token_signals(&["sync"], &here, "");
        let rows = history_rows(&sig, &[], "", Some(tmp.path()), false);
        let mut words: Vec<&str> = rows.iter().map(|s| s.insertion.as_str()).collect();
        words.sort_unstable();
        assert_eq!(
            words,
            [
                "feature/x",
                "host:backup/",
                "https://example.com/",
                "~/gone/"
            ]
        );
    }

    /// A word only failed runs had is a typo or a gone script: never a
    /// row. One success is enough — `git push` that was rejected once
    /// and then went through stays.
    #[test]
    fn history_rows_skip_words_only_failed_runs_had() {
        let history = HistoryStore::empty();
        let failed = |command: &str| {
            history
                .record(nerv_engine::history::Entry {
                    ts: nerv_engine::history::now_unix(),
                    exit: 1,
                    cwd: "/".into(),
                    prev: String::new(),
                    command: command.into(),
                    expanded: String::new(),
                })
                .unwrap();
        };
        let imported = |command: &str| {
            history
                .record(nerv_engine::history::Entry {
                    ts: nerv_engine::history::now_unix(),
                    exit: 0,
                    cwd: String::new(),
                    prev: String::new(),
                    command: command.into(),
                    expanded: String::new(),
                })
                .unwrap();
        };
        failed("yarn web:deployㅔ");
        failed("yarn web:deployㅔ");
        failed("yarn web:start");
        run(&history, "yarn web:start", "/");
        // Imported rows have no status: once is a typo, twice a habit.
        imported("yarn web:deploy:de");
        imported("yarn web:test");
        imported("yarn web:test");
        let sig = history.token_signals(&completed_words("yarn we"), "/", "");
        let rows = history_rows(&sig, &[], partial_word("yarn we"), None, false);
        let mut words: Vec<&str> = rows.iter().map(|s| s.insertion.as_str()).collect();
        words.sort_unstable();
        assert_eq!(words, ["web:start", "web:test"]);
    }

    /// History rows come after the spec's, whatever their score (Fig
    /// orders them the same way): `cherry`, typed five times, still sits
    /// below the spec's `checkout`.
    #[test]
    fn history_rows_follow_the_spec_rows() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("git.json"),
            r#"{"name":"git","subcommands":[{"name":"checkout"},{"name":"commit"}]}"#,
        )
        .expect("write spec");
        let registry = SpecRegistry::at_dir(tmp.path());
        let history = HistoryStore::empty();
        for _ in 0..5 {
            run(&history, "git cherry", "/");
        }
        run(&history, "git commit", "/");
        let resp = engine_complete(
            &registry,
            Ranking {
                frecency: &FrecencyStore::empty(),
                history: Some(&history),
                prev: "",
            },
            None,
            "git c",
            5,
            Some("/"),
            MatchMode::default(),
        );
        let Response::Suggestions { items, .. } = resp else {
            panic!("expected rows, got {resp:?}");
        };
        let words: Vec<&str> = items.iter().map(|s| s.insertion.as_str()).collect();
        assert_eq!(words, ["commit", "checkout", "cherry"]);
    }

    /// Regression (2026-10-07): `mv ` listed files and folders typed
    /// without a trailing `/` that had since been moved away. Where the
    /// spec says the argument is a path, a history word is one too, so it
    /// is offered only while it exists; an option is not a path.
    #[test]
    fn history_rows_drop_paths_that_are_gone_where_a_path_goes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("mv.json"),
            r#"{"name":"mv","args":[{"name":"source","template":"filepaths","is_variadic":true}]}"#,
        )
        .expect("write spec");
        std::fs::write(tmp.path().join("kept.txt"), "").unwrap();
        let here = tmp.path().to_string_lossy().into_owned();
        let registry = SpecRegistry::at_dir(tmp.path());
        let history = HistoryStore::empty();
        run(&history, "mv gone.txt elsewhere", &here);
        run(&history, "mv gone-folder elsewhere", &here);
        run(&history, "mv kept.txt elsewhere", &here);
        run(&history, "mv -v x y", &here);
        let resp = engine_complete(
            &registry,
            Ranking {
                frecency: &FrecencyStore::empty(),
                history: Some(&history),
                prev: "",
            },
            None,
            "mv ",
            3,
            Some(&here),
            MatchMode::default(),
        );
        let Response::Suggestions { items, .. } = resp else {
            panic!("expected rows, got {resp:?}");
        };
        let mut words: Vec<&str> = items.iter().map(|s| s.insertion.as_str()).collect();
        words.sort_unstable();
        assert_eq!(words, ["-v", "kept.txt", "mv.json"]);
    }

    /// Where the spec does not say "path", a history word may be a branch
    /// or a host: nothing on disk, still offered.
    #[test]
    fn history_rows_keep_words_where_no_path_goes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("git.json"),
            r#"{"name":"git","subcommands":[{"name":"checkout","args":[{"name":"branch"}]}]}"#,
        )
        .expect("write spec");
        let here = tmp.path().to_string_lossy().into_owned();
        let registry = SpecRegistry::at_dir(tmp.path());
        let history = HistoryStore::empty();
        run(&history, "git checkout feature-x", &here);
        let resp = engine_complete(
            &registry,
            Ranking {
                frecency: &FrecencyStore::empty(),
                history: Some(&history),
                prev: "",
            },
            None,
            "git checkout ",
            13,
            Some(&here),
            MatchMode::default(),
        );
        let Response::Suggestions { items, .. } = resp else {
            panic!("expected rows, got {resp:?}");
        };
        let words: Vec<&str> = items.iter().map(|s| s.insertion.as_str()).collect();
        assert_eq!(words, ["feature-x"]);
    }

    /// Regression (2026-10-01): `cd ` listed `~` and `-` above a subfolder
    /// never cd'd into. Those two carry picks and runs from every
    /// directory, while a folder that is only here has none, so a pure
    /// score order buried it. Folders that exist here come first; the
    /// score still orders within each group.
    #[test]
    fn folders_here_lead_the_spec_constants() {
        let specs = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            specs.path().join("cd.json"),
            r#"{"name":"cd","args":[{"suggestions":[{"name":"-"},{"name":"~"}],
                "generators":[{"type":"filepaths","folders_only":true}]}]}"#,
        )
        .expect("write spec");
        let cwd = tempfile::tempdir().expect("tempdir");
        for d in ["alpha", "beta", "gamma"] {
            std::fs::create_dir(cwd.path().join(d)).expect("mkdir");
        }
        let cwd_str = cwd.path().to_str().expect("utf-8 tempdir");
        let frecency = FrecencyStore::empty();
        let history = HistoryStore::empty();
        for _ in 0..3 {
            frecency.record("cd", "-");
            run(&history, "cd ~", "/elsewhere");
        }
        run(&history, "cd beta/", cwd_str);
        let resp = engine_complete(
            &SpecRegistry::at_dir(specs.path()),
            Ranking {
                frecency: &frecency,
                history: Some(&history),
                prev: "",
            },
            None,
            "cd ",
            3,
            Some(cwd_str),
            MatchMode::default(),
        );
        let Response::Suggestions { items, .. } = resp else {
            panic!("expected rows, got {resp:?}");
        };
        let words: Vec<&str> = items.iter().map(|s| s.insertion.as_str()).collect();
        assert_eq!(words, ["beta/", "alpha/", "gamma/", "-", "~"]);
    }

    #[test]
    fn a_no_spec_reply_stays_empty_for_the_miss_tally() {
        let history = HistoryStore::empty();
        run(&history, "nosuchbin foo", "/");
        let resp = engine_complete(
            &SpecRegistry::default(),
            Ranking {
                frecency: &FrecencyStore::empty(),
                history: Some(&history),
                prev: "",
            },
            None,
            "nosuchbin ",
            10,
            Some("/"),
            MatchMode::default(),
        );
        match resp {
            Response::Empty {
                reason: Some(r), ..
            } => assert!(no_spec_binary(&r).is_some(), "{r}"),
            other => panic!("history rows turned a no-spec reply into {other:?}"),
        }
    }

    #[test]
    fn a_sequence_seen_once_does_not_reorder_the_popup() {
        let history = HistoryStore::empty();
        for _ in 0..3 {
            run(&history, "git push", "/");
        }
        run_after(&history, "git stash", "git add .");
        let ranked = rank_by_frecency(
            vec![sugg("push"), sugg("stash")],
            "git",
            &FrecencyStore::empty(),
            Some(&*history.token_signals(&completed_words("git "), "/", "git add .")),
            MAX_SUGGESTIONS,
        );
        assert_eq!(order(&ranked)[0], "push");
    }

    #[test]
    fn partial_word_is_the_token_at_the_cursor() {
        assert_eq!(partial_word("git ch"), "ch");
        assert_eq!(partial_word("git "), "");
        assert_eq!(partial_word("cd x;git"), "git");
        assert_eq!(partial_word("ls 한"), "한");
    }

    #[test]
    fn completed_words_takes_the_last_segment() {
        assert_eq!(completed_words("git ch"), ["git"]);
        assert_eq!(completed_words("git checkout "), ["git", "checkout"]);
        assert_eq!(completed_words("make && git ch"), ["git"]);
        assert_eq!(completed_words("cd x; git ch"), ["git"]);
        assert_eq!(completed_words("cat f|grep "), ["grep"]);
        assert_eq!(completed_words("make&&git "), ["git"]);
        assert!(completed_words("gi").is_empty());
    }

    #[test]
    fn a_character_cursor_on_a_non_ascii_line_does_not_panic() {
        // `ls 한글` is 5 characters and 9 bytes; the widget sends 5.
        assert_eq!(before_cursor("ls 한글", 5), "ls 한글");
        assert_eq!(before_cursor("ls 한글", 4), "ls 한");
        assert_eq!(before_cursor("ls 한글 ", 99), "ls 한글 ");
        // The byte slice this replaced: `&"ls 한글"[..5]` splits `한`.
        assert!(!"ls 한글".is_char_boundary(5));
        assert_eq!(
            completed_words(before_cursor("ls 한글 ", 6)),
            ["ls", "한글"]
        );
    }

    #[test]
    fn accepts_and_runs_add_up_before_normalising() {
        // `log` was picked three times; `status` picked once and run twice.
        // Summed, status (1+2) beats log (3) on recency-free ln(1+n) sums:
        // ln2 + ln3 > ln4. Either signal alone would not.
        let history = HistoryStore::empty();
        run(&history, "git status", "/r");
        run(&history, "git status", "/r");
        let frecency = FrecencyStore::empty();
        frecency.record("git", "log");
        frecency.record("git", "log");
        frecency.record("git", "status");
        frecency.record("git", "log");
        let signals = history.token_signals(&completed_words("git "), "/elsewhere", "");
        let ranked = rank_by_frecency(
            vec![sugg("commit"), sugg("log"), sugg("status")],
            "git",
            &frecency,
            Some(&*signals),
            MAX_SUGGESTIONS,
        );
        assert_eq!(order(&ranked), ["status", "log", "commit"]);
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

    #[test]
    fn zoxide_rows_keep_engine_order_despite_frecency() {
        // Regression (2026-09-15): `z tak-bro` listed the folder literally
        // named `tak-bro` fifth, under frecent children whose only match
        // is the `/tak-bro/` parent segment. The engine already ranks
        // zoxide rows (name hits first, zoxide's own frecency within);
        // re-ranking them here with nerv frecency undid that.
        let zoxide = |name: &str| Suggestion {
            source_ranked: true,
            replace: None,
            ..sugg(name)
        };
        let frecency = FrecencyStore::empty();
        for _ in 0..3 {
            frecency.record("z", "claude-code");
        }
        let items = vec![zoxide("tak-bro"), zoxide("claude-code")];
        let ranked = rank_by_frecency(items, "z", &frecency, None, MAX_SUGGESTIONS);
        let order: Vec<&str> = ranked.iter().map(|s| s.display.as_str()).collect();
        assert_eq!(order, ["tak-bro", "claude-code"]);
        // History signals leave source-ranked rows alone too.
        let history = HistoryStore::empty();
        for _ in 0..3 {
            history
                .record(nerv_engine::history::Entry {
                    ts: nerv_engine::history::now_unix(),
                    exit: 0,
                    cwd: "/".into(),
                    prev: String::new(),
                    command: "z claude-code".into(),
                    expanded: String::new(),
                })
                .unwrap();
        }
        let items = vec![zoxide("tak-bro"), zoxide("claude-code")];
        let signals = history.token_signals(&["z"], "/", "");
        let ranked = rank_by_frecency(items, "z", &frecency, Some(&*signals), MAX_SUGGESTIONS);
        let order: Vec<&str> = ranked.iter().map(|s| s.display.as_str()).collect();
        assert_eq!(order, ["tak-bro", "claude-code"]);
    }
}
