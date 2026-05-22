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
// Static helpers (chunk 2) — pure spec-tree queries used by the state
// machine in chunks 3-5. No state, no allocations beyond the obvious.
//
// `#[allow(dead_code)]` until chunks 3-5 wire the matching state machine
// that consumes these helpers. Unit tests in `mod tests` exercise each.
// ---------------------------------------------------------------------------

/// Find a child subcommand by name or alias. `None` if no match.
///
/// Search order matches the TS `findSubcommand`: scan `subcommands` in
/// declaration order, returning the first whose primary name or alias
/// equals `needle`. Aliases shadow nothing — primary names always win
/// because they're searched first per node.
#[allow(dead_code)]
pub(crate) fn find_subcommand<'a>(parent: &'a Subcommand, needle: &str) -> Option<&'a Subcommand> {
    parent
        .subcommands
        .iter()
        .find(|sc| sc.name == needle || sc.aliases.iter().any(|a| a == needle))
}

/// Find an option on `subcommand` whose `names` contains `needle`.
///
/// Mirrors TS `findOption`: linear scan, first match wins. The TS
/// implementation also handles `-xvf`-style chained shorts elsewhere;
/// that's the caller's job (chunk 5).
#[allow(dead_code)]
pub(crate) fn find_option<'a>(subcommand: &'a Subcommand, needle: &str) -> Option<&'a Opt> {
    subcommand
        .options
        .iter()
        .find(|o| o.names.iter().any(|n| n == needle))
}

/// Two options are "equal" iff they share at least one name. This is
/// the TS `optionsAreEqual` rule — used by `count_equal_options` to
/// enforce `is_repeatable`.
#[allow(dead_code)]
pub(crate) fn options_are_equal(a: &Opt, b: &Opt) -> bool {
    a.names.iter().any(|n| b.names.iter().any(|m| m == n))
}

/// Count how many times `opt` (or an alias of it) already appears in
/// `seen`. The state machine uses this to reject a second `--foo`
/// when `opt.is_repeatable == false`.
#[allow(dead_code)]
pub(crate) fn count_equal_options(opt: &Opt, seen: &[Opt]) -> usize {
    seen.iter().filter(|s| options_are_equal(s, opt)).count()
}

/// `true` when the option *may* be parsed again at this point — either
/// it's repeatable, or it has never been seen.
#[allow(dead_code)]
pub(crate) fn can_consume_option(opt: &Opt, seen: &[Opt]) -> bool {
    opt.is_repeatable || count_equal_options(opt, seen) == 0
}

/// `true` when the argument is required and not variadic — used by
/// chunk 5 to decide whether the cursor must stay in `Arg` context
/// (vs falling through to the next positional / option).
#[allow(dead_code)]
pub(crate) fn is_mandatory_or_variadic(arg: &Arg) -> bool {
    !arg.is_optional || arg.is_variadic
}

// ---------------------------------------------------------------------------
// Entry point (chunks 3-5 wire the real implementation)
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

    // ---- chunk 2: static helpers --------------------------------------

    fn opt(names: &[&str]) -> Opt {
        Opt {
            names: names.iter().map(|s| (*s).to_string()).collect(),
            ..Default::default()
        }
    }

    fn repeatable_opt(names: &[&str]) -> Opt {
        Opt {
            names: names.iter().map(|s| (*s).to_string()).collect(),
            is_repeatable: true,
            ..Default::default()
        }
    }

    #[test]
    fn find_subcommand_matches_primary_name() {
        let g = git_spec();
        let s = find_subcommand(&g, "status").unwrap();
        assert_eq!(s.name, "status");
    }

    #[test]
    fn find_subcommand_matches_alias() {
        let mut g = git_spec();
        g.subcommands[0].aliases = vec!["st".into()];
        let s = find_subcommand(&g, "st").unwrap();
        assert_eq!(s.name, "status");
    }

    #[test]
    fn find_subcommand_returns_none_for_unknown() {
        let g = git_spec();
        assert!(find_subcommand(&g, "nonexistent").is_none());
    }

    #[test]
    fn find_subcommand_primary_wins_over_later_alias() {
        // Two subcommands; second has an alias equal to first's name.
        // The first is returned because the scan is in order.
        let g = Subcommand {
            name: "root".into(),
            subcommands: vec![
                Subcommand {
                    name: "a".into(),
                    ..Default::default()
                },
                Subcommand {
                    name: "b".into(),
                    aliases: vec!["a".into()],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(find_subcommand(&g, "a").unwrap().name, "a");
    }

    #[test]
    fn find_option_matches_any_name() {
        let g = git_spec();
        let commit = &g.subcommands[1];
        assert!(find_option(commit, "-m").is_some());
        assert!(find_option(commit, "--message").is_some());
        assert!(find_option(commit, "--nope").is_none());
    }

    #[test]
    fn options_are_equal_shares_a_name() {
        let a = opt(&["-v", "--verbose"]);
        let b = opt(&["--verbose"]);
        let c = opt(&["-q"]);
        assert!(options_are_equal(&a, &b));
        assert!(!options_are_equal(&a, &c));
    }

    #[test]
    fn count_equal_options_counts_repetitions() {
        let v = opt(&["-v"]);
        let seen = vec![opt(&["-v"]), opt(&["-q"]), opt(&["-v", "--verbose"])];
        assert_eq!(count_equal_options(&v, &seen), 2);
    }

    #[test]
    fn can_consume_option_blocks_non_repeatable_second_use() {
        let v = opt(&["-v"]);
        let seen = vec![opt(&["-v"])];
        assert!(!can_consume_option(&v, &seen));
    }

    #[test]
    fn can_consume_option_allows_repeatable() {
        let v = repeatable_opt(&["-v"]);
        let seen = vec![opt(&["-v"]); 3];
        assert!(can_consume_option(&v, &seen));
    }

    #[test]
    fn can_consume_option_allows_unseen() {
        let v = opt(&["-v"]);
        assert!(can_consume_option(&v, &[]));
    }

    #[test]
    fn is_mandatory_or_variadic_classification() {
        let required = Arg {
            is_optional: false,
            is_variadic: false,
            ..Default::default()
        };
        let optional = Arg {
            is_optional: true,
            is_variadic: false,
            ..Default::default()
        };
        let variadic_optional = Arg {
            is_optional: true,
            is_variadic: true,
            ..Default::default()
        };
        let variadic_required = Arg {
            is_optional: false,
            is_variadic: true,
            ..Default::default()
        };

        assert!(is_mandatory_or_variadic(&required));
        assert!(!is_mandatory_or_variadic(&optional));
        assert!(is_mandatory_or_variadic(&variadic_optional));
        assert!(is_mandatory_or_variadic(&variadic_required));
    }
}
