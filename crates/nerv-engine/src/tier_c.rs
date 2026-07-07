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
//!  - Fig runtime shims: the sandbox is primed with the TS async helpers
//!    (`__awaiter`/`__generator`), a real `executeShellCommand` /
//!    `executeCommand` host binding (spawns the spec-defined command in
//!    the client cwd — same trust boundary as a Tier B `script`), and
//!    the live `tokens` array via `globalThis.__nerv_tokens`. Closures
//!    that reach for anything else (`process`, `fetch`, filesystem) throw
//!    and we treat that as a soft failure (no candidates).
//!
//! Module is gated by `cfg(feature = "quickjs")` at the declaration
//! site in `lib.rs`; no inner `#![cfg(...)]` needed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use nerv_quickjs::Sandbox;
use serde_json::Value;

/// 200ms ceiling on Tier C closure runtime. Matches the Tier B script
/// runner so a single slow closure can't blow the whole 25ms input
/// latency budget — it just yields zero candidates and the dispatcher
/// falls through to the next generator.
pub const DEFAULT_BUDGET: Duration = Duration::from_millis(200);

/// Execute a captured Custom closure with the live `tokens` list and the
/// client `cwd`. The source is an expression that resolves to the
/// closure's return value (the converter emits closure-call form). The
/// sandbox is primed with the TS async shims, a real `executeShellCommand`
/// host binding (spawns in `cwd`, same trust as a Tier B script), and the
/// token array. Returns the extracted candidate strings, or `None` on any
/// failure (parse / throw / timeout / non-settling promise / non-array).
pub fn execute_custom_source(
    source: &str,
    tokens: &[String],
    cwd: Option<&Path>,
) -> Option<Vec<String>> {
    execute_with_budget(source, tokens, cwd, DEFAULT_BUDGET)
}

/// Lower-level entry point that lets the caller dial the budget. Tests
/// use this to assert the timeout path.
pub fn execute_with_budget(
    source: &str,
    tokens: &[String],
    cwd: Option<&Path>,
    budget: Duration,
) -> Option<Vec<String>> {
    run_in_sandbox(source, tokens, cwd, budget)
        .ok()
        .and_then(|v| extract_string_candidates(&v))
}

/// Shared sandbox pipeline. Surfaces the failing stage as a string so a
/// diagnostic harness can bucket errors; production callers ignore it.
fn run_in_sandbox(
    source: &str,
    tokens: &[String],
    cwd: Option<&Path>,
    budget: Duration,
) -> Result<Value, String> {
    let sandbox = Sandbox::new().map_err(|e| format!("init: {e}"))?;
    sandbox
        .install_ts_helpers()
        .map_err(|e| format!("helpers: {e}"))?;
    // Real shell host binding: closures that `await executeShellCommand(…)`
    // spawn the spec-defined command in the client cwd (200ms/drain cap).
    let exec_cwd: Option<PathBuf> = cwd.map(Path::to_path_buf);
    sandbox
        .set_shell_exec(move |cmd| run_shell(&cmd, exec_cwd.as_deref()))
        .map_err(|e| format!("exec-bind: {e}"))?;
    let bind = format!(
        "globalThis.__nerv_tokens = {};\n",
        serde_json::to_string(tokens).map_err(|e| format!("tokens: {e}"))?
    );
    sandbox
        .eval_isolated(&bind)
        .map_err(|e| format!("tokens-eval: {e}"))?;
    sandbox
        .eval_resolved(&wrap_source(source), budget)
        .map_err(|e| format!("eval: {e}"))
}

/// Diagnostic entry point for the recovery-measurement harness. Returns
/// the raw JSON value or the failing-stage string. Not used in
/// production — only `tests/measure_tierc.rs` calls it.
pub fn execute_debug(
    source: &str,
    tokens: &[String],
    cwd: Option<&Path>,
    budget: Duration,
) -> Result<Value, String> {
    run_in_sandbox(source, tokens, cwd, budget)
}

/// Spawn `sh -c <cmd>` in `cwd` and return its stdout (empty on failure
/// or timeout). The command text originates from the vendored spec
/// closure — the same trust boundary as a Tier B `script` generator.
fn run_shell(cmd: &str, cwd: Option<&Path>) -> String {
    use std::process::{Command, Stdio};
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    let Ok(child) = command.spawn() else {
        return String::new();
    };
    crate::complete::spawn_with_timeout(child, 8192)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
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
        let got = execute_custom_source(src, &[], None);
        assert_eq!(got, Some(vec!["main".into(), "dev".into()]));
    }

    #[test]
    fn extracts_object_array_via_name_field() {
        let src = r#"[{ name: "alpha" }, { name: "beta" }]"#;
        let got = execute_custom_source(src, &[], None);
        assert_eq!(got, Some(vec!["alpha".into(), "beta".into()]));
    }

    #[test]
    fn skips_unrecognised_items() {
        // Mix valid strings, objects with `name`, and unrecognised
        // values. Output preserves the valid ones in order.
        let src = r#"["good", 42, { other: 1 }, { name: "also" }, null]"#;
        let got = execute_custom_source(src, &[], None).unwrap();
        assert_eq!(got, vec!["good".to_string(), "also".to_string()]);
    }

    #[test]
    fn tokens_are_visible_to_closure() {
        // The closure can read the array we pinned via globalThis.
        let src = "globalThis.__nerv_tokens.map(t => t.toUpperCase())";
        let got = execute_custom_source(src, &["git".to_string(), "co".to_string()], None);
        assert_eq!(got, Some(vec!["GIT".into(), "CO".into()]));
    }

    #[test]
    fn returns_none_on_non_array_result() {
        let src = r#""just a string""#;
        let got = execute_custom_source(src, &[], None);
        assert_eq!(got, None);
    }

    #[test]
    fn returns_none_on_thrown_error() {
        let src = r#"throw new Error("nope")"#;
        let got = execute_custom_source(src, &[], None);
        assert_eq!(got, None);
    }

    #[test]
    fn budget_kills_infinite_loop() {
        // Pin the budget to a small value so the test stays fast.
        let src = "while (true) {}";
        let got = execute_with_budget(src, &[], None, Duration::from_millis(50));
        assert_eq!(got, None);
    }

    #[test]
    fn closure_shells_out_via_exec_binding() {
        // End-to-end: an async closure runs a real command through the
        // host binding and splits stdout into candidates.
        let src = r#"(async () => { const o = await executeShellCommand("printf 'main\ndev\nfeat'"); return o.split("\n"); })()"#;
        let got = execute_custom_source(src, &[], None);
        assert_eq!(got, Some(vec!["main".into(), "dev".into(), "feat".into()]));
    }

    #[test]
    fn host_bindings_throw_softly() {
        // The sandbox shims console / process / executeShellCommand, but
        // not the browser/network surface — a closure reaching for `fetch`
        // throws and we fall through to None rather than crash.
        let src = "fetch('https://example.com').then(r => [r])";
        let got = execute_custom_source(src, &[], None);
        assert_eq!(got, None);
    }

    #[test]
    fn process_env_shim_is_readable() {
        // `process.env` is seeded from the daemon env so path-building
        // closures work (`environmentVariables` aliases it too).
        let src = "[typeof process.env, typeof environmentVariables, typeof console.log]";
        let got = execute_custom_source(src, &[], None).unwrap();
        assert_eq!(got, vec!["object", "object", "function"]);
    }
}
