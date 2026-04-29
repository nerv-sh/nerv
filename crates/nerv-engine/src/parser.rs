//! Tokenization and position inference.
//!
//! Given an input line and cursor offset, produce:
//! - the sequence of tokens up to (and including) the one being typed
//! - a `Position` enum indicating what the user is currently typing
//!
//! v1.0 implementation: minimal whitespace-aware tokenization. Quotes
//! and escape handling are intentionally simplified — the goal is fast,
//! correct enough for the 50 spec set. Edge cases land in v1.x.

/// A single command-line token plus its byte span in the original line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

/// What is the cursor currently sitting on?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Position {
    /// `git ⎵`  — about to type a subcommand.
    SubcommandSlot {
        /// Tokens consumed up to this point (program name, parent subcommands).
        path: Vec<String>,
    },
    /// `git --⎵` or `git -⎵` — typing a flag.
    FlagSlot {
        path: Vec<String>,
        /// `--` or `-` prefix already typed, plus partial flag name.
        partial: String,
    },
    /// `git checkout <branch>` — typing a positional argument.
    ArgumentSlot { path: Vec<String>, partial: String },
    /// Unparseable / unsupported (treat as "no completion").
    Unknown,
}

/// Tokenize and infer the position. Returns `None` if `cursor` is out
/// of bounds for `line`.
///
/// **Stub** — full implementation lands in M0-1 and M1 0–6주차.
pub fn parse(line: &str, cursor: usize) -> Option<(Vec<Token>, Position)> {
    if cursor > line.len() {
        return None;
    }
    // TODO(M0-1): real tokenizer + position inference.
    let _ = (line, cursor);
    Some((Vec::new(), Position::Unknown))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_returns_unknown_for_now() {
        let (toks, pos) = parse("git ", 4).unwrap();
        assert!(toks.is_empty());
        assert_eq!(pos, Position::Unknown);
    }

    #[test]
    fn parse_rejects_out_of_bounds_cursor() {
        assert!(parse("git", 99).is_none());
    }
}
