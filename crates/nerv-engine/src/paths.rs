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
