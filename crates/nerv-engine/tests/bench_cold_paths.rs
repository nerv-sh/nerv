//! Cold-path latency benches — the tail the per-keystroke bench never
//! sees. `bench_latency` hammers a warm git-sized spec, so the paths
//! that actually stall mid-typing (big-spec cold parse, reload after
//! LRU eviction) stay invisible to it. Run manually:
//!
//! ```sh
//! cargo test --release -p nerv-engine --test bench_cold_paths -- --ignored --nocapture
//! ```
//!
//! Requires the converted spec set at `tests/fixtures/converted/`
//! (`cd tools/ts-to-json && bun run convert:all`) — skips with a note
//! when absent (the directory is .gitignored, CI won't have it).

use std::path::{Path, PathBuf};
use std::time::Instant;

fn converted_dir() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("converted");
    (dir.join("aws.json").exists() || dir.join("aws.json.gz").exists()).then_some(dir)
}

#[test]
#[ignore = "manual bench — needs converted specs (bun convert:all)"]
fn bench_aws_cold_parse_warm_hit_and_evict_reload() {
    let Some(dir) = converted_dir() else {
        eprintln!("[bench-cold] SKIP — converted specs missing");
        return;
    };
    let reg = nerv_engine::SpecRegistry::at_dir(&dir);

    let t0 = Instant::now();
    assert!(reg.lookup("aws").is_some(), "aws spec must load");
    let cold = t0.elapsed();

    let t1 = Instant::now();
    assert!(reg.lookup("aws").is_some());
    let warm = t1.elapsed();

    // Churn enough distinct small specs through the lazy cache to push
    // aws past the count cap, then time the forced re-parse — this is
    // the intermittent mid-typing stall a user hits after touching many
    // other commands.
    let mut churned = 0;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(stem) = name.to_str().and_then(|s| {
                s.strip_suffix(".json")
                    .or_else(|| s.strip_suffix(".json.gz"))
            }) else {
                continue;
            };
            if stem == "aws" || stem.is_empty() {
                continue;
            }
            if reg.lookup(stem).is_some() {
                churned += 1;
            }
            if churned > 20 {
                break;
            }
        }
    }
    let t2 = Instant::now();
    assert!(reg.lookup("aws").is_some());
    let reload = t2.elapsed();

    eprintln!(
        "[bench-cold] aws cold={cold:?} warm={warm:?} \
         reload-after-evict={reload:?} (churned {churned} specs)"
    );
    // Sanity floor, not a budget: the warm hit must be orders of
    // magnitude under the cold parse or the cache is broken.
    assert!(warm < cold / 10, "warm hit suspiciously slow: {warm:?}");
}
