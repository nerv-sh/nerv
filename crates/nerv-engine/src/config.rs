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

#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    #[serde(default)]
    matching: Option<RawMatching>,
}

#[derive(Debug, Default, Deserialize)]
struct RawMatching {
    #[serde(default)]
    mode: Option<String>,
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
    pub fn load_default() -> Self {
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
        Self { mode }
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
