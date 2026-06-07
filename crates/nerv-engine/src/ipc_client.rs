//! Async client for the `nervd` UDS protocol — the single source of
//! truth for how a client talks to the daemon.
//!
//! Both the `nerv _complete` / `_record` CLI bridge and the PTY shim
//! (`nerv-pty`) speak the same one-shot JSON-RPC-over-UDS contract:
//! connect to [`paths::socket_path`], write one [`Request`] as a single
//! `\n`-terminated JSON line, read one [`Response`] line back. Keeping
//! the framing here means the wire format (a CLAUDE.md §4 invariant)
//! can't drift between the two callers.
//!
//! Callers layer their own policy on top: the PTY ghost path wraps
//! [`query`] in a 50 ms timeout, the CLI bridge runs it on a
//! current-thread runtime. Fire-and-forget callers (RecordAccept) just
//! ignore the returned [`Response::Empty`].

use crate::paths;
use crate::{Request, Response};
use anyhow::{Result, anyhow};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Send one `req` to the daemon and return its single-line response.
///
/// Errors on a missing socket path, a connection failure, or an
/// unparseable reply — callers decide whether that means "draw nothing"
/// or propagate.
pub async fn query(req: &Request) -> Result<Response> {
    let sock_path = paths::socket_path().ok_or_else(|| anyhow!("HOME unset; no socket path"))?;
    let stream = UnixStream::connect(&sock_path).await?;
    let (read_half, mut write_half) = stream.into_split();

    let mut json = serde_json::to_string(req)?;
    json.push('\n');
    write_half.write_all(json.as_bytes()).await?;

    let mut reader = BufReader::new(read_half);
    let mut resp_line = String::new();
    reader.read_line(&mut resp_line).await?;
    Ok(serde_json::from_str(resp_line.trim())?)
}
