//! Protocol-level e2e: scenarios the M0-1 / fuzzy tests don't cover.
//!
//! Each test spawns a fresh `nervd` with the workspace fixture specs
//! and a temp UDS, then exercises one corner of the IPC contract:
//!   - malformed JSON → `Response::Error`
//!   - `RecordAccept` → subsequent `Complete` reorders by frecency
//!   - `DoctorAutorun` → stub `Empty { reason }`
//!   - multiple requests pipelined on one connection
//!   - unknown binary → `Empty` with a reason string
//!
//! Tests share a small spawn helper so each scenario stays focused.

use nerv_engine::{Request, Response, SuggestionKind};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

fn nervd_bin() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_BIN_EXE_nervd"));
    assert!(path.exists(), "nervd binary not found at {path:?}");
    path
}

fn fixture_specs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("nerv-engine")
        .join("tests")
        .join("fixtures")
        .join("specs")
}

/// Spawn `nervd` with a temp socket + PID + ephemeral frecency file
/// pointing at the workspace fixture specs. The caller is responsible
/// for killing the child via `kill_on_drop`.
struct DaemonHandle {
    child: tokio::process::Child,
    sock: PathBuf,
    _tmp: tempfile::TempDir,
    frecency: PathBuf,
}

impl DaemonHandle {
    async fn spawn(frecency_mode: FrecencyMode) -> Self {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let sock = tmp.path().join("nervd.sock");
        let pid = tmp.path().join("nervd.pid");
        let frecency = match frecency_mode {
            FrecencyMode::Disabled => PathBuf::from("-"),
            FrecencyMode::Tempfile => tmp.path().join("frecency.tsv"),
        };
        let specs = fixture_specs_dir();
        assert!(specs.exists(), "fixture specs dir missing");

        let child = tokio::process::Command::new(nervd_bin())
            .env("NERV_SOCK", &sock)
            .env("NERV_PID", &pid)
            .env("NERV_SPECS_DIR", &specs)
            .env("NERV_FRECENCY_FILE", &frecency)
            .env("NERV_LOG", "warn")
            .kill_on_drop(true)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn nervd");

        // Wait up to 3s for the socket to appear.
        for _ in 0..30 {
            if sock.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(sock.exists(), "nervd socket did not appear");

        Self {
            child,
            sock,
            _tmp: tmp,
            frecency,
        }
    }

    async fn connect(&self) -> UnixStream {
        UnixStream::connect(&self.sock)
            .await
            .expect("connect to nervd")
    }

    async fn shutdown(mut self) {
        self.child.kill().await.ok();
        self.child.wait().await.ok();
    }
}

#[derive(Copy, Clone)]
enum FrecencyMode {
    /// `NERV_FRECENCY_FILE=-` — no on-disk persistence, scores all zero.
    Disabled,
    /// Persist to a tempfile so RecordAccept side-effects are visible.
    Tempfile,
}

/// One-shot send + read on a fresh connection. Avoids reusing the
/// stream between scenarios so a wedged read can't poison the next
/// case.
async fn round_trip(stream: &mut UnixStream, req: &Request) -> Response {
    let (read_half, mut write_half) = stream.split();
    let mut json = serde_json::to_string(req).expect("encode request");
    json.push('\n');
    write_half.write_all(json.as_bytes()).await.expect("write");
    write_half.flush().await.expect("flush");
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    reader.read_line(&mut line).await.expect("read");
    serde_json::from_str(line.trim()).expect("decode response")
}

#[tokio::test]
async fn invalid_json_returns_error_response() {
    let daemon = DaemonHandle::spawn(FrecencyMode::Disabled).await;
    let run = tokio::time::timeout(Duration::from_secs(5), async {
        let stream = daemon.connect().await;
        let (read_half, mut write_half) = stream.into_split();
        write_half
            .write_all(b"this is not json at all\n")
            .await
            .expect("write garbage");
        write_half.flush().await.expect("flush");
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read");
        let resp: Response = serde_json::from_str(line.trim()).expect("decode");
        match resp {
            Response::Error { message } => {
                assert!(
                    message.contains("invalid request"),
                    "unexpected error message: {message}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    })
    .await;
    daemon.shutdown().await;
    run.expect("test timeout");
}

#[tokio::test]
async fn doctor_autorun_returns_empty_stub() {
    let daemon = DaemonHandle::spawn(FrecencyMode::Disabled).await;
    let run = tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = daemon.connect().await;
        let resp = round_trip(&mut stream, &Request::DoctorAutorun).await;
        match resp {
            Response::Empty { reason } => {
                assert_eq!(reason.as_deref(), Some("doctor-autorun-stub"));
            }
            other => panic!("expected Empty, got {other:?}"),
        }
    })
    .await;
    daemon.shutdown().await;
    run.expect("test timeout");
}

#[tokio::test]
async fn unknown_binary_returns_empty_with_reason() {
    let daemon = DaemonHandle::spawn(FrecencyMode::Disabled).await;
    let run = tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = daemon.connect().await;
        let resp = round_trip(
            &mut stream,
            &Request::Complete {
                line: "nosuchbin foo".into(),
                cursor: 13,
                cwd: None,
            },
        )
        .await;
        match resp {
            Response::Empty { reason } => {
                assert!(
                    reason.is_some(),
                    "expected non-empty reason for unknown binary"
                );
            }
            other => panic!("expected Empty, got {other:?}"),
        }
    })
    .await;
    daemon.shutdown().await;
    run.expect("test timeout");
}

#[tokio::test]
async fn pipelined_requests_share_one_connection() {
    let daemon = DaemonHandle::spawn(FrecencyMode::Disabled).await;
    let run = tokio::time::timeout(Duration::from_secs(5), async {
        let stream = daemon.connect().await;
        let (read_half, mut write_half) = stream.into_split();
        // Send Ping + Complete + Ping back-to-back. The daemon reads
        // line-by-line so order is preserved.
        for req in [
            Request::Ping,
            Request::Complete {
                line: "git ".into(),
                cursor: 4,
                cwd: None,
            },
            Request::Ping,
        ] {
            let mut json = serde_json::to_string(&req).unwrap();
            json.push('\n');
            write_half.write_all(json.as_bytes()).await.expect("write");
        }
        write_half.flush().await.expect("flush");

        let mut reader = BufReader::new(read_half);
        let mut buf = String::new();
        let mut responses = Vec::new();
        for _ in 0..3 {
            buf.clear();
            reader.read_line(&mut buf).await.expect("read");
            responses.push(serde_json::from_str::<Response>(buf.trim()).expect("decode"));
        }
        assert!(matches!(responses[0], Response::Pong { .. }));
        match &responses[1] {
            Response::Suggestions { items } => assert!(!items.is_empty()),
            other => panic!("expected Suggestions, got {other:?}"),
        }
        assert!(matches!(responses[2], Response::Pong { .. }));
    })
    .await;
    daemon.shutdown().await;
    run.expect("test timeout");
}

#[tokio::test]
async fn record_accept_boosts_subsequent_complete() {
    let daemon = DaemonHandle::spawn(FrecencyMode::Tempfile).await;
    let frecency_path = daemon.frecency.clone();
    let run = tokio::time::timeout(Duration::from_secs(6), async {
        // Baseline: `git ` returns alphabetical [checkout, commit, log,
        // status]. Hammer RecordAccept(`git`, `status`) a few times,
        // then re-issue Complete and verify `status` floats to first.
        let mut stream = daemon.connect().await;
        let baseline = round_trip(
            &mut stream,
            &Request::Complete {
                line: "git ".into(),
                cursor: 4,
                cwd: None,
            },
        )
        .await;
        match &baseline {
            Response::Suggestions { items } => {
                assert_eq!(
                    items.first().map(|s| s.insertion.as_str()),
                    Some("checkout"),
                    "baseline first should be alpha-sorted"
                );
            }
            other => panic!("expected Suggestions, got {other:?}"),
        }

        // The frecency boost only kicks in from the 2nd accept onward
        // (count-1 numerator in score = (count-1)/(1+age_days)).
        for _ in 0..3 {
            let resp = round_trip(
                &mut stream,
                &Request::RecordAccept {
                    spec: "git".into(),
                    insertion: "status".into(),
                },
            )
            .await;
            match resp {
                Response::Empty { reason } => {
                    assert_eq!(reason.as_deref(), Some("recorded"));
                }
                other => panic!("expected Empty(recorded), got {other:?}"),
            }
        }

        let boosted = round_trip(
            &mut stream,
            &Request::Complete {
                line: "git ".into(),
                cursor: 4,
                cwd: None,
            },
        )
        .await;
        match boosted {
            Response::Suggestions { items } => {
                assert_eq!(
                    items.first().map(|s| s.insertion.as_str()),
                    Some("status"),
                    "status should be boosted by frecency"
                );
                // Subcommands stay subcommands — frecency just reorders.
                assert!(items.iter().all(|s| s.kind == SuggestionKind::Subcommand));
            }
            other => panic!("expected Suggestions, got {other:?}"),
        }

        // Read the persisted frecency file BEFORE the daemon handle
        // drops — the tempdir cleanup that runs on drop removes the
        // file underneath us otherwise.
        assert!(
            frecency_path.exists(),
            "frecency.tsv missing after RecordAccept (path: {})",
            frecency_path.display(),
        );
        let body = std::fs::read_to_string(&frecency_path).expect("read frecency");
        assert!(
            body.contains("git\tstatus"),
            "frecency entry not persisted: {body:?}"
        );
    })
    .await;
    daemon.shutdown().await;
    run.expect("test timeout");
}
