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
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long one help invocation may take. A CLI that has not printed
/// its own usage in a second is not going to.
const HELP_TIMEOUT: Duration = Duration::from_millis(1000);

/// Cap on captured output. Real help texts are a few KB; the cap is
/// there so a command that streams instead of printing usage cannot
/// grow the daemon's memory.
const MAX_HELP_BYTES: usize = 256 * 1024;

/// Tried in order until one produces text that parses. `--help` first
/// because it is near-universal; `help` last because on a command that
/// does not know it, it can look like a real subcommand invocation.
const HELP_ARGS: [&[&str]; 3] = [&["--help"], &["-h"], &["help"]];

/// Shell builtins and keywords. They have no binary to ask, and the
/// bundled set already covers the ones worth completing.
const NOT_DERIVABLE: &[&str] = &[
    "alias", "bg", "bind", "break", "builtin", "case", "cd", "command", "continue", "declare",
    "do", "done", "echo", "elif", "else", "esac", "eval", "exec", "exit", "export", "fc", "fg",
    "fi", "for", "function", "getopts", "hash", "if", "in", "jobs", "kill", "let", "local",
    "logout", "popd", "printf", "pushd", "pwd", "read", "readonly", "return", "select", "set",
    "shift", "source", "test", "then", "times", "trap", "type", "typeset", "ulimit", "umask",
    "unalias", "unset", "until", "wait", "while",
];

/// A derived spec must have at least one subcommand or this many
/// options, or it is not written. A usage error, a version banner, or a
/// paragraph of prose can all come back on stdout; none of them should
/// become a spec.
const MIN_OPTIONS: usize = 3;

/// Longest description kept. `--help` descriptions run to several
/// lines once continuations are joined, and the popup footer shows one
/// line.
const MAX_DESCRIPTION: usize = 80;

/// Parse `--help` (or `man`) output into a spec shaped like the ones
/// `build-specs` writes. `None` when the text carries too little
/// structure to be a spec — see [`MIN_OPTIONS`].
///
/// `name` is the command the text came from; it becomes `spec.name`.
pub fn parse_help(name: &str, text: &str) -> Option<Subcommand> {
    let mut spec = Subcommand {
        name: name.to_string(),
        ..Default::default()
    };
    let mut section = Section::None;
    // Column the current entry's name started at, while one is open. A
    // continuation is any line indented past it that does not itself
    // parse as an entry; it belongs to the last entry of the section's
    // list. `None` after a blank line or a heading.
    let mut open_at: Option<usize> = None;

    for raw in text.lines() {
        let line = strip_overstrike(raw);
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() {
            open_at = None;
            continue;
        }
        let indent = trimmed.len() - trimmed.trim_start().len();

        if indent == 0 {
            // A flush-left line is either a section heading or prose;
            // either way the previous section ends here.
            section = heading_kind(trimmed.trim()).unwrap_or(Section::None);
            open_at = None;
            continue;
        }

        // Indented headings exist too (`  Flags - Message Options:`).
        // Only treat one as a heading when it names a section, so an
        // ordinary item that happens to end in `:` is not swallowed.
        if let Some(kind) = heading_kind(trimmed.trim()) {
            section = kind;
            open_at = None;
            continue;
        }

        let pushed = match section {
            Section::None => continue,
            Section::Subcommands => {
                parse_subcommand_line(trimmed).map(|item| push_subcommand(&mut spec, item))
            }
            Section::Options => parse_option_line(trimmed).map(|item| push_option(&mut spec, item)),
        };
        match pushed {
            // A new entry opens; a duplicate closes whatever was open so
            // its continuation lines do not land on the earlier copy.
            Some(true) => open_at = Some(indent),
            Some(false) => open_at = None,
            // Not an entry: a wrapped line of the open one, if deeper.
            None => {
                if open_at.is_some_and(|col| indent > col) {
                    absorb_continuation(&mut spec, section, trimmed.trim());
                }
            }
        }
    }

    if spec.subcommands.is_empty() && spec.options.len() < MIN_OPTIONS {
        return None;
    }
    truncate_descriptions(&mut spec);
    Some(spec)
}

/// Whether `name` may be asked for its own help: a plain executable
/// stem ([`crate::paths::is_command_stem`]) that is not a shell builtin.
pub fn is_derivable(name: &str) -> bool {
    crate::paths::is_command_stem(name) && !NOT_DERIVABLE.contains(&name)
}

/// Derive a spec for `name` into `dir`, returning the file written (or
/// the still-current one already there). `None` when the command is not
/// on `PATH`, refuses to describe itself, or says too little to be a
/// spec.
///
/// Runs the command. That is safe only because of what it does *not*
/// do: no shell, no string-built command line, and a resolved absolute
/// path — see [`run_help`].
pub fn derive(name: &str, dir: &Path) -> Option<PathBuf> {
    if !is_derivable(name) {
        return None;
    }
    let bin = which::which(name).ok()?;
    derive_from_binary(name, &bin, dir)
}

/// The half of [`derive`] that takes an already-resolved binary, so
/// tests can point at a fixture executable without touching `PATH`.
pub fn derive_from_binary(name: &str, bin: &Path, dir: &Path) -> Option<PathBuf> {
    let out = dir.join(format!("{name}.json"));
    // A derived file older than the binary it came from is stale: the
    // tool was upgraded and its help may list new commands. Anything
    // else is reused as-is, so a command is asked for help once, not
    // once per keystroke.
    if is_current(&out, bin) {
        return Some(out);
    }
    let spec = help_spec(name, bin)?;
    let json = crate::spec_loader::write_spec_str(&spec).ok()?;
    crate::paths::write_atomic(&out, &json).ok()?;
    Some(out)
}

