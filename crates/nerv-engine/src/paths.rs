//! Path resolution shared between `nervd` and `nerv-cli`.
//!
//! These constants are the **authoritative** paths documented in
//! `docs/uninstall-spec.md` §2. Changing them changes the uninstall
//! contract — coordinate with that doc first.

use std::path::PathBuf;

/// `~/Library/Caches/nerv/`
pub const CACHE_SUBDIR: &str = "Library/Caches/nerv";

/// `~/Library/Logs/nerv/`
pub const LOG_SUBDIR: &str = "Library/Logs/nerv";

/// `~/.config/nerv/` (XDG_CONFIG_HOME ignored on macOS for v1.0;
/// add Linux handling when v1.4 lands).
pub const CONFIG_SUBDIR: &str = ".config/nerv";

pub const SOCKET_NAME: &str = "nervd.sock";
pub const PID_NAME: &str = "nervd.pid";
pub const DAEMON_LOG_NAME: &str = "nervd.log";
pub const SPECS_SUBDIR: &str = "specs";
pub const FRECENCY_NAME: &str = "frecency.tsv";
pub const MISSES_NAME: &str = "misses.tsv";
pub const HISTORY_NAME: &str = "history.tsv";
/// The zsh widget script cached by `nerv init zsh --shell-script`.
pub const INIT_CACHE_NAME: &str = "init.zsh";
/// Written by the zsh widget when it finds zsh-autosuggestions: the mode
/// (`strategy` or `yield`). While it holds the same mode, later shells
/// skip the notice; `nerv doctor` reports it.
pub const AUTOSUGGEST_SEEN_NAME: &str = "autosuggest-seen";
pub const DERIVED_SUBDIR: &str = "derived";

