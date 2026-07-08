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
//! - **Fresh runtime per `Sandbox`** — every eval starts from a new
//!   `Runtime` so a prior call can't leak state, and a hung evaluator
//!   drops without ripping the process down.
//! - **Opt-in Fig host surface** — the base sandbox is bare ECMAScript
//!   (`Math`, `JSON`, `Array`, …). Callers explicitly opt into the Fig
//!   runtime: [`Sandbox::install_ts_helpers`] (`__awaiter`/`__generator`)
//!   and [`Sandbox::set_shell_exec`] (`executeShellCommand` /
//!   `executeCommand` / `console` / `process`). `fetch`, network, and
//!   arbitrary filesystem stay absent. The shell binding is the same
//!   trust boundary as a Tier B `script` — the command text is spec-
//!   authored, not user input.
//! - **Wall-clock budget** — `eval_with_budget` / `eval_resolved` abort
//!   after the supplied `Duration` via QuickJS's interrupt callback.
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

    /// Promise-aware evaluation with a wall-clock budget. Fig spec
    /// closures are almost all `async` — a bare `eval` of `(async …)()`
    /// yields a **pending Promise**, not the array we want. This drives
    /// the QuickJS job queue ([`rquickjs::Promise::finish`], which pumps
    /// microtasks until the promise settles) and unwraps the resolved
    /// value. Non-promise results pass straight through. A promise that
    /// can't settle without external work (a real async host call that
    /// never resolves) surfaces as `Javascript("would block")`.
    pub fn eval_resolved(&self, source: &str, budget: Duration) -> Result<Value, SandboxError> {
        let deadline = Instant::now() + budget;
        let budget_ms = budget.as_millis();
        self.runtime
            .set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
        let result = self.context.with(|ctx| {
            let raw: rquickjs::Value<'_> = ctx
                .eval(source)
                .map_err(|e| classify(&ctx, e, budget_ms, deadline))?;
            let settled = if raw.is_promise() {
                let promise = raw.into_promise().expect("is_promise() just checked");
                promise
                    .finish::<rquickjs::Value<'_>>()
                    .map_err(|e| classify(&ctx, e, budget_ms, deadline))?
            } else {
                raw
            };
            value_to_json(settled).map_err(|e| SandboxError::Serde(e.to_string()))
        });
        self.runtime.set_interrupt_handler(None);
        result
    }

    /// Install the TypeScript async-runtime shims (`__awaiter`,
    /// `__generator`) that `tsc` emits when it down-levels `async`/`await`.
    /// 21% of captured closures reference these; without them the eval
    /// throws `ReferenceError`. Idempotent — evaluated once per sandbox.
    pub fn install_ts_helpers(&self) -> Result<(), SandboxError> {
        self.context.with(|ctx| {
            ctx.eval::<(), _>(TS_HELPERS)
                .map_err(|e| SandboxError::Javascript(e.to_string()))
        })
    }

    /// Install the Fig shell host bindings. `exec` is a caller-supplied
    /// synchronous spawn (the engine owns the timeout + cwd + argv policy
    /// so this crate stays dependency-light and shell-agnostic); it takes
    /// the command line and returns stdout. Two globals are exposed to
    /// match the two Fig runtime shapes closures reach for:
    ///
    /// - `executeShellCommand(cmd)` → stdout string  (legacy)
    /// - `executeCommand({command, args})` → `{stdout, stderr, status,
    ///   exitCode}`  (current)
    ///
    /// Both resolve synchronously; `await` on the string/object just
    /// yields it, and [`Self::eval_resolved`] drains the microtask queue.
    /// This is the same trust boundary as a Tier B `script` generator —
    /// the command text comes from the vendored spec, not user input.
    pub fn set_shell_exec<F>(&self, exec: F) -> Result<(), SandboxError>
    where
        F: Fn(String) -> String + 'static,
    {
        self.context.with(|ctx| {
            let func = rquickjs::Function::new(ctx.clone(), move |cmd: String| exec(cmd))
                .map_err(|e| SandboxError::Javascript(e.to_string()))?;
            ctx.globals()
                .set("__nerv_run", func)
                .map_err(|e| SandboxError::Javascript(e.to_string()))?;
            Ok::<_, SandboxError>(())
        })?;
        let shim = host_shim();
        self.context.with(|ctx| {
            ctx.eval::<(), _>(shim.as_str())
                .map_err(|e| SandboxError::Javascript(e.to_string()))
        })
    }
}

