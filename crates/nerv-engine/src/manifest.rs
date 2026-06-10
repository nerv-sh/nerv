//! Spec-cache manifest + schema-version gate (error-states.md §3.5 / E5).
//!
//! `build-specs` writes a `manifest.json` next to the spec files recording
//! the `schema_version` the cache was built for. The daemon compares that
//! against [`SUPPORTED_SCHEMA_VERSION`] at startup: a mismatch means the
//! user swapped in a cache from a different nerv build, so autocomplete is
//! disabled wholesale until they reinstall (vs. E2, which disables a single
//! broken spec). A *missing* manifest is treated leniently — pre-manifest
//! installs keep working — so only an explicit version mismatch trips E5.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::spec::Manifest;

/// Schema version this build of nerv understands. Bump when the spec JSON
/// layout changes incompatibly; `build-specs` stamps the same value into
/// `manifest.json` so old caches are rejected. Matches the `v2` documented
/// in spec.rs / spec-conversion-policy.md.
pub const SUPPORTED_SCHEMA_VERSION: u32 = 2;

/// File name of the manifest inside the specs dir.
pub const MANIFEST_NAME: &str = "manifest.json";

/// Result of comparing the on-disk manifest to [`SUPPORTED_SCHEMA_VERSION`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaStatus {
    /// Manifest present and the version matches.
    Ok,
    /// No manifest (or unreadable/unparseable) — treated leniently; the
    /// daemon keeps serving so pre-manifest caches don't break.
    Missing,
    /// Manifest present but its version differs — E5 trips.
    Mismatch { found: u32 },
}

/// `<specs_dir>/manifest.json`.
pub fn manifest_path(specs_dir: &Path) -> PathBuf {
    specs_dir.join(MANIFEST_NAME)
}

/// Compare the manifest in `specs_dir` against the supported schema.
///
/// Only [`SchemaStatus::Mismatch`] should disable autocomplete; `Missing`
/// is non-fatal by design (lenient toward older installs).
pub fn check_schema(specs_dir: &Path) -> SchemaStatus {
    // Lean read: we only need the version field, so we deserialize into a
    // tiny struct that ignores every other manifest key. This keeps the
    // gate cheap and tolerant of forward-compatible manifest additions.
    #[derive(Deserialize)]
    struct VersionOnly {
        schema_version: u32,
    }

    let path = manifest_path(specs_dir);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return SchemaStatus::Missing;
    };
    match serde_json::from_str::<VersionOnly>(&text) {
        Ok(v) if v.schema_version == SUPPORTED_SCHEMA_VERSION => SchemaStatus::Ok,
        Ok(v) => SchemaStatus::Mismatch {
            found: v.schema_version,
        },
        // A corrupt manifest is treated as missing rather than a hard
        // mismatch — we can't trust a number we couldn't parse.
        Err(_) => SchemaStatus::Missing,
    }
}

/// Write `manifest.json` into `specs_dir` stamped with the supported
/// schema version. Called by `build-specs` after it finishes writing the
/// individual spec files. `spec_count` is recorded for diagnostics; the
/// per-spec `specs` detail (tier/sha256, error-states §3.6.2) is left for
/// a later pass and serialized as an empty list for now.
pub fn write_manifest(specs_dir: &Path, spec_count: usize) -> std::io::Result<()> {
    let build_date = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string());
    let manifest = Manifest {
        schema_version: SUPPORTED_SCHEMA_VERSION,
        nerv_version: env!("CARGO_PKG_VERSION").to_string(),
        withfig_commit: "vendored".to_string(),
        build_date,
        specs: Vec::new(),
    };
    let _ = spec_count; // recorded via specs.len() once §3.6.2 lands.
    let json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(manifest_path(specs_dir), json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn missing_manifest_is_lenient() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(check_schema(dir.path()), SchemaStatus::Missing);
    }

    #[test]
    fn write_then_check_roundtrips_ok() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), 42).unwrap();
        assert_eq!(check_schema(dir.path()), SchemaStatus::Ok);
    }

    #[test]
    fn older_version_is_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            manifest_path(dir.path()),
            r#"{"schema_version":1,"nerv_version":"x","withfig_commit":"y","build_date":"0","specs":[]}"#,
        )
        .unwrap();
        assert_eq!(
            check_schema(dir.path()),
            SchemaStatus::Mismatch { found: 1 }
        );
    }

    #[test]
    fn corrupt_manifest_is_missing_not_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(manifest_path(dir.path()), "{not json").unwrap();
        assert_eq!(check_schema(dir.path()), SchemaStatus::Missing);
    }

    #[test]
    fn forward_compatible_extra_keys_ignored() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            manifest_path(dir.path()),
            format!(r#"{{"schema_version":{SUPPORTED_SCHEMA_VERSION},"future_key":true}}"#),
        )
        .unwrap();
        assert_eq!(check_schema(dir.path()), SchemaStatus::Ok);
    }
}