/// Whether `name` is an executable file in one of `dirs`.
///
/// The spec-miss filter asks this of a name the daemon completed empty
/// for: a name that is not a runnable program cannot be a missing spec.
/// Callers pass the directory list they already hold (`$PATH` split, or
/// a cached copy) so a keystroke never re-reads the environment.
pub fn on_path_in(dirs: &[PathBuf], name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    if name.is_empty() || name.contains('/') {
        return false;
    }
    dirs.iter().any(|dir| {
        std::fs::metadata(dir.join(name))
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

/// Whether `name` is an executable in one of `$PATH`'s directories.
///
/// The convenience form for one-shot callers (`nerv doctor`); the
/// daemon passes its own list to [`on_path_in`].
pub fn on_path(name: &str) -> bool {
    std::env::var("PATH").is_ok_and(|path| on_path_in(&crate::complete::path_dirs(&path), name))
}

/// Write `content` to `path` through a sibling temp file and a rename.
///
/// Every file nerv writes that another process may be reading goes
/// through here: rc files the user's shell sources, and the cache files
/// (`frecency.tsv`, `misses.tsv`, `derived/*.json`) that `nerv doctor`
/// reads while the daemon writes. A plain `fs::write` truncates first,
/// so a concurrent reader can see a half-written file; rename is atomic
/// on the same filesystem, so it sees either the old file or the new.
///
/// An existing target keeps its permissions — an rc file the user
/// made `0600` must not come back `0644`. Cache files are created
/// fresh and take the default.
/// Temp path for [`write_atomic`]: pid + a process-wide counter, so two
/// concurrent writers never share one. `create_new` below turns a
/// (near-impossible) collision into an error instead of interleaved
/// bytes surviving the atomic rename.
fn unique_tmp_path(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let file_name = path.file_name()?.to_str()?;
    static TMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Some(path.with_file_name(format!("{file_name}.nerv-tmp.{}.{}", std::process::id(), n)))
}

pub fn write_atomic(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Every early return unlinks the temp file: `nerv uninstall` rewrites
    // rc files through here and must leave no trace, not even on failure
    // (uninstall-spec §4).
    let write = |tmp: &std::path::Path| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
        if let Ok(meta) = std::fs::metadata(path) {
            let _ = std::fs::set_permissions(tmp, meta.permissions());
        }
        std::fs::rename(tmp, path)
    };
    // A stale tmp from a crashed run with a recycled pid would trip
    // `create_new`: bump past it (bounded — a persistent failure is a
    // real FS problem and surfaces as Err).
    let mut last_err = std::io::Error::other("no temp path available");
    for _ in 0..8 {
        let Some(tmp) = unique_tmp_path(path) else {
            return Err(std::io::Error::other("path has no file name"));
        };
        match write(&tmp) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                last_err = e;
                continue;
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        }
    }
    Err(last_err)
}

/// Stderr traces for poisoned ranking locks, bounded: the first few
/// (one per known call site) print, then silence. Callers degrade to
/// cold defaults either way. Engine has no logger; the daemon captures
/// stderr.
pub(crate) fn warn_poison_once(what: &str) {
    static WARNED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if WARNED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 8 {
        eprintln!("nerv: {what} lock poisoned — ranking degraded to cold defaults");
    }
}

/// A plain executable stem as the user would type it: 2–64 chars of
/// `[A-Za-z0-9_.+-]`, starting alphanumeric. Rejects paths (`./x`,
/// `/usr/bin/x`), single letters, and anything carrying whitespace or a
/// TSV delimiter. Shared by the miss tally (what to count) and spec
/// derivation (what to ask for `--help`).
pub fn is_command_stem(name: &str) -> bool {
    if name.len() < 2 || name.len() > 64 {
        return false;
    }
    if !name.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+'))
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn cache_dir() -> Option<PathBuf> {
    home().map(|h| h.join(CACHE_SUBDIR))
}

pub fn log_dir() -> Option<PathBuf> {
    home().map(|h| h.join(LOG_SUBDIR))
}

pub fn config_dir() -> Option<PathBuf> {
    home().map(|h| h.join(CONFIG_SUBDIR))
}

pub fn socket_path() -> Option<PathBuf> {
    cache_dir().map(|c| c.join(SOCKET_NAME))
}

pub fn pid_path() -> Option<PathBuf> {
    cache_dir().map(|c| c.join(PID_NAME))
}

pub fn daemon_log_path() -> Option<PathBuf> {
    log_dir().map(|d| d.join(DAEMON_LOG_NAME))
}

/// `~/Library/Caches/nerv/misses.tsv` — local tally of commands that
/// completed empty for want of a spec (`crate::misses`). Cache, not
/// config: it is regenerable diagnostics and `nerv uninstall` sweeps
/// it with the rest of the cache dir (`docs/uninstall-spec.md` §2).
pub fn misses_path() -> Option<PathBuf> {
    cache_dir().map(|c| c.join(MISSES_NAME))
}

/// `~/Library/Caches/nerv/history.tsv` — executed commands behind the
/// history-ranked ghost (`crate::history`). `NERV_HISTORY_FILE`
/// overrides it (tests); `-` means "no file". Swept with the cache dir
/// by `nerv uninstall` (`docs/uninstall-spec.md` §2).
pub fn history_path() -> Option<PathBuf> {
    match std::env::var_os("NERV_HISTORY_FILE") {
        Some(p) => Some(PathBuf::from(p)),
        None => cache_dir().map(|c| c.join(HISTORY_NAME)),
    }
}

/// `~/Library/Caches/nerv/derived/` — specs derived from a command's
/// own `--help` output (`crate::derived`). Cache, not config: every
/// file here is regenerable from the installed binary, so `nerv
/// uninstall` sweeps it with the rest of the cache and `--keep-config`
/// does not preserve it.
pub fn derived_specs_dir() -> Option<PathBuf> {
    cache_dir().map(|c| c.join(DERIVED_SUBDIR))
}

/// `~/Library/Caches/nerv/specs/` — JSON spec cache populated by
/// the build-specs binary.
pub fn specs_dir() -> Option<PathBuf> {
    cache_dir().map(|c| c.join(SPECS_SUBDIR))
}

/// True when `dir` holds at least one installed spec (`manifest.json`,
/// `*.json`, or `*.json.gz`). A missing or empty dir is "no specs" —
/// the signal `resolve_specs_dir` uses to fall back to the bundled set.
fn has_specs(dir: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.file_name()
            .to_str()
            .is_some_and(|n| n.ends_with(".json") || n.ends_with(".json.gz"))
    })
}

