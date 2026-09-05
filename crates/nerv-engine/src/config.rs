//! User configuration loaded from `~/.config/nerv/nerv.toml`.
//!
//! v1.0 defaults to prefix matching (PLAN §5.1). M1 introduces the
//! `[matching] mode = "fuzzy"` opt-in. Missing file or parse failure
//! falls back to the default — no hard errors on config so the daemon
//! always starts cleanly.

use std::path::Path;

use serde::Deserialize;

/// How candidates are tested against the typed prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MatchMode {
    /// `git co` → `commit` / `config` only. v1.0 default.
    #[default]
    Prefix,
    /// Subsequence match — `git chk` → `checkout`. M1 opt-in.
    Fuzzy,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MatchingConfig {
    pub mode: MatchMode,
}

/// Whether a command with no spec in any layer may have one derived
/// from its own `--help` output (`crate::derived`). On by default —
/// the whole point is that the long tail completes without the user
/// doing anything. `[derived] enabled = false` turns it off, and then
/// nothing is ever spawned.
#[derive(Debug, Clone, Copy)]
pub struct DerivedConfig {
    pub enabled: bool,
}

impl Default for DerivedConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    matching: Option<RawMatching>,
    derived: Option<RawDerived>,
}

#[derive(Debug, Default, Deserialize)]
struct RawMatching {
    mode: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawDerived {
    enabled: Option<bool>,
}

/// Everything `~/.config/nerv/nerv.toml` carries, read once. The
/// daemon reads it at boot; the CLI reads it once per process. A
/// missing file or malformed TOML leaves every section at its default
/// so the daemon always starts cleanly.
#[derive(Debug, Clone, Copy, Default)]
pub struct Config {
    pub matching: MatchingConfig,
    pub derived: DerivedConfig,
}

impl Config {
    /// Read the config file at `path`. Any failure (missing file,
    /// permission denied, malformed TOML, unknown mode value) returns
    /// the default.
    pub fn load_from_path(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        Self::parse(&text)
    }

    /// Read from `~/.config/nerv/nerv.toml` (the documented location).
    /// `NERV_CONFIG_FILE` overrides the path entirely — useful for
    /// e2e tests that need to load a non-default config without
    /// touching the user's real `~/.config`.
    pub fn load_default() -> Self {
        if let Some(override_path) = std::env::var_os("NERV_CONFIG_FILE") {
            return Self::load_from_path(Path::new(&override_path));
        }
        match crate::paths::config_dir() {
            Some(dir) => Self::load_from_path(&dir.join("nerv.toml")),
            None => Self::default(),
        }
    }

    fn parse(text: &str) -> Self {
        let raw: RawConfig = match toml::from_str(text) {
            Ok(r) => r,
            Err(_) => return Self::default(),
        };
        let mode = raw
            .matching
            .and_then(|m| m.mode)
            .map(|s| s.to_ascii_lowercase())
            .as_deref()
            .map(parse_mode)
            .unwrap_or_default();
        let enabled = raw.derived.and_then(|d| d.enabled).unwrap_or(true);
        Self {
            matching: MatchingConfig { mode },
            derived: DerivedConfig { enabled },
        }
    }
}

fn parse_mode(s: &str) -> MatchMode {
    match s {
        "fuzzy" => MatchMode::Fuzzy,
        _ => MatchMode::Prefix,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_defaults_to_enabled() {
        let d = Config::parse("").derived;
        assert!(d.enabled, "the long tail completes without opt-in");
    }

    #[test]
    fn derived_can_be_disabled() {
        let d = Config::parse("[derived]\nenabled = false\n").derived;
        assert!(!d.enabled);
    }

    #[test]
    fn derived_and_matching_read_from_one_file() {
        let c = Config::parse("[matching]\nmode = \"fuzzy\"\n\n[derived]\nenabled = false\n");
        assert_eq!(c.matching.mode, MatchMode::Fuzzy);
        assert!(!c.derived.enabled);
    }

    #[test]
    fn malformed_toml_leaves_both_sections_at_default() {
        let c = Config::parse("[matching\nmode = ");
        assert_eq!(c.matching.mode, MatchMode::Prefix);
        assert!(c.derived.enabled);
    }

    #[test]
    fn default_is_prefix() {
        let cfg = MatchingConfig::default();
        assert_eq!(cfg.mode, MatchMode::Prefix);
    }

    #[test]
    fn missing_file_is_default() {
        let cfg = Config::load_from_path(Path::new("/does/not/exist")).matching;
        assert_eq!(cfg.mode, MatchMode::Prefix);
    }

    #[test]
    fn fuzzy_mode_parses() {
        let cfg = Config::parse("[matching]\nmode = \"fuzzy\"\n").matching;
        assert_eq!(cfg.mode, MatchMode::Fuzzy);
    }

    #[test]
    fn prefix_mode_parses() {
        let cfg = Config::parse("[matching]\nmode = \"prefix\"\n").matching;
        assert_eq!(cfg.mode, MatchMode::Prefix);
    }

    #[test]
    fn unknown_mode_falls_back_to_prefix() {
        let cfg = Config::parse("[matching]\nmode = \"banana\"\n").matching;
        assert_eq!(cfg.mode, MatchMode::Prefix);
    }

    #[test]
    fn empty_toml_is_default() {
        let cfg = Config::parse("").matching;
        assert_eq!(cfg.mode, MatchMode::Prefix);
    }

    #[test]
    fn missing_matching_section_is_default() {
        let cfg = Config::parse("[other]\nfoo = 1\n").matching;
        assert_eq!(cfg.mode, MatchMode::Prefix);
    }

    #[test]
    fn malformed_toml_is_default() {
        let cfg = Config::parse("not = valid = toml").matching;
        assert_eq!(cfg.mode, MatchMode::Prefix);
    }

    #[test]
    fn mode_is_case_insensitive() {
        let cfg = Config::parse("[matching]\nmode = \"FUZZY\"\n").matching;
        assert_eq!(cfg.mode, MatchMode::Fuzzy);
    }
}
