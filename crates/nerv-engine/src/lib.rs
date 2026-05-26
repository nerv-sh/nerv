//! nerv-engine — tokenization, position inference, and ranking.
//!
//! This crate is the heart of Nerv's matching pipeline:
//! 1. Take an input line + cursor position from the ZLE widget.
//! 2. Tokenize and infer the current position (subcommand / flag / argument).
//! 3. Look up matching specs and produce ranked completion candidates.
//!
//! v1.0 uses **prefix-match only** with an in-memory usage counter — no fuzzy,
//! no SQLite frecency. See PLAN.md §5.1 / §7 (v1.0 비목표 — fuzzy matching).
//!
//! Design references:
//! - `docs/error-states.md` — disabled spec handling (E2)
//! - `docs/spec-conversion-policy.md` — Tier B `(limited)` arg behavior
//! - `docs/first-5-min.md` — expected outputs per scenario step

#![deny(rust_2018_idioms)]
#![warn(missing_debug_implementations)]

pub mod complete;
pub mod ipc;
pub mod parser;
pub mod paths;
pub mod ranker;
pub mod shell_parser;
pub mod spec;
pub mod spec_loader;
pub mod spec_parser;

pub mod frecency;

pub use complete::{CompleteResult, SpecRegistry, complete, complete_in};
pub use frecency::FrecencyStore;
pub use ipc::{Request, Response, Suggestion, SuggestionKind};
pub use parser::{Position, Token};
pub use shell_parser::{Node, NodeKind, NodeOperator, Operator};
pub use spec_loader::{
    SpecLoadError, load_spec_file, parse_spec_str, write_spec_file, write_spec_str,
};
pub use spec_parser::{
    Annotation, Arg, CursorContext, Generator, Opt, ParserResult, Spec, Subcommand, TemplateKind,
    TokenKind, parse_arguments,
};

/// Result type used throughout nerv-engine.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors surfaced by the engine.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("spec parse error: {0}")]
    SpecParse(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}
