//! Manifest data model — the `manifest.json` written by the `build-specs`
//! binary and read by the E5 schema gate (`manifest.rs`). The per-spec JSON
//! body model lives in `spec_parser`, not here.
//!
//! See `docs/spec-conversion-policy.md` for Tier A/B/C semantics and
//! `manifest.json` schema (v2).

use serde::{Deserialize, Serialize};

/// Top-level manifest written by the spec build.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub nerv_version: String,
    pub withfig_commit: String,
    /// ISO-8601 build date. Used for the `nerv doctor` "spec age" notice
    /// (error-states.md §3.6.2).
    pub build_date: String,
    pub specs: Vec<SpecMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecMeta {
    pub name: String,
    pub tier: Tier,
    /// Set on Tier B specs only — the static-extraction-impossible argument paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limited_args: Vec<LimitedArg>,
    pub sha256: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Tier {
    /// Fully static (no dynamic generators).
    A,
    /// Subcommands + flags static; some args dynamic.
    B,
    /// Excluded — unparseable or oversized. Not present in manifest;
    /// listed only in build logs.
    C,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitedArg {
    /// `subcommand/<arg>` style path, e.g., `checkout/<arg>`.
    pub path: String,
    /// Short reason like `dynamic-branch-list`.
    pub reason: String,
    /// Suggested shell command for the §5.1 hint UX.
    pub hint: String,
}
