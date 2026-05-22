//! spec_parser — match a parsed shell command against a Fig-style
//! `Spec` tree and report which subcommand / option / argument position
//! the cursor is in, plus the candidate completions at that position.
//!
//! Consumes the output of [`crate::shell_parser::parse`] (a `Program`
//! node tree) together with a [`Spec`] description of the target CLI
//! (built at compile time from the vendored `withfig/autocomplete`
//! specs by `nerv-engine::spec_loader` — M0-6) and produces a
//! [`ParserResult`] describing the current cursor context.
//!
//! Adapted to Rust from the TypeScript implementation in
//! `aws/amazon-q-developer-cli-autocomplete` (Apache-2.0 + MIT),
//! file `packages/autocomplete-parser/src/parseArguments.ts`. The
//! Fig spec format itself is a documented public specification
//! (the `@fig/autocomplete-types` npm package); the Rust expression
//! below is original — enum + struct types, slice views, no
//! closures over mutable state.
//!
//! Tier C scope: dynamic `generator.custom` / `postProcess` execution
//! is **deferred to M1** behind the `rquickjs` opt-in (PRD v0.6 §5.7).
//! This module surfaces Tier C metadata via [`Generator::Custom`] /
//! [`Generator::Script`] markers but does not invoke them — Tier B
//! callers fall back to the §5.1 dynamic-hint UX.
//!
//! Status: M0-5 chunk 1 — public types + entry-point stub. Chunks
//! 2-7 land the matching logic, state machine, candidate generation,
//! and regression scenarios.

use std::ops::Range;

// ---------------------------------------------------------------------------
// Spec data model (chunk 1)
// ---------------------------------------------------------------------------

/// A complete CLI specification — the root is a [`Subcommand`]
/// named after the CLI itself (e.g. `git`, `docker`, `kubectl`).
pub type Spec = Subcommand;

/// A subcommand node — recursive: subcommands contain subcommands.
///
/// Layout mirrors the `@fig/autocomplete-types` `Subcommand` shape:
/// name + zero or more options + zero or more positional args +
/// zero or more nested subcommands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Subcommand {
    /// Primary name. (Aliases live in [`Self::aliases`].)
    pub name: String,
    /// Alternative names that resolve to this same subcommand.
    pub aliases: Vec<String>,
    /// One-line description (rendered in the `?` help popup).
    pub description: Option<String>,
    /// Nested subcommands. Searched in order; the first matching
    /// name (or alias) wins.
    pub subcommands: Vec<Subcommand>,
    /// Long / short options accepted at this level.
    pub options: Vec<Opt>,
    /// Positional arguments consumed in order.
    pub args: Vec<Arg>,
    /// `true` if this subcommand requires `--` before positional
    /// args (i.e. `git -- log` style).
    pub requires_double_dash: bool,
    /// Hidden from the suggestion list but still parseable.
    pub hidden: bool,
}

/// A long / short option flag, possibly with one or more attached
/// argument(s).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Opt {
    /// All names that select this option (e.g. `["-h", "--help"]`).
    pub names: Vec<String>,
    /// One-line description.
    pub description: Option<String>,
    /// Argument(s) consumed after the option. Empty for flag-only
    /// options (`--verbose`).
    pub args: Vec<Arg>,
    /// Other options that, once seen, exclude this one.
    pub exclusive_on: Vec<String>,
    /// Options this one *requires* (must also appear).
    pub depends_on: Vec<String>,
    /// Required (must appear) — surfaces in `nerv doctor`.
    pub is_required: bool,
    /// May be repeated (e.g. `-v -v -v`).
    pub is_repeatable: bool,
    /// Hidden from the suggestion list but still parseable.
    pub hidden: bool,
}

/// A positional or option-bound argument.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Arg {
    /// Display name (e.g. `<file>`, `<branch>`, `<image>`).
    pub name: Option<String>,
    /// One-line description.
    pub description: Option<String>,
    /// Optional vs required.
    pub is_optional: bool,
    /// Consumes one-or-more rest tokens.
    pub is_variadic: bool,
    /// Static enum choices (e.g. `["yes", "no"]`). When non-empty,
    /// these are emitted directly as completions.
    pub suggestions: Vec<String>,
    /// Static filepath template (relative / absolute / extension
    /// filter). Empty when the arg is not a filepath.
    pub template: Option<TemplateKind>,
    /// Dynamic-generator markers. M0-5 surfaces these as
    /// metadata only; M1 + `rquickjs` opt-in executes them.
    pub generators: Vec<Generator>,
}

/// Filepath-style template suggestion source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TemplateKind {
    /// Any file path.
    Filepaths,
    /// Directory paths only.
    Folders,
    /// Shell history entries.
    History,
    /// `nerv help` topics (rare).
    Help,
}

/// Marker for a dynamic-generator entry. M0-5 records the shape so
/// downstream UX can show the §5.1 dynamic-hint (PRD v0.6 §5.7);
/// M1 + `rquickjs` opt-in runs the script / closure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Generator {
    /// `generator.template` — runs `script` and parses each line
    /// of stdout as a candidate. Static when `script` is a constant
    /// command string; dynamic otherwise.
    Template { script: Vec<String> },
    /// `generator.script` + `postProcess` (JS closure). Tier C —
    /// deferred to M1 rquickjs.
    Script {
        script: Vec<String>,
        has_post_process: bool,
    },
    /// `generator.custom!(tokens, executeCommand, ctx)` — pure JS
    /// closure that returns candidates. Tier C — deferred to M1.
    Custom { description_hint: Option<String> },
}

