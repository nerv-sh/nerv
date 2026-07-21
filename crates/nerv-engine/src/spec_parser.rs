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
//! Status: M0-5 chunks 1-5 complete — public types, static helpers,
//! state machine, token shape classifier, and matcher are wired.
//! `parse_arguments` walks tokens left-to-right and emits per-token
//! annotations + cursor context. Chunks 6-7 add candidate emission
//! and end-to-end spec-fixture regression.

use serde::{Deserialize, Serialize};
use std::ops::Range;

// ---------------------------------------------------------------------------
// Spec data model (chunk 1)
// ---------------------------------------------------------------------------

/// A complete CLI specification — the root is a [`Subcommand`]
/// named after the CLI itself (e.g. `git`, `docker`, `kubectl`).
pub type Spec = Subcommand;

/// Description interning — dedup at *deserialization* time.
///
/// Large converted specs repeat description strings heavily (measured:
/// aws 54% duplicate occurrences ≈ 17MB of bytes, gcloud 86% ≈ 10MB),
/// and every duplicate is its own small heap allocation. Deduping after
/// the parse wouldn't lower the daemon's resident footprint — freed
/// small blocks stay on resident pages — so the dedup has to happen
/// before the duplicate is ever allocated: a thread-local pool scoped
/// to one parse hands out shared `Arc<str>`s while serde walks the
/// tree. The pool is dropped at scope end; sharing never leaks across
/// specs, so evicting a spec frees all of its descriptions.
pub(crate) mod intern {
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::sync::Arc;

    thread_local! {
        static POOL: RefCell<Option<HashSet<Arc<str>>>> = const { RefCell::new(None) };
    }

    /// Run `f` with an active intern pool on this thread. Nested use
    /// keeps the outer pool.
    pub fn scope<T>(f: impl FnOnce() -> T) -> T {
        let fresh = POOL.with(|p| {
            let mut b = p.borrow_mut();
            if b.is_none() {
                *b = Some(HashSet::new());
                true
            } else {
                false
            }
        });
        let out = f();
        if fresh {
            POOL.with(|p| *p.borrow_mut() = None);
        }
        out
    }

    /// Shared `Arc<str>` for `s` — pooled inside a [`scope`], a plain
    /// one-off allocation outside (tests, hand-built specs).
    pub fn intern(s: &str) -> Arc<str> {
        POOL.with(|p| {
            let mut b = p.borrow_mut();
            match b.as_mut() {
                Some(set) => match set.get(s) {
                    Some(a) => a.clone(),
                    None => {
                        let a: Arc<str> = Arc::from(s);
                        set.insert(a.clone());
                        a
                    }
                },
                None => Arc::from(s),
            }
        })
    }

    /// `deserialize_with` adapter for `Option<Arc<str>>` fields.
    /// Borrows from the JSON buffer when possible, so a pool hit
    /// allocates nothing at all.
    pub fn de_opt<'de, D>(d: D) -> Result<Option<Arc<str>>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s: Option<std::borrow::Cow<'de, str>> = serde::Deserialize::deserialize(d)?;
        Ok(s.map(|s| intern(&s)))
    }
}

/// A subcommand node — recursive: subcommands contain subcommands.
///
/// Layout mirrors the `@fig/autocomplete-types` `Subcommand` shape:
/// name + zero or more options + zero or more positional args +
/// zero or more nested subcommands.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Subcommand {
    /// Primary name. (Aliases live in [`Self::aliases`].)
    pub name: String,
    /// Alternative names that resolve to this same subcommand.
    pub aliases: Vec<String>,
    /// One-line description (rendered in the `?` help popup).
    /// `Arc<str>`: heavily duplicated across nodes — interned per parse.
    #[serde(deserialize_with = "intern::de_opt")]
    pub description: Option<std::sync::Arc<str>>,
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
    /// Fig parity sort hint (higher = earlier). Falls back to alpha
    /// when equal / absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u32>,
    /// Fig parity icon glyph (emoji or single visible char). Stripped
    /// by the TS converter for `fig://*` URLs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Fig parity `parserDirectives.flagsArePosixNoncompliant`: when
    /// true, single-dash multi-char tokens (`-foo`) are treated as
    /// long options, not chained short flags. Common with Go-style
    /// CLIs (`docker`, `kubectl`).
    #[serde(default, rename = "flagsArePosixNoncompliant")]
    pub flags_are_posix_noncompliant: bool,
}

/// A long / short option flag, possibly with one or more attached
/// argument(s).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Opt {
    /// All names that select this option (e.g. `["-h", "--help"]`).
    pub names: Vec<String>,
    /// One-line description. Interned — see [`Subcommand::description`].
    #[serde(deserialize_with = "intern::de_opt")]
    pub description: Option<std::sync::Arc<str>>,
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
    /// Fig parity: when true, this option propagates to every
    /// descendant subcommand. e.g. `git --help isPersistent: true`
    /// makes `git commit --help` valid even though `commit`'s
    /// option table doesn't list `--help`.
    #[serde(default, rename = "isPersistent", alias = "is_persistent")]
    pub is_persistent: bool,
    /// Fig parity sort hint. Higher = earlier in the popup. Default
    /// 50 (Fig convention). Falls back to alpha when equal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u32>,
    /// Fig parity: when true, the option's argument MUST follow with
    /// `=` (or a custom string in Fig — we support bool only). Example:
    /// `--color=auto` valid, `--color auto` is two separate tokens.
    /// Completion appends `=` to the option insertion so the user
    /// continues into the arg in one motion.
    #[serde(default, rename = "requiresSeparator", alias = "requires_separator")]
    pub requires_separator: bool,
    /// Fig parity icon glyph. Same rules as Subcommand.icon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// A positional or option-bound argument.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Arg {
    /// Display name (e.g. `<file>`, `<branch>`, `<image>`).
    pub name: Option<String>,
    /// One-line description. Interned — see [`Subcommand::description`].
    #[serde(deserialize_with = "intern::de_opt")]
    pub description: Option<std::sync::Arc<str>>,
    /// Optional vs required.
    pub is_optional: bool,
    /// Consumes one-or-more rest tokens.
    pub is_variadic: bool,
    /// Static enum choices (e.g. `["yes", "no"]`). When non-empty,
    /// these are emitted directly as completions. Each entry can be
    /// a plain string or an object with description / displayName /
    /// insertValue / icon / priority — Fig spec parity.
    pub suggestions: Vec<RawSuggestion>,
    /// Static filepath template (relative / absolute / extension
    /// filter). Empty when the arg is not a filepath.
    pub template: Option<TemplateKind>,
    /// Dynamic-generator markers. M0-5 surfaces these as
    /// metadata only; M1 + `rquickjs` opt-in executes them.
    pub generators: Vec<Generator>,
    /// Fig parity: characters that split the typed token into
    /// "context prefix + query". e.g. `cargo search "tokio,serde"` →
    /// after comma, only the trailing `serde` is the query; the
    /// completion preserves `tokio,` in the insertion. Stored as
    /// a string of single-byte delimiter chars (`","`, `"@"`, etc.).
    /// Function-form `getQueryTerm` is Tier C and deferred to M1
    /// (PLAN §5.7 — `rquickjs` opt-in). The TS converter drops
    /// function-form values and only emits string/array forms.
    #[serde(
        default,
        rename = "getQueryTerm",
        skip_serializing_if = "Option::is_none"
    )]
    pub get_query_term: Option<String>,
    /// Fig parity: how typed prefix matches against candidates.
    /// Values: `"prefix"` (default) | `"substring"` | `"fuzzy"`.
    /// **Fuzzy is M1 opt-in only** (PLAN §5.1) — v1.0 silently
    /// downgrades `"fuzzy"` to prefix matching.
    #[serde(
        default,
        rename = "filterStrategy",
        skip_serializing_if = "Option::is_none"
    )]
    pub filter_strategy: Option<String>,
}

