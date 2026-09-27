//! IPC message types between ZLE widget and `nervd`.
//!
//! Wire format: JSON-RPC over Unix domain socket. See PLAN.md §6 architecture.

use serde::{Deserialize, Serialize};

/// Request from ZLE widget to daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Request {
    /// Ask for completion suggestions at the given cursor position.
    Complete {
        /// The full left buffer (`$LBUFFER` in zsh) up to the cursor.
        line: String,
        /// Byte offset of the cursor in `line`.
        cursor: usize,
        /// Client's working directory at request time. Used by
        /// filesystem-aware generators (e.g. `package.json` script
        /// discovery). Optional for backwards-compat with older
        /// widget builds; daemon falls back to its own cwd when
        /// absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// The command run before this one in the same shell — the
        /// sequence signal of the history ghost.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prev: Option<String>,
        /// `line` as the user typed it, when the widget sent an
        /// alias-expanded `line`. History holds typed lines, so the ghost
        /// matches against this.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        typed: Option<String>,
    },
    /// Health check (used by `nerv doctor`).
    Ping,
    /// Trigger automatic doctor self-check (see error-states.md §3.6).
    DoctorAutorun,
    /// Record a user-accepted suggestion for frecency ranking.
    /// Fire-and-forget: daemon updates its in-memory table and
    /// flushes opportunistically; response is `Empty`.
    RecordAccept {
        /// Top-level binary name (e.g. `git`, `cd`).
        spec: String,
        /// The insertion string the user committed.
        insertion: String,
    },
    /// Register a zsh session's shell function·alias names so they
    /// surface as first-token completion candidates (plan slice 02).
    /// Fire-and-forget: the daemon keeps them memory-only and replies
    /// `Empty`. The names are dotfile content — the daemon never
    /// writes them anywhere.
    RegisterShellNames {
        /// User-visible function·alias names (internals already
        /// filtered out by the sender).
        names: Vec<String>,
    },
    /// Record one command the user ran (zsh `preexec` → `precmd`), for
    /// history-ranked suggestions. The widget has already dropped what
    /// zsh itself would not remember (`hist_ignore_space`,
    /// `HISTORY_IGNORE`). Fire-and-forget; the reply is `Empty`.
    RecordCommand {
        /// The line as typed — what a ghost offers back.
        command: String,
        /// zsh's alias-expanded form; empty when identical.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        expanded: String,
        cwd: String,
        exit: i32,
        /// The command run before this one in the same shell; empty for
        /// the first, or when the one before was ignored.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        prev: String,
    },
    /// Seed an empty command history from a zsh history file (`$HISTFILE`).
    /// A no-op once the history holds anything.
    ImportHistory { path: String },
    /// The command to offer on an empty prompt, from what usually follows
    /// `prev`. Replies `Empty`; `ghost` is absent when there is no prediction.
    Predict {
        prev: String,
        #[serde(default)]
        cwd: String,
    },
}

/// Response from daemon to ZLE widget.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    /// Normal completion result.
    Suggestions {
        items: Vec<Suggestion>,
        /// The token under the cursor already names a candidate; the
        /// rows only extend it. `#[serde(default)]` keeps a reply from an
        /// older daemon (no field) decodable as "not complete".
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        token_complete: bool,
        /// No hand-written spec covers the command (see
        /// `CompleteResult::unspecced`). Absent from an older daemon's
        /// reply, which decodes as "a written spec answered".
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        unspecced: bool,
        /// The history command to offer as the inline ghost — the whole
        /// command, not the remainder. `Some("")` = the history was
        /// consulted and has nothing; `None` = no history to consult (an
        /// older daemon, or an empty history), so the widget falls back
        /// to zsh's own `$history`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ghost: Option<String>,
    },
    /// Daemon is alive (Ping reply). `pid` is the daemon's process id, so a
    /// client (`nerv stop` / `uninstall`) can signal it even when the PID
    /// file is missing. `#[serde(default)]` keeps replies from an older
    /// daemon (no `pid`) decodable — they deserialize with `pid == 0`.
    Pong {
        version: String,
        #[serde(default)]
        pid: u32,
    },
    /// Empty result — typically because the spec is disabled (E2).
    Empty {
        reason: Option<String>,
        /// Same as on `Suggestions`: an empty reply for a command with no
        /// written spec still lets the widget ask the shell.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        unspecced: bool,
        /// Same as on `Suggestions`: no rows, but maybe a history ghost.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ghost: Option<String>,
    },
    /// An error condition the widget should surface (rare; usually
    /// daemon stays silent and just returns Empty).
    Error { message: String },
}

impl Response {
    /// `Empty` with a reason and nothing else — the acknowledgement of
    /// every fire-and-forget request.
    pub fn empty(reason: impl Into<String>) -> Self {
        Response::Empty {
            reason: Some(reason.into()),
            unspecced: false,
            ghost: None,
        }
    }
}