// ---------------------------------------------------------------------------
// Parser state (chunk 1 — types only; chunks 3–5 wire transitions)
// ---------------------------------------------------------------------------

/// Why the parser is sitting where it is — used by chunk 6 to decide
/// which candidate set (subcommand names / option names / arg suggestions)
/// to emit at the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CursorContext {
    /// Cursor is at a position that expects a subcommand name.
    #[default]
    Subcommand,
    /// Cursor is at a position that expects an option name (`-…` or `--…`).
    OptionName,
    /// Cursor is at a position that expects an argument value (positional
    /// or option-bound).
    Arg,
    /// Parser ran out of legal positions (cursor is past the last
    /// expected arg of a non-variadic, non-subcommand spec).
    Done,
}

/// Classification of each shell-parser token, attached as an
/// [`Annotation`] to the corresponding source span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    /// Matched a subcommand.
    Subcommand,
    /// Matched an option name.
    OptionName,
    /// Argument value bound to the preceding option.
    OptionArg,
    /// Positional argument of the current subcommand.
    SubcommandArg,
    /// Bundled short-flag run (e.g. `-xvf` → x + v + f).
    ChainedOption,
    /// `--` separator.
    DoubleDash,
    /// Token didn't match any expected position.
    Unknown,
}

/// A single token + how the parser classified it.
#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    /// Source-span (byte range in the user's input line).
    pub span: Range<usize>,
    /// The substring covered by `span`.
    pub text: String,
    /// What this token bound to.
    pub kind: TokenKind,
}

/// Result of running [`parse_arguments`] over one command tree.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParserResult {
    /// Token-by-token classification for the whole command line.
    pub annotations: Vec<Annotation>,
    /// What the cursor is "on" right now (subcommand / option / arg).
    pub cursor_context: CursorContext,
    /// Subcommand chain from root to current (e.g. `["git", "remote", "add"]`).
    /// Cloned names; the spec tree is not borrowed.
    pub subcommand_path: Vec<String>,
}

// ---------------------------------------------------------------------------
// Entry point (chunks 2-5 wire the real implementation)
// ---------------------------------------------------------------------------

/// Match the shell-parser tokens for the command at the cursor against
/// `spec` and report the cursor context + token annotations.
///
/// `tokens` is the flat sequence of source spans inside the active
/// [`crate::shell_parser::Node::Command`] — the caller (typically the
/// daemon's `complete` handler) flattens the relevant Command's
/// children before calling this fn.
///
/// `cursor` is a byte offset into the original input line; the result's
/// `cursor_context` reflects what the parser thinks is expected at
/// that position.
///
/// Chunk 1 returns an empty result so downstream callers can integrate
/// against the public API; chunks 2–5 wire the matching state machine.
pub fn parse_arguments(spec: &Spec, tokens: &[Annotation], cursor: usize) -> ParserResult {
    let _ = (spec, tokens, cursor);
    ParserResult::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_spec() -> Spec {
        Subcommand {
            name: "git".into(),
            description: Some("Distributed version control".into()),
            subcommands: vec![
                Subcommand {
                    name: "status".into(),
                    description: Some("Show the working tree status".into()),
                    ..Default::default()
                },
                Subcommand {
                    name: "commit".into(),
                    description: Some("Record changes to the repository".into()),
                    options: vec![Opt {
                        names: vec!["-m".into(), "--message".into()],
                        description: Some("Commit message".into()),
                        args: vec![Arg {
                            name: Some("msg".into()),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn spec_default_is_empty_subcommand() {
        let s = Spec::default();
        assert_eq!(s.name, "");
        assert!(s.subcommands.is_empty());
        assert!(s.options.is_empty());
        assert!(s.args.is_empty());
    }

    #[test]
    fn spec_fixture_has_two_subcommands() {
        let s = git_spec();
        assert_eq!(s.name, "git");
        assert_eq!(s.subcommands.len(), 2);
        assert_eq!(s.subcommands[0].name, "status");
        assert_eq!(s.subcommands[1].name, "commit");
    }

    #[test]
    fn opt_with_two_names_carries_both() {
        let s = git_spec();
        let commit = &s.subcommands[1];
        assert_eq!(commit.options.len(), 1);
        assert_eq!(commit.options[0].names, vec!["-m", "--message"]);
    }

    #[test]
    fn arg_default_is_required_non_variadic() {
        let arg = Arg::default();
        assert!(!arg.is_optional);
        assert!(!arg.is_variadic);
        assert!(arg.suggestions.is_empty());
        assert!(arg.generators.is_empty());
        assert!(arg.template.is_none());
    }

    #[test]
    fn cursor_context_default_is_subcommand() {
        let r = ParserResult::default();
        assert_eq!(r.cursor_context, CursorContext::Subcommand);
        assert!(r.annotations.is_empty());
        assert!(r.subcommand_path.is_empty());
    }

    #[test]
    fn parse_arguments_stub_returns_default() {
        let s = git_spec();
        let r = parse_arguments(&s, &[], 0);
        assert_eq!(r, ParserResult::default());
    }

    #[test]
    fn generator_template_carries_script() {
        let g = Generator::Template {
            script: vec!["git".into(), "branch".into(), "--list".into()],
        };
        if let Generator::Template { script } = &g {
            assert_eq!(script.len(), 3);
        } else {
            panic!("expected Template");
        }
    }

    #[test]
    fn template_kind_variants_are_distinct() {
        // Sanity check: enum variants compare by value, not identity.
        assert_ne!(TemplateKind::Filepaths, TemplateKind::Folders);
        assert_ne!(TemplateKind::History, TemplateKind::Help);
    }
}
