//! The widget's row format, and the text protocol that carries it over the
//! daemon socket.
//!
//! One `_complete` reply is a list of lines: an optional history ghost row
//! (`\x1fghost\t<command>`), then one row per suggestion, and an exit code.
//! The CLI bridge prints the lines and exits with the code. The same lines
//! travel over the socket when zsh talks to the daemon directly
//! (`zsh/net/socket`, no process per keystroke), ended by
//! `\x1fend\t<seq>\t<code>`. Both paths render through [`complete_output`],
//! so the widget parses one format whichever way it asked.
//!
//! # Text requests
//!
//! A text request is one line of fields separated by `\x1f` (US), the verb
//! first. A field escapes `\` as `\\`, newline as `\n` and US as `\u`, so
//! it can carry any command. The daemon tells the two protocols apart by
//! the first field ([`is_text_request`]): a known verb is a text request,
//! anything else — JSON or garbage — takes the JSON path.
//!
//! | verb | fields | reply |
//! |---|---|---|
//! | `complete` | seq, line, cursor, cwd, prev, typed, flags | rows, then the end line |
//! | `predict` | seq, prev, cwd | ghost row if any, then the end line |
//! | `record` | command, expanded, cwd, exit, prev | none |
//!
//! `flags` holds `c` when the widget reads the shell-completion exit codes
//! (`--compsys`). `seq` is echoed on the end line, so a reply that arrives
//! after the widget gave up on it is recognised and skipped.

use crate::{Request, Response, Suggestion};

/// Field separator of a text request.
pub const US: char = '\x1f';

/// Collapse tab/newline/CR to a space. The row format is tab-separated
/// with one suggestion per line, so a stray tab or newline in ANY field
/// (a generator that echoes `git remote -v`'s `origin\t<url>`, a spec
/// with a multi-line description, …) shifts every field after it and
/// tears the popup box. Sanitising every field at the wire boundary makes
/// the format robust no matter what a generator returns.
pub fn wire_field(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

/// The popup footer shows a single clamped line, so shipping a full
/// multi-hundred-char description (aws service blurbs run 500–2000 chars)
/// is pure IPC + zsh-scan overhead — with ~600 aws subcommands it turned
/// a `aws ` completion into a 376 KB response that the widget then
/// re-scanned on every keystroke. Collapse tabs/newlines (they'd corrupt
/// the tab-separated wire format) and cap the length; the widget clamps
/// to the box width anyway.
pub fn wire_desc(desc: &str) -> String {
    const MAX: usize = 200;
    let cleaned = wire_field(desc);
    if cleaned.chars().count() <= MAX {
        cleaned
    } else {
        let mut out: String = cleaned.chars().take(MAX).collect();
        out.push('…');
        out
    }
}

/// One `_complete` row: insertion \t display \t description \t icon \t
/// replace. Icon is empty when None (the widget renders it as a prefix
/// glyph). Replace is `start,end` — the character span of the line the
/// row rewrites (a corrected command word) — and empty for the usual
/// "replace the token under the cursor".
pub fn wire_line(s: &Suggestion) -> String {
    let insertion = wire_field(&s.insertion);
    let display = wire_field(&s.display);
    let desc = wire_desc(s.description.as_deref().unwrap_or(""));
    let icon = wire_field(s.icon.as_deref().unwrap_or(""));
    let replace = s
        .replace
        .map(|r| format!("{},{}", r.start, r.end))
        .unwrap_or_default();
    format!("{insertion}\t{display}\t{desc}\t{icon}\t{replace}")
}

/// The history ghost rides the row stream as its first line:
/// `\x1fghost\t<command>`, only for a widget that asked (it sets
/// `NERV_TYPED`, or talks over the socket). The unit separator never
/// starts a real row. An empty command means "the history has no match".
pub fn ghost_line(ghost: &str) -> String {
    format!("\x1fghost\t{}", wire_field(ghost))
}

/// The `_complete` exit code for a reply that printed its rows.
///
/// - 4: the typed token already names one of the candidates. The widget
///   keeps the rows and preselects "run the line" instead of the first
///   longer name.
/// - 5: no hand-written spec covers the command, so the widget may ask
///   the shell's own completion (`__nerv_compsys_capture`).
/// - 6: both.
///
/// 5 and 6 only with `--compsys`: a shell still running a widget from
/// before the flag reads any unknown non-zero code as "daemon down".
pub fn complete_exit_code(token_complete: bool, unspecced: bool, compsys: bool) -> i32 {
    match (token_complete, unspecced && compsys) {
        (false, false) => 0,
        (true, false) => 4,
        (false, true) => 5,
        (true, true) => 6,
    }
}

/// Exit code of a spec-schema mismatch (E5): the daemon is up but its
/// spec cache is the wrong version. The widget shows a one-line hint.
pub const EXIT_SCHEMA_MISMATCH: i32 = 3;

/// Exit code of a still-loading empty reply: the spec parse or `--help`
/// derivation lands on a later keystroke. The widget shows its grey
/// one-line loading hint. An older widget reads any unknown non-zero
/// code as "daemon down" (same caveat as [`EXIT_SCHEMA_MISMATCH`]) —
/// transient while a new daemon meets an old widget after upgrading.
pub const EXIT_SPEC_LOADING: i32 = 7;

/// A rendered `_complete` reply.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CompleteOutput {
    pub lines: Vec<String>,
    pub code: i32,
    /// What the CLI prints on stderr (the E5 reason).
    pub stderr: Option<String>,
}

