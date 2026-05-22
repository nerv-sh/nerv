//! M0-7 latency bench — measure round-trip from a fresh UDS
//! connection to the daemon's Complete response.
//!
//! This is `#[ignore]` so the regular test suite stays fast. Run via:
//!
//! ```sh
//! cargo test -p nerv-daemon --test bench_latency \
//!   -- --ignored --nocapture
//! ```
//!
//! Measures the IPC + engine path (no `nerv` CLI cold-start). The
//! widget's actual per-keystroke cost adds the CLI spawn time on
//! top of these numbers — see CLAUDE.md §3 M0-7.
//!
//! Refs: PLAN.md §10 M0-7 (p95 < 25 ms acceptance criterion).

use nerv_engine::{Request, Response};
use std::path::PathBuf;
use std::time::{Duration, Instant};
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

/// How many IPC roundtrips to sample.
const ITERATIONS: usize = 500;
/// p95 acceptance threshold (PLAN.md §10 M0-7).
const P95_THRESHOLD_MS: f64 = 25.0;

#[tokio::test]
#[ignore]
async fn ipc_roundtrip_p95_under_threshold() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let sock_path = tmp.path().join("nervd.sock");
    let pid_path = tmp.path().join("nervd.pid");
    let specs_dir = fixture_specs_dir();

    let mut child = tokio::process::Command::new(nervd_bin())
        .env("NERV_SOCK", &sock_path)
        .env("NERV_PID", &pid_path)
        .env("NERV_SPECS_DIR", &specs_dir)
        .env("NERV_LOG", "warn")
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn nervd");

    for _ in 0..30 {
        if sock_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(sock_path.exists(), "nervd socket did not appear");

    // Warm-up: 10 untimed roundtrips so any first-call setup (TLB
    // misses, syscall caches, allocator first-page) doesn't skew the
    // histogram.
    for _ in 0..10 {
        roundtrip(&sock_path).await;
    }

    let mut samples_us: Vec<u128> = Vec::with_capacity(ITERATIONS);
    for _ in 0..ITERATIONS {
        let start = Instant::now();
        roundtrip(&sock_path).await;
        samples_us.push(start.elapsed().as_micros());
    }

    samples_us.sort_unstable();
    let p50 = percentile_us(&samples_us, 50.0);
    let p95 = percentile_us(&samples_us, 95.0);
    let p99 = percentile_us(&samples_us, 99.0);
    let max = *samples_us.last().unwrap();
    let p95_ms = p95 as f64 / 1000.0;

    println!("nervd IPC latency over {ITERATIONS} samples (after 10 warm-up):");
    println!("  p50: {:>7.3} ms", p50 as f64 / 1000.0);
    println!("  p95: {:>7.3} ms", p95_ms);
    println!("  p99: {:>7.3} ms", p99 as f64 / 1000.0);
    println!("  max: {:>7.3} ms", max as f64 / 1000.0);

    child.kill().await.ok();

    assert!(
        p95_ms < P95_THRESHOLD_MS,
        "p95 latency {p95_ms:.3} ms exceeds threshold {P95_THRESHOLD_MS} ms"
    );
}

/// Single Complete-response roundtrip on a fresh UDS connection.
async fn roundtrip(sock_path: &std::path::Path) {
    let stream = UnixStream::connect(sock_path).await.expect("connect");
    let (read_half, mut write_half) = stream.into_split();
    let req = Request::Complete {
        line: "git commit --".to_string(),
        cursor: 13,
    };
    let mut json = serde_json::to_string(&req).unwrap();
    json.push('\n');
    write_half.write_all(json.as_bytes()).await.unwrap();

    let mut reader = BufReader::new(read_half);
    let mut resp_line = String::new();
    reader.read_line(&mut resp_line).await.unwrap();
    let resp: Response = serde_json::from_str(resp_line.trim()).unwrap();
    match resp {
        Response::Suggestions { items } => assert!(!items.is_empty()),
        other => panic!("unexpected: {other:?}"),
    }
}

fn percentile_us(sorted: &[u128], pct: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((pct / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}
