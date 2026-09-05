//! derived — build a spec from a command's own `--help` output.
//!
//! upstream `withfig/autocomplete` stopped in 2025-05, so the bundled
//! set never grows. Anything the user wrote themselves, or any tool
//! niche enough that Fig never covered it, completes empty. The user
//! overlay (`~/.config/nerv/specs/`) is the hand-authored answer; this
//! module is the automatic one, for the long tail nobody will write by
//! hand.
//!
//! Only the *shape* is derived: subcommand names, option names, and
//! argument placeholders. No generators, no dynamic values — those
//! need semantics `--help` does not carry.
//!
//! [`parse_help`] is deliberately pure: text in, [`Subcommand`] out. It
//! is the whole seam under test, and it never spawns anything.

use crate::spec_parser::{Arg, Opt, Subcommand};

/// A derived spec must clear one of these bars, or it is not written.
/// A usage error, a version banner, or a paragraph of prose can all
/// come back on stdout; none of them should become a spec.
const MIN_SUBCOMMANDS: usize = 1;
const MIN_OPTIONS: usize = 3;

/// Longest description kept. `--help` descriptions run to several
/// lines once continuations are joined, and the popup footer shows one
/// line.
const MAX_DESCRIPTION: usize = 80;

/// Parse `--help` (or `man`) output into a spec shaped like the ones
/// `build-specs` writes. `None` when the text carries too little
/// structure to be a spec — see [`MIN_SUBCOMMANDS`] / [`MIN_OPTIONS`].
///
/// `name` is the command the text came from; it becomes `spec.name`.
pub fn parse_help(name: &str, text: &str) -> Option<Subcommand> {
    let mut spec = Subcommand {
        name: name.to_string(),
        ..Default::default()
    };
    let mut section = Section::None;
    // Index of the item the next continuation line belongs to, plus the
    // column its name started at. A continuation is any line indented
    // past that column which does not itself parse as an item.
    let mut pending: Option<(Pending, usize)> = None;

    for raw in text.lines() {
        let line = strip_overstrike(raw);
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() {
            pending = None;
            continue;
        }
        let indent = trimmed.len() - trimmed.trim_start().len();

        if indent == 0 {
            // A flush-left line is either a section heading or prose;
            // either way the previous section ends here.
            section = classify_heading(trimmed.trim());
            pending = None;
            continue;
        }

        // Indented headings exist too (`  Flags - Message Options:`).
        // Only treat one as a heading when it names a section, so an
        // ordinary item that happens to end in `:` is not swallowed.
        if let Some(kind) = heading_kind(trimmed.trim()) {
            section = kind;
            pending = None;
            continue;
        }

        match section {
            Section::None => pending = None,
            Section::Subcommands => {
                if let Some(item) = parse_subcommand_line(trimmed) {
                    push_subcommand(&mut spec, item);
                    pending = Some((Pending::Subcommand(spec.subcommands.len() - 1), indent));
                } else {
                    absorb_continuation(&mut spec, &pending, indent, trimmed.trim());
                }
            }
            Section::Options => {
                if let Some(item) = parse_option_line(trimmed) {
                    if push_option(&mut spec, item) {
                        pending = Some((Pending::Option(spec.options.len() - 1), indent));
                    } else {
                        pending = None;
                    }
                } else {
                    absorb_continuation(&mut spec, &pending, indent, trimmed.trim());
                }
            }
        }
    }

    if spec.subcommands.len() < MIN_SUBCOMMANDS && spec.options.len() < MIN_OPTIONS {
        return None;
    }
    truncate_descriptions(&mut spec);
    Some(spec)
}

#[derive(Clone, Copy)]
enum Section {
    None,
    Subcommands,
    Options,
}

#[derive(Clone, Copy)]
enum Pending {
    Subcommand(usize),
    Option(usize),
}

