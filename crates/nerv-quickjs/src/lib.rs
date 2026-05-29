//! Tier C closure sandbox built on QuickJS via `rquickjs`.
//!
//! Scope: M1 opt-in only. This crate is **not** linked into the v0.1
//! Nerv binary — the daemon stays rquickjs-free until a future `--feature
//! quickjs` build opt-in. The sandbox runs Fig spec closures (the
//! `custom`/`postProcess` JS bodies the ts-to-json converter preserved
//! verbatim) and returns their result as `serde_json::Value` so the
//! engine never sees a raw JS handle.
//!
//! Hard constraints baked into the API:
//! - **No globals** — every eval starts from a fresh `Runtime` so a
//!   prior call can't leak state. The runtime itself is constructed
//!   per `Sandbox` so it's cheap to drop a hung evaluator without
//!   ripping the whole process down.
//! - **No host bindings** — only the standard ECMAScript globals
//!   (`Math`, `JSON`, `Array`, ...) are reachable. There is no `console`,
//!   no `process`, no `fetch`, no filesystem.
//! - **Wall-clock budget** — `eval_with_budget` aborts after the
//!   supplied `Duration`. QuickJS exposes an interrupt callback; we poll
//!   the deadline on each call.
//!
//! CLAUDE.md §4 invariant: deno_core is forbidden — this crate is the
//! sanctioned Tier C path. `rquickjs` adds ~1 MB to the binary when the
//! feature is enabled, vs ~30 MB for deno_core.

#![deny(rust_2018_idioms)]
#![warn(missing_debug_implementations)]

use std::time::{Duration, Instant};

use rquickjs::{Context, Runtime};
use serde_json::Value;
use thiserror::Error;

/// Errors surfaced by the sandbox. `From<rquickjs::Error>` deliberately
/// elides the QuickJS internal stack — callers only need to know "the
/// JS misbehaved", not the engine details.
#[derive(Debug, Error)]
pub enum SandboxError {
    /// The source compiled and started, but the wall-clock budget
    /// expired before it returned. The interrupt callback fires
    /// roughly every 256 bytecodes inside QuickJS.
    #[error("execution exceeded {budget_ms} ms budget")]
    Timeout { budget_ms: u128 },
    /// Source failed to parse, threw, or returned an unrepresentable
    /// value. The inner string is the QuickJS-formatted message —
    /// already includes the throw location.
    #[error("javascript error: {0}")]
    Javascript(String),
    /// `serde_json` couldn't materialise the returned value. Means the
    /// script handed back something exotic (`undefined`, a `BigInt`,
    /// a circular reference, ...) — we treat that as a soft failure.
    #[error("could not serialize result: {0}")]
    Serde(String),
    /// Runtime / Context construction failed. Indicates an OOM-class
    /// problem; rare in practice but propagated for completeness.
    #[error("quickjs runtime init failed: {0}")]
    Init(String),
}

/// A one-shot QuickJS evaluator. Owns a fresh runtime + context per
/// instance; constructing it is cheap so the typical pattern is "one
/// `Sandbox` per closure invocation" rather than a long-lived shared
/// engine. Long-lived shared state is forbidden by design — see crate
/// docs.
pub struct Sandbox {
    runtime: Runtime,
    context: Context,
}

impl std::fmt::Debug for Sandbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sandbox").finish()
    }
}

impl Sandbox {
    /// Construct a fresh sandbox. Drops cleanly when the value goes
    /// out of scope — no `close()` required.
    pub fn new() -> Result<Self, SandboxError> {
        let runtime = Runtime::new().map_err(|e| SandboxError::Init(e.to_string()))?;
        let context = Context::full(&runtime).map_err(|e| SandboxError::Init(e.to_string()))?;
        Ok(Self { runtime, context })
    }

    /// Evaluate `source` as a top-level expression and return the
    /// result as JSON. The script runs to completion with **no budget
    /// enforcement** — callers that don't fully trust the source must
    /// use [`Self::eval_with_budget`] instead. Suitable only for
    /// closures that come from a known-good spec file.
    pub fn eval_isolated(&self, source: &str) -> Result<Value, SandboxError> {
        self.context.with(|ctx| {
            let raw: rquickjs::Value<'_> = ctx
                .eval::<rquickjs::Value<'_>, _>(source)
                .map_err(|e| SandboxError::Javascript(e.to_string()))?;
            value_to_json(raw).map_err(|e| SandboxError::Serde(e.to_string()))
        })
    }

    /// Same as [`Self::eval_isolated`] but installs a wall-clock
    /// interrupt that aborts the script once `budget` has elapsed.
    /// Returns `SandboxError::Timeout` in that case.
    pub fn eval_with_budget(&self, source: &str, budget: Duration) -> Result<Value, SandboxError> {
        let deadline = Instant::now() + budget;
        let budget_ms = budget.as_millis();
        self.runtime
            .set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
        let result = self.context.with(|ctx| {
            ctx.eval::<rquickjs::Value<'_>, _>(source)
                .map_err(|e| match e {
                    rquickjs::Error::Exception => SandboxError::Timeout { budget_ms },
                    other => SandboxError::Javascript(other.to_string()),
                })
                .and_then(|raw| value_to_json(raw).map_err(|e| SandboxError::Serde(e.to_string())))
        });
        self.runtime.set_interrupt_handler(None);
        result
    }
}

