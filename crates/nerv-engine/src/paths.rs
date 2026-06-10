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

/// `~/Library/Caches/nerv/specs/` — JSON spec cache populated by
/// the build-specs binary.
pub fn specs_dir() -> Option<PathBuf> {
    cache_dir().map(|c| c.join(SPECS_SUBDIR))
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
            && specs_dir().is_none();
        if let Some(p) = prev {
            unsafe { std::env::set_var("HOME", p) };
        }
        assert!(all_none);
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
