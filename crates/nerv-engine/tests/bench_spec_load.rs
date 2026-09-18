//! Cold-load cost of the largest bundled specs (`--ignored`, release).
//!
//! `cargo test --release -p nerv-engine --test bench_spec_load -- --ignored --nocapture`
//!
//! Numbers here decide whether the on-disk format is worth changing: the
//! registry parses off the keystroke path but only blocks for
//! `SPEC_LOAD_SYNC_WAIT` (50 ms), so anything slower than that costs the
//! user an empty first keystroke (CLAUDE.md §3 async 스펙 로드).
//! Needs the converted cache (`bun run convert:all`); skips without it.

use std::path::PathBuf;
use std::time::Instant;

fn converted(name: &str) -> PathBuf {
    [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "fixtures",
        "converted",
        name,
    ]
    .iter()
    .collect()
}

/// Resident set of this process, in MB, via `ps` — good enough to size a
/// parsed tree against its source text.
fn rss_mb() -> f64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<f64>()
        .unwrap_or(0.0)
        / 1024.0
}

#[test]
#[ignore = "benchmark — run explicitly with --release"]
fn cold_load_cost_of_the_big_specs() {
    for name in ["aws.json", "gcloud.json", "cargo.json", "git.json"] {
        let path = converted(name);
        if !path.exists() {
            eprintln!("[skip] {name} absent — run `bun run convert:all`");
            continue;
        }
        let before = rss_mb();
        let read_start = Instant::now();
        let text = std::fs::read_to_string(&path).unwrap();
        let read_ms = read_start.elapsed().as_secs_f64() * 1000.0;

        let parse_start = Instant::now();
        let spec = nerv_engine::parse_spec_str(&text, &path).unwrap();
        let parse_ms = parse_start.elapsed().as_secs_f64() * 1000.0;
        let after = rss_mb();

        println!(
            "{name:14} text {:>7.1} MB  read {read_ms:>7.1} ms  parse {parse_ms:>7.1} ms  rss +{:>6.1} MB",
            text.len() as f64 / 1e6,
            after - before
        );
        drop(spec);
    }
}

/// End-to-end cost of the split layout, through the real registry: what a
/// keystroke on `aws ` and then on `aws iam ` actually pays.
///
/// Point `NERV_SPLIT_DIR` at a `build-specs` output directory:
/// `cargo run --release -p nerv-engine --bin build-specs -- --input
/// crates/nerv-engine/tests/fixtures/converted/ --output /tmp/split --only aws`
#[test]
#[ignore = "benchmark — run explicitly with --release"]
fn keystroke_cost_of_a_split_spec() {
    let Some(dir) = std::env::var_os("NERV_SPLIT_DIR") else {
        eprintln!("[skip] set NERV_SPLIT_DIR to a build-specs output dir");
        return;
    };
    let dir = PathBuf::from(dir);
    let registry = nerv_engine::SpecRegistry::at_dir(&dir);

    for line in ["aws ", "aws iam ", "aws iam create-u", "aws s3 "] {
        let before = rss_mb();
        // First call: cold. Keep asking until rows land, reporting both
        // the first call's latency and how long the whole thing took —
        // the first is what the user feels, the second is the real work.
        let first = Instant::now();
        let mut rows = nerv_engine::complete(line, line.len(), &registry)
            .items
            .len();
        let first_ms = first.elapsed().as_secs_f64() * 1000.0;
        let settle = Instant::now();
        while rows == 0 && settle.elapsed().as_secs_f64() < 5.0 {
            std::thread::sleep(std::time::Duration::from_millis(5));
            rows = nerv_engine::complete(line, line.len(), &registry)
                .items
                .len();
        }
        let settle_ms = settle.elapsed().as_secs_f64() * 1000.0;
        println!(
            "{line:18} first {first_ms:>7.1} ms ({rows} rows)  settled after {settle_ms:>7.1} ms  rss +{:>6.1} MB",
            rss_mb() - before
        );
    }
}

/// The split layout must complete identically to the unsplit spec, on the
/// real `aws` tree rather than a fixture — 409 services, generators,
/// persistent root options and all.
///
/// Needs both: the converted cache for the whole spec and
/// `NERV_SPLIT_DIR` for the split one.
#[test]
#[ignore = "acceptance — run explicitly with --release"]
fn a_split_aws_completes_like_the_whole_one() {
    let Some(split_dir) = std::env::var_os("NERV_SPLIT_DIR") else {
        eprintln!("[skip] set NERV_SPLIT_DIR to a build-specs output dir");
        return;
    };
    let whole_dir = converted("aws.json");
    if !whole_dir.exists() {
        eprintln!("[skip] converted cache absent");
        return;
    }
    let whole = nerv_engine::SpecRegistry::at_dir(whole_dir.parent().unwrap());
    let split = nerv_engine::SpecRegistry::at_dir(&PathBuf::from(split_dir));

    let lines = [
        "aws ",
        "aws ec",
        "aws ec2 ",
        "aws ec2 describe-inst",
        "aws ec2 describe-instances --",
        "aws s3 ",
        "aws s3 ls ",
        "aws iam create-u",
        "aws iam create-user --",
        "aws configure ",
        "aws --region ",
    ];
    let mut bad = Vec::new();
    for line in lines {
        let a = settled_rows(&whole, line);
        let b = settled_rows(&split, line);
        if a != b {
            bad.push(format!(
                "  `{line}`: whole {} rows, split {} rows\n    whole: {:?}\n    split: {:?}",
                a.len(),
                b.len(),
                a.iter().take(6).collect::<Vec<_>>(),
                b.iter().take(6).collect::<Vec<_>>()
            ));
        } else {
            println!("{line:32} {} rows match", a.len());
        }
    }
    assert!(
        bad.is_empty(),
        "split and whole disagree:\n{}",
        bad.join("\n")
    );
}

/// Completions for `line`, waiting out a cold parse.
fn settled_rows(registry: &nerv_engine::SpecRegistry, line: &str) -> Vec<String> {
    let deadline = Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let r = nerv_engine::complete(line, line.len(), registry);
        if !r.items.is_empty() || Instant::now() >= deadline {
            return r.items.into_iter().map(|s| s.insertion).collect();
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