/// Rich suggestion entry. Mirrors the relevant fields of
/// Fig.Suggestion that affect completion behavior:
/// - `name`: canonical token (used for prefix-matching + fallback
///   insertion).
/// - `description`: footer text in the popup.
/// - `display_name`: label shown to the user (falls back to `name`).
/// - `insert_value`: text to actually insert (falls back to `name`).
///   Many specs use this for `pkg@version` or short-form aliases.
/// - `icon`: emoji / glyph for the menu row.
/// - `priority`: sort hint (Fig 1-100). Higher = earlier.
///
/// Deserialised from either a bare string (`"yes"`) or a JSON
/// object (`{"name": "yes", "description": "..."}`) via the
/// custom `Deserialize` impl below.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RawSuggestion {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<std::sync::Arc<str>>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "displayName")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "insertValue")]
    pub insert_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u32>,
}

impl<'de> serde::Deserialize<'de> for RawSuggestion {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Input {
            Bare(String),
            Obj {
                #[serde(default)]
                name: Option<String>,
                #[serde(default)]
                description: Option<String>,
                #[serde(default, alias = "displayName")]
                display_name: Option<String>,
                #[serde(default, alias = "insertValue")]
                insert_value: Option<String>,
                #[serde(default)]
                icon: Option<String>,
                #[serde(default)]
                priority: Option<u32>,
            },
        }
        match Input::deserialize(d)? {
            Input::Bare(s) => Ok(RawSuggestion {
                name: s,
                ..Default::default()
            }),
            Input::Obj {
                name,
                description,
                display_name,
                insert_value,
                icon,
                priority,
            } => Ok(RawSuggestion {
                name: name.unwrap_or_default(),
                description: description.map(|s| intern::intern(&s)),
                display_name,
                insert_value,
                icon,
                priority,
            }),
        }
    }
}