/// Build the Fig host-runtime shim. On top of the Rust `__nerv_run`
/// (string command → stdout) it defines the globals Fig closures reach
/// for across API generations:
///
/// - `executeShellCommand` — **polymorphic**: a string yields stdout
///   (legacy), an object `{command, args}` yields `{stdout, …}` (current).
/// - `executeCommand` — always the record shape.
/// - `console` — a no-op sink (closures log freely; we don't care).
/// - `process` — `{ env, platform }` seeded from the daemon's real
///   environment so `process.env.HOME`-style path building works.
fn host_shim() -> String {
    let env_json = process_env_json();
    format!(
        r#"
globalThis.console = {{ log: function () {{}}, error: function () {{}}, warn: function () {{}}, info: function () {{}}, debug: function () {{}} }};
globalThis.process = {{ env: {env_json}, platform: "darwin" }};
globalThis.environmentVariables = globalThis.process.env;
globalThis.__figLine = function (input) {{
  return typeof input === "string" ? input : [input.command].concat(input.args || []).join(" ");
}};
globalThis.executeShellCommand = function (input) {{
  var out = globalThis.__nerv_run(globalThis.__figLine(input));
  return typeof input === "string" ? out : {{ stdout: out, stderr: "", status: "success", exitCode: 0 }};
}};
globalThis.executeCommand = function (input) {{
  return {{ stdout: globalThis.__nerv_run(globalThis.__figLine(input)), stderr: "", status: "success", exitCode: 0 }};
}};
"#
    )
}

/// A small allowlist of the daemon's environment, JSON-encoded for the
/// `process.env` shim. Only path-shaped vars closures actually read —
/// not the whole environment.
fn process_env_json() -> String {
    let mut map = serde_json::Map::new();
    for key in ["HOME", "USER", "PATH", "PWD", "SHELL", "LANG", "TMPDIR"] {
        if let Ok(val) = std::env::var(key) {
            map.insert(key.to_string(), Value::String(val));
        }
    }
    Value::Object(map).to_string()
}

/// Map an rquickjs error to the sandbox's error taxonomy. Both a budget
/// interrupt AND a real `throw` surface as `Error::Exception`; they are
/// told apart by the deadline — past it, the interrupt fired (Timeout);
/// otherwise the script threw, and [`Ctx::catch`] recovers the message.
/// A promise that drains its job queue without settling is `WouldBlock`.
fn classify(
    ctx: &rquickjs::Ctx<'_>,
    e: rquickjs::Error,
    budget_ms: u128,
    deadline: Instant,
) -> SandboxError {
    match e {
        rquickjs::Error::Exception if Instant::now() >= deadline => {
            SandboxError::Timeout { budget_ms }
        }
        rquickjs::Error::Exception => {
            let caught = ctx.catch();
            let msg = caught
                .as_exception()
                .and_then(|ex| ex.message())
                .or_else(|| caught.as_string().and_then(|s| s.to_string().ok()))
                .unwrap_or_else(|| "uncaught exception".to_string());
            SandboxError::Javascript(msg)
        }
        rquickjs::Error::WouldBlock => SandboxError::Javascript("promise did not settle".into()),
        other => SandboxError::Javascript(other.to_string()),
    }
}

