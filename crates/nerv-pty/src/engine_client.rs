//! Thin async client to the `nervd` completion daemon.
//!
//! PTY mode (Phase 3) reuses the exact same JSON-RPC-over-UDS contract
//! the ZLE widget / `nerv _complete` bridge uses: connect to
//! `paths::socket_path()`, send a [`Request::Complete`], read one line of
//! [`Response`]. We deliberately reuse `nerv_engine`'s wire types so the
//! daemon never has to distinguish a ZLE client from a figterm client.
//!
//! Unlike the desktop `Hostbound`/`Clientbound` remote path (which targets
//! a Wry webview that does not exist in Nerv), this talks straight to the
//! daemon that already serves the M0 widget.

use std::time::Duration;

use anyhow::{Result, anyhow};
use nerv_engine::{Request, Response, Suggestion};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Record an accepted suggestion for frecency ranking (fire-and-forget).
/// Mirrors what the M0 ZLE widget does via `nerv _record`: the daemon
/// bumps its in-memory table so the next request can float this pick to
/// the top. Errors are swallowed — a missing boost must never disrupt
/// the keystroke path.
pub async fn record_accept(spec: String, insertion: String) {
    let _ = record_inner(spec, insertion).await;
}

async fn record_inner(spec: String, insertion: String) -> Result<()> {
    let sock_path =
        nerv_engine::paths::socket_path().ok_or_else(|| anyhow!("HOME unset; no socket path"))?;
    let stream = UnixStream::connect(&sock_path).await?;
    let (read_half, mut write_half) = stream.into_split();

    let req = Request::RecordAccept { spec, insertion };
    let mut json = serde_json::to_string(&req)?;
    json.push('\n');
    write_half.write_all(json.as_bytes()).await?;

    // Drain the one-line ack so the daemon closes the conn cleanly.
    let mut reader = BufReader::new(read_half);
    let mut resp_line = String::new();
    let _ = reader.read_line(&mut resp_line).await;
    Ok(())
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
    match tokio::time::timeout(QUERY_TIMEOUT, complete_inner(line, cursor, cwd)).await {
        Ok(Ok(items)) => items,
        Ok(Err(_)) | Err(_) => Vec::new(),
    }
}

async fn complete_inner(line: &str, cursor: usize, cwd: Option<String>) -> Result<Vec<Suggestion>> {
    let sock_path =
        nerv_engine::paths::socket_path().ok_or_else(|| anyhow!("HOME unset; no socket path"))?;

    let stream = UnixStream::connect(&sock_path).await?;
    let (read_half, mut write_half) = stream.into_split();

    let req = Request::Complete {
        line: line.to_string(),
        cursor,
        cwd,
    };
    let mut json = serde_json::to_string(&req)?;
    json.push('\n');
    write_half.write_all(json.as_bytes()).await?;

    let mut reader = BufReader::new(read_half);
    let mut resp_line = String::new();
    reader.read_line(&mut resp_line).await?;

    match serde_json::from_str::<Response>(resp_line.trim())? {
        Response::Suggestions { items } => Ok(items),
        _ => Ok(Vec::new()),
    }
}