/// Filepath-style template suggestion source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
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
    /// closure that returns candidates. Tier C — historically deferred
    /// because no JS runtime shipped with v0.1.
    ///
    /// `source` is the verbatim function body (or full closure source)
    /// captured by the ts-to-json converter. Absent when the converter
    /// wasn't asked to emit it; `feature = "quickjs"` builds use it as
    /// the script body for [`crate::tier_c::execute_custom_source`].
    /// `feature` off → field is parsed and ignored, so existing JSON
    /// files keep loading.
    Custom {
        description_hint: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
    },
    /// Well-known: `npmScriptsGenerator` from `@withfig/autocomplete`.
    /// Reused by npm / yarn / pnpm / bun / rushx / nr specs. The Rust
    /// engine walks up from the CWD to the nearest `package.json`,
    /// parses it, and emits the `scripts` keys as completions —
    /// avoiding the postProcess closure (which is otherwise Tier C).
    PackageJsonScripts,
    /// Well-known: `filepaths()` / `folders()` from
    /// `@fig/autocomplete-generators`. Reused by `cd`, `cat`, `ls`,
    /// etc. The Rust engine lists entries in the directory implied by
    /// the current token, filtering on the trailing basename and on
    /// `folders_only`.
    Filepaths {
        #[serde(default)]
        folders_only: bool,
    },
    /// Well-known: zoxide directory history (`z`, `zoxide` specs).
    /// The Rust engine runs `zoxide query --list --score` and parses
    /// scored entries — closures in Fig's spec post-process the same
    /// list, but the underlying command is fixed.
    ZoxideQuery,
    /// Well-known: SSH host enumeration (`ssh`, `scp`, `sftp`, `mosh`,
    /// `rsync`). Fig's `knownHosts` + `configHosts` closures read
    /// `~/.ssh/known_hosts` + `~/.ssh/config` (with Include support).
    /// The Rust engine does the same in pure Rust to skip the Tier C
    /// closure roundtrip.
    SshHosts,
    /// Well-known: `make`'s `listTargets` closure. Parses `Makefile`
    /// (or `makefile` / `GNUmakefile`) in the current working dir and
    /// emits target names. Fig runs the closure via Node; we recover
    /// in pure Rust by scanning for `^[A-Za-z0-9_./-]+:` lines.
    MakefileTargets,
    /// Well-known: `man`'s `generateManualPages` closure. Walks
    /// `MANPATH` (or the default `/usr/share/man`,
    /// `/usr/local/share/man`, `/opt/homebrew/share/man`) for
    /// `manN/*.N` / `manN/*.N.gz` and emits the bare page name.
    ManPages,
    /// Well-known: npm's `dependenciesGenerator`. Walks up from the
    /// cwd to the nearest `package.json` and emits the union of
    /// `dependencies` / `devDependencies` / `optionalDependencies`
    /// keys. Used by `npm uninstall <pkg>` / `npm update <pkg>` /
    /// `npm outdated <pkg>` and equivalents (pnpm / yarn rm).
    PackageJsonDeps,
    /// Well-known: kubectl's `scripts.types` resource-type list.
    /// Runs `kubectl api-resources -o name` (Tier B). Used by
    /// `kubectl get <type>` / `describe` / `delete` / `logs`'s
    /// first positional arg, where the closure version reaches for
    /// `typeWithoutName(context[len-1])` and we can substitute the
    /// underlying `api-resources` call directly.
    KubectlResources,
    /// Well-known: cargo's `targetGenerator({ kind })` — runs
    /// `cargo metadata --format-version 1 --no-deps`, walks
    /// `packages[*].targets[*]`, and (when `kind` is set) filters by
    /// `target.kind.includes(kind)`. Used by `cargo run/build/test/
    /// bench/install/...` for `--bin / --example / --test / --bench`
    /// completions. `kind = None` returns every target regardless of
    /// kind (matches the fallthrough branch in the upstream closure).
    CargoTargets {
        #[serde(default)]
        kind: Option<String>,
    },
    /// Well-known: script-form generators (e.g. aws ec2 / iam) whose
    /// `postProcess` closure runs `JSON.parse(stdout)[parentKey]` then
    /// maps to either each element directly or `elm[idField]`. The
    /// Rust engine recovers the same shape without a JS runtime:
    /// run `script`, parse stdout as JSON, walk to `parent_key`,
    /// emit either the array elements (when `id_field` is None) or
    /// `array[i][id_field]`.
    ScriptWithJsonPath {
        script: Vec<String>,
        parent_key: String,
        #[serde(default)]
        id_field: Option<String>,
    },
    /// Well-known: aws `listCustomGenerator(tokens, exec, command,
    /// options, parentKey, childKey)` family. The helper is locally
    /// defined per aws spec file (lambda.ts / iam.ts / cloudformation.ts
    /// / …) but the data shape is uniform: build
    /// `aws <service> <verb> [<flag> <token-after-flag>]*`, run it,
    /// then walk into `parent_key` of the JSON response and project
    /// to `id_field`. Captured as data so no JS runtime is needed.
    ///
    /// `lookup_flags` lists option names whose value the closure pulls
    /// from the currently-typed tokens (e.g. `--function-name` is
    /// matched against `tokens` and the next token after it becomes
    /// the CLI value).
    AwsList {
        service: String,
        verb: String,
        #[serde(default)]
        lookup_flags: Vec<String>,
        parent_key: String,
        #[serde(default)]
        id_field: Option<String>,
    },
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
    /// When the cursor sits on an option's expected argument value
    /// (i.e. the previous token was an option with un-consumed arg
    /// slots), this carries `(option_name, arg_index)` so the caller
    /// can dispatch completion to the *option's* args[arg_index]
    /// instead of the surrounding subcommand's positional args.
    /// `None` everywhere else.
    pub active_option_arg: Option<(String, usize)>,
    /// Which positional slot of the current subcommand the cursor is
    /// awaiting. `ArgState` already tracks this (advancing per consumed
    /// token, saturating on a variadic tail); this exposes it so
    /// `complete` can dispatch to `args[idx]` instead of assuming
    /// `args[0]` — `git push origin <here>` is slot 1 (branch), not
    /// slot 0 (remote). `None` once a non-variadic arg list is
    /// exhausted, i.e. there is no legal positional left to complete.
    pub subcommand_arg_index: Option<usize>,
    /// Options already consumed at the *current* subcommand level
    /// (cleared on each subcommand descend, mirroring the matcher's
    /// own repeat-rejection scope). `complete` uses this with
    /// [`can_consume_option`] to stop re-suggesting a non-repeatable
    /// flag the user already typed (`docker run --rm --<tab>`).
    pub consumed_options: Vec<Opt>,
}

// ---------------------------------------------------------------------------
// Static helpers — pure spec-tree queries consumed by the matching state
// machine below (`step` / `get_initial_state`) and by `complete`. No state,
// no allocations beyond the obvious. Unit tests in `mod tests` exercise each.
// ---------------------------------------------------------------------------

/// Find a child subcommand by name or alias. `None` if no match.
///
/// Search order matches the TS `findSubcommand`: scan `subcommands` in
/// declaration order, returning the first whose primary name or alias
/// equals `needle`. Aliases shadow nothing — primary names always win
/// because they're searched first per node.
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
pub(crate) fn find_option<'a>(subcommand: &'a Subcommand, needle: &str) -> Option<&'a Opt> {
    subcommand
        .options
        .iter()
        .find(|o| o.names.iter().any(|n| n == needle))
}

/// Fig parity: when looking up `needle` on the current subcommand
/// (deepest in `path`), fall back to ancestor subcommands' options
/// whose `is_persistent` flag is set. `root` is the root spec;
/// `path` is the subcommand chain (path[0] is root.name).
pub(crate) fn find_option_inherited<'a>(
    root: &'a Spec,
    path: &[String],
    needle: &str,
) -> Option<&'a Opt> {
    // Walk leaf → root. Leaf can match any option; ancestors only
    // contribute options with is_persistent = true.
    let mut node: &Spec = root;
    let mut chain: Vec<&Spec> = vec![root];
    for name in path.iter().skip(1) {
        let next = find_subcommand(node, name)?;
        chain.push(next);
        node = next;
    }
    // Leaf (last) — any option.
    if let Some(opt) = chain.last().and_then(|sc| find_option(sc, needle)) {
        return Some(opt);
    }
    // Ancestors — only persistent.
    for sc in chain.iter().rev().skip(1) {
        if let Some(opt) = find_option(sc, needle) {
            if opt.is_persistent {
                return Some(opt);
            }
        }
    }
    None
}

/// Two options are "equal" iff they share at least one name. This is
/// the TS `optionsAreEqual` rule — used by `count_equal_options` to
/// enforce `is_repeatable`.
pub(crate) fn options_are_equal(a: &Opt, b: &Opt) -> bool {
    a.names.iter().any(|n| b.names.iter().any(|m| m == n))
}

/// Count how many times `opt` (or an alias of it) already appears in
/// `seen`. The state machine uses this to reject a second `--foo`
/// when `opt.is_repeatable == false`.
pub(crate) fn count_equal_options(opt: &Opt, seen: &[Opt]) -> usize {
    seen.iter().filter(|s| options_are_equal(s, opt)).count()
}

/// `true` when the option *may* be parsed again at this point — either
/// it's repeatable, or it has never been seen.
pub(crate) fn can_consume_option(opt: &Opt, seen: &[Opt]) -> bool {
    opt.is_repeatable || count_equal_options(opt, seen) == 0
}