/// Render a completion reply the way the widget reads it. `ghost`: the
/// caller understands the ghost row.
pub fn complete_output(resp: Response, compsys: bool, ghost: bool) -> CompleteOutput {
    let mut out = CompleteOutput::default();
    if ghost {
        if let Response::Suggestions { ghost: Some(g), .. }
        | Response::Empty { ghost: Some(g), .. } = &resp
        {
            out.lines.push(ghost_line(g));
        }
    }
    out.code = match resp {
        Response::Suggestions {
            items,
            token_complete,
            unspecced,
            ..
        } => {
            out.lines.extend(items.iter().map(wire_line));
            complete_exit_code(token_complete, unspecced, compsys)
        }
        Response::Empty {
            reason: Some(r), ..
        } if r.starts_with("spec schema mismatch") => {
            out.stderr = Some(format!("[nerv] {r}"));
            EXIT_SCHEMA_MISMATCH
        }
        // Still loading: no rows, but not a miss either. The code is
        // the widget's only side channel — the reason string itself
        // never crosses the wire, and no row field is added.
        // A command with no written spec keeps the shell's own completion
        // while its `--help` is read: that answer is ready now.
        Response::Empty {
            reason: Some(r),
            unspecced,
            ..
        } if crate::loading_kind(r.as_str()).is_some() && !(unspecced && compsys) => {
            EXIT_SPEC_LOADING
        }
        // No rows → no popup in zsh, unless the shell may answer.
        Response::Empty { unspecced, .. } => complete_exit_code(false, unspecced, compsys),
        _ => 0,
    };
    out
}

/// The last line of a socket reply.
pub fn end_line(seq: &str, code: i32) -> String {
    format!("\x1fend\t{}\t{code}", wire_field(seq))
}

/// Escape one text-request field (see the module docs).
pub fn escape_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            US => out.push_str("\\u"),
            c => out.push(c),
        }
    }
    out
}

/// Inverse of [`escape_field`]. An unknown escape keeps its character.
pub fn unescape_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('u') => out.push(US),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// A parsed text request.
#[derive(Debug, PartialEq, Eq)]
pub enum TextRequest {
    /// Reply with rows and an end line carrying `seq`.
    Complete {
        seq: String,
        request: Request,
        compsys: bool,
    },
    /// Reply with the ghost row, if any, and an end line.
    Predict { seq: String, request: Request },
    /// Fire-and-forget: no reply at all.
    Record(Request),
}

/// Whether `line` is a text request: its first field is a known verb.
/// Anything else — JSON, or garbage — stays on the JSON path, which
/// answers garbage with a JSON `Error`.
pub fn is_text_request(line: &str) -> bool {
    matches!(
        line.split(US).next(),
        Some("complete" | "predict" | "record")
    )
}

/// Parse one text request line (without its newline).
pub fn parse_text_request(line: &str) -> Result<TextRequest, String> {
    let fields: Vec<String> = line.split(US).map(unescape_field).collect();
    let field = |i: usize| fields.get(i).cloned().unwrap_or_default();
    let some = |s: String| (!s.is_empty()).then_some(s);
    match fields.first().map(String::as_str) {
        Some("complete") => {
            let cursor = field(3)
                .parse()
                .map_err(|e| format!("complete: bad cursor: {e}"))?;
            Ok(TextRequest::Complete {
                seq: field(1),
                request: Request::Complete {
                    line: field(2),
                    cursor,
                    cwd: some(field(4)),
                    prev: some(field(5)),
                    typed: some(field(6)),
                },
                compsys: field(7).contains('c'),
            })
        }
        Some("predict") => Ok(TextRequest::Predict {
            seq: field(1),
            request: Request::Predict {
                prev: field(2),
                cwd: field(3),
            },
        }),
        Some("record") => Ok(TextRequest::Record(Request::RecordCommand {
            command: field(1),
            expanded: field(2),
            cwd: field(3),
            exit: field(4)
                .parse()
                .map_err(|e| format!("record: bad exit: {e}"))?,
            prev: field(5),
        })),
        Some(other) => Err(format!("unknown text verb {other:?}")),
        None => Err("empty text request".to_string()),
    }
}

