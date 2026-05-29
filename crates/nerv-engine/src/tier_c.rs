//! Tier C closure executor — opt-in via `feature = "quickjs"`.
//!
//! Runs the JS body of a `Generator::Custom { source: Some(...) }`
//! inside a fresh [`nerv_quickjs::Sandbox`] and coerces the result
//! into a list of candidate strings the engine can surface.
//!
//! Scope of this module:
//!  - Provide one entry point — [`execute_custom_source`] — that the
//!    generator dispatcher in `complete.rs` can call when (a) the
//!    `quickjs` feature is on AND (b) the Custom variant carries an
//!    inline `source` string.
//!  - Strict execution budget: 200ms by default, matching the Tier B
//!    `script` runner. Stale shell completions are worse than missing
//!    ones.
//!  - No host bindings: a `tokens` array is the only context the
//!    closure sees, exposed as a top-level `arguments`-style variable.
//!    Closures that reach for `executeCommand`, `currentWorkingDirectory`,
//!    or `generatorContext` will throw, and we treat that as a soft
//!    failure (no candidates).
//!
//! Wire-up to `complete.rs` is intentionally deferred — the converter
//! must first emit `source` (today it doesn't). This module is the
//! callable surface that wire-up will consume; tests below exercise
//! the same shape with hand-built sources.
//!
//! Module is gated by `cfg(feature = "quickjs")` at the declaration
//! site in `lib.rs`; no inner `#![cfg(...)]` needed.

use std::time::Duration;

use nerv_quickjs::Sandbox;
use serde_json::Value;

/// 200ms ceiling on Tier C closure runtime. Matches the Tier B script
/// runner so a single slow closure can't blow the whole 25ms input
/// latency budget — it just yields zero candidates and the dispatcher
/// falls through to the next generator.
pub const DEFAULT_BUDGET: Duration = Duration::from_millis(200);

/// Execute a captured Custom closure with a `tokens` array as its sole
/// argument. The source MUST be an expression that evaluates to the
/// closure's return value — typically the converter wraps the original
/// arrow function in `(<arrow>)(tokens)` so the eval immediately invokes
/// it. Returns the candidate strings extracted from the result, or
/// `None` for any failure mode (parse / throw / timeout / non-array
/// return). Callers map `None` to "no Tier C candidates for this
/// generator" and let the next generator try.
pub fn execute_custom_source(source: &str, tokens: &[String]) -> Option<Vec<String>> {
    execute_with_budget(source, tokens, DEFAULT_BUDGET)
}

/// Lower-level entry point that lets the caller dial the budget. Tests
/// use this to assert the timeout path.
pub fn execute_with_budget(
    source: &str,
    tokens: &[String],
    budget: Duration,
) -> Option<Vec<String>> {
    let sandbox = Sandbox::new().ok()?;
    let bind = format!(
        "globalThis.__nerv_tokens = {};\n",
        serde_json::to_string(tokens).ok()?
    );
    // Eval the binding first (no budget needed for the assignment).
    sandbox.eval_isolated(&bind).ok()?;
    let result = sandbox
        .eval_with_budget(&wrap_source(source), budget)
        .ok()?;
    extract_string_candidates(&result)
}

/// Wrap user `source` so it can read tokens via a stable name. The
/// converter is expected to emit closure-call form already (so the
/// expression resolves to the return value), e.g.:
///
/// ```text
/// (async (tokens, exec) => tokens.map(t => t.toUpperCase()))(
///   globalThis.__nerv_tokens, () => Promise.resolve(""),
/// )
/// ```
///
/// This function is a no-op trim/wrap; we keep it as a seam so a
/// future converter pass can change the shape without recompiling the
/// engine.
fn wrap_source(source: &str) -> String {
    source.to_string()
}

/// Walk a serde_json::Value into a flat candidate list. Accepted shapes
/// mirror what real Fig closures return:
///   - `["a", "b"]`                        → ["a", "b"]
///   - `[{ name: "a" }, { name: "b" }]`    → ["a", "b"]
///   - anything else                       → None
fn extract_string_candidates(v: &Value) -> Option<Vec<String>> {
    let arr = v.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        if let Some(s) = item.as_str() {
            out.push(s.to_string());
        } else if let Some(obj) = item.as_object() {
            if let Some(name) = obj.get("name").and_then(|n| n.as_str()) {
                out.push(name.to_string());
            }
        }
        // Silently skip unrecognised shapes — same policy the engine
        // applies to generators that emit `null` items.
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_string_array() {
        let src = r#"["main", "dev"]"#;
        let got = execute_custom_source(src, &[]);
        assert_eq!(got, Some(vec!["main".into(), "dev".into()]));
    }

    #[test]
    fn extracts_object_array_via_name_field() {
        let src = r#"[{ name: "alpha" }, { name: "beta" }]"#;
        let got = execute_custom_source(src, &[]);
        assert_eq!(got, Some(vec!["alpha".into(), "beta".into()]));
    }

    #[test]
    fn skips_unrecognised_items() {
        // Mix valid strings, objects with `name`, and unrecognised
        // values. Output preserves the valid ones in order.
        let src = r#"["good", 42, { other: 1 }, { name: "also" }, null]"#;
        let got = execute_custom_source(src, &[]).unwrap();
        assert_eq!(got, vec!["good".to_string(), "also".to_string()]);
    }

    #[test]
    fn tokens_are_visible_to_closure() {
        // The closure can read the array we pinned via globalThis.
        let src = "globalThis.__nerv_tokens.map(t => t.toUpperCase())";
        let got = execute_custom_source(src, &["git".to_string(), "co".to_string()]);
        assert_eq!(got, Some(vec!["GIT".into(), "CO".into()]));
    }

    #[test]
    fn returns_none_on_non_array_result() {
        let src = r#""just a string""#;
        let got = execute_custom_source(src, &[]);
        assert_eq!(got, None);
    }

    #[test]
    fn returns_none_on_thrown_error() {
        let src = r#"throw new Error("nope")"#;
        let got = execute_custom_source(src, &[]);
        assert_eq!(got, None);
    }

    #[test]
    fn budget_kills_infinite_loop() {
        // Pin the budget to a small value so the test stays fast.
        let src = "while (true) {}";
        let got = execute_with_budget(src, &[], Duration::from_millis(50));
        assert_eq!(got, None);
    }

    #[test]
    fn host_bindings_throw_softly() {
        // `process` / `executeCommand` aren't in the sandbox — closures
        // that touch them throw, and we fall through to None.
        let src = "process.env.PATH.split(':')";
        let got = execute_custom_source(src, &[]);
        assert_eq!(got, None);
    }
}