/// Read-only spec sets shipped alongside the binary, in probe order:
/// Homebrew keg layout (`bin/../share/nerv/specs`, from
/// `pkgshare.install "specs"`) then the flat tarball layout (`specs/`
/// next to the binary).
fn bundled_specs_dirs() -> Vec<PathBuf> {
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    let Some(bin_dir) = exe.parent() else {
        return Vec::new();
    };
    vec![
        bin_dir.join("../share/nerv/specs"),
        bin_dir.join(SPECS_SUBDIR),
    ]
}

/// Resolve the *primary* spec directory, in priority order (consumers
/// go through [`resolve_spec_layers`], which layers the user overlay on
/// top of this):
///
/// 1. `NERV_SPECS_DIR` env override (tests, power users) — always wins,
///    even when empty or stale, so test isolation is airtight.
/// 2. The user cache (`~/Library/Caches/nerv/specs/`) *if it actually
///    holds specs and this build can read them* — a `build-specs` run
///    there overrides the bundle.
/// 3. A bundled read-only set next to the binary (Homebrew `share/`,
///    or `specs/` in an unpacked tarball) — what a fresh `brew install`
///    user completes against without ever running `build-specs`.
/// 4. The user cache path regardless, so existing "dir missing /
///    empty" error paths and doctor hints keep pointing at the
///    documented location.
fn resolve_specs_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("NERV_SPECS_DIR") {
        return Some(PathBuf::from(d));
    }
    let user = specs_dir();
    if let Some(u) = &user {
        if has_specs(u) && !is_unreadable_schema(u) {
            return user;
        }
    }
    for b in bundled_specs_dirs() {
        if has_specs(&b) {
            // Canonicalize the `bin/../share` hop for clean display in
            // doctor/logs; the dir exists (has_specs read it), so this
            // only fails on exotic FS races — fall back to the raw path.
            return Some(std::fs::canonicalize(&b).unwrap_or(b));
        }
    }
    user
}

/// Does `dir` hold specs stamped for a different schema version?
///
/// Only an explicit mismatch counts. A missing or corrupt manifest is
/// read leniently everywhere else (pre-manifest installs keep working)
/// and is read leniently here too.
fn is_unreadable_schema(dir: &std::path::Path) -> bool {
    matches!(
        crate::manifest::check_schema(dir),
        crate::manifest::SchemaStatus::Mismatch { .. }
    )
}

/// The user spec cache when it is being **skipped** for holding specs of
/// another schema version, together with the version found there.
///
/// An upgrade that bumps the schema strands every cache a user ever
/// filled with `build-specs`, and the cache outranks the bundle, so
/// before this the whole install went dark: completion disabled by the
/// E5 gate, and `nerv doctor` advising `brew reinstall nerv`, which
/// reinstalls the bundle the cache is already winning against
/// (observed on 0.1.13 → 0.1.14, the v2 → v3 split-spec bump).
///
/// Returns `None` when the cache is usable, absent, empty, or when
/// `NERV_SPECS_DIR` is set — an explicit choice is not second-guessed.
pub fn stale_spec_cache() -> Option<(PathBuf, u32)> {
    if std::env::var_os("NERV_SPECS_DIR").is_some() {
        return None;
    }
    let user = specs_dir()?;
    if !has_specs(&user) {
        return None;
    }
    match crate::manifest::check_schema(&user) {
        crate::manifest::SchemaStatus::Mismatch { found } => Some((user, found)),
        _ => None,
    }
}