/// The reply to a text request the daemon could not serve — one that
/// did not parse, or one over the request size cap. It still echoes the
/// request's seq, read from the line's head, so the widget stops waiting
/// at once instead of running out its read bound. A `record` gets no
/// reply at all: nobody reads one, and an unread line would sit in front
/// of the next reply.
pub fn text_error_reply(head: &str) -> Vec<String> {
    let mut fields = head.split(US);
    if fields.next() == Some("record") {
        return Vec::new();
    }
    vec![end_line(&unescape_field(fields.next().unwrap_or("")), 1)]
}

/// Render the reply to a text request: the lines to write, ending with
/// the end line, or nothing for a fire-and-forget verb.
pub fn text_reply(req: &TextRequest, resp: Response) -> Vec<String> {
    match req {
        TextRequest::Complete { seq, compsys, .. } => {
            let out = complete_output(resp, *compsys, true);
            let mut lines = out.lines;
            lines.push(end_line(seq, out.code));
            lines
        }
        TextRequest::Predict { seq, .. } => {
            let mut lines = Vec::new();
            let code = match resp {
                Response::Empty { ghost, .. } => {
                    if let Some(g) = ghost.filter(|g| !g.is_empty()) {
                        lines.push(ghost_line(&g));
                    }
                    0
                }
                _ => 1,
            };
            lines.push(end_line(seq, code));
            lines
        }
        TextRequest::Record(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReplaceSpan;

    /// Exit codes are the widget's only side channel. Without `--compsys`
    /// an unspecced reply must look exactly as it did before the flag
    /// existed: a shell still running an older widget reads any other
    /// non-zero code as "daemon not running".
    #[test]
    fn complete_exit_code_reports_unspecced_only_when_asked() {
        assert_eq!(complete_exit_code(false, false, true), 0);
        assert_eq!(complete_exit_code(true, false, true), 4);
        assert_eq!(complete_exit_code(false, true, true), 5);
        assert_eq!(complete_exit_code(true, true, true), 6);
        assert_eq!(complete_exit_code(false, true, false), 0);
        assert_eq!(complete_exit_code(true, true, false), 4);
    }

    /// The widget splits rows on tabs, so the field order is a contract:
    /// a plain row keeps its first four fields byte-for-byte and gains an
    /// empty fifth; a correction carries its span there.
    #[test]
    fn wire_line_carries_the_replace_span_as_a_fifth_field() {
        let plain = Suggestion {
            insertion: "checkout".into(),
            display: "checkout".into(),
            description: Some("Switch branches".into()),
            ..Suggestion::default()
        };
        assert_eq!(wire_line(&plain), "checkout\tcheckout\tSwitch branches\t\t");
        let fix = Suggestion {
            insertion: "zeph".into(),
            display: "zeph".into(),
            description: Some("did you mean".into()),
            replace: Some(ReplaceSpan { start: 5, end: 9 }),
            ..Suggestion::default()
        };
        assert_eq!(wire_line(&fix), "zeph\tzeph\tdid you mean\t\t5,9");
    }

    #[test]
    fn ghost_line_is_one_marked_row() {
        assert_eq!(ghost_line("git status"), "\x1fghost\tgit status");
        assert_eq!(ghost_line(""), "\x1fghost\t");
        assert!(!ghost_line("a\tb\nc").contains('\n'));
    }

    #[test]
    fn fields_round_trip_any_text() {
        for s in [
            "plain",
            "a\\b",
            "multi\nline",
            "with\x1fus",
            "\\n literal",
            "",
            "한글",
        ] {
            let e = escape_field(s);
            assert!(!e.contains('\n') && !e.contains(US), "{e:?}");
            assert_eq!(unescape_field(&e), s);
        }
    }

    fn text(fields: &[&str]) -> String {
        fields
            .iter()
            .map(|f| escape_field(f))
            .collect::<Vec<_>>()
            .join("\x1f")
    }

    #[test]
    fn parses_the_three_verbs() {
        let req = parse_text_request(&text(&[
            "complete",
            "7",
            "git ch",
            "6",
            "/r",
            "git add .",
            "g ch",
            "c",
        ]))
        .unwrap();
        assert_eq!(
            req,
            TextRequest::Complete {
                seq: "7".into(),
                request: Request::Complete {
                    line: "git ch".into(),
                    cursor: 6,
                    cwd: Some("/r".into()),
                    prev: Some("git add .".into()),
                    typed: Some("g ch".into()),
                },
                compsys: true,
            }
        );
        let bare =
            parse_text_request(&text(&["complete", "1", "ls", "2", "", "", "", ""])).unwrap();
        assert!(matches!(
            bare,
            TextRequest::Complete {
                compsys: false,
                request: Request::Complete {
                    cwd: None,
                    prev: None,
                    typed: None,
                    ..
                },
                ..
            }
        ));
        let rec =
            parse_text_request(&text(&["record", "for f\nin x", "", "/r", "1", "ls"])).unwrap();
        assert_eq!(
            rec,
            TextRequest::Record(Request::RecordCommand {
                command: "for f\nin x".into(),
                expanded: String::new(),
                cwd: "/r".into(),
                exit: 1,
                prev: "ls".into(),
            })
        );
        assert!(matches!(
            parse_text_request(&text(&["predict", "3", "git add .", "/r"])).unwrap(),
            TextRequest::Predict { .. }
        ));
        assert!(parse_text_request("bogus").is_err());
        assert!(is_text_request("complete\x1f1"));
        assert!(!is_text_request("{\"method\":\"ping\"}"));
        assert!(!is_text_request("this is not json at all"));
        // A verb must be the whole first field.
        assert!(!is_text_request("completely wrong"));
        assert!(parse_text_request(&text(&["complete", "1", "ls", "x"])).is_err());
    }

    #[test]
    fn a_failed_text_request_still_ends_with_its_seq() {
        assert_eq!(
            text_error_reply(&text(&["complete", "7", "ls", "x"])),
            ["\x1fend\t7\t1".to_string()]
        );
        // Only the head of an oversized line is kept; the seq is in it.
        assert_eq!(
            text_error_reply("predict\x1f12\x1fgit add"),
            ["\x1fend\t12\t1".to_string()]
        );
        assert!(text_error_reply(&text(&["record", "ls", "", "/", "x"])).is_empty());
    }

    #[test]
    fn text_reply_ends_with_the_seq_and_the_exit_code() {
        let req =
            parse_text_request(&text(&["complete", "42", "git ", "4", "", "", "", "c"])).unwrap();
        let resp = Response::Suggestions {
            items: vec![Suggestion {
                insertion: "status".into(),
                display: "status".into(),
                ..Suggestion::default()
            }],
            token_complete: false,
            unspecced: true,
            ghost: Some("git status".into()),
        };
        assert_eq!(
            text_reply(&req, resp),
            [
                "\x1fghost\tgit status".to_string(),
                "status\tstatus\t\t\t".to_string(),
                "\x1fend\t42\t5".to_string(),
            ]
        );
        let rec = parse_text_request(&text(&["record", "ls", "", "/", "0", ""])).unwrap();
        assert!(text_reply(&rec, Response::empty("recorded")).is_empty());
    }

    #[test]
    fn complete_output_matches_the_cli_contract() {
        let e5 = complete_output(
            Response::Empty {
                reason: Some("spec schema mismatch — x".into()),
                unspecced: false,
                ghost: None,
            },
            true,
            true,
        );
        assert_eq!(e5.code, EXIT_SCHEMA_MISMATCH);
        assert!(e5.lines.is_empty());
        assert!(
            e5.stderr
                .unwrap()
                .starts_with("[nerv] spec schema mismatch")
        );
        // A widget that did not ask never sees the ghost row.
        let no_ghost = complete_output(
            Response::Empty {
                reason: None,
                unspecced: false,
                ghost: Some("git status".into()),
            },
            false,
            false,
        );
        assert!(no_ghost.lines.is_empty());
    }

    /// M2: a still-loading empty exits with the loading code so the
    /// widget can show its one-line hint; a settled miss stays 0.
    #[test]
    fn loading_empty_exits_with_the_loading_code() {
        let loading = complete_output(
            Response::Empty {
                reason: Some(format!("{}spec", crate::LOADING_REASON_PREFIX)),
                unspecced: false,
                ghost: None,
            },
            true,
            true,
        );
        assert_eq!(loading.code, EXIT_SPEC_LOADING);
        assert!(loading.lines.is_empty());
        assert!(loading.stderr.is_none());
        let settled = complete_output(
            Response::Empty {
                reason: Some(format!("{}aws", crate::NO_SPEC_REASON_PREFIX)),
                unspecced: false,
                ghost: None,
            },
            true,
            true,
        );
        assert_eq!(settled.code, 0);
        // No written spec, and the widget can ask the shell: it does,
        // rather than wait on `--help` with nothing on screen.
        let derived = |compsys| {
            complete_output(
                Response::Empty {
                    reason: Some(format!("{}derived", crate::LOADING_REASON_PREFIX)),
                    unspecced: true,
                    ghost: None,
                },
                compsys,
                true,
            )
            .code
        };
        assert_eq!(derived(true), 5);
        assert_eq!(derived(false), EXIT_SPEC_LOADING);
    }
}