/// tslib `__awaiter` + `__generator`, verbatim from the TypeScript
/// runtime. `tsc` references these by name in every down-levelled
/// `async` function body; injecting them lets those closures run.
const TS_HELPERS: &str = r#"
globalThis.__awaiter = function (thisArg, _arguments, P, generator) {
  function adopt(value) { return value instanceof P ? value : new P(function (resolve) { resolve(value); }); }
  return new (P || (P = Promise))(function (resolve, reject) {
    function fulfilled(value) { try { step(generator.next(value)); } catch (e) { reject(e); } }
    function rejected(value) { try { step(generator["throw"](value)); } catch (e) { reject(e); } }
    function step(result) { result.done ? resolve(result.value) : adopt(result.value).then(fulfilled, rejected); }
    step((generator = generator.apply(thisArg, _arguments || [])).next());
  });
};
globalThis.__generator = function (thisArg, body) {
  var _ = { label: 0, sent: function () { if (t[0] & 1) throw t[1]; return t[1]; }, trys: [], ops: [] }, f, y, t, g;
  return g = { next: verb(0), "throw": verb(1), "return": verb(2) }, typeof Symbol === "function" && (g[Symbol.iterator] = function () { return this; }), g;
  function verb(n) { return function (v) { return step([n, v]); }; }
  function step(op) {
    if (f) throw new TypeError("Generator is already executing.");
    while (g && (g = 0, op[0] && (_ = 0)), _) try {
      if (f = 1, y && (t = op[0] & 2 ? y["return"] : op[0] ? y["throw"] || ((t = y["return"]) && t.call(y), 0) : y.next) && !(t = t.call(y, op[1])).done) return t;
      if (y = 0, t) op = [op[0] & 2, t.value];
      switch (op[0]) {
        case 0: case 1: t = op; break;
        case 4: _.label++; return { value: op[1], done: false };
        case 5: _.label++; y = op[1]; op = [0]; continue;
        case 7: op = _.ops.pop(); _.trys.pop(); continue;
        default:
          if (!(t = _.trys, t = t.length > 0 && t[t.length - 1]) && (op[0] === 6 || op[0] === 2)) { _ = 0; continue; }
          if (op[0] === 3 && (!t || (op[1] > t[0] && op[1] < t[3]))) { _.label = op[1]; break; }
          if (op[0] === 6 && _.label < t[1]) { _.label = t[1]; t = op; break; }
          if (t && _.label < t[2]) { _.label = t[2]; _.ops.push(op); break; }
          if (t[2]) _.ops.pop();
          _.trys.pop(); continue;
      }
      op = body.call(thisArg, _);
    } catch (e) { op = [6, e]; y = 0; } finally { f = t = 0; }
    if (op[0] & 5) throw op[1]; return { value: op[0] ? op[1] : void 0, done: true };
  }
};
"#;

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

    #[test]
    fn eval_resolved_unwraps_async_closure() {
        // The core Tier C case: `(async () => [...])()` is a Promise.
        // eval_resolved must drive the job queue and return the array.
        let sb = Sandbox::new().unwrap();
        let v = sb
            .eval_resolved("(async () => ['main', 'dev'])()", Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            v,
            Value::Array(vec![
                Value::String("main".into()),
                Value::String("dev".into()),
            ])
        );
    }

    #[test]
    fn eval_resolved_awaits_inside_async() {
        let sb = Sandbox::new().unwrap();
        let v = sb
            .eval_resolved(
                "(async () => { const x = await Promise.resolve(21); return [String(x * 2)]; })()",
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(v, Value::Array(vec![Value::String("42".into())]));
    }

    #[test]
    fn eval_resolved_passes_through_non_promise() {
        let sb = Sandbox::new().unwrap();
        let v = sb
            .eval_resolved("['a', 'b']", Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            v,
            Value::Array(vec![Value::String("a".into()), Value::String("b".into())])
        );
    }

    #[test]
    fn shell_exec_binding_feeds_closure() {
        // A closure that shells out: `executeShellCommand` returns the
        // stubbed stdout, the closure splits it into candidates.
        let sb = Sandbox::new().unwrap();
        sb.set_shell_exec(|cmd| {
            assert!(cmd.contains("branch"), "got cmd: {cmd}");
            "main\ndev\nfeature/x".to_string()
        })
        .unwrap();
        let v = sb
            .eval_resolved(
                "(async () => { const o = await executeShellCommand('git branch'); \
                 return o.split('\\n'); })()",
                Duration::from_secs(1),
            )
            .unwrap();
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
    fn execute_command_shim_returns_record() {
        // Modern shape: `executeCommand({command, args})` → `{stdout,…}`.
        let sb = Sandbox::new().unwrap();
        sb.set_shell_exec(|cmd| {
            assert_eq!(cmd, "aws configure list-profiles");
            "default\nlemon".to_string()
        })
        .unwrap();
        let v = sb
            .eval_resolved(
                "(async () => { const r = await executeCommand({ command: 'aws', \
                 args: ['configure', 'list-profiles'] }); return r.stdout.split('\\n'); })()",
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(
            v,
            Value::Array(vec![
                Value::String("default".into()),
                Value::String("lemon".into()),
            ])
        );
    }

    #[test]
    fn ts_helpers_enable_awaiter_transpiled_closure() {
        // What `tsc` emits for `async () => ['x']` at ES5 target.
        let sb = Sandbox::new().unwrap();
        sb.install_ts_helpers().unwrap();
        let src = "(function () { return __awaiter(this, void 0, void 0, function () { \
                   return __generator(this, function (_a) { return [2, ['x', 'y']]; }); }); })()";
        let v = sb.eval_resolved(src, Duration::from_secs(1)).unwrap();
        assert_eq!(
            v,
            Value::Array(vec![Value::String("x".into()), Value::String("y".into())])
        );
    }
}