/// `~/.config/nerv/specs/` — user-authored overlay specs (tools upstream
/// never covered: `claude`, in-house CLIs). Lives under the *config* dir,
/// not the cache, because it is user content: `nerv uninstall` removes it
/// with `~/.config/nerv/` unless `--keep-config`
/// (`docs/uninstall-spec.md` §2 row 6).
pub fn user_specs_dir() -> Option<PathBuf> {
    config_dir().map(|c| c.join(SPECS_SUBDIR))
}

/// The spec dirs a consumer (daemon, doctor, `spec list`) reads. Hand
/// [`SpecLayers::dirs`] to `SpecRegistry::at_dirs`, which resolves each
/// stem from the first layer that has it (a user file replaces the
/// bundled one of the same name wholesale; no merge). The E5 schema gate
/// looks at `primary` only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecLayers {
    /// `~/.config/nerv/specs/` when that directory exists (even empty —
    /// it is watched from daemon start, so a file added later hot-loads).
    pub overlay: Option<PathBuf>,
    /// The [`resolve_specs_dir`] chain: env → user cache → bundled.
    pub primary: PathBuf,
    /// `~/Library/Caches/nerv/derived/` when derivation is enabled.
    /// Last, so a hand-authored overlay spec and the bundled set both
    /// beat anything scraped out of `--help`.
    pub derived: Option<PathBuf>,
}

impl SpecLayers {
    /// Registry order: overlay, primary, derived.
    pub fn dirs(&self) -> Vec<PathBuf> {
        self.overlay
            .iter()
            .cloned()
            .chain(std::iter::once(self.primary.clone()))
            .chain(self.derived.iter().cloned())
            .collect()
    }
}

/// Resolve every layer:
///
/// 1. `NERV_SPECS_DIR` set → that dir **alone** as primary, no overlay
///    and no derived layer. Test isolation must stay airtight, so
///    neither a developer's real overlay nor a derived cache ever leaks
///    into an e2e run.
/// 2. Otherwise overlay = the user dir if it exists, primary = the
///    unchanged [`resolve_specs_dir`] chain, derived = the cache dir
///    when `derive_enabled`.
///
/// Contract: `docs/spec-conversion-policy.md` §6.1 "사용자 overlay",
/// §6.2 "파생 spec".
pub fn resolve_spec_layers(derive_enabled: bool) -> Option<SpecLayers> {
    if let Some(d) = std::env::var_os("NERV_SPECS_DIR") {
        return Some(SpecLayers {
            overlay: None,
            primary: PathBuf::from(d),
            derived: None,
        });
    }
    let primary = resolve_specs_dir()?;
    let overlay = user_specs_dir().filter(|o| o.is_dir());
    let derived = if derive_enabled {
        derived_specs_dir()
    } else {
        None
    };
    Some(SpecLayers {
        overlay,
        primary,
        derived,
    })
}

#[cfg(test)]
mod tests {
    //! The `PathBuf` shapes returned here ARE the contract
    //! documented in `docs/uninstall-spec.md` §2. Each test below
    //! locks one path's suffix; bumping any of them is the
    //! authoritative trigger for a coordinated doc + uninstaller
    //! change.

    use super::*;
    use std::path::Path;
    use std::sync::Mutex;

    /// HOME is process-global; every test that mutates it must hold this
    /// lock so parallel `set_var`/`remove_var` calls can't interleave.
    /// Poison-tolerant: a panicking test still releases a usable guard.
    static HOME_LOCK: Mutex<()> = Mutex::new(());

