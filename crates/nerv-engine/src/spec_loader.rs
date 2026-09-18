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

use crate::spec_parser::{Spec, Subcommand};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use std::fs;
use std::io::{Read, Write};
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

impl SpecLoadError {
    /// The spec file this error is about — lets a caller attribute it to
    /// the layer (bundled vs user overlay) the file lives in.
    pub fn path(&self) -> &str {
        match self {
            Self::NotFound(p) => p,
            Self::Io { path, .. } | Self::Parse { path, .. } => path,
        }
    }
}

/// Load a single spec from disk. Recognizes plain `*.json` and
/// gzip-compressed `*.json.gz` by file extension.
///
/// Returns [`SpecLoadError::NotFound`] when the path doesn't exist,
/// distinguishing it from generic I/O so callers can fall back to
/// the embedded default cache without log noise.
pub fn load_spec_file(path: &Path) -> Result<Spec, SpecLoadError> {
    load_spec_file_with_size(path).map(|(spec, _)| spec)
}

/// [`load_spec_file`] variant that also returns the decompressed
/// source-JSON length — the registry's cheap proxy for the parsed
/// tree's heap footprint (its byte-budget eviction sums these).
pub fn load_spec_file_with_size(path: &Path) -> Result<(Spec, usize), SpecLoadError> {
    if !path.exists() {
        return Err(SpecLoadError::NotFound(path.display().to_string()));
    }
    let text = if path.extension().and_then(|e| e.to_str()) == Some("gz") {
        let file = fs::File::open(path).map_err(|e| SpecLoadError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        let mut decoder = GzDecoder::new(file);
        let mut s = String::new();
        decoder
            .read_to_string(&mut s)
            .map_err(|e| SpecLoadError::Io {
                path: path.display().to_string(),
                source: e,
            })?;
        s
    } else {
        fs::read_to_string(path).map_err(|e| SpecLoadError::Io {
            path: path.display().to_string(),
            source: e,
        })?
    };
    parse_spec_str(&text, path).map(|spec| (spec, text.len()))
}

/// Parse a spec from an in-memory JSON string. Useful for tests
/// and for the build-spec binary's intermediate output.
pub fn parse_spec_str(text: &str, origin: &Path) -> Result<Spec, SpecLoadError> {
    // Intern scope: duplicate description strings inside this one spec
    // deserialize to shared Arc<str>s instead of separate allocations
    // (aws: 54% duplicates ≈ 17MB). Pool dies with the scope.
    crate::spec_parser::intern::scope(|| serde_json::from_str::<Spec>(text)).map_err(|e| {
        SpecLoadError::Parse {
            path: origin.display().to_string(),
            source: e,
        }
    })
}

/// A spec bigger than this is split: its top-level subcommands are
/// written as their own files, largest first, until the root is small
/// enough to parse on a keystroke.
///
/// 4 MB is roughly 45 ms of parsing (117 MB of `aws` takes 1307 ms in a
/// release build, measured 2026-09-18), which is where a cold load stops
/// fitting in the registry's `SPEC_LOAD_SYNC_WAIT` window and starts
/// costing the user a blank first keystroke. Only `aws` (117 MB) and
/// `gcloud` (36 MB) are above it in the current corpus; the next biggest
/// spec is 2 MB.
pub const SPLIT_SPEC_ABOVE_BYTES: usize = 4 * 1024 * 1024;

/// What a split aims to leave behind: roughly 11 ms of parsing, so the
/// root lands inside the sync window with room for the subtree that
/// follows it on the same keystroke.
///
/// The target is what decides how many subcommands move out, rather than
/// a fixed per-subcommand threshold: a fixed one leaves however much the
/// tail happens to weigh. A 64 KB threshold left `aws` with a 7.7 MB
/// root — 136 services that were each small enough to keep, and together
/// far too big (measured 2026-09-18).
pub const SPLIT_ROOT_TARGET_BYTES: usize = 1024 * 1024;

/// Split `spec` into a root with stubs plus the extracted subtrees, or
/// return it unchanged when it is small enough to parse in one go.
///
/// A stub keeps everything the *parent* level renders — name, aliases,
/// description, icon, priority, hidden — and drops what only matters
/// once the line descends into it. `external` marks it so the registry
/// knows to go looking for the rest.
///
/// Splitting is one level deep on purpose. It buys the whole win (`aws
/// s3 ls` needs 90 KB of a 117 MB spec) and keeps each subtree file a
/// plain spec that the existing loader, the mtime watcher and
/// `nerv spec list` already understand.
pub fn split_oversized(spec: Spec) -> (Spec, Vec<(String, Spec)>) {
    let total = serialized_len(&spec);
    if total <= SPLIT_SPEC_ABOVE_BYTES {
        return (spec, Vec::new());
    }
    let mut root = spec;

    // Biggest first, stopping as soon as what is left fits the target:
    // the file that saves the most parsing per extra open goes first, and
    // the small tail stays inline where it costs nothing to carry.
    let mut sizes: Vec<(usize, usize)> = root
        .subcommands
        .iter()
        .enumerate()
        .map(|(i, sub)| (i, serialized_len(sub)))
        .collect();
    sizes.sort_by(|a, b| b.1.cmp(&a.1));

    let mut remaining = total;
    let mut take: Vec<usize> = Vec::new();
    for (idx, size) in sizes {
        if remaining <= SPLIT_ROOT_TARGET_BYTES {
            break;
        }
        if root.subcommands[idx].name.is_empty() {
            continue;
        }
        remaining -= size;
        take.push(idx);
    }
    take.sort_unstable();

    let mut extracted = Vec::with_capacity(take.len());
    for idx in take {
        let sub = &mut root.subcommands[idx];
        let stub = Subcommand {
            name: sub.name.clone(),
            aliases: sub.aliases.clone(),
            description: sub.description.clone(),
            icon: sub.icon.clone(),
            priority: sub.priority,
            hidden: sub.hidden,
            external: true,
            ..Default::default()
        };
        let name = sub.name.clone();
        extracted.push((name, std::mem::replace(sub, stub)));
    }
    (root, extracted)
}

fn serialized_len(spec: &Subcommand) -> usize {
    serde_json::to_string(spec).map(|s| s.len()).unwrap_or(0)
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
/// needed so callers don't have to pre-`mkdir -p`. If `path` ends
/// in `.gz`, the output is gzip-compressed.
pub fn write_spec_file(spec: &Spec, path: &Path) -> Result<(), SpecLoadError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| SpecLoadError::Io {
            path: parent.display().to_string(),
            source: e,
        })?;
    }
    let text = write_spec_str(spec)?;
    if path.extension().and_then(|e| e.to_str()) == Some("gz") {
        let file = fs::File::create(path).map_err(|e| SpecLoadError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        let mut encoder = GzEncoder::new(file, Compression::default());
        encoder
            .write_all(text.as_bytes())
            .map_err(|e| SpecLoadError::Io {
                path: path.display().to_string(),
                source: e,
            })?;
        encoder.finish().map_err(|e| SpecLoadError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        return Ok(());
    }
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
    fn descriptions_intern_within_one_parse() {
        // aws-style duplication: identical description strings across
        // nodes must share ONE allocation after the parse — this is the
        // memory contract behind Option<Arc<str>> + intern::scope.
        let json = r#"{"name":"x","subcommands":[
            {"name":"a","description":"same text"},
            {"name":"b","description":"same text"},
            {"name":"c","options":[{"names":["-v"],"description":"same text"}]}
        ]}"#;
        let spec = parse_spec_str(json, Path::new("<test>")).unwrap();
        let a = spec.subcommands[0].description.as_ref().unwrap();
        let b = spec.subcommands[1].description.as_ref().unwrap();
        let c = spec.subcommands[2].options[0].description.as_ref().unwrap();
        assert!(
            std::sync::Arc::ptr_eq(a, b) && std::sync::Arc::ptr_eq(b, c),
            "duplicate descriptions must share one allocation"
        );
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
    fn round_trip_via_gzipped_tempfile() {
        let tmp = std::env::temp_dir().join("nerv-spec-loader-round-trip.json.gz");
        let _ = fs::remove_file(&tmp);
        let spec = git_minimal();
        write_spec_file(&spec, &tmp).unwrap();
        // Disk should be smaller than the in-memory JSON string.
        let raw = write_spec_str(&spec).unwrap();
        let on_disk = fs::metadata(&tmp).unwrap().len() as usize;
        assert!(
            on_disk < raw.len(),
            "gzipped {on_disk} should be smaller than raw {} bytes",
            raw.len()
        );
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

    /// `*.gz` extension with non-gzip bytes underneath surfaces as
    /// `Io` (the gzip decoder bails on the magic-number mismatch),
    /// not as `Parse`. Locks the failure routing so a corrupted
    /// install's doctor output points the user at "reinstall" not
    /// "spec invalid".
    #[test]
    fn gz_extension_with_plain_bytes_is_io_error() {
        let tmp = std::env::temp_dir().join(format!(
            "nerv-spec-loader-fake-{}.json.gz",
            std::process::id()
        ));
        let _ = fs::remove_file(&tmp);
        // Plain JSON text — but the extension claims gzip.
        fs::write(&tmp, b"{\"name\":\"x\"}").unwrap();
        let err = load_spec_file(&tmp).unwrap_err();
        assert!(
            matches!(err, SpecLoadError::Io { .. }),
            "expected Io, got {err:?}"
        );
        fs::remove_file(&tmp).ok();
    }

    /// Truncated JSON — valid prefix, EOF mid-object — surfaces as
    /// `Parse`. Distinct from `NotFound` so callers can show the
    /// right hint ("spec corrupted" vs "spec missing").
    #[test]
    fn truncated_json_is_parse_error() {
        let err =
            parse_spec_str("{ \"name\": \"git\", \"subcomma", Path::new("<test>")).unwrap_err();
        assert!(matches!(err, SpecLoadError::Parse { .. }));
        // The error message must include the origin path so the
        // doctor table can show which file failed.
        assert!(err.to_string().contains("<test>"));
    }

    /// `write_spec_file` creates parent directories on demand — the
    /// build-specs binary depends on this to populate the spec cache
    /// dir on first run.
    #[test]
    fn write_creates_parent_dirs() {
        let base = std::env::temp_dir().join(format!("nerv-spec-mkdir-{}", std::process::id()));
        // Two missing levels.
        let nested = base.join("inner/leaf/git.json");
        let _ = fs::remove_dir_all(&base);
        write_spec_file(&git_minimal(), &nested).unwrap();
        assert!(nested.exists());
        let restored = load_spec_file(&nested).unwrap();
        assert_eq!(restored, git_minimal());
        let _ = fs::remove_dir_all(&base);
    }

    /// Multiple Generator variants survive serialize → parse with
    /// every field preserved. Catches a regression where a new
    /// variant lands in spec_parser without an explicit serde tag.
    #[test]
    fn generator_variants_round_trip_full_matrix() {
        let spec = Subcommand {
            name: "x".into(),
            args: vec![Arg {
                name: Some("multi".into()),
                generators: vec![
                    Generator::Template {
                        script: vec!["echo".into(), "alpha".into()],
                    },
                    Generator::PackageJsonScripts,
                    Generator::Filepaths { folders_only: true },
                    Generator::ZoxideQuery,
                    Generator::SshHosts,
                    Generator::MakefileTargets,
                    Generator::Custom {
                        description_hint: Some("hint".into()),
                        source: Some("(()=>[\"a\"])()".into()),
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let json = write_spec_str(&spec).unwrap();
        let restored = parse_spec_str(&json, Path::new("<test>")).unwrap();
        assert_eq!(spec, restored);
    }

    /// A subcommand whose serialized form is at least `bytes` long,
    /// with the weight in a nested child so a stub can be told from the
    /// payload.
    fn bulky(name: &str, bytes: usize) -> Subcommand {
        Subcommand {
            name: name.to_string(),
            subcommands: vec![Subcommand {
                name: format!("{name}-child"),
                description: Some("x".repeat(bytes).into()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn a_small_spec_is_left_whole() {
        let spec = Spec {
            name: "git".into(),
            subcommands: vec![bulky("log", 128 * 1024)],
            ..Default::default()
        };
        let (root, extracted) = split_oversized(spec.clone());
        assert_eq!(
            root, spec,
            "a spec under the threshold must be byte-identical"
        );
        assert!(extracted.is_empty());
    }

    #[test]
    fn an_oversized_spec_gives_up_its_big_subcommands() {
        let spec = Spec {
            name: "aws".into(),
            subcommands: vec![bulky("iam", SPLIT_SPEC_ABOVE_BYTES), bulky("tiny", 1024)],
            ..Default::default()
        };
        let (root, extracted) = split_oversized(spec);
        assert_eq!(extracted.len(), 1, "only the big subcommand moves out");
        assert_eq!(extracted[0].0, "iam");
        assert!(
            !extracted[0].1.subcommands.is_empty(),
            "the payload moves with it"
        );
        assert_eq!(root.subcommands[1].name, "tiny");
        assert!(
            !root.subcommands[1].subcommands.is_empty(),
            "small ones stay inline"
        );
    }

    #[test]
    fn a_stub_keeps_what_the_parent_level_renders() {
        let mut big = bulky("iam", SPLIT_SPEC_ABOVE_BYTES);
        big.aliases = vec!["identity".into()];
        big.description = Some("Identity and Access Management".into());
        big.icon = Some("i".into());
        big.priority = Some(60);
        let spec = Spec {
            name: "aws".into(),
            subcommands: vec![big],
            ..Default::default()
        };
        let (root, _) = split_oversized(spec);
        let stub = &root.subcommands[0];
        assert!(stub.external, "a stub must say so");
        assert_eq!(stub.aliases, vec!["identity".to_string()]);
        assert_eq!(
            stub.description.as_deref(),
            Some("Identity and Access Management")
        );
        assert_eq!(stub.icon.as_deref(), Some("i"));
        assert_eq!(stub.priority, Some(60));
        assert!(
            stub.subcommands.is_empty(),
            "the payload must not be duplicated"
        );
        assert!(stub.options.is_empty());
    }

    #[test]
    fn splitting_keeps_the_root_small_enough_to_parse_on_a_keystroke() {
        // Eight services, each under any plausible fixed threshold for a
        // single subcommand, that together blow the budget.
        let mut subs: Vec<Subcommand> = (0..8)
            .map(|i| bulky(&format!("svc{i}"), SPLIT_SPEC_ABOVE_BYTES / 4))
            .collect();
        subs.push(bulky("small", 1024));
        let spec = Spec {
            name: "aws".into(),
            subcommands: subs,
            ..Default::default()
        };
        let (root, extracted) = split_oversized(spec);
        assert!(!extracted.is_empty());
        assert!(
            serialized_len(&root) <= SPLIT_ROOT_TARGET_BYTES,
            "the split must reach the target, not stop at a fixed per-subcommand size: {} bytes",
            serialized_len(&root)
        );
    }

    #[test]
    fn a_stub_round_trips_through_the_file_format() {
        let spec = Spec {
            name: "aws".into(),
            subcommands: vec![Subcommand {
                name: "iam".into(),
                external: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let restored =
            parse_spec_str(&write_spec_str(&spec).unwrap(), Path::new("<test>")).unwrap();
        assert!(restored.subcommands[0].external);
    }

    /// An ordinary spec must not grow an `external` key — every bundled
    /// file would churn and the field would stop meaning "stub".
    #[test]
    fn an_ordinary_subcommand_serializes_without_the_marker() {
        let spec = Spec {
            name: "git".into(),
            subcommands: vec![Subcommand {
                name: "log".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(!write_spec_str(&spec).unwrap().contains("external"));
    }

    /// `SpecLoadError::Parse` Display surfaces both the origin path
    /// and the inner serde_json error message. Locks the format
    /// `nerv doctor`'s `spec health` row depends on.
    #[test]
    fn parse_error_display_includes_path_and_source() {
        let err = parse_spec_str("not json", Path::new("/tmp/bad-spec.json")).unwrap_err();
        let s = err.to_string();
        assert!(s.contains("/tmp/bad-spec.json"), "missing path: {s}");
        assert!(s.contains("json error"), "missing prefix: {s}");
    }

    /// `NotFound` Display surfaces the path so doctor's error row
    /// is actionable.
    #[test]
    fn not_found_display_includes_path() {
        let err = load_spec_file(Path::new("/this/does/not/exist/spec.json")).unwrap_err();
        let s = err.to_string();
        assert!(s.starts_with("spec file not found:"));
        assert!(s.contains("/this/does/not/exist/spec.json"));
    }
}
