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
pub const DERIVED_SUBDIR: &str = "derived";

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
pub fn write_atomic(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| std::io::Error::other("path has no file name"))?;
    let tmp = path.with_file_name(format!("{file_name}.nerv-tmp"));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
    }
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path)
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
///    even when empty, so test isolation is airtight.
/// 2. The user cache (`~/Library/Caches/nerv/specs/`) *if it actually
///    holds specs* — a `build-specs` run there overrides the bundle.
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
        if has_specs(u) {
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
}