/// True when `out` exists and is at least as new as `bin`.
fn is_current(out: &Path, bin: &Path) -> bool {
    let (Ok(out_meta), Ok(bin_meta)) = (std::fs::metadata(out), std::fs::metadata(bin)) else {
        return false;
    };
    let (Ok(out_time), Ok(bin_time)) = (out_meta.modified(), bin_meta.modified()) else {
        return false;
    };
    out_time >= bin_time
}

/// First candidate whose output actually *parses* into a spec, trying
/// each help flag in turn and falling back to `man`.
///
/// Selecting on "looks like help" instead of "is a spec" is a trap:
/// `ls -h` prints a directory listing, which is long, multi-line, and
/// utterly unlike help — yet it would be accepted, and the `man ls`
/// fallback that does work would never run. Only the parser can tell.
fn help_spec(name: &str, bin: &Path) -> Option<Subcommand> {
    for args in HELP_ARGS {
        if let Some(text) = run_help(bin, args) {
            if let Some(spec) = parse_help(name, &text) {
                return Some(spec);
            }
        }
    }
    let text = man_text(bin)?;
    parse_help(name, &text)
}

/// Run `bin args…` and capture stdout+stderr.
///
/// Every property here is deliberate. No shell, so the user's aliases
/// and rc files cannot change what runs and there is no string to
/// inject into. An absolute path, already resolved, so nothing about
/// the child's environment can change *which* binary runs. An
/// otherwise cleared environment, so nothing leaks into the child and
/// colour is off. stdin at `/dev/null`, so a command that would prompt
/// exits instead of hanging. A temp cwd, so a tool that writes on
/// startup does it somewhere harmless.
///
/// `PATH` is the one variable inherited rather than fixed. Interpreted
/// tools start with `#!/usr/bin/env <interp>`, and a minimal `PATH`
/// cannot find an interpreter installed under Homebrew or a version
/// manager — `zeph --help` printed `env: node: No such file or
/// directory` and derived nothing. Inheriting costs no safety here,
/// because the binary is already resolved to an absolute path.
fn run_help(bin: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = sandboxed(bin);
    cmd.args(args)
        .env("HOME", std::env::temp_dir())
        .env("NO_COLOR", "1")
        .env("COLUMNS", "200")
        .stderr(std::process::Stdio::piped());
    capture(cmd)
}

/// The hardening every child gets — one place, so `man` and `--help`
/// cannot drift apart. Callers add only what their program needs on
/// top and choose what to do with stderr.
fn sandboxed(bin: &Path) -> std::process::Command {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(bin);
    cmd.env_clear()
        .env("PATH", inherited_path())
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    cmd
}

/// The daemon's own `PATH`, or a POSIX minimum when it has none.
fn inherited_path() -> std::ffi::OsString {
    std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin:/usr/sbin:/sbin".into())
}

/// `man <name>` as a last resort, for tools that predate `--help`.
/// The absolute path matters: many users alias `man` to something else
/// entirely, and an alias would not be a man page.
fn man_text(bin: &Path) -> Option<String> {
    let name = bin.file_name()?.to_str()?;
    let man = Path::new("/usr/bin/man");
    if !man.exists() {
        return None;
    }
    let mut cmd = sandboxed(man);
    cmd.arg(name)
        .env("MANWIDTH", "200")
        .env("MANPAGER", "cat")
        .env("PAGER", "cat")
        .stderr(std::process::Stdio::null());
    capture(cmd)
}

/// Spawn, read both pipes on their own threads (a child that fills one
/// pipe while the parent reads the other deadlocks), and give up after
/// [`HELP_TIMEOUT`].
fn capture(mut cmd: std::process::Command) -> Option<String> {
    use std::io::Read;
    use std::sync::mpsc;
    let mut child = cmd.spawn().ok()?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (tx, rx) = mpsc::channel::<(Vec<u8>, Vec<u8>)>();
    std::thread::spawn(move || {
        let mut o = Vec::new();
        let mut e = Vec::new();
        if let Some(s) = stdout {
            let _ = s.take(MAX_HELP_BYTES as u64).read_to_end(&mut o);
        }
        if let Some(s) = stderr {
            let _ = s.take(MAX_HELP_BYTES as u64).read_to_end(&mut e);
        }
        let _ = tx.send((o, e));
    });
    let (out, err) = match rx.recv_timeout(HELP_TIMEOUT) {
        Ok(pair) => {
            let _ = child.wait();
            pair
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    // Many CLIs print usage to stderr, and some split it across both.
    // Whichever carries more text is the help.
    let text = if out.len() >= err.len() { out } else { err };
    let text = String::from_utf8_lossy(&text).into_owned();
    Some(crate::complete::strip_ansi(&text))
}

#[derive(Clone, Copy)]
enum Section {
    None,
    Subcommands,
    Options,
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

/// Append a wrapped line to the last entry of the open section.
fn absorb_continuation(spec: &mut Subcommand, section: Section, text: &str) {
    let existing = match section {
        Section::Subcommands => spec.subcommands.last_mut().map(|c| &mut c.description),
        Section::Options => spec.options.last_mut().map(|o| &mut o.description),
        Section::None => None,
    };
    let Some(existing) = existing else {
        return;
    };
    let joined = match existing.as_deref() {
        Some(d) => format!("{d} {text}"),
        None => text.to_string(),
    };
    *existing = Some(std::sync::Arc::from(joined.as_str()));
}

/// Keep the first spelling of a repeated name — help texts list the
/// same command under several headings (gh's `help`, aliases). Returns
/// whether it was added.
fn push_subcommand(spec: &mut Subcommand, item: Subcommand) -> bool {
    if spec.subcommands.iter().any(|c| c.name == item.name) {
        return false;
    }
    spec.subcommands.push(item);
    true
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
