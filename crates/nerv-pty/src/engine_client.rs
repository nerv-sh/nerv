//! Thin async client to the `nervd` completion daemon for PTY mode.
//!
//! The wire framing lives in [`nerv_engine::ipc_client`] — shared with the
//! `nerv _complete` / `_record` CLI bridge so the daemon never has to
//! distinguish a ZLE client from a figterm client, and the protocol has a
//! single source of truth. This module only layers PTY-specific policy on
//! top: a hard timeout on the keystroke path and a fire-and-forget record.
//!
//! Unlike the desktop `Hostbound`/`Clientbound` remote path (which targets
//! a Wry webview that does not exist in Nerv), this talks straight to the
//! daemon that already serves the M0 widget.

use std::time::Duration;

use nerv_engine::ipc_client;
use nerv_engine::{Request, Response, Suggestion};

/// Record an accepted suggestion for frecency ranking (fire-and-forget).
/// Mirrors what the M0 ZLE widget does via `nerv _record`: the daemon
/// bumps its in-memory table so the next request can float this pick to
/// the top. Errors are swallowed — a missing boost must never disrupt
/// the keystroke path.
pub async fn record_accept(spec: String, insertion: String) {
    let _ = ipc_client::query(&Request::RecordAccept { spec, insertion }).await;
}

/// Hard ceiling on a single completion round-trip. The daemon's own p95
/// is well under a millisecond (see CLAUDE.md §3 latency bench); this is
/// a safety valve so a wedged daemon can't stall keystroke echo. On
/// timeout we yield no suggestions and the caller renders nothing.
const QUERY_TIMEOUT: Duration = Duration::from_millis(50);

/// Query the daemon for completions at `cursor` within `line`.
///
/// `line` is the prompt buffer up to the cursor (zsh `$LBUFFER`
/// equivalent) and `cursor` is the byte offset. `cwd` is the shell's
/// working directory so filesystem-aware generators resolve relative to
/// the user, not the daemon.
///
/// Returns an empty vector (not an error) for every "no suggestions"
/// outcome — daemon down, timeout, non-`Suggestions` response — so the
/// render layer has a single uniform "draw nothing" path.
pub async fn complete(line: &str, cursor: usize, cwd: Option<String>) -> Vec<Suggestion> {
    let req = Request::Complete {
        line: line.to_string(),
        cursor,
        cwd,
    };
    match tokio::time::timeout(QUERY_TIMEOUT, ipc_client::query(&req)).await {
        Ok(Ok(Response::Suggestions { items })) => items,
        _ => Vec::new(),
    }
}