// ---------------------------------------------------------------------------
// Internal state machine (chunk 3) — drives token-by-token matching.
//
// `ParserState` is the snapshot the matcher mutates as it walks the
// token stream. `ArgState` is the per-position arg cursor (which arg
// of the current option / subcommand is "expected next"). Both are
// crate-private; the public surface is [`ParserResult`].
// ---------------------------------------------------------------------------

/// Cursor inside a fixed-length arg list. Tracks which arg slot is
/// expected next; clamped to the variadic slot once `args.len()`
/// runs out and the last arg is variadic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArgState {
    /// Index of the next arg to consume. `args.len()` once exhausted
    /// for non-variadic specs; pinned to `args.len()-1` for variadic.
    pub idx: usize,
    /// Total arg count of the originating spec — owned so we can
    /// classify "is the cursor past the end" without re-borrowing
    /// the spec subtree.
    pub total: usize,
    /// `true` when the originating spec's last arg is variadic
    /// (so `idx` saturates instead of advancing past `total`).
    pub last_is_variadic: bool,
}

/// Snapshot of "where the matcher is" between two tokens. The state
/// machine in chunk 5 consumes one token per step and mutates a clone.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ParserState {
    /// Chain of subcommand names walked so far, root included.
    pub subcommand_path: Vec<String>,
    /// Options already consumed at the current subcommand level —
    /// used by [`can_consume_option`] / [`count_equal_options`].
    pub consumed_options: Vec<Opt>,
    /// Positional-arg cursor for the current subcommand. `None`
    /// when the current subcommand has no args (or all consumed
    /// and not variadic).
    pub subcommand_args: Option<ArgState>,
    /// When inside an option's arg list, cursor into that option's
    /// `args`. `None` between options.
    pub option_args: Option<ArgState>,
    /// `true` once `--` has been parsed — disables further option
    /// matching, all subsequent tokens are subcommand_args.
    pub past_double_dash: bool,
}

impl ArgState {
    /// Build an `ArgState` for `args` if it has at least one entry.
    pub(crate) fn new(args: &[Arg]) -> Option<Self> {
        if args.is_empty() {
            return None;
        }
        Some(Self {
            idx: 0,
            total: args.len(),
            last_is_variadic: args.last().is_some_and(|a| a.is_variadic),
        })
    }

    /// `true` if more args remain to consume.
    pub(crate) fn has_more(&self) -> bool {
        self.idx < self.total || self.last_is_variadic
    }

    /// Advance the cursor by one. Saturates at `total - 1` for variadic
    /// specs so the variadic slot keeps accepting tokens.
    pub(crate) fn advance(&mut self) {
        if self.last_is_variadic && self.idx + 1 >= self.total {
            self.idx = self.total - 1;
        } else {
            self.idx += 1;
        }
    }
}

/// Build the initial parser state for a root spec.
///
/// `subcommand_path` carries the root spec's `name`; the args cursor
/// is initialized to the root's positional args (if any). No options
/// have been seen.
pub(crate) fn get_initial_state(root: &Spec) -> ParserState {
    ParserState {
        subcommand_path: vec![root.name.clone()],
        consumed_options: Vec::new(),
        subcommand_args: ArgState::new(&root.args),
        option_args: None,
        past_double_dash: false,
    }
}

// ---------------------------------------------------------------------------
// Token shape classification (chunk 4) — pure string predicates over
// the *source text* of a token, no spec involvement. Chunk 5's state
// machine combines these with [`find_subcommand`] / [`find_option`]
// to pick the right transition.
// ---------------------------------------------------------------------------

/// Categorize a token by its surface form. Spec lookup happens later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TokenShape {
    /// `--` separator — disables option parsing for the rest of the line.
    DoubleDash,
    /// `--name` or `--name=value` long option. `value` is `Some` when
    /// the `=` form was used.
    LongOption { name: String, value: Option<String> },
    /// `-x` short option, or `-xvf` chain. `chars` lists each flag char.
    ShortOption { chars: Vec<char> },
    /// Anything else — subcommand candidate / arg value / numeric literal.
    Word,
    /// Empty token (cursor sitting right after whitespace).
    Empty,
}

/// Classify a token's surface form. Cheap — no spec lookup, just
/// shape inspection. Inputs come from the shell_parser's tokenized
/// spans (already quote-stripped by [`crate::shell_parser`]).
pub(crate) fn classify_token_shape(text: &str) -> TokenShape {
    if text.is_empty() {
        return TokenShape::Empty;
    }
    if text == "--" {
        return TokenShape::DoubleDash;
    }
    // Long option: starts with `--` (but `--` alone is already handled).
    if let Some(rest) = text.strip_prefix("--") {
        let (name, value) = match rest.find('=') {
            Some(eq) => (
                format!("--{}", &rest[..eq]),
                Some(rest[eq + 1..].to_string()),
            ),
            None => (format!("--{rest}"), None),
        };
        return TokenShape::LongOption { name, value };
    }
    // Short option: starts with `-` followed by at least one non-`-`
    // char. `-` alone is a literal word (e.g. stdin marker).
    if let Some(rest) = text.strip_prefix('-') {
        if rest.is_empty() {
            return TokenShape::Word;
        }
        // `-x=value` shape — treat as long-style for binding.
        if let Some(eq) = rest.find('=') {
            let name = format!("-{}", &rest[..eq]);
            let value = rest[eq + 1..].to_string();
            return TokenShape::LongOption {
                name,
                value: Some(value),
            };
        }
        // All-alphanumeric run → chained shorts (`-xvf`). Reject if
        // it contains a digit-only run like `-9` (that's a numeric
        // arg) — but only when the whole thing parses as a number.
        if rest.chars().all(|c| c.is_ascii_digit()) {
            return TokenShape::Word;
        }
        return TokenShape::ShortOption {
            chars: rest.chars().collect(),
        };
    }
    TokenShape::Word
}

// ---------------------------------------------------------------------------
// State machine (chunk 5) — consume tokens left-to-right, mutating
// `ParserState`, emitting one `TokenKind` per token.
// ---------------------------------------------------------------------------

/// Walk `path` through `root` to find the spec the matcher is currently
/// sitting inside. `path[0]` is the root's own name and is skipped.
/// Returns `None` if any segment fails to resolve (shouldn't happen
/// since the matcher only pushes names it just found via `find_subcommand`).
fn current_subcommand<'a>(root: &'a Spec, path: &[String]) -> Option<&'a Subcommand> {
    let mut node = root;
    for name in path.iter().skip(1) {
        node = find_subcommand(node, name)?;
    }
    Some(node)
}

