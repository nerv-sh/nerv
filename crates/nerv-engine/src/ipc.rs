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
    },
    /// Health check (used by `nerv doctor`).
    Ping,
    /// Trigger automatic doctor self-check (see error-states.md §3.6).
    DoctorAutorun,
}

/// Response from daemon to ZLE widget.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    /// Normal completion result.
    Suggestions { items: Vec<Suggestion> },
    /// Dynamic generator hint — see PLAN.md §5.1 & first-5-min.md.
    DynamicHint {
        /// Short reason (e.g., "dynamic-branch-list").
        reason: String,
        /// Recommended shell command to discover candidates manually.
        hint_command: String,
        /// URL for filing a "want this dynamic" feedback issue.
        feedback_url: String,
    },
    /// Daemon is alive (Ping reply).
    Pong { version: String },
    /// Empty result — typically because the spec is disabled (E2).
    Empty { reason: Option<String> },
    /// An error condition the widget should surface (rare; usually
    /// daemon stays silent and just returns Empty).
    Error { message: String },
}

/// A single completion suggestion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Suggestion {
    /// What gets inserted on Tab.
    pub insertion: String,
    /// Human-readable label (often == insertion).
    pub display: String,
    /// One-line description shown when the user presses `?`.
    pub description: Option<String>,
    /// Subcommand / flag / argument.
    pub kind: SuggestionKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionKind {
    Subcommand,
    Flag,
    Argument,
}
