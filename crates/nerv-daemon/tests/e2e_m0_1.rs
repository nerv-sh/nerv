//! M0-6 e2e: verify the daemon returns real-engine suggestions over UDS.
//!
//! This test spawns `nervd` as a child process with a temp socket and
//! the workspace `tests/fixtures/specs/` dir as the spec source, sends
//! a Complete request, and asserts the engine pipeline drives the
//! response (replaces M0-1 hardcoded stub fixtures).

use nerv_engine::{Request, Response};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Build the nervd binary path from the cargo target directory.
fn nervd_bin() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_BIN_EXE_nervd"));
    assert!(path.exists(), "nervd binary not found at {path:?}");
    path
}

/// Path to the nerv-engine workspace fixtures dir, used as the
/// daemon's spec source so the test doesn't touch the real
/// `~/Library/Caches/nerv/specs/`.
fn fixture_specs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("nerv-engine")
        .join("tests")
        .join("fixtures")
        .join("specs")
}

#[tokio::test]
async fn complete_returns_real_engine_suggestions() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let sock_path = tmp.path().join("nervd.sock");
    let pid_path = tmp.path().join("nervd.pid");
    let specs_dir = fixture_specs_dir();
    assert!(
        specs_dir.exists(),
        "fixture specs dir missing: {}",
        specs_dir.display()
    );

    // Start nervd with temp socket pointing at fixture specs.
    let mut child = tokio::process::Command::new(nervd_bin())
        .env("NERV_SOCK", &sock_path)
        .env("NERV_PID", &pid_path)
        .env("NERV_SPECS_DIR", &specs_dir)
        // Suppress the user's real frecency history so test ordering
        // is deterministic regardless of local accept counts.
        .env("NERV_FRECENCY_FILE", "-")
        .env("NERV_LOG", "debug")
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn nervd");

    // Wait for socket to appear (up to 3s).
    for _ in 0..30 {
        if sock_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(sock_path.exists(), "nervd socket did not appear");

    // Wrap the actual test logic in a block so child is always killed.
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        // Connect and send Complete request.
        let stream = UnixStream::connect(&sock_path)
            .await
            .expect("connect to nervd");
        let (read_half, mut write_half) = stream.into_split();

        let req = Request::Complete {
            line: "git ".to_string(),
            cursor: 4,
            cwd: None,
        };
        let mut json = serde_json::to_string(&req).unwrap();
        json.push('\n');
        write_half.write_all(json.as_bytes()).await.unwrap();

        // Read response.
        let mut reader = BufReader::new(read_half);
        let mut resp_line = String::new();
        reader.read_line(&mut resp_line).await.unwrap();

        let resp: Response = serde_json::from_str(resp_line.trim()).unwrap();
        match resp {
            Response::Suggestions { items } => {
                let names: Vec<&str> = items.iter().map(|s| s.insertion.as_str()).collect();
                // git.json fixture has 4 subcommands: status, log, checkout,
                // commit. Engine emits alphabetically sorted.
                assert_eq!(names, ["checkout", "commit", "log", "status"]);
            }
            other => panic!("expected Suggestions, got: {other:?}"),
        }

        // Also test Ping.
        let ping_req = serde_json::to_string(&Request::Ping).unwrap() + "\n";
        write_half.write_all(ping_req.as_bytes()).await.unwrap();
        resp_line.clear();
        reader.read_line(&mut resp_line).await.unwrap();
        let pong: Response = serde_json::from_str(resp_line.trim()).unwrap();
        assert!(matches!(pong, Response::Pong { .. }));
    })
    .await;

    // Always kill the daemon.
    child.kill().await.ok();
    let output = child.wait_with_output().await.ok();
    if let Some(out) = &output {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !stderr.is_empty() {
            eprintln!("--- nervd stderr ---\n{stderr}\n--- end ---");
        }
    }

    result.expect("test timed out after 5s");
}