/// Consume one token, mutating `state` and returning the kind we
/// just bound this token to.
fn step(state: &mut ParserState, text: &str, root: &Spec) -> TokenKind {
    if state.option_args.is_some() {
        if let Some(args) = state.option_args.as_mut() {
            args.advance();
            if !args.has_more() {
                state.option_args = None;
            }
        }
        return TokenKind::OptionArg;
    }

    if state.past_double_dash {
        if let Some(args) = state.subcommand_args.as_mut() {
            args.advance();
            if !args.has_more() {
                state.subcommand_args = None;
            }
        }
        return TokenKind::SubcommandArg;
    }

    let shape = classify_token_shape(text);

    match shape {
        TokenShape::DoubleDash => {
            state.past_double_dash = true;
            TokenKind::DoubleDash
        }
        TokenShape::Empty => TokenKind::Unknown,
        TokenShape::LongOption { name, value } => {
            let Some(opt) = find_option_inherited(root, &state.subcommand_path, &name) else {
                return TokenKind::Unknown;
            };
            if !can_consume_option(opt, &state.consumed_options) {
                return TokenKind::Unknown;
            }
            state.consumed_options.push(opt.clone());
            let mut arg_state = ArgState::new(&opt.args);
            if value.is_some() {
                if let Some(args) = arg_state.as_mut() {
                    args.advance();
                    if !args.has_more() {
                        arg_state = None;
                    }
                }
            } else if opt.requires_separator {
                // `--color` without `=val` cannot bind the next
                // whitespace-separated token as its arg.
                arg_state = None;
            }
            state.option_args = arg_state;
            TokenKind::OptionName
        }
        TokenShape::ShortOption { chars } => {
            // Single short → normal option; chain (≥2) → ChainedOption.
            // Exception: `parserDirectives.flagsArePosixNoncompliant`
            // at the root spec treats `-foo` as a long option named
            // `-foo`, not as chained shorts. Common with Go-style
            // CLIs (`-format`, `-output`).
            if chars.len() > 1 && root.flags_are_posix_noncompliant {
                let lookup: String = std::iter::once('-').chain(chars.iter().copied()).collect();
                let Some(opt) = find_option_inherited(root, &state.subcommand_path, &lookup) else {
                    return TokenKind::Unknown;
                };
                if !can_consume_option(opt, &state.consumed_options) {
                    return TokenKind::Unknown;
                }
                state.consumed_options.push(opt.clone());
                state.option_args = if opt.requires_separator {
                    None
                } else {
                    ArgState::new(&opt.args)
                };
                return TokenKind::OptionName;
            }
            if chars.len() == 1 {
                let lookup = format!("-{}", chars[0]);
                let Some(opt) = find_option_inherited(root, &state.subcommand_path, &lookup) else {
                    return TokenKind::Unknown;
                };
                if !can_consume_option(opt, &state.consumed_options) {
                    return TokenKind::Unknown;
                }
                state.consumed_options.push(opt.clone());
                state.option_args = if opt.requires_separator {
                    None
                } else {
                    ArgState::new(&opt.args)
                };
                return TokenKind::OptionName;
            }
            // Chained: each char must resolve to a flag-only (no args)
            // option. If any binding fails or a flag would take args,
            // mark Unknown — TS reference treats those as opaque.
            for c in &chars {
                let lookup = format!("-{c}");
                let Some(opt) = find_option_inherited(root, &state.subcommand_path, &lookup) else {
                    return TokenKind::Unknown;
                };
                if !opt.args.is_empty() {
                    return TokenKind::Unknown;
                }
                if !can_consume_option(opt, &state.consumed_options) {
                    return TokenKind::Unknown;
                }
                state.consumed_options.push(opt.clone());
            }
            TokenKind::ChainedOption
        }
        TokenShape::Word => {
            let Some(node) = current_subcommand(root, &state.subcommand_path) else {
                return TokenKind::Unknown;
            };
            // Subcommand match is only legal at the start of a position
            // where no positional arg has been consumed yet *and* the
            // current spec doesn't require `--` first.
            let no_positionals_consumed = state.subcommand_args.as_ref().is_none_or(|s| s.idx == 0);
            if no_positionals_consumed && !node.requires_double_dash {
                if let Some(sub) = find_subcommand(node, text) {
                    state.subcommand_path.push(sub.name.clone());
                    state.consumed_options.clear();
                    state.subcommand_args = ArgState::new(&sub.args);
                    state.option_args = None;
                    return TokenKind::Subcommand;
                }
            }
            if let Some(args) = state.subcommand_args.as_mut() {
                if args.has_more() {
                    args.advance();
                    if !args.has_more() {
                        state.subcommand_args = None;
                    }
                    return TokenKind::SubcommandArg;
                }
            }
            TokenKind::Unknown
        }
    }
}

/// Decide what the cursor "is on" given the final state.
fn compute_cursor_context(
    root: &Spec,
    state: &ParserState,
    _last_token_text: Option<&str>,
) -> CursorContext {
    if state.past_double_dash {
        return if state.subcommand_args.as_ref().is_some_and(|a| a.has_more()) {
            CursorContext::Arg
        } else {
            CursorContext::Done
        };
    }
    if state.option_args.as_ref().is_some_and(|a| a.has_more()) {
        return CursorContext::Arg;
    }
    if state.subcommand_args.as_ref().is_some_and(|a| a.has_more()) {
        return CursorContext::Arg;
    }
    let Some(node) = current_subcommand(root, &state.subcommand_path) else {
        return CursorContext::Done;
    };
    if !node.subcommands.is_empty() {
        CursorContext::Subcommand
    } else if !node.options.is_empty() {
        CursorContext::OptionName
    } else {
        CursorContext::Done
    }
}

