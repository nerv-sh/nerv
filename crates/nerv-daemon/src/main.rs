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
    Config, FrecencyStore, MatchMode, Request, Response, SpecRegistry, Suggestion, complete_in,
    manifest, no_spec_binary, paths,
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

    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            res = listener.accept() => {
                match res {
                    Ok((stream, _addr)) => {
                        let registry = registry.clone();
                        let frecency = frecency.clone();
                        let misses = misses.clone();
                        let names = names.clone();
                        let schema_block = schema_block.clone();
                        tokio::spawn(handle_connection(
                            stream,
                            registry,
                            frecency,
                            misses,
                            names,
                            config.matching.mode,
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

    // Last chance to persist the tally: `flush_if_dirty` is throttled
    // (MIN_FLUSH_INTERVAL) so the counts from the final window are still
    // in memory here.
    misses.flush_now();

    let _ = tokio::fs::remove_file(&sock_path).await;
    let _ = tokio::fs::remove_file(&pid_path).await;
    Ok(())
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    registry: Arc<SpecRegistry>,
    frecency: Arc<FrecencyStore>,
    misses: Arc<MissCounter>,
    names: Arc<NameCache>,
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
                    let misses = misses.clone();
                    let names = names.clone();
                    tokio::task::spawn_blocking(move || {
                        // Building the name list means stat-ing every
                        // spec layer, cloning ~700 stems and folding the
                        // frecency table. The engine calls this only for
                        // the first token or a command word with no spec
                        // — a small minority of keystrokes.
                        let cmd_names = || names.names(&registry, &frecency);
                        let resp = engine_complete(
                            &registry,
                            &frecency,
                            Some(&cmd_names),
                            &line,
                            cursor,
                            cwd.as_deref(),
                            mode,
                        );
                        // A "no spec for X" empty is the only response the
                        // tally cares about — and only once it is settled.
                        // The first keystroke on a cold stem returns empty
                        // while the spec is still parsing or being derived
                        // from `--help`; counting that would list commands
                        // that complete fine one key later. The flush is
                        // throttled inside the counter.
                        if let Response::Empty { reason: Some(r) } = &resp {
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
        )
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

/// Dispatch a Complete request through the real engine pipeline.
/// After the engine returns, apply a frecency boost so suggestions
/// the user has accepted before float to the top of the list.
fn engine_complete(
    registry: &SpecRegistry,
    frecency: &FrecencyStore,
    names: Option<&dyn Fn() -> CommandNames>,
    line: &str,
    cursor: usize,
    cwd: Option<&str>,
    mode: MatchMode,
) -> Response {
    let cwd_path = cwd.map(std::path::Path::new);
    let mut result = complete_in(line, cursor, registry, cwd_path, mode, names);
    if result.items.is_empty() {
        return Response::Empty {
            reason: result.reason,
        };
    }
    // Extract the binary name once — frecency keys are per-spec.
    if let Some(spec_name) = line.split_whitespace().next() {
        result.items = rank_by_frecency(
            std::mem::take(&mut result.items),
            spec_name,
            frecency,
            MAX_SUGGESTIONS,
        );
    }
    // Ranking already truncated to the transport cap (MAX_SUGGESTIONS).
    debug_assert!(result.items.len() <= MAX_SUGGESTIONS);
    Response::Suggestions {
        items: result.items,
        token_complete: result.token_complete,
    }
}

/// Score each item with the user's frecency for `spec_name`, then order
/// them for display via [`rank_completions`]. Source-ranked rows (zoxide)
/// score zero so the stable sort keeps the engine's order for them. The
/// list is truncated to `cap` after sorting (dropping the least-relevant
/// tail) so the re-collect below never materializes rows that get dropped.
fn rank_by_frecency(
    items: Vec<Suggestion>,
    spec_name: &str,
    frecency: &FrecencyStore,
    cap: usize,
) -> Vec<Suggestion> {
    let mut scored: Vec<(f64, Suggestion)> = items
        .into_iter()
        .map(|s| {
            let score = if s.source_ranked {
                0.0
            } else {
                frecency.score(spec_name, &s.insertion)
            };
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
        let items = rank_by_frecency(items, "brew", &frecency, MAX_SUGGESTIONS);
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
        let ranked = rank_by_frecency(items, "z", &frecency, MAX_SUGGESTIONS);
        let order: Vec<&str> = ranked.iter().map(|s| s.display.as_str()).collect();
        assert_eq!(order, ["tak-bro", "claude-code"]);
    }
}