    /// Helper: HOME → tempdir for the test, restored on drop. Serialized
    /// against every other HOME mutator via `HOME_LOCK`.
    fn with_temp_home<R>(f: impl FnOnce(&Path) -> R) -> R {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        // SAFETY: env mutation, serialized by HOME_LOCK. The functions
        // under test read HOME through `std::env::var_os` at call time.
        let prev = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", tmp.path()) };
        let out = f(tmp.path());
        match prev {
            Some(p) => unsafe { std::env::set_var("HOME", p) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        out
    }

    /// The admissibility gate the miss tally and `--help` derivation
    /// both trust: table-driven over the documented contract (2–64
    /// chars, leading alnum, `[A-Za-z0-9_.+-]`, no paths/whitespace).
    #[test]
    fn is_command_stem_table() {
        let long = "x".repeat(65);
        let cases: Vec<(&str, bool)> = vec![
            ("git", true),
            ("g++", true),
            ("foo-bar_v1.2", true),
            ("a", false),
            ("./x", false),
            ("/usr/bin/x", false),
            ("", false),
            ("a b", false),
            ("a\tb", false),
            (&long, false),
        ];
        for (s, want) in cases {
            assert_eq!(is_command_stem(s), want, "stem {s:?}");
        }
    }

    /// Concurrent writers must never share a temp path: successive
    /// names are unique (pid + counter), so one's bytes can't land in
    /// the other's file.
    #[test]
    fn tmp_paths_are_unique() {
        use std::collections::HashSet;
        let base = std::env::temp_dir().join("nerv-tmp-names.tsv");
        let names: HashSet<_> = (0..1000).map(|_| unique_tmp_path(&base)).collect();
        assert_eq!(names.len(), 1000);
    }

    /// Concurrent `write_atomic` calls all succeed and the surviving
    /// file is exactly one input — never interleaved bytes.
    #[test]
    fn concurrent_writes_never_interleave() {
        let path = std::env::temp_dir().join(format!("nerv-tmp-race-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let payloads: Vec<String> = (0..8).map(|i| format!("{i}\n").repeat(20000)).collect();
        std::thread::scope(|s| {
            for body in &payloads {
                s.spawn(|| {
                    write_atomic(&path, body).unwrap();
                });
            }
        });
        let final_text = std::fs::read_to_string(&path).unwrap();
        assert!(
            payloads.iter().any(|b| *b == final_text),
            "interleaved bytes survived"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A failed write leaves no temp file behind — `nerv uninstall`
    /// rewrites rc files through here and is specced for trace zero
    /// even when a step fails.
    #[test]
    fn write_atomic_failure_leaves_no_temp_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // The target is a non-empty directory: the temp file is created
        // and written fine, and only the final rename fails. That is the
        // branch that must clean up.
        let target = tmp.path().join("rc");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("occupant"), b"x").unwrap();
        assert!(write_atomic(&target, "content").is_err());
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("nerv-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp left behind: {leftovers:?}");
    }

    /// The happy path replaces by rename and keeps the target's mode.
    #[test]
    fn write_atomic_preserves_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("rc");
        std::fs::write(&target, "old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_atomic(&target, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a 0600 rc must not come back 0644");
    }

    #[test]
    fn cache_dir_macos_layout() {
        with_temp_home(|home| {
            let got = cache_dir().expect("cache_dir");
            assert_eq!(got, home.join("Library/Caches/nerv"));
        });
    }

    #[test]
    fn log_dir_macos_layout() {
        with_temp_home(|home| {
            let got = log_dir().expect("log_dir");
            assert_eq!(got, home.join("Library/Logs/nerv"));
        });
    }

    #[test]
    fn config_dir_dotfile_layout() {
        with_temp_home(|home| {
            let got = config_dir().expect("config_dir");
            assert_eq!(got, home.join(".config/nerv"));
        });
    }

    #[test]
    fn socket_pid_log_specs_under_cache() {
        with_temp_home(|home| {
            let cache = home.join("Library/Caches/nerv");
            assert_eq!(socket_path().unwrap(), cache.join("nervd.sock"));
            assert_eq!(pid_path().unwrap(), cache.join("nervd.pid"));
            assert_eq!(specs_dir().unwrap(), cache.join("specs"));
            assert_eq!(misses_path().unwrap(), cache.join("misses.tsv"));
            assert_eq!(
                daemon_log_path().unwrap(),
                home.join("Library/Logs/nerv/nervd.log"),
            );
        });
    }

    /// HOME unset → every getter returns None instead of panicking.
    /// uninstall depends on this branch for the "HOME unset" warning.
    #[test]
    fn home_unset_returns_none_everywhere() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("HOME");
        unsafe { std::env::remove_var("HOME") };
        let all_none = cache_dir().is_none()
            && log_dir().is_none()
            && config_dir().is_none()
            && socket_path().is_none()
            && pid_path().is_none()
            && daemon_log_path().is_none()
            && misses_path().is_none()
            && derived_specs_dir().is_none()
            && specs_dir().is_none();
        if let Some(p) = prev {
            unsafe { std::env::set_var("HOME", p) };
        }
        assert!(all_none);
    }

    /// `NERV_SPECS_DIR` always wins resolution — test isolation depends
    /// on it, so it beats even a populated user cache.
    #[test]
    fn resolve_env_override_beats_user_cache() {
        with_temp_home(|home| {
            // Populated user cache that would otherwise win.
            let user = home.join("Library/Caches/nerv/specs");
            std::fs::create_dir_all(&user).unwrap();
            std::fs::write(user.join("git.json"), "{}").unwrap();
            let over = home.join("override-specs");
            // Env mutation — serialized by HOME_LOCK via with_temp_home.
            unsafe { std::env::set_var("NERV_SPECS_DIR", &over) };
            let got = resolve_specs_dir();
            unsafe { std::env::remove_var("NERV_SPECS_DIR") };
            assert_eq!(got, Some(over));
        });
    }

    /// A user cache holding at least one spec wins over any bundled set
    /// (a local `build-specs` run overrides the shipped bundle).
    #[test]
    fn resolve_prefers_populated_user_cache() {
        with_temp_home(|home| {
            let user = home.join("Library/Caches/nerv/specs");
            std::fs::create_dir_all(&user).unwrap();
            std::fs::write(user.join("git.json.gz"), "x").unwrap();
            assert_eq!(resolve_specs_dir(), Some(user));
        });
    }

    /// Empty (or missing) user cache and no bundle next to the test
    /// binary → resolution still lands on the documented user path, so
    /// existing "dir missing" error flows keep their hint target.
    #[test]
    fn resolve_falls_back_to_user_path_when_nothing_found() {
        with_temp_home(|home| {
            let got = resolve_specs_dir();
            assert_eq!(got, Some(home.join("Library/Caches/nerv/specs")));
        });
    }

    /// Write a manifest stamping `dir` for `version`.
    #[cfg(test)]
    fn stamp_schema(dir: &std::path::Path, version: u32) {
        std::fs::write(
            dir.join("manifest.json"),
            format!(r#"{{"schema_version":{version},"nerv_version":"t","withfig_commit":"t","build_date":"0","specs":[]}}"#),
        )
        .unwrap();
    }

    /// A bundled spec set next to the running test binary, removed again
    /// however the test ends — left behind it would make every later
    /// resolution in this crate find a bundle that isn't there.
    struct BundledSpecs(PathBuf);

    impl BundledSpecs {
        fn create() -> Self {
            let dir = std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .join(SPECS_SUBDIR);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("git.json"), "{}").unwrap();
            stamp_schema(&dir, crate::manifest::SUPPORTED_SCHEMA_VERSION);
            Self(dir)
        }
    }

    impl Drop for BundledSpecs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A cache built by an older nerv must not win over the bundle.
    ///
    /// It used to, and the E5 gate then disabled completion wholesale:
    /// every user who had ever run `build-specs` went dark on the v2 → v3
    /// bump, with doctor advising a reinstall that replaces the bundle
    /// the cache was beating (observed on 0.1.13 → 0.1.14).
    #[test]
    fn resolve_skips_a_user_cache_built_for_another_schema() {
        with_temp_home(|home| {
            let bundled = BundledSpecs::create();
            let user = home.join("Library/Caches/nerv/specs");
            std::fs::create_dir_all(&user).unwrap();
            std::fs::write(user.join("git.json"), "{}").unwrap();
            stamp_schema(&user, crate::manifest::SUPPORTED_SCHEMA_VERSION + 1);

            let got = resolve_specs_dir().unwrap();
            assert_ne!(got, user, "a cache of the wrong schema must not be read");
            assert_eq!(got, std::fs::canonicalize(&bundled.0).unwrap());
        });
    }

    /// The same cache stamped for *this* schema still wins — the skip is
    /// about being unreadable, not about being a cache.
    #[test]
    fn resolve_keeps_a_user_cache_built_for_this_schema() {
        with_temp_home(|home| {
            let _bundled = BundledSpecs::create();
            let user = home.join("Library/Caches/nerv/specs");
            std::fs::create_dir_all(&user).unwrap();
            std::fs::write(user.join("git.json"), "{}").unwrap();
            stamp_schema(&user, crate::manifest::SUPPORTED_SCHEMA_VERSION);
            assert_eq!(resolve_specs_dir(), Some(user));
        });
    }

    /// No manifest at all is read leniently everywhere else (pre-manifest
    /// installs keep working), so it is read leniently here too.
    #[test]
    fn resolve_keeps_a_user_cache_with_no_manifest() {
        with_temp_home(|home| {
            let _bundled = BundledSpecs::create();
            let user = home.join("Library/Caches/nerv/specs");
            std::fs::create_dir_all(&user).unwrap();
            std::fs::write(user.join("git.json"), "{}").unwrap();
            assert_eq!(resolve_specs_dir(), Some(user));
        });
    }

    /// What doctor reports. Some only while a cache is actually being
    /// skipped — a usable one, or an explicit `NERV_SPECS_DIR`, is not
    /// something to warn about.
    #[test]
    fn stale_cache_is_reported_only_while_it_is_skipped() {
        with_temp_home(|home| {
            let user = home.join("Library/Caches/nerv/specs");
            std::fs::create_dir_all(&user).unwrap();
            assert_eq!(stale_spec_cache(), None, "empty cache");

            std::fs::write(user.join("git.json"), "{}").unwrap();
            assert_eq!(stale_spec_cache(), None, "no manifest");

            stamp_schema(&user, crate::manifest::SUPPORTED_SCHEMA_VERSION);
            assert_eq!(stale_spec_cache(), None, "current schema");

            stamp_schema(&user, 1);
            assert_eq!(stale_spec_cache(), Some((user.clone(), 1)));

            unsafe { std::env::set_var("NERV_SPECS_DIR", &user) };
            let under_override = stale_spec_cache();
            unsafe { std::env::remove_var("NERV_SPECS_DIR") };
            assert_eq!(
                under_override, None,
                "an explicit choice is not second-guessed"
            );
        });
    }

    #[test]
    fn user_specs_dir_under_config() {
        with_temp_home(|home| {
            assert_eq!(user_specs_dir().unwrap(), home.join(".config/nerv/specs"));
        });
    }

    /// No overlay dir → primary only — a fresh install behaves exactly
    /// as before.
    #[test]
    fn layers_without_overlay_is_primary_only() {
        with_temp_home(|home| {
            let got = resolve_spec_layers(false).unwrap();
            assert_eq!(got.overlay, None);
            assert_eq!(got.primary, home.join("Library/Caches/nerv/specs"));
            assert_eq!(got.dirs(), vec![home.join("Library/Caches/nerv/specs")]);
        });
    }

    /// The derived layer goes last: a hand-authored overlay spec and the
    /// bundled set both beat anything scraped out of `--help`.
    #[test]
    fn derived_layer_is_last_when_enabled() {
        with_temp_home(|home| {
            let overlay = home.join(".config/nerv/specs");
            std::fs::create_dir_all(&overlay).unwrap();
            let got = resolve_spec_layers(true).unwrap();
            assert_eq!(
                got.dirs(),
                vec![
                    overlay,
                    home.join("Library/Caches/nerv/specs"),
                    home.join("Library/Caches/nerv/derived"),
                ]
            );
        });
    }

    /// Derivation off → the layer is absent entirely, so nothing can
    /// read a stale derived file either.
    #[test]
    fn derived_layer_absent_when_disabled() {
        with_temp_home(|home| {
            let got = resolve_spec_layers(false).unwrap();
            assert_eq!(got.derived, None);
            assert_eq!(got.dirs(), vec![home.join("Library/Caches/nerv/specs")]);
        });
    }

    /// An overlay dir that exists goes *first* — even while empty, so the
    /// daemon watches it and a file copied in later hot-loads without a
    /// restart (spec-conversion-policy §6.1 hot-reload row).
    #[test]
    fn layers_include_existing_overlay_even_when_empty() {
        with_temp_home(|home| {
            let overlay = home.join(".config/nerv/specs");
            std::fs::create_dir_all(&overlay).unwrap();
            let got = resolve_spec_layers(false).unwrap();
            assert_eq!(
                got.dirs(),
                vec![overlay, home.join("Library/Caches/nerv/specs")]
            );
        });
    }

    /// `NERV_SPECS_DIR` collapses resolution to that one dir even when a
    /// populated overlay exists — e2e isolation must not see the
    /// developer's own overlay.
    #[test]
    fn layers_env_override_excludes_overlay() {
        with_temp_home(|home| {
            let overlay = home.join(".config/nerv/specs");
            std::fs::create_dir_all(&overlay).unwrap();
            std::fs::write(overlay.join("claude.json"), "{}").unwrap();
            let over = home.join("override-specs");
            unsafe { std::env::set_var("NERV_SPECS_DIR", &over) };
            // Even with derivation on, the env override collapses to one dir.
            let got = resolve_spec_layers(true);
            unsafe { std::env::remove_var("NERV_SPECS_DIR") };
            assert_eq!(
                got,
                Some(SpecLayers {
                    overlay: None,
                    primary: over,
                    derived: None,
                })
            );
        });
    }

    /// The has_specs gate: dirs with only unrelated files don't count.
    #[test]
    fn has_specs_ignores_non_spec_files() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!has_specs(tmp.path()));
        std::fs::write(tmp.path().join("README.md"), "x").unwrap();
        assert!(!has_specs(tmp.path()));
        std::fs::write(tmp.path().join("aws.json.gz"), "x").unwrap();
        assert!(has_specs(tmp.path()));
    }