// ---------------------------------------------------------------------------
// Entry point
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
pub fn parse_arguments(spec: &Spec, tokens: &[Annotation], cursor: usize) -> ParserResult {
    let mut state = get_initial_state(spec);
    let mut annotations = Vec::with_capacity(tokens.len());

    // The first token is the binary name itself — already consumed by
    // get_initial_state; emit it as Subcommand and skip the state step.
    let mut iter = tokens.iter();
    if let Some(first) = iter.next() {
        annotations.push(Annotation {
            span: first.span.clone(),
            text: first.text.clone(),
            kind: TokenKind::Subcommand,
        });
    }

    let mut last_consumed_text: Option<String> = None;

    for tok in iter {
        // Cursor sits inside, at the end of, or before this token →
        // stop consuming so the partially-typed token doesn't lock
        // the matcher into a wrong context. Treating end-of-token
        // as partial is what "git co|" should do — the user is mid-
        // word for completion, not done typing "co".
        if tok.span.end >= cursor {
            break;
        }
        let kind = step(&mut state, &tok.text, spec);
        last_consumed_text = Some(tok.text.clone());
        annotations.push(Annotation {
            span: tok.span.clone(),
            text: tok.text.clone(),
            kind,
        });
    }

    let cursor_context = compute_cursor_context(spec, &state, last_consumed_text.as_deref());

    // Capture which option-arg slot the cursor is awaiting, if any.
    // Lives alongside cursor_context = Arg; downstream uses it to
    // dispatch to the option's args[idx] generators instead of the
    // subcommand's positionals.
    let active_option_arg = match (state.option_args.as_ref(), state.consumed_options.last()) {
        (Some(args), Some(opt)) if args.has_more() => {
            opt.names.first().map(|n| (n.clone(), args.idx))
        }
        _ => None,
    };

    // Same idea for the positional slot: `has_more` gates it so an
    // exhausted non-variadic list reports "no slot" rather than
    // pointing past the end of `args`.
    let subcommand_arg_index = state
        .subcommand_args
        .as_ref()
        .filter(|args| args.has_more())
        .map(|args| args.idx);

    ParserResult {
        annotations,
        cursor_context,
        subcommand_path: state.subcommand_path,
        active_option_arg,
        subcommand_arg_index,
        consumed_options: state.consumed_options,
    }
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
    fn parse_arguments_empty_tokens_returns_root_context() {
        let s = git_spec();
        let r = parse_arguments(&s, &[], 0);
        assert!(r.annotations.is_empty());
        assert_eq!(r.subcommand_path, vec!["git".to_string()]);
        // git has subcommands → next position is a subcommand.
        assert_eq!(r.cursor_context, CursorContext::Subcommand);
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

    // ---- chunk 3: state machine types --------------------------------

    fn req_arg() -> Arg {
        Arg {
            name: Some("a".into()),
            ..Default::default()
        }
    }

    fn variadic_arg() -> Arg {
        Arg {
            name: Some("rest".into()),
            is_variadic: true,
            ..Default::default()
        }
    }

    #[test]
    fn arg_state_new_empty_returns_none() {
        assert!(ArgState::new(&[]).is_none());
    }

    #[test]
    fn arg_state_new_tracks_total_and_variadic_flag() {
        let s = ArgState::new(&[req_arg(), variadic_arg()]).unwrap();
        assert_eq!(s.idx, 0);
        assert_eq!(s.total, 2);
        assert!(s.last_is_variadic);

        let s2 = ArgState::new(&[req_arg(), req_arg()]).unwrap();
        assert!(!s2.last_is_variadic);
    }

    #[test]
    fn arg_state_advance_walks_then_stops_non_variadic() {
        let mut s = ArgState::new(&[req_arg(), req_arg()]).unwrap();
        assert_eq!(s.idx, 0);
        s.advance();
        assert_eq!(s.idx, 1);
        assert!(s.has_more());
        s.advance();
        assert_eq!(s.idx, 2);
        assert!(!s.has_more());
    }

    #[test]
    fn arg_state_advance_saturates_variadic() {
        let mut s = ArgState::new(&[req_arg(), variadic_arg()]).unwrap();
        s.advance(); // idx 0 → 1
        s.advance(); // saturates at 1
        s.advance(); // still saturated
        assert_eq!(s.idx, 1);
        assert!(s.has_more());
    }

    #[test]
    fn arg_state_single_variadic_stays_at_zero() {
        let mut s = ArgState::new(&[variadic_arg()]).unwrap();
        s.advance();
        s.advance();
        assert_eq!(s.idx, 0);
        assert!(s.has_more());
    }

    #[test]
    fn get_initial_state_records_root_name_and_args() {
        let g = git_spec();
        let st = get_initial_state(&g);
        assert_eq!(st.subcommand_path, vec!["git".to_string()]);
        assert!(st.subcommand_args.is_none());
        assert!(st.option_args.is_none());
        assert!(!st.past_double_dash);
        assert!(st.consumed_options.is_empty());
    }

    #[test]
    fn get_initial_state_with_root_args_populates_arg_state() {
        let spec = Subcommand {
            name: "ls".into(),
            args: vec![req_arg(), req_arg(), variadic_arg()],
            ..Default::default()
        };
        let st = get_initial_state(&spec);
        let args = st.subcommand_args.unwrap();
        assert_eq!(args.total, 3);
        assert_eq!(args.idx, 0);
        assert!(args.last_is_variadic);
    }

    // ---- chunk 4: token shape classifier -----------------------------

    #[test]
    fn classify_empty_token() {
        assert_eq!(classify_token_shape(""), TokenShape::Empty);
    }

    #[test]
    fn classify_double_dash_separator() {
        assert_eq!(classify_token_shape("--"), TokenShape::DoubleDash);
    }

    #[test]
    fn classify_long_option_bare() {
        assert_eq!(
            classify_token_shape("--verbose"),
            TokenShape::LongOption {
                name: "--verbose".into(),
                value: None,
            }
        );
    }

    #[test]
    fn classify_long_option_with_equals() {
        assert_eq!(
            classify_token_shape("--message=hello"),
            TokenShape::LongOption {
                name: "--message".into(),
                value: Some("hello".into()),
            }
        );
    }

    #[test]
    fn classify_long_option_empty_value_after_equals() {
        assert_eq!(
            classify_token_shape("--foo="),
            TokenShape::LongOption {
                name: "--foo".into(),
                value: Some(String::new()),
            }
        );
    }

    #[test]
    fn classify_short_option_single() {
        assert_eq!(
            classify_token_shape("-v"),
            TokenShape::ShortOption { chars: vec!['v'] }
        );
    }

    #[test]
    fn classify_short_option_chain() {
        assert_eq!(
            classify_token_shape("-xvf"),
            TokenShape::ShortOption {
                chars: vec!['x', 'v', 'f'],
            }
        );
    }

    #[test]
    fn classify_short_option_with_equals_is_long_shape() {
        assert_eq!(
            classify_token_shape("-m=msg"),
            TokenShape::LongOption {
                name: "-m".into(),
                value: Some("msg".into()),
            }
        );
    }

    #[test]
    fn classify_dash_alone_is_word() {
        assert_eq!(classify_token_shape("-"), TokenShape::Word);
    }

    #[test]
    fn classify_numeric_dash_is_word() {
        assert_eq!(classify_token_shape("-1"), TokenShape::Word);
        assert_eq!(classify_token_shape("-42"), TokenShape::Word);
    }

    #[test]
    fn classify_plain_word() {
        assert_eq!(classify_token_shape("status"), TokenShape::Word);
        assert_eq!(classify_token_shape("file.txt"), TokenShape::Word);
    }

    // ---- chunk 5: parse_arguments end-to-end -------------------------

    fn ann(text: &str, start: usize) -> Annotation {
        Annotation {
            span: start..start + text.len(),
            text: text.to_string(),
            kind: TokenKind::Unknown,
        }
    }

    /// Tokenize a space-separated input line into [`Annotation`] tokens,
    /// for test convenience. Real callers get tokens from `shell_parser`.
    fn tokenize(line: &str) -> Vec<Annotation> {
        let mut out = Vec::new();
        let mut start = 0usize;
        for word in line.split(' ') {
            if !word.is_empty() {
                out.push(ann(word, start));
            }
            start += word.len() + 1; // +1 for the space
        }
        out
    }

    #[test]
    fn parse_walks_subcommand() {
        let s = git_spec();
        let toks = tokenize("git status");
        let r = parse_arguments(&s, &toks, 999);
        assert_eq!(r.subcommand_path, vec!["git", "status"]);
        assert_eq!(r.annotations.len(), 2);
        assert_eq!(r.annotations[0].kind, TokenKind::Subcommand);
        assert_eq!(r.annotations[1].kind, TokenKind::Subcommand);
        // `status` has no subcommands / options / args → Done.
        assert_eq!(r.cursor_context, CursorContext::Done);
    }

    #[test]
    fn parse_long_option_with_separate_arg() {
        let s = git_spec();
        let toks = tokenize("git commit --message hello");
        let r = parse_arguments(&s, &toks, 999);
        assert_eq!(r.annotations[0].kind, TokenKind::Subcommand); // git
        assert_eq!(r.annotations[1].kind, TokenKind::Subcommand); // commit
        assert_eq!(r.annotations[2].kind, TokenKind::OptionName); // --message
        assert_eq!(r.annotations[3].kind, TokenKind::OptionArg); // hello
    }

    #[test]
    fn parse_long_option_with_equals_value() {
        let s = git_spec();
        let toks = tokenize("git commit --message=hello");
        let r = parse_arguments(&s, &toks, 999);
        assert_eq!(r.annotations[2].kind, TokenKind::OptionName);
    }

    #[test]
    fn parse_short_option() {
        let s = git_spec();
        let toks = tokenize("git commit -m hello");
        let r = parse_arguments(&s, &toks, 999);
        assert_eq!(r.annotations[2].kind, TokenKind::OptionName);
        assert_eq!(r.annotations[3].kind, TokenKind::OptionArg);
    }

    #[test]
    fn parse_unknown_subcommand_falls_through() {
        let s = git_spec();
        let toks = tokenize("git nonexistent");
        let r = parse_arguments(&s, &toks, 999);
        // `git` has no positional args, so "nonexistent" can't bind
        // as either subcommand or arg → Unknown.
        assert_eq!(r.annotations[1].kind, TokenKind::Unknown);
        assert_eq!(r.subcommand_path, vec!["git"]);
    }

    #[test]
    fn parse_double_dash_disables_option_matching() {
        let spec = Subcommand {
            name: "rm".into(),
            options: vec![Opt {
                names: vec!["-f".into()],
                ..Default::default()
            }],
            args: vec![Arg {
                name: Some("file".into()),
                is_variadic: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let toks = tokenize("rm -- -f");
        let r = parse_arguments(&spec, &toks, 999);
        assert_eq!(r.annotations[1].kind, TokenKind::DoubleDash);
        // After --, `-f` is a literal arg, not an option flag.
        assert_eq!(r.annotations[2].kind, TokenKind::SubcommandArg);
    }

    #[test]
    fn parse_repeatable_option_allowed_twice() {
        let spec = Subcommand {
            name: "x".into(),
            options: vec![Opt {
                names: vec!["-v".into()],
                is_repeatable: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let toks = tokenize("x -v -v");
        let r = parse_arguments(&spec, &toks, 999);
        assert_eq!(r.annotations[1].kind, TokenKind::OptionName);
        assert_eq!(r.annotations[2].kind, TokenKind::OptionName);
    }

    #[test]
    fn parse_non_repeatable_option_second_use_unknown() {
        let spec = Subcommand {
            name: "x".into(),
            options: vec![Opt {
                names: vec!["-v".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let toks = tokenize("x -v -v");
        let r = parse_arguments(&spec, &toks, 999);
        assert_eq!(r.annotations[1].kind, TokenKind::OptionName);
        assert_eq!(r.annotations[2].kind, TokenKind::Unknown);
    }

    #[test]
    fn parse_chained_short_options() {
        let spec = Subcommand {
            name: "x".into(),
            options: vec![
                Opt {
                    names: vec!["-x".into()],
                    ..Default::default()
                },
                Opt {
                    names: vec!["-v".into()],
                    ..Default::default()
                },
                Opt {
                    names: vec!["-f".into()],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let toks = tokenize("x -xvf");
        let r = parse_arguments(&spec, &toks, 999);
        assert_eq!(r.annotations[1].kind, TokenKind::ChainedOption);
    }

    #[test]
    fn parse_positional_arg_consumed() {
        let spec = Subcommand {
            name: "echo".into(),
            args: vec![Arg {
                name: Some("msg".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let toks = tokenize("echo hello");
        let r = parse_arguments(&spec, &toks, 999);
        assert_eq!(r.annotations[1].kind, TokenKind::SubcommandArg);
        assert_eq!(r.cursor_context, CursorContext::Done);
    }

    #[test]
    fn parse_variadic_arg_keeps_accepting() {
        let spec = Subcommand {
            name: "cat".into(),
            args: vec![Arg {
                name: Some("files".into()),
                is_variadic: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let toks = tokenize("cat a b c");
        let r = parse_arguments(&spec, &toks, 999);
        assert_eq!(r.annotations[1].kind, TokenKind::SubcommandArg);
        assert_eq!(r.annotations[2].kind, TokenKind::SubcommandArg);
        assert_eq!(r.annotations[3].kind, TokenKind::SubcommandArg);
        assert_eq!(r.cursor_context, CursorContext::Arg);
    }

    #[test]
    fn parse_cursor_inside_token_does_not_consume_partial() {
        let s = git_spec();
        // "git sta" with cursor at byte 6 (start of "sta")
        let toks = tokenize("git sta");
        let r = parse_arguments(&s, &toks, 4);
        // Only `git` consumed; "sta" left for completion.
        assert_eq!(r.annotations.len(), 1);
        assert_eq!(r.subcommand_path, vec!["git"]);
        assert_eq!(r.cursor_context, CursorContext::Subcommand);
    }

    #[test]
    fn parse_cursor_after_subcommand_expects_subcommand_args_or_options() {
        let s = git_spec();
        let toks = tokenize("git commit");
        let r = parse_arguments(&s, &toks, 999);
        assert_eq!(r.subcommand_path, vec!["git", "commit"]);
        // commit has options but no subcommands → OptionName.
        assert_eq!(r.cursor_context, CursorContext::OptionName);
    }

    #[test]
    fn parse_first_token_emitted_even_with_zero_cursor() {
        let s = git_spec();
        let toks = tokenize("git");
        let r = parse_arguments(&s, &toks, 0);
        // Even at cursor 0, the binary-name token is preserved so
        // downstream can render it.
        assert_eq!(r.annotations.len(), 1);
        assert_eq!(r.annotations[0].kind, TokenKind::Subcommand);
    }

    #[test]
    fn active_option_arg_after_option_with_arg() {
        // `git commit -m <here>` — cursor after `-m`, awaiting the
        // option's `msg` arg value.
        let s = git_spec();
        let toks = tokenize("git commit -m ");
        let r = parse_arguments(&s, &toks, 14);
        assert_eq!(r.cursor_context, CursorContext::Arg);
        assert_eq!(
            r.active_option_arg,
            Some(("-m".to_string(), 0)),
            "cursor on `git commit -m ` should bind active_option_arg to -m"
        );
    }

    #[test]
    fn active_option_arg_is_none_when_no_option_pending() {
        let s = git_spec();
        let toks = tokenize("git commit");
        let r = parse_arguments(&s, &toks, 999);
        assert_eq!(r.active_option_arg, None);
    }

    fn root_with_persistent_help() -> Spec {
        Subcommand {
            name: "git".into(),
            options: vec![Opt {
                names: vec!["--help".into()],
                description: Some("show help".into()),
                is_persistent: true,
                ..Default::default()
            }],
            subcommands: vec![Subcommand {
                name: "commit".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn persistent_option_resolves_in_child_subcommand() {
        let s = root_with_persistent_help();
        // commit doesn't list --help locally, but root's --help
        // isPersistent → should still bind.
        let opt = find_option_inherited(&s, &["git".into(), "commit".into()], "--help");
        assert!(opt.is_some(), "persistent --help should resolve in commit");
    }

    #[test]
    fn non_persistent_option_does_not_inherit() {
        let mut s = root_with_persistent_help();
        s.options[0].is_persistent = false;
        let opt = find_option_inherited(&s, &["git".into(), "commit".into()], "--help");
        assert!(opt.is_none(), "non-persistent --help must not inherit");
    }

    fn root_with_requires_separator() -> Spec {
        Subcommand {
            name: "ls".into(),
            options: vec![Opt {
                names: vec!["--color".into()],
                requires_separator: true,
                args: vec![Arg {
                    name: Some("when".into()),
                    suggestions: vec![
                        RawSuggestion {
                            name: "auto".into(),
                            ..Default::default()
                        },
                        RawSuggestion {
                            name: "never".into(),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn posix_noncompliant_treats_multichar_short_as_long() {
        // Go-style CLI: `-format json` is one option named `-format`,
        // not chained `-f -o -r -m -a -t`.
        let s = Subcommand {
            name: "go".into(),
            flags_are_posix_noncompliant: true,
            options: vec![Opt {
                names: vec!["-format".into()],
                args: vec![Arg::default()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let toks = tokenize("go -format json");
        let r = parse_arguments(&s, &toks, 999);
        // `-format` should be matched as the option (not Unknown).
        let kinds: Vec<_> = r.annotations.iter().map(|a| a.kind).collect();
        assert!(
            kinds.contains(&TokenKind::OptionName),
            "expected -format to bind as long-style option, got {:?}",
            kinds
        );
    }

    #[test]
    fn posix_compliant_default_chains_shorts() {
        // Without the directive, `-foo` is chained `-f -o -o` and
        // is Unknown unless all single-char flags exist.
        let s = Subcommand {
            name: "git".into(),
            options: vec![Opt {
                names: vec!["-format".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let toks = tokenize("git -format");
        let r = parse_arguments(&s, &toks, 999);
        // `-format` does NOT bind as long option in posix mode →
        // each char lookup fails → Unknown.
        let last = r.annotations.last().unwrap();
        assert_eq!(last.kind, TokenKind::Unknown);
    }

    #[test]
    fn requires_separator_eq_form_consumes_arg() {
        // `ls --color=auto` — opt + arg both bound in one token.
        let s = root_with_requires_separator();
        let toks = tokenize("ls --color=auto");
        let r = parse_arguments(&s, &toks, 999);
        // After `=value` form, option_args advances and clears →
        // no pending active_option_arg.
        assert_eq!(r.active_option_arg, None);
    }

    #[test]
    fn requires_separator_space_form_does_not_consume_next() {
        // `ls --color auto` — without `=`, the next token is NOT
        // the option arg. active_option_arg stays None and the next
        // token classifies as a Word (subcommand arg, not option arg).
        let s = root_with_requires_separator();
        let toks = tokenize("ls --color ");
        let r = parse_arguments(&s, &toks, 11);
        assert_eq!(
            r.active_option_arg, None,
            "requires_separator opt without `=` must not pend arg"
        );
    }

    #[test]
    fn leaf_options_take_priority_over_ancestor() {
        // Same flag name on both — leaf wins (even if not persistent).
        let s = Subcommand {
            name: "git".into(),
            options: vec![Opt {
                names: vec!["--mode".into()],
                description: Some("root mode".into()),
                is_persistent: true,
                ..Default::default()
            }],
            subcommands: vec![Subcommand {
                name: "commit".into(),
                options: vec![Opt {
                    names: vec!["--mode".into()],
                    description: Some("commit mode".into()),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let opt = find_option_inherited(&s, &["git".into(), "commit".into()], "--mode").unwrap();
        assert_eq!(opt.description.as_deref(), Some("commit mode"));
    }
}