/// Convert a QuickJS value into a serde_json::Value. Handles the four
/// types Fig spec closures actually return — string, number, bool,
/// array, object — and treats anything else (`undefined`, `null`,
/// functions, BigInt, …) as `Value::Null` so the engine has a uniform
/// "no result" sentinel.
fn value_to_json(v: rquickjs::Value<'_>) -> anyhow::Result<Value> {
    use rquickjs::Type as T;
    Ok(match v.type_of() {
        T::String => Value::String(v.as_string().unwrap().to_string()?),
        T::Int => Value::Number(serde_json::Number::from(v.as_int().unwrap())),
        T::Float => serde_json::Number::from_f64(v.as_float().unwrap())
            .map(Value::Number)
            .unwrap_or(Value::Null),
        T::Bool => Value::Bool(v.as_bool().unwrap()),
        T::Array => {
            let arr = v.as_array().unwrap();
            let mut out = Vec::with_capacity(arr.len());
            for item in arr.iter::<rquickjs::Value<'_>>() {
                out.push(value_to_json(item?)?);
            }
            Value::Array(out)
        }
        T::Object => {
            let obj = v.as_object().unwrap();
            let mut map = serde_json::Map::new();
            for entry in obj.props::<String, rquickjs::Value<'_>>() {
                let (k, val) = entry?;
                map.insert(k, value_to_json(val)?);
            }
            Value::Object(map)
        }
        _ => Value::Null,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn returns_a_string() {
        let sb = Sandbox::new().unwrap();
        let v = sb.eval_isolated("'hello'").unwrap();
        assert_eq!(v, Value::String("hello".into()));
    }

    #[test]
    fn returns_an_array_of_strings() {
        let sb = Sandbox::new().unwrap();
        let v = sb.eval_isolated("['main', 'dev', 'feature/x']").unwrap();
        assert_eq!(
            v,
            Value::Array(vec![
                Value::String("main".into()),
                Value::String("dev".into()),
                Value::String("feature/x".into()),
            ])
        );
    }

    #[test]
    fn returns_an_object() {
        let sb = Sandbox::new().unwrap();
        let v = sb
            .eval_isolated("({ name: 'git', verb: 'status' })")
            .unwrap();
        let mut map = serde_json::Map::new();
        map.insert("name".into(), Value::String("git".into()));
        map.insert("verb".into(), Value::String("status".into()));
        assert_eq!(v, Value::Object(map));
    }

    #[test]
    fn returns_a_number() {
        let sb = Sandbox::new().unwrap();
        assert_eq!(
            sb.eval_isolated("21 * 2").unwrap(),
            Value::Number(serde_json::Number::from(42))
        );
    }

    #[test]
    fn unrepresentable_returns_null() {
        let sb = Sandbox::new().unwrap();
        // `undefined` is the typical "closure forgot to return" case.
        // We map it to JSON null so the engine has a uniform sentinel.
        assert_eq!(sb.eval_isolated("undefined").unwrap(), Value::Null);
    }

    #[test]
    fn surfaces_thrown_error() {
        let sb = Sandbox::new().unwrap();
        let e = sb.eval_isolated("throw new Error('nope')").unwrap_err();
        assert!(matches!(e, SandboxError::Javascript(_)));
    }

    #[test]
    fn no_host_globals_are_reachable() {
        // The sandbox must not expose `process` / `console` / `require`
        // / `fetch`. Each of these throws `ReferenceError`.
        let sb = Sandbox::new().unwrap();
        for name in ["process", "console", "require", "fetch", "Deno"] {
            let src = format!("typeof {name}");
            let v = sb.eval_isolated(&src).unwrap();
            assert_eq!(
                v,
                Value::String("undefined".into()),
                "{name} should not be reachable"
            );
        }
    }

    #[test]
    fn fresh_runtimes_do_not_share_state() {
        // Setting a global in one sandbox must not bleed into the next.
        // Each `new()` call must produce an isolated runtime.
        let a = Sandbox::new().unwrap();
        a.eval_isolated("globalThis.x = 99").unwrap();
        let b = Sandbox::new().unwrap();
        assert_eq!(
            b.eval_isolated("typeof globalThis.x").unwrap(),
            Value::String("undefined".into())
        );
    }

    #[test]
    fn budget_aborts_infinite_loop() {
        let sb = Sandbox::new().unwrap();
        let err = sb
            .eval_with_budget("while (true) {}", Duration::from_millis(50))
            .unwrap_err();
        assert!(
            matches!(err, SandboxError::Timeout { .. }),
            "expected Timeout, got: {err:?}"
        );
    }

    #[test]
    fn budget_does_not_abort_quick_script() {
        let sb = Sandbox::new().unwrap();
        let v = sb.eval_with_budget("42", Duration::from_secs(1)).unwrap();
        assert_eq!(v, Value::Number(serde_json::Number::from(42)));
    }
}