    /// Subdir constants are the documented strings — locking them
    /// here makes a casual rename a doc-coordinated change.
    #[test]
    fn subdir_constants_locked() {
        assert_eq!(CACHE_SUBDIR, "Library/Caches/nerv");
        assert_eq!(LOG_SUBDIR, "Library/Logs/nerv");
        assert_eq!(CONFIG_SUBDIR, ".config/nerv");
        assert_eq!(SOCKET_NAME, "nervd.sock");
        assert_eq!(PID_NAME, "nervd.pid");
        assert_eq!(DAEMON_LOG_NAME, "nervd.log");
        assert_eq!(SPECS_SUBDIR, "specs");
    }

    /// The spec-miss filter asks "can this name actually run?" of a
    /// directory list, not of `$PATH`: the daemon passes the list it
    /// already has, and doctor passes the live one. Only a regular,
    /// executable file counts — a directory with a matching name or a
    /// non-executable file is not a runnable command.
    #[test]
    fn on_path_in_requires_an_executable_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "nerv-on-path-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("mytool");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let plain = dir.join("notes");
        std::fs::write(&plain, "x").unwrap();
        std::fs::create_dir(dir.join("adir")).unwrap();

        let dirs = vec![dir.clone()];
        assert!(on_path_in(&dirs, "mytool"));
        assert!(!on_path_in(&dirs, "notes"), "not executable");
        assert!(!on_path_in(&dirs, "adir"), "a directory is not a command");
        assert!(!on_path_in(&dirs, "missing"));
        assert!(!on_path_in(&dirs, ""), "empty name");
        assert!(!on_path_in(&dirs, "sub/mytool"), "path-like, not a stem");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
