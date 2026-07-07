//! One-off Tier C recovery measurement (opt-in). See header in the test.
//!   cargo test -p nerv-engine --features quickjs --test measure_tierc \
//!       -- --ignored --nocapture
#![cfg(feature = "quickjs")]

use std::collections::BTreeMap;
use std::time::Duration;

#[test]
#[ignore]
fn measure_recovery() {
    let raw =
        std::fs::read_to_string("/tmp/tierc_sources.json").expect("run the python extractor first");
    let items: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let arr = items.as_array().unwrap();
    let (mut ok_ge1, mut empty, mut non_array, mut err) = (0usize, 0usize, 0usize, 0usize);
    let mut stages: BTreeMap<String, usize> = BTreeMap::new();
    let budget = Duration::from_millis(200);
    for it in arr {
        let stem = it["stem"].as_str().unwrap_or("x");
        let src = it["src"].as_str().unwrap_or("");
        let tokens = vec![stem.to_string(), "a".to_string()];
        match nerv_engine::tier_c::execute_debug(src, &tokens, None, budget) {
            Ok(v) => match v.as_array() {
                Some(a) if !a.is_empty() => ok_ge1 += 1,
                Some(_) => empty += 1,
                None => non_array += 1,
            },
            Err(e) => {
                err += 1;
                // bucket by stage prefix + first line of message
                let key: String = e.chars().take(60).collect();
                *stages.entry(key).or_default() += 1;
            }
        }
    }
    let total = arr.len();
    let ran = ok_ge1 + empty + non_array;
    println!(
        "\n=== Tier C recovery ===\ntotal={total}\n\
         ran(settled)={ran} ({:.0}%)  ok_ge1={ok_ge1} ({:.0}%)  empty={empty}  \
         non_array={non_array}\nerr={err} ({:.0}%)",
        100.0 * ran as f64 / total as f64,
        100.0 * ok_ge1 as f64 / total as f64,
        100.0 * err as f64 / total as f64,
    );
    println!("\n--- top error stages ---");
    let mut v: Vec<_> = stages.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    for (k, c) in v.into_iter().take(12) {
        println!("{c:4}  {k}");
    }
}