/// A single completion suggestion.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Suggestion {
    /// What gets inserted on Tab.
    pub insertion: String,
    /// Human-readable label (often == insertion).
    pub display: String,
    /// One-line description shown when the user presses `?`.
    pub description: Option<String>,
    /// Subcommand / flag / argument.
    pub kind: SuggestionKind,
    /// Fig parity sort hint. Higher → earlier in the popup.
    /// Default 50 (Fig convention). Falls back to alpha when equal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u32>,
    /// Fig parity icon glyph. Single grapheme or short text only.
    /// `fig://icon?type=...` URLs from upstream specs are stripped
    /// (they reference Fig's icon registry which is irrelevant in
    /// the terminal). The widget renders this verbatim as a prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// The generator already ordered this row by its own usage history
    /// (zoxide's frecency score), so the daemon must not re-rank it with
    /// nerv's frecency — that double-counts visits and lets a frecent
    /// path-only hit bury an exact folder-name match (`z tak-bro`).
    /// In-process only: never crosses the socket.
    #[serde(skip)]
    pub source_ranked: bool,
    /// Span of the line this row replaces, when it is not the token under
    /// the cursor. Set only by the command-name correction offered after a
    /// space (`zpeh li` → `zeph`), which rewrites the command word and
    /// leaves the arguments alone. `None` = replace the current token, the
    /// behavior every other row has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace: Option<ReplaceSpan>,
}

