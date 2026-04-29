//! Spec data model — mirrors the JSON produced by `build/spec-transpile`.
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

/// Body of a single spec JSON file (one per CLI). Schema TBD in M1.
///
/// **Stub** — populated when spec-transpile is implemented.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Spec {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub subcommands: Vec<Subcommand>,
    #[serde(default)]
    pub options: Vec<Option_>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subcommand {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub subcommands: Vec<Subcommand>,
    #[serde(default)]
    pub options: Vec<Option_>,
    #[serde(default)]
    pub args: Vec<Argument>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename = "Option")]
pub struct Option_ {
    /// All names the option can be invoked under (e.g., `["-m", "--message"]`).
    pub names: Vec<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub args: Vec<Argument>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Argument {
    pub name: String,
    pub description: Option<String>,
    /// If true, this argument needed a dynamic generator at the upstream
    /// spec; v1.0 surfaces a §5.1 hint instead of completing.
    #[serde(default)]
    pub limited: bool,
}
