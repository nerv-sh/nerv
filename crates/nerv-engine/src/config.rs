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
    #[serde(default)]
    matching: Option<RawMatching>,
    #[serde(default)]
    derived: Option<RawDerived>,
}

#[derive(Debug, Default, Deserialize)]
struct RawMatching {
    #[serde(default)]
    mode: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawDerived {
    #[serde(default)]
    enabled: Option<bool>,
}

impl MatchingConfig {
    /// Read the config file at `path`. Any failure (missing file,
    /// permission denied, malformed TOML, unknown mode value) returns
    /// the default so the daemon stays up.
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
        parse_config(text).0
    }
}

impl DerivedConfig {
    /// Same file, same failure policy as [`MatchingConfig`]: anything
    /// unreadable or malformed leaves the default in place.
    pub fn load_from_path(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        parse_config(&text).1
    }

    /// Read from `~/.config/nerv/nerv.toml`, honouring the same
    /// `NERV_CONFIG_FILE` override as [`MatchingConfig::load_default`].
    pub fn load_default() -> Self {
        if let Some(override_path) = std::env::var_os("NERV_CONFIG_FILE") {
            return Self::load_from_path(Path::new(&override_path));
        }
        match crate::paths::config_dir() {
            Some(dir) => Self::load_from_path(&dir.join("nerv.toml")),
            None => Self::default(),
        }
    }
}

/// One parse for both sections — the file is read once per consumer,
/// and a malformed file must degrade both settings the same way.
fn parse_config(text: &str) -> (MatchingConfig, DerivedConfig) {
    let raw: RawConfig = match toml::from_str(text) {
        Ok(r) => r,
        Err(_) => return (MatchingConfig::default(), DerivedConfig::default()),
    };
    let mode = raw
        .matching
        .and_then(|m| m.mode)
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
        .map(parse_mode)
        .unwrap_or_default();
    let enabled = raw
        .derived
        .and_then(|d| d.enabled)
        .unwrap_or(DerivedConfig::default().enabled);
    (MatchingConfig { mode }, DerivedConfig { enabled })
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
        let (_, d) = parse_config("");
        assert!(d.enabled, "the long tail completes without opt-in");
    }

    #[test]
    fn derived_can_be_disabled() {
        let (_, d) = parse_config("[derived]\nenabled = false\n");
        assert!(!d.enabled);
    }

    #[test]
    fn derived_and_matching_read_from_one_file() {
        let (m, d) = parse_config("[matching]\nmode = \"fuzzy\"\n\n[derived]\nenabled = false\n");
        assert_eq!(m.mode, MatchMode::Fuzzy);
        assert!(!d.enabled);
    }

    #[test]
    fn malformed_toml_leaves_both_sections_at_default() {
        let (m, d) = parse_config("[matching\nmode = ");
        assert_eq!(m.mode, MatchMode::Prefix);
        assert!(d.enabled);
    }

    #[test]
    fn default_is_prefix() {
        let cfg = MatchingConfig::default();
        assert_eq!(cfg.mode, MatchMode::Prefix);
    }

    #[test]
    fn missing_file_is_default() {
        let cfg = MatchingConfig::load_from_path(Path::new("/does/not/exist"));
        assert_eq!(cfg.mode, MatchMode::Prefix);
    }

    #[test]
    fn fuzzy_mode_parses() {
        let cfg = MatchingConfig::parse("[matching]\nmode = \"fuzzy\"\n");
        assert_eq!(cfg.mode, MatchMode::Fuzzy);
    }

    #[test]
    fn prefix_mode_parses() {
        let cfg = MatchingConfig::parse("[matching]\nmode = \"prefix\"\n");
        assert_eq!(cfg.mode, MatchMode::Prefix);
    }

    #[test]
    fn unknown_mode_falls_back_to_prefix() {
        let cfg = MatchingConfig::parse("[matching]\nmode = \"banana\"\n");
        assert_eq!(cfg.mode, MatchMode::Prefix);
    }

    #[test]
    fn empty_toml_is_default() {
        let cfg = MatchingConfig::parse("");
        assert_eq!(cfg.mode, MatchMode::Prefix);
    }

    #[test]
    fn missing_matching_section_is_default() {
        let cfg = MatchingConfig::parse("[other]\nfoo = 1\n");
        assert_eq!(cfg.mode, MatchMode::Prefix);
    }

    #[test]
    fn malformed_toml_is_default() {
        let cfg = MatchingConfig::parse("not = valid = toml");
        assert_eq!(cfg.mode, MatchMode::Prefix);
    }

    #[test]
    fn mode_is_case_insensitive() {
        let cfg = MatchingConfig::parse("[matching]\nmode = \"FUZZY\"\n");
        assert_eq!(cfg.mode, MatchMode::Fuzzy);
    }
}
