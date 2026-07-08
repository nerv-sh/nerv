//! IPC message types between ZLE widget and `nervd`.
//!
//! Wire format: JSON-RPC over Unix domain socket. See PLAN.md §6 architecture.

use serde::{Deserialize, Serialize};

/// Request from ZLE widget to daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

/// Response from daemon to ZLE widget.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    /// Normal completion result.
    Suggestions { items: Vec<Suggestion> },
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
    Empty { reason: Option<String> },
    /// An error condition the widget should surface (rare; usually
    /// daemon stays silent and just returns Empty).
    Error { message: String },
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
        };
        let j = serde_json::to_string(&r).unwrap();
        assert!(!j.contains("cwd"), "cwd should be omitted when None: {j}");
    }
}
