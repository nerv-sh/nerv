//! spec_loader — load Fig-style autocomplete specs from disk (JSON).
//!
//! Bridges between the build-time spec cache
//! (`~/Library/Caches/nerv/specs/<name>.json` or the embedded
//! workspace fixture directory) and [`crate::spec_parser::Spec`].
//!
//! Adapted to Rust from the TypeScript implementation in
//! `aws/amazon-q-developer-cli-autocomplete` (Apache-2.0 + MIT),
//! file `packages/autocomplete-parser/src/loadSpec.ts`. The Rust
//! version drops Tier C dynamic loading — the M0 path is static
//! JSON only, with the expectation that build-time conversion has
//! already produced Tier A / B JSON from the vendored TS specs.
//!
//! Status: M0-6 chunk 1 — load / save by path, error type, basic
//! round-trip. Chunks 2-4 wire the build-spec binary and bulk cache.
//!
//! Refs: docs/spec-conversion-policy.md §3 (Tier A/B static JSON),
//! PRD v0.6 §10 M0-6.

use crate::spec_parser::Spec;
use std::fs;
use std::path::Path;

/// Errors surfaced by the spec loader.
#[derive(Debug, thiserror::Error)]
pub enum SpecLoadError {
    /// File missing or not readable.
    #[error("spec file not found: {0}")]
    NotFound(String),
    /// I/O error reading the spec file.
    #[error("io error reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// JSON parse error.
    #[error("json error in {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
}

/// Load a single spec from a JSON file on disk.
///
/// Returns [`SpecLoadError::NotFound`] when the path doesn't exist,
/// distinguishing it from generic I/O so callers can fall back to
/// the embedded default cache without log noise.
pub fn load_spec_file(path: &Path) -> Result<Spec, SpecLoadError> {
    if !path.exists() {
        return Err(SpecLoadError::NotFound(path.display().to_string()));
    }
    let text = fs::read_to_string(path).map_err(|e| SpecLoadError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    parse_spec_str(&text, path)
}

/// Parse a spec from an in-memory JSON string. Useful for tests
/// and for the build-spec binary's intermediate output.
pub fn parse_spec_str(text: &str, origin: &Path) -> Result<Spec, SpecLoadError> {
    serde_json::from_str::<Spec>(text).map_err(|e| SpecLoadError::Parse {
        path: origin.display().to_string(),
        source: e,
    })
}

/// Serialize a spec to a JSON string. Pretty-printed for human
/// inspection of the build cache.
pub fn write_spec_str(spec: &Spec) -> Result<String, SpecLoadError> {
    serde_json::to_string_pretty(spec).map_err(|e| SpecLoadError::Parse {
        path: "<in-memory>".into(),
        source: e,
    })
}

/// Write a spec to disk as JSON. Creates parent directories as
/// needed so callers don't have to pre-`mkdir -p`.
pub fn write_spec_file(spec: &Spec, path: &Path) -> Result<(), SpecLoadError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| SpecLoadError::Io {
            path: parent.display().to_string(),
            source: e,
        })?;
    }
    let text = write_spec_str(spec)?;
    fs::write(path, text).map_err(|e| SpecLoadError::Io {
        path: path.display().to_string(),
        source: e,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec_parser::{Arg, Generator, Opt, Subcommand, TemplateKind};
    use std::path::PathBuf;

    fn git_minimal() -> Spec {
        Subcommand {
            name: "git".into(),
            description: Some("VCS".into()),
            subcommands: vec![Subcommand {
                name: "status".into(),
                description: Some("Show working tree status".into()),
                ..Default::default()
            }],
            options: vec![Opt {
                names: vec!["-v".into(), "--version".into()],
                description: Some("Print version".into()),
                ..Default::default()
            }],
            args: vec![Arg {
                name: Some("path".into()),
                template: Some(TemplateKind::Filepaths),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn round_trip_in_memory_string() {
        let spec = git_minimal();
        let json = write_spec_str(&spec).unwrap();
        let restored = parse_spec_str(&json, Path::new("<test>")).unwrap();
        assert_eq!(spec, restored);
    }

    #[test]
    fn json_contains_expected_field_names() {
        let json = write_spec_str(&git_minimal()).unwrap();
        assert!(json.contains("\"name\": \"git\""));
        assert!(json.contains("\"subcommands\""));
        assert!(json.contains("\"options\""));
        assert!(json.contains("\"template\": \"filepaths\""));
    }

    #[test]
    fn load_missing_file_returns_not_found() {
        let path = PathBuf::from("/tmp/nerv-spec-loader-test-missing.json");
        // Ensure it doesn't exist.
        let _ = fs::remove_file(&path);
        let err = load_spec_file(&path).unwrap_err();
        assert!(matches!(err, SpecLoadError::NotFound(_)));
    }

    #[test]
    fn round_trip_via_tempfile() {
        let tmp = std::env::temp_dir().join("nerv-spec-loader-round-trip.json");
        let _ = fs::remove_file(&tmp);
        let spec = git_minimal();
        write_spec_file(&spec, &tmp).unwrap();
        let restored = load_spec_file(&tmp).unwrap();
        assert_eq!(spec, restored);
        fs::remove_file(&tmp).ok();
    }

    #[test]
    fn parse_malformed_returns_parse_error() {
        let err = parse_spec_str("{ this is not json", Path::new("<test>")).unwrap_err();
        assert!(matches!(err, SpecLoadError::Parse { .. }));
    }

    #[test]
    fn empty_object_decodes_as_default_spec() {
        let spec = parse_spec_str("{}", Path::new("<test>")).unwrap();
        assert_eq!(spec, Spec::default());
    }

    #[test]
    fn generator_template_round_trip() {
        let spec = Subcommand {
            name: "x".into(),
            args: vec![Arg {
                name: Some("branch".into()),
                generators: vec![Generator::Template {
                    script: vec!["git".into(), "branch".into()],
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let json = write_spec_str(&spec).unwrap();
        let restored = parse_spec_str(&json, Path::new("<test>")).unwrap();
        assert_eq!(spec, restored);
    }
}
