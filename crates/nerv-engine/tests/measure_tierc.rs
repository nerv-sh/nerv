//! One-off Tier C recovery measurement (opt-in). See header in the test.
//!   cargo test -p nerv-engine --features quickjs --test measure_tierc \
//!       -- --ignored --nocapture
#![cfg(feature = "quickjs")]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

#[derive(Default)]
struct StemTally {
    total: usize,
    ok_ge1: usize,
    empty: usize,
    non_array: usize,
    err: usize,
}

#[test]
#[ignore]
fn measure_recovery() {
    let raw =
        std::fs::read_to_string("/tmp/tierc_sources.json").expect("run the python extractor first");
    let items: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let arr = items.as_array().unwrap();
    let (mut ok_ge1, mut empty, mut non_array, mut err) = (0usize, 0usize, 0usize, 0usize);
    let mut stages: BTreeMap<String, usize> = BTreeMap::new();
    let mut per_stem: BTreeMap<String, StemTally> = BTreeMap::new();
    let budget = Duration::from_millis(200);
    for it in arr {
        let stem = it["stem"].as_str().unwrap_or("x");
        let src = it["src"].as_str().unwrap_or("");
        let tokens = vec![stem.to_string(), "a".to_string()];
        let tally = per_stem.entry(stem.to_string()).or_default();
        tally.total += 1;
        match nerv_engine::tier_c::execute_debug(src, &tokens, None, budget) {
            Ok(v) => match v.as_array() {
                Some(a) if !a.is_empty() => {
                    ok_ge1 += 1;
                    tally.ok_ge1 += 1;
                }
                Some(_) => {
                    empty += 1;
                    tally.empty += 1;
                }
                None => {
                    non_array += 1;
                    tally.non_array += 1;
                }
            },
            Err(e) => {
                err += 1;
                tally.err += 1;
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
    println!("\n--- per-stem recovery (settled/total, ok_ge1) sorted by total ---");
    let mut s: Vec<_> = per_stem.into_iter().collect();
    s.sort_by(|a, b| b.1.total.cmp(&a.1.total));
    for (stem, t) in s.into_iter().take(30) {
        let settled = t.ok_ge1 + t.empty + t.non_array;
        println!(
            "{:>5}/{:<5} settled  ok_ge1={:<4} err={:<4}  {stem}",
            settled, t.total, t.ok_ge1, t.err
        );
    }
}

/// Categorise `'X' is not defined` failures: is `X` actually declared as a
/// top-level name in the captured source (→ a scope/concatenation bug we can
/// fix by hoisting) or absent entirely (→ a lexical var captured from an
/// enclosing runtime scope, or a missing import — the former is unfixable by
/// any bundler since `fn.toString()` drops the closure environment)? This is
/// the discriminating fact for whether an esbuild module-bundler is worth it.
#[test]
#[ignore]
fn measure_undefined_scope() {
    let raw =
        std::fs::read_to_string("/tmp/tierc_sources.json").expect("run the python extractor first");
    let items: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let arr = items.as_array().unwrap();
    let budget = Duration::from_millis(200);
    let (mut declared, mut absent, mut other_err) = (0usize, 0usize, 0usize);
    let mut absent_ids: BTreeMap<String, usize> = BTreeMap::new();
    let mut absent_examples: Vec<(String, String)> = Vec::new();
    for it in arr {
        let stem = it["stem"].as_str().unwrap_or("x");
        let src = it["src"].as_str().unwrap_or("");
        let tokens = vec![stem.to_string(), "a".to_string()];
        let Err(e) = nerv_engine::tier_c::execute_debug(src, &tokens, None, budget) else {
            continue;
        };
        // Parse "'ID' is not defined" out of the error string.
        let Some(id) = e
            .split_once('\'')
            .and_then(|(_, r)| r.split_once('\''))
            .map(|(id, _)| id.to_string())
            .filter(|_| e.contains("is not defined"))
        else {
            other_err += 1;
            continue;
        };
        // Is `id` declared at the top level of the captured source?
        let decl = [
            format!("function {id}"),
            format!("const {id}"),
            format!("let {id}"),
            format!("var {id}"),
            format!("globalThis.{id} ="),
        ]
        .iter()
        .any(|pat| src.contains(pat.as_str()));
        if decl {
            declared += 1;
        } else {
            absent += 1;
            *absent_ids.entry(id.clone()).or_default() += 1;
            if absent_examples.len() < 6 {
                absent_examples.push((stem.to_string(), id));
            }
        }
    }
    println!("\n=== undefined-identifier failure scope ===");
    println!("declared-in-src (hoist/scope bug, fixable): {declared}");
    println!("absent-from-src (lexical loss OR missing import): {absent}");
    println!("other errors (non 'not defined'): {other_err}");
    println!("\n--- top absent identifiers ---");
    let mut v: Vec<_> = absent_ids.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    for (id, c) in v.into_iter().take(15) {
        println!("{c:4}  {id}");
    }
    println!("\n--- absent examples (stem, id) ---");
    for (stem, id) in &absent_examples {
        println!("  {stem}: {id}");
    }
}

/// Stage-by-stage latency of the Tier C hot path. The question the perf
/// plan hinges on: where does the per-keystroke cost go, and is it inside
/// the 25ms input budget? Shell exec is stubbed to a no-op so we isolate
/// the token-independent JS machinery (Sandbox::new + helpers + host shim
/// + prelude/closure eval) from the subprocess cost (already SHELL_CACHE'd).
#[test]
#[ignore]
fn measure_latency() {
    use nerv_quickjs::Sandbox;

    let raw =
        std::fs::read_to_string("/tmp/tierc_sources.json").expect("run the python extractor first");
    let items: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let arr = items.as_array().unwrap();
    let budget = Duration::from_millis(200);

    // Representative sources: the biggest stems. First closure per stem.
    let stems = ["aws", "cargo", "npm", "nx", "gh"];
    println!("\n=== Tier C stage latency (avg over 50 iters, shell=no-op) ===");
    println!("stem     new     helpers  shim     tokens   eval     TOTAL");
    for want in stems {
        let Some(it) = arr.iter().find(|it| it["stem"].as_str() == Some(want)) else {
            continue;
        };
        let src = it["src"].as_str().unwrap_or("");
        let bind = format!(
            "globalThis.__nerv_tokens = {};\n",
            serde_json::to_string(&[want, "a"]).unwrap()
        );
        let iters = 50u32;
        let (mut t_new, mut t_help, mut t_shim, mut t_tok, mut t_eval) =
            (0u128, 0u128, 0u128, 0u128, 0u128);
        for _ in 0..iters {
            let a = Instant::now();
            let sb = Sandbox::new().unwrap();
            t_new += a.elapsed().as_micros();

            let a = Instant::now();
            sb.install_ts_helpers().unwrap();
            t_help += a.elapsed().as_micros();

            let a = Instant::now();
            sb.set_shell_exec(|_| String::new()).unwrap();
            t_shim += a.elapsed().as_micros();

            let a = Instant::now();
            let _ = sb.eval_isolated(&bind);
            t_tok += a.elapsed().as_micros();

            let a = Instant::now();
            let _ = sb.eval_resolved(src, budget);
            t_eval += a.elapsed().as_micros();
        }
        let n = iters as u128;
        let total = (t_new + t_help + t_shim + t_tok + t_eval) / n;
        println!(
            "{want:<8} {:<7} {:<8} {:<8} {:<8} {:<8} {}us",
            t_new / n,
            t_help / n,
            t_shim / n,
            t_tok / n,
            t_eval / n,
            total,
        );
    }
}
