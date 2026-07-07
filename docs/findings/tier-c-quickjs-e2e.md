# Finding — Tier C (quickjs) recovery: 0% → 32% (machinery fixed)

**Date**: 2026-06-06 (0% baseline) · **2026-07-07 update: machinery fixed, 32%**
**Status**: 🔧 **re-opened and largely fixed**. The three 0%-era root causes are
resolved; the executor now runs closures for real. Remaining ceiling is closure
**lexical scope** (module-level helpers the converter doesn't capture), not the
runtime. Still `--features quickjs` opt-in.

## 2026-07-07 update — machinery works

The 0% was an unfinished executor, not an rquickjs limit. Fixed, all within
rquickjs (+0.78 MB, no deno_core, no invariant break):

- **async/Promise** — `Sandbox::eval_resolved` drives the job queue
  (`Promise::finish`) and unwraps the settled value.
- **`__awaiter`/`__generator`** — tslib shims injected via `install_ts_helpers`.
- **host bindings** — real `executeShellCommand` (polymorphic string **and**
  `{command,args}` object) + `executeCommand`, spawning `sh -c` in the client
  cwd (200 ms cap, same trust as a Tier B `script`). Plus `console` / `process`
  / `environmentVariables` shims.
- **converter** — `captureClosureSource` now passes the real
  `(tokens, executeShellCommand, generatorContext)` instead of a stub.

**Measured on the 473-closure corpus** (`tests/measure_tierc.rs`, cwd=None, no
cloud creds): **settled=150 (32%)**, err=323. The earlier "435 timeouts" were a
misdiagnosis — the classifier mapped every `Error::Exception` to Timeout; they
were actually JS throws. Real error breakdown after the fixes:

| Remaining error | Count | Nature |
|-----------------|-------|--------|
| module-level helper not defined (`customGenerator`, `separator`, `getSuggestions`, …) | ~250 | closure references a top-level const/fn from its spec's `.ts` module; converter captures only the closure body, losing lexical scope |
| misc (`environmentVariables.HOME` shapes, spec-specific) | ~70 | assorted |

`ran(settled)=150` undercounts real-world recovery: `empty=112` of those are
closures that ran cleanly but had no data **in this sandbox** (missing CLIs / no
creds / cwd=None). On a real shell they return results.

**Next ceiling = module-scope bundling**: capture each spec module's top-level
declarations and prepend them to the closure source. High-value (one `aws`
`customGenerator` helper unblocks 61 closures) but a real converter project
(AST-extract top-level decls) + JSON bloat. Deferred pending a go decision.

---

## Original 0% finding (2026-06-06)

**Branch**: feat/m1-batch-v3
**Status**: ✅ **decided — defer** (superseded by the 2026-07-07 update above).

## TL;DR

The `--features quickjs` Tier C closure path currently recovers **0 candidates
out of 473 captured closure sources**. Shipping it (default or opt-in) provides
no functional benefit in its current state.

## How it was measured

1. Re-ran `tools/ts-to-json` (`bun run convert:all`) → 715 specs, **473 Custom
   `source` strings captured** (matches the CLAUDE.md §3 claim).
2. Dumped all 473 sources, ran every one through
   `nerv_engine::tier_c::execute_custom_source(src, &["git", ""])` with the
   `quickjs` feature on.
3. Result: `total=473  ok_ge1=0  empty=0  none=473`.

> "473 closure source 캡처 (100% capture rate)" in CLAUDE.md §3 measures
> **capture**, not **execution**. Execution rate is 0%.

## Root causes (3)

| Cause | Count | Why it returns `None` |
|-------|-------|------------------------|
| `async` closures | 362 (76%) | `(async …)()` evaluates to a **Promise**. `nerv-quickjs::Sandbox` calls `ctx.eval()` only — it never drains the job queue (`Runtime::execute_pending_jobs`) nor resolves the returned Promise. `value_to_json` sees a Promise object, not an array → `extract_string_candidates` → `None`. |
| `__awaiter`-wrapped | 101 (21%) | TypeScript transpiles `async`/`await` into `__awaiter(this, …, function* (){…})`. The `__awaiter` helper is **not defined** in the sandbox (no host globals) → `ReferenceError` → `None`. |
| shell-dependent (sync) | 10 | Converter stubs `executeShellCommand` as `() => Promise.resolve("")`. Closures that shell out get empty stdout → empty/`None`. |

## Binary cost (for reference)

`nervd`: 2.14 MB (default) → 2.92 MB (quickjs) = **+0.78 MB**. The `nerv` CLI is
unaffected — it is only the IPC bridge and dead-code-strips rquickjs.

## Implications for the ship decision

- **Default-on**: +0.78 MB for 0 recovery → reject. (PLAN §390 already schedules
  general quickjs supply for v1.1, not v1.0.)
- **Opt-in as-is**: functionally inert; the feature flag currently does nothing
  useful on the real corpus.

## To make Tier C actually work (option A, large)

1. Drive the QuickJS event loop: after eval, call
   `Runtime::execute_pending_jobs()` and unwrap the resolved Promise value
   (rquickjs `Promise::finish` / async eval support).
2. Define/inject the `__awaiter` + `__generator` TS helper shims, **or** change
   the converter to emit closures without the TS async transpile wrapper.
3. Provide a real `executeShellCommand` host binding — **conflicts** with the
   crate's "no host bindings" security stance (`nerv-quickjs` lib docs) and the
   200 ms latency budget. Needs a policy decision before implementing.

Well-known Rust-native recovery (`aws_list` 89, `kubectl_resources` 86,
`package_json_scripts` 28, …) is unaffected and keeps working — those do not go
through the quickjs path.