/// Half-open range of the request `line`, in **characters** (not bytes):
/// the zsh widget indexes `$BUFFER` by character, so a byte offset would
/// land mid-glyph after any multibyte text before the command word.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplaceSpan {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionKind {
    #[default]
    Subcommand,
    Flag,
    Argument,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `unspecced` flag of a completion reply; false for any other
    /// variant.
    fn unspecced(resp: &Response) -> bool {
        match resp {
            Response::Suggestions { unspecced, .. } | Response::Empty { unspecced, .. } => {
                *unspecced
            }
            _ => false,
        }
    }

    /// `RegisterShellNames` (plan slice 02): one zsh session's
    /// function·alias names ride a single request. The wire tag follows
    /// the enum's snake_case convention.
    #[test]
    fn register_shell_names_roundtrips() {
        let req = Request::RegisterShellNames {
            names: vec!["p10k".into(), "g".into()],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(
            json,
            r#"{"method":"register_shell_names","names":["p10k","g"]}"#
        );
        let back: Request = serde_json::from_str(&json).unwrap();
        match back {
            Request::RegisterShellNames { names } => assert_eq!(names.len(), 2),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    /// `ghost` keeps three states apart on the wire: absent (no history —
    /// the widget uses `$history`), `""` (ranked, no match), a command.
    #[test]
    fn ghost_distinguishes_absent_from_empty() {
        let cases: [(Option<&str>, Option<&str>); 3] = [
            (None, None),
            (Some(""), Some(r#""ghost":"""#)),
            (Some("git status"), Some(r#""ghost":"git status""#)),
        ];
        for (ghost, wire) in cases {
            let resp = Response::Empty {
                reason: None,
                unspecced: false,
                ghost: ghost.map(String::from),
            };
            let json = serde_json::to_string(&resp).unwrap();
            match wire {
                Some(w) => assert!(json.contains(w), "{json}"),
                None => assert!(!json.contains("ghost"), "{json}"),
            }
            match serde_json::from_str::<Response>(&json).unwrap() {
                Response::Empty { ghost: back, .. } => assert_eq!(back.as_deref(), ghost),
                other => panic!("{other:?}"),
            }
        }
    }

    /// `Predict` decodes without a cwd (it defaults to empty).
    #[test]
    fn predict_request_decodes_with_and_without_cwd() {
        for (json, cwd) in [
            (
                r#"{"method":"predict","prev":"git add .","cwd":"/r"}"#,
                "/r",
            ),
            (r#"{"method":"predict","prev":"git add ."}"#, ""),
        ] {
            match serde_json::from_str::<Request>(json).unwrap() {
                Request::Predict { prev, cwd: got } => {
                    assert_eq!(prev, "git add .");
                    assert_eq!(got, cwd);
                }
                other => panic!("{other:?}"),
            }
        }
    }

    /// History requests: the optional fields default when absent, so a
    /// widget that omits them (first command, no alias) still decodes.
    #[test]
    fn history_requests_decode_with_and_without_optional_fields() {
        let full = r#"{"method":"record_command","command":"g st","expanded":"git status","cwd":"/r","exit":1,"prev":"ls"}"#;
        match serde_json::from_str::<Request>(full).unwrap() {
            Request::RecordCommand {
                command,
                expanded,
                cwd,
                exit,
                prev,
            } => {
                assert_eq!(
                    (
                        command.as_str(),
                        expanded.as_str(),
                        cwd.as_str(),
                        exit,
                        prev.as_str()
                    ),
                    ("g st", "git status", "/r", 1, "ls")
                );
            }
            other => panic!("{other:?}"),
        }
        let bare = r#"{"method":"record_command","command":"ls","cwd":"/","exit":0}"#;
        match serde_json::from_str::<Request>(bare).unwrap() {
            Request::RecordCommand { expanded, prev, .. } => {
                assert!(expanded.is_empty() && prev.is_empty());
            }
            other => panic!("{other:?}"),
        }
        // Empty optionals are not put on the wire.
        let json = serde_json::to_string(&Request::RecordCommand {
            command: "ls".into(),
            expanded: String::new(),
            cwd: "/".into(),
            exit: 0,
            prev: String::new(),
        })
        .unwrap();
        assert!(
            !json.contains("expanded") && !json.contains("prev"),
            "{json}"
        );
        let import = r#"{"method":"import_history","path":"/h"}"#;
        assert!(matches!(
            serde_json::from_str::<Request>(import).unwrap(),
            Request::ImportHistory { path } if path == "/h"
        ));
    }

    /// A daemon from before `unspecced` sends no such key; both variants
    /// must still decode, as "a written spec answered".
    #[test]
    fn responses_without_unspecced_still_decode() {
        for old in [
            r#"{"kind":"suggestions","items":[]}"#,
            r#"{"kind":"empty","reason":null}"#,
        ] {
            let resp: Response = serde_json::from_str(old).unwrap();
            assert!(!unspecced(&resp), "{old}");
        }
        for resp in [
            Response::Suggestions {
                items: vec![],
                token_complete: false,
                unspecced: true,
                ghost: None,
            },
            Response::Empty {
                reason: None,
                unspecced: true,
                ghost: None,
            },
        ] {
            let json = serde_json::to_string(&resp).unwrap();
            assert!(
                unspecced(&serde_json::from_str::<Response>(&json).unwrap()),
                "{json}"
            );
        }
    }

    /// A daemon from before `token_complete` sends no such key; the new
    /// client must still read its rows.
    #[test]
    fn suggestions_without_token_complete_still_decode() {
        let old = r#"{"kind":"suggestions","items":[]}"#;
        let resp: Response = serde_json::from_str(old).unwrap();
        assert!(matches!(
            resp,
            Response::Suggestions {
                token_complete: false,
                ..
            }
        ));
        let done = Response::Suggestions {
            items: vec![],
            token_complete: true,
            unspecced: false,
            ghost: None,
        };
        let json = serde_json::to_string(&done).unwrap();
        assert!(matches!(
            serde_json::from_str::<Response>(&json).unwrap(),
            Response::Suggestions {
                token_complete: true,
                ..
            }
        ));
    }

    #[test]
    fn suggestion_roundtrip_minimal() {
        let s = Suggestion {
            insertion: "git".into(),
            display: "git".into(),
            kind: SuggestionKind::Subcommand,
            ..Default::default()
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Suggestion = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
        // priority + icon use skip_serializing_if; description does
        // not (kept as `null` for wire stability).
        assert!(!json.contains("\"priority\""));
        assert!(!json.contains("\"icon\""));
    }

    #[test]
    fn suggestion_roundtrip_full() {
        let s = Suggestion {
            insertion: "git".into(),
            display: "git".into(),
            description: Some("VCS".into()),
            kind: SuggestionKind::Subcommand,
            priority: Some(75),
            icon: Some("📦".into()),
            source_ranked: false,
            replace: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Suggestion = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
        assert!(json.contains("\"priority\":75"));
        assert!(json.contains("\"icon\":\"📦\""));
    }

    #[test]
    fn suggestion_kind_serializes_snake_case() {
        for (kind, expected) in [
            (SuggestionKind::Subcommand, "\"subcommand\""),
            (SuggestionKind::Flag, "\"flag\""),
            (SuggestionKind::Argument, "\"argument\""),
        ] {
            let j = serde_json::to_string(&kind).unwrap();
            assert_eq!(j, expected);
        }
    }

    #[test]
    fn request_complete_with_cwd() {
        let r = Request::Complete {
            line: "git ".into(),
            cursor: 4,
            cwd: Some("/tmp".into()),
            prev: None,
            typed: None,
        };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"method\":\"complete\""));
        assert!(j.contains("\"cwd\":\"/tmp\""));
        let back: Request = serde_json::from_str(&j).unwrap();
        match back {
            Request::Complete { cursor, cwd, .. } => {
                assert_eq!(cursor, 4);
                assert_eq!(cwd.as_deref(), Some("/tmp"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn request_complete_omits_cwd_when_none() {
        let r = Request::Complete {
            line: "git ".into(),
            cursor: 4,
            cwd: None,
            prev: None,
            typed: None,
        };
        let j = serde_json::to_string(&r).unwrap();
        assert!(!j.contains("cwd"), "cwd should be omitted when None: {j}");
    }
}