/// `man` renders bold as `X\x08X` and underline as `_\x08X`. Collapse
/// both back to the bare character before anything else looks at the
/// line.
fn strip_overstrike(line: &str) -> String {
    if !line.contains('\u{8}') {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    for c in line.chars() {
        if c == '\u{8}' {
            out.pop();
        } else {
            out.push(c);
        }
    }
    out
}

/// A flush-left line either opens a section or closes the current one.
fn classify_heading(line: &str) -> Section {
    heading_kind(line).unwrap_or(Section::None)
}

/// Does this line name a section? Handles `Commands:`, `Available
/// Commands:`, `Flags - Message Options:`, and bare uppercase headings
/// like `CORE COMMANDS` / `FLAGS`.
fn heading_kind(line: &str) -> Option<Section> {
    let bare = line.strip_suffix(':').unwrap_or(line);
    let is_heading = line.ends_with(':')
        || (!bare.is_empty()
            && bare
                .chars()
                .all(|c| c.is_ascii_uppercase() || c == ' ' || c.is_ascii_digit()));
    if !is_heading {
        return None;
    }
    let lower = bare.to_ascii_lowercase();
    // "positional arguments:" (argparse) lists the subcommand *set* as
    // one brace-delimited blob, not one command per line; its useful
    // rows live in the indented block below and are picked up as plain
    // subcommand lines, so treat it as a command section too.
    if lower.contains("command") || lower.contains("subcommand") {
        Some(Section::Subcommands)
    } else if lower.contains("option") || lower.contains("flag") {
        Some(Section::Options)
    } else {
        None
    }
}

/// `  run      Run a command or script` → (`run`, arg?, description).
///
/// The hard part is telling an entry from a wrapped description line,
/// because both are indented and both start with a word. Help texts
/// answer it with columns, so this does too: a real entry is a name
/// **followed by a column break** — two or more spaces, a bracketed
/// placeholder, or the end of the line. A wrapped sentence ("Prints one
/// JSON line…") has single spaces between its words and is rejected.
fn parse_subcommand_line(line: &str) -> Option<Subcommand> {
    let body = line.trim_start();
    let (head, rest) = split_token(body)?;
    let name = head.strip_suffix(':').unwrap_or(head);
    if !is_command_name(name) {
        return None;
    }
    let column_break =
        rest.is_empty() || rest.starts_with("  ") || rest.trim_start().starts_with(['<', '[']);
    if !column_break {
        return None;
    }
    let (arg, rest) = take_placeholder(rest);
    Some(Subcommand {
        name: name.to_string(),
        description: description_of(rest),
        args: arg.into_iter().collect(),
        ..Default::default()
    })
}

/// `  -g, --generate <number>   Number of messages` → names + arg +
/// description.
fn parse_option_line(line: &str) -> Option<Opt> {
    let mut rest = line.trim_start();
    if !rest.starts_with('-') {
        return None;
    }
    let mut names: Vec<String> = Vec::new();
    let mut is_repeatable = false;
    loop {
        let (tok, tail) = split_token(rest)?;
        // clap marks a repeatable flag by appending `...` to its name
        // (`-v, --verbose...`). Strip the marker before validating, or
        // the whole name is mistaken for description text.
        let flag = tok.trim_end_matches(',');
        let trimmed = flag.trim_end_matches('.');
        if trimmed.len() != flag.len() {
            is_repeatable = true;
        }
        if !is_flag_name(trimmed) {
            break;
        }
        names.push(trimmed.to_string());
        rest = tail;
        // Another name only follows a comma.
        if !tok.ends_with(',') {
            break;
        }
    }
    if names.is_empty() {
        return None;
    }
    let (arg, rest) = take_placeholder(rest);
    Some(Opt {
        names,
        description: description_of(rest),
        args: arg.into_iter().collect(),
        is_repeatable,
        ..Default::default()
    })
}

/// Split off the first whitespace-delimited token, returning it and the
/// untrimmed remainder (the caller needs the remainder's leading spaces
/// to tell an argument from a description).
fn split_token(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    match s.find(char::is_whitespace) {
        Some(i) => Some((&s[..i], &s[i..])),
        None => Some((s, "")),
    }
}

/// Take an argument placeholder if one directly follows the name.
///
/// Two shapes count. A bracketed token (`<id>`, `[args…]`) is an
/// argument wherever it sits. A bare word is an argument only when it
/// hugs the name (one space) *and* the description is then set off by
/// two or more — the difference between `--completion string   Generates
/// …` and `--color                       Colored output`, which would
/// otherwise both look like "flag, word, words".
fn take_placeholder(rest: &str) -> (Option<Arg>, &str) {
    let gap = rest.len() - rest.trim_start().len();
    let Some((tok, tail)) = split_token(rest) else {
        return (None, rest);
    };
    let bracketed = (tok.starts_with('<') && tok.ends_with('>'))
        || (tok.starts_with('[') && tok.ends_with(']'));
    let bare_arg = gap == 1
        && !tok.starts_with('-')
        && tok.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && tail.starts_with("  ");
    if !bracketed && !bare_arg {
        return (None, rest);
    }
    let is_optional = tok.starts_with('[');
    let inner = tok
        .trim_matches(|c| matches!(c, '<' | '>' | '[' | ']'))
        .trim_end_matches(['.', '…']);
    if inner.is_empty() {
        return (None, rest);
    }
    (
        Some(Arg {
            name: Some(inner.to_string()),
            is_optional,
            is_variadic: tok.contains('…') || tok.contains("..."),
            ..Default::default()
        }),
        tail,
    )
}

fn description_of(rest: &str) -> Option<std::sync::Arc<str>> {
    let d = rest.trim();
    if d.is_empty() {
        None
    } else {
        Some(std::sync::Arc::from(d))
    }
}

/// Append a wrapped line to whichever item is still open.
fn absorb_continuation(
    spec: &mut Subcommand,
    pending: &Option<(Pending, usize)>,
    indent: usize,
    text: &str,
) {
    let Some((slot, name_indent)) = pending else {
        return;
    };
    if indent <= *name_indent {
        return;
    }
    let existing = match slot {
        Pending::Subcommand(i) => &mut spec.subcommands[*i].description,
        Pending::Option(i) => &mut spec.options[*i].description,
    };
    let joined = match existing.as_deref() {
        Some(d) => format!("{d} {text}"),
        None => text.to_string(),
    };
    *existing = Some(std::sync::Arc::from(joined.as_str()));
}

/// Keep the first spelling of a repeated name — help texts list the
/// same command under several headings (gh's `help`, aliases).
fn push_subcommand(spec: &mut Subcommand, item: Subcommand) {
    if spec.subcommands.iter().any(|c| c.name == item.name) {
        return;
    }
    spec.subcommands.push(item);
}

/// Returns whether the option was added (a duplicate is dropped, and
/// its continuation lines must not land on the earlier entry).
fn push_option(spec: &mut Subcommand, item: Opt) -> bool {
    let clash = spec
        .options
        .iter()
        .any(|o| o.names.iter().any(|n| item.names.contains(n)));
    if clash {
        return false;
    }
    spec.options.push(item);
    true
}

fn is_command_name(s: &str) -> bool {
    !s.is_empty()
        && s.starts_with(|c: char| c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
        && !s.ends_with('.')
}

fn is_flag_name(s: &str) -> bool {
    let body = s.strip_prefix("--").or_else(|| s.strip_prefix('-'));
    match body {
        Some(b) => {
            !b.is_empty()
                && b.starts_with(|c: char| c.is_ascii_alphanumeric())
                && b.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        }
        None => false,
    }
}

fn truncate_descriptions(spec: &mut Subcommand) {
    fn cut(d: &mut Option<std::sync::Arc<str>>) {
        let Some(text) = d.as_deref() else { return };
        if text.chars().count() <= MAX_DESCRIPTION {
            return;
        }
        let short: String = text.chars().take(MAX_DESCRIPTION - 1).collect();
        *d = Some(std::sync::Arc::from(
            format!("{}…", short.trim_end()).as_str(),
        ));
    }
    for c in &mut spec.subcommands {
        cut(&mut c.description);
    }
    for o in &mut spec.options {
        cut(&mut o.description);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UV: &str = include_str!("../tests/fixtures/help/uv.txt");
    const AICOMMIT2: &str = include_str!("../tests/fixtures/help/aicommit2.txt");
    const GH: &str = include_str!("../tests/fixtures/help/gh.txt");
    const ZEPH: &str = include_str!("../tests/fixtures/help/zeph.txt");

    fn sub_names(s: &Subcommand) -> Vec<&str> {
        s.subcommands.iter().map(|c| c.name.as_str()).collect()
    }

    fn opt_names(s: &Subcommand) -> Vec<&str> {
        s.options
            .iter()
            .flat_map(|o| o.names.iter().map(|n| n.as_str()))
            .collect()
    }

    fn find_opt<'a>(s: &'a Subcommand, name: &str) -> &'a Opt {
        s.options
            .iter()
            .find(|o| o.names.iter().any(|n| n == name))
            .unwrap_or_else(|| panic!("option {name} not found in {:?}", opt_names(s)))
    }

    /// clap's default layout: `Commands:` with name + two spaces +
    /// description, then `Options:` where the description sits on the
    /// *next*, more-indented line.
    #[test]
    fn parses_clap_layout_with_next_line_descriptions() {
        let spec = parse_help("uv", UV).expect("uv help is a spec");
        assert_eq!(spec.name, "uv");
        let subs = sub_names(&spec);
        assert!(subs.contains(&"run"), "{subs:?}");
        assert!(subs.contains(&"add"), "{subs:?}");
        assert!(subs.contains(&"venv"), "{subs:?}");
        assert!(
            subs.len() >= 15,
            "expected uv's full command list: {subs:?}"
        );

        let run = spec
            .subcommands
            .iter()
            .find(|c| c.name == "run")
            .expect("run");
        assert_eq!(
            run.description.as_deref(),
            Some("Run a command or script"),
            "description comes from the same line"
        );

        // Description on the following line, not the flag's own.
        let offline = find_opt(&spec, "--offline");
        assert_eq!(
            offline.description.as_deref(),
            Some("Disable network access [env: UV_OFFLINE=]")
        );
        assert!(offline.args.is_empty(), "--offline takes no argument");

        // `--directory <DIRECTORY>` — the placeholder is an argument.
        let dir = find_opt(&spec, "--directory");
        assert_eq!(dir.args.len(), 1, "{:?}", dir.args);
        assert_eq!(dir.args[0].name.as_deref(), Some("DIRECTORY"));

        // Short and long names collapse into one option.
        let help = find_opt(&spec, "--help");
        assert_eq!(help.names, vec!["-h".to_string(), "--help".to_string()]);
    }

    /// Several option sections under different headings, all of which
    /// must land in one flat option list.
    #[test]
    fn parses_multiple_flag_sections() {
        let spec = parse_help("aicommit2", AICOMMIT2).expect("aicommit2 help is a spec");
        let subs = sub_names(&spec);
        for expected in [
            "config",
            "doctor",
            "github-login",
            "hook",
            "log",
            "rewrite",
            "setup",
            "stats",
        ] {
            assert!(subs.contains(&expected), "missing {expected} in {subs:?}");
        }

        let opts = opt_names(&spec);
        // Message Options, Behavior, VCS Selection, Hook Integration,
        // Formatting, Debug, Other — every section contributes.
        for expected in [
            "--generate",
            "--clipboard",
            "--jj",
            "--pre-commit",
            "--exclude",
            "--verbose",
            "--version",
        ] {
            assert!(opts.contains(&expected), "missing {expected} in {opts:?}");
        }

        let generate = find_opt(&spec, "--generate");
        assert_eq!(
            generate.names,
            vec!["-g".to_string(), "--generate".to_string()]
        );
        assert_eq!(generate.args.len(), 1);
        assert_eq!(generate.args[0].name.as_deref(), Some("number"));
    }

    /// Cobra-ish layout: uppercase headings with no colon, and command
    /// names that carry a trailing colon.
    #[test]
    fn parses_uppercase_headings_and_strips_name_colons() {
        let spec = parse_help("gh", GH).expect("gh help is a spec");
        let subs = sub_names(&spec);
        for expected in ["auth", "pr", "repo", "run", "alias", "api"] {
            assert!(subs.contains(&expected), "missing {expected} in {subs:?}");
        }
        assert!(
            !subs.iter().any(|n| n.ends_with(':')),
            "trailing colons must be stripped: {subs:?}"
        );
        let auth = spec
            .subcommands
            .iter()
            .find(|c| c.name == "auth")
            .expect("auth");
        assert_eq!(
            auth.description.as_deref(),
            Some("Authenticate gh and git with GitHub")
        );
        let opts = opt_names(&spec);
        assert!(opts.contains(&"--version"), "{opts:?}");
    }

    /// A hand-rolled help text: an argument separated from its
    /// description by a single space, and wrapped continuation lines.
    #[test]
    fn parses_hand_rolled_layout_with_args_and_continuations() {
        let spec = parse_help("zeph", ZEPH).expect("zeph help is a spec");
        let subs = sub_names(&spec);
        for expected in ["install", "login", "notify", "ask", "dismiss", "cc"] {
            assert!(subs.contains(&expected), "missing {expected} in {subs:?}");
        }

        // `dismiss <id>` — the placeholder is the subcommand's argument,
        // never part of its name.
        let dismiss = spec
            .subcommands
            .iter()
            .find(|c| c.name == "dismiss")
            .expect("dismiss");
        assert_eq!(dismiss.args.len(), 1, "{:?}", dismiss.args);
        assert_eq!(dismiss.args[0].name.as_deref(), Some("id"));
        assert!(!dismiss.args[0].is_optional, "<id> is required");

        // `cc [args…]` — one space before the description, optional arg.
        let cc = spec
            .subcommands
            .iter()
            .find(|c| c.name == "cc")
            .expect("cc");
        assert_eq!(cc.args.len(), 1, "{:?}", cc.args);
        assert!(cc.args[0].is_optional, "[args…] is optional");
        assert!(
            cc.description.as_deref().unwrap_or("").starts_with("Run "),
            "{:?}",
            cc.description
        );
    }

    /// Wrapped description lines sit at the same indent as entries in
    /// some hand-written help texts, so indentation alone cannot tell
    /// them apart. Prose must not become subcommands.
    #[test]
    fn wrapped_prose_is_not_mistaken_for_subcommands() {
        let spec = parse_help("zeph", ZEPH).expect("spec");
        let subs = sub_names(&spec);
        for junk in [
            "Prints", "there", "the", "never", "configs", "tmux", "after", "phone", "show",
        ] {
            assert!(
                !subs.contains(&junk),
                "wrapped description word {junk:?} leaked into {subs:?}"
            );
        }
        // The real list is 21 commands; anything much larger means
        // sentences are being swallowed again.
        assert!(subs.len() <= 22, "{} entries: {subs:?}", subs.len());

        // The continuation text still lands on the entry it belongs to.
        let ask = spec
            .subcommands
            .iter()
            .find(|c| c.name == "ask")
            .expect("ask");
        assert!(
            ask.description.as_deref().unwrap_or("").contains("--title"),
            "{:?}",
            ask.description
        );
    }

    /// clap writes a repeatable flag as `--verbose...`; the dots are
    /// notation, not part of the name.
    #[test]
    fn repeatable_flag_markers_are_stripped() {
        let spec = parse_help("uv", UV).expect("spec");
        let quiet = find_opt(&spec, "--quiet");
        assert_eq!(quiet.names, vec!["-q".to_string(), "--quiet".to_string()]);
        assert!(quiet.is_repeatable);
        assert_eq!(quiet.description.as_deref(), Some("Use quiet output"));
    }

    /// Descriptions are capped so the popup footer stays one line.
    #[test]
    fn descriptions_are_truncated() {
        let spec = parse_help("zeph", ZEPH).expect("spec");
        for c in &spec.subcommands {
            if let Some(d) = &c.description {
                assert!(d.chars().count() <= MAX_DESCRIPTION, "{d:?}");
            }
        }
    }

    /// Below the structure bar there is no spec — a usage error, a
    /// version banner, or prose must not become one.
    #[test]
    fn too_little_structure_is_not_a_spec() {
        assert!(parse_help("x", "").is_none(), "empty");
        assert!(parse_help("x", "x 1.2.3\n").is_none(), "version banner");
        assert!(
            parse_help("x", "error: unknown flag --help\nTry 'x -h'.\n").is_none(),
            "usage error"
        );
        assert!(
            parse_help(
                "x",
                "Usage: x [options]\n\nOptions:\n  -v   Verbose\n  -q   Quiet\n"
            )
            .is_none(),
            "2 options is under MIN_OPTIONS and there are no subcommands"
        );
        assert!(
            parse_help(
                "x",
                "Usage: x [options]\n\nOptions:\n  -v   Verbose\n  -q   Quiet\n  -f   Force\n"
            )
            .is_some(),
            "3 options clears the bar"
        );
    }

    /// A name repeated across sections is kept once. gh lists `help`
    /// among commands and `--help` among flags; neither may duplicate.
    #[test]
    fn names_are_deduplicated() {
        let spec = parse_help("gh", GH).expect("spec");
        let mut subs = sub_names(&spec);
        let before = subs.len();
        subs.sort_unstable();
        subs.dedup();
        assert_eq!(subs.len(), before, "duplicate subcommand names");

        let mut opts = opt_names(&spec);
        let before = opts.len();
        opts.sort_unstable();
        opts.dedup();
        assert_eq!(opts.len(), before, "duplicate option names");
    }

    /// The output has to survive a round trip through the on-disk spec
    /// format, since that is how it reaches the registry.
    #[test]
    fn derived_spec_round_trips_through_json() {
        let spec = parse_help("uv", UV).expect("spec");
        let json = crate::spec_loader::write_spec_str(&spec).expect("serialize");
        let back =
            crate::spec_loader::parse_spec_str(&json, std::path::Path::new("derived/uv.json"))
                .expect("deserialize");
        assert_eq!(back.name, spec.name);
        assert_eq!(back.subcommands.len(), spec.subcommands.len());
        assert_eq!(back.options.len(), spec.options.len());
    }
}
