//! e2e: fuzzy matching opt-in flows through IPC.
//!
//! Spawns `nervd` with a non-default `nerv.toml` (`[matching] mode =
//! "fuzzy"`) via the `NERV_CONFIG_FILE` env override, sends a
//! Complete request with a fuzzy-only query (`git chk`), and asserts
//! the engine surfaces `checkout` — something the v1.0 prefix mode
//! would never produce.

use nerv_engine::{Request, Response};
use std::path::PathBuf;
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

#[tokio::test]
async fn fuzzy_mode_recovers_checkout_from_chk() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let sock_path = tmp.path().join("nervd.sock");
    let pid_path = tmp.path().join("nervd.pid");
    let config_path = tmp.path().join("nerv.toml");
    let specs_dir = fixture_specs_dir();
    assert!(specs_dir.exists());

    std::fs::write(&config_path, "[matching]\nmode = \"fuzzy\"\n").expect("write fuzzy config");

    let mut child = tokio::process::Command::new(nervd_bin())
        .env("NERV_SOCK", &sock_path)
        .env("NERV_PID", &pid_path)
        .env("NERV_SPECS_DIR", &specs_dir)
        .env("NERV_CONFIG_FILE", &config_path)
        .env("NERV_FRECENCY_FILE", "-")
        .env("NERV_MISSES_FILE", "-")
        .env("NERV_LOG", "debug")
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn nervd");

    for _ in 0..30 {
        if sock_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(sock_path.exists(), "nervd socket did not appear");

    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let stream = UnixStream::connect(&sock_path).await.expect("connect");
        let (read_half, mut write_half) = stream.into_split();

        // `git chk` — only matches `checkout` under fuzzy. Prefix mode
        // would return zero suggestions for this query (`chk` is not
        // a prefix of any git subcommand in the fixture).
        let req = Request::Complete {
            line: "git chk".to_string(),
            cursor: 7,
            cwd: None,
        };
        let mut json = serde_json::to_string(&req).unwrap();
        json.push('\n');
        write_half.write_all(json.as_bytes()).await.unwrap();

        let mut reader = BufReader::new(read_half);
        let mut resp_line = String::new();
        reader.read_line(&mut resp_line).await.unwrap();
        let resp: Response = serde_json::from_str(resp_line.trim()).unwrap();
        match resp {
            Response::Suggestions { items } => {
                let names: Vec<&str> = items.iter().map(|s| s.insertion.as_str()).collect();
                assert!(
                    names.contains(&"checkout"),
                    "expected `checkout` in fuzzy results, got: {names:?}"
                );
            }
            other => panic!("expected Suggestions, got: {other:?}"),
        }
    })
    .await;

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

#[tokio::test]
async fn default_prefix_mode_rejects_fuzzy_query() {
    // Belt-and-suspenders: same scenario without NERV_CONFIG_FILE
    // should hit the prefix-only path and NOT recover `checkout`
    // from `chk`. Guards against accidentally flipping the default.
    let tmp = tempfile::tempdir().expect("create tempdir");
    let sock_path = tmp.path().join("nervd.sock");
    let pid_path = tmp.path().join("nervd.pid");
    let bogus_config = tmp.path().join("does-not-exist.toml");
    let specs_dir = fixture_specs_dir();

    let mut child = tokio::process::Command::new(nervd_bin())
        .env("NERV_SOCK", &sock_path)
        .env("NERV_PID", &pid_path)
        .env("NERV_SPECS_DIR", &specs_dir)
        // Point at a missing file so load_from_path → default
        // (Prefix) without leaking the user's real ~/.config.
        .env("NERV_CONFIG_FILE", &bogus_config)
        .env("NERV_FRECENCY_FILE", "-")
        .env("NERV_MISSES_FILE", "-")
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn nervd");

    for _ in 0..30 {
        if sock_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(sock_path.exists());

    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let stream = UnixStream::connect(&sock_path).await.expect("connect");
        let (read_half, mut write_half) = stream.into_split();

        let req = Request::Complete {
            line: "git chk".to_string(),
            cursor: 7,
            cwd: None,
        };
        let mut json = serde_json::to_string(&req).unwrap();
        json.push('\n');
        write_half.write_all(json.as_bytes()).await.unwrap();

        let mut reader = BufReader::new(read_half);
        let mut resp_line = String::new();
        reader.read_line(&mut resp_line).await.unwrap();
        let resp: Response = serde_json::from_str(resp_line.trim()).unwrap();
        // No git subcommand starts with `chk` → expect Empty.
        assert!(
            matches!(resp, Response::Empty { .. }),
            "expected Empty (no prefix matches), got: {resp:?}"
        );
    })
    .await;

    child.kill().await.ok();
    result.expect("test timed out after 5s");
}
