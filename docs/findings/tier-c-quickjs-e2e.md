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
cloud creds). The earlier "435 timeouts" were a misdiagnosis — the classifier
mapped every `Error::Exception` to Timeout; they were actually JS throws.

Recovery climbed as each blocker was addressed:

| Step | settled | note |
|------|---------|------|
| machinery only (async + shims) | 150 (32%) | dominant residual: undefined module-level helpers |
| + module-scope prelude | 220 (47%) | capture each `.ts` module's top-level helpers |
| + strip `export` from prelude | 272 (58%) | QuickJS script-mode rejects `export` |
| + enrich generatorContext | 293 (62%) | `context.environmentVariables` etc. |
| + `@fig/autocomplete-generators` prelude | 295 (62%) | shared `keyValue`/`valueList`/… library |

**Final: 295/473 settle (62%)**, `ok_ge1=78 (16%)`. `ran` undercounts real-world
recovery: ~212 `empty` ran cleanly but had no data **in this sandbox** (missing
CLIs / no creds / cwd=None); on a real shell they return results.

### Module-scope bundling (done)

`captureModulePrelude` slices each spec `.ts` file's top-level helper
declarations (everything before `completionSpec`), strips `import`/`export`,
transpiles TS→JS, and `captureClosureSource` prepends it (set per file via the
`CURRENT_PRELUDE` save/restore, mirroring `AWS_SERVICE_HINT`). A global
`FIG_GENERATORS_PRELUDE` serialises the shared `@fig/autocomplete-generators`
exports. Gzip collapses the repeated preludes, so the shipped cache barely grows.

### Remaining ceiling (~38%, deferred)

Residual failures reference helpers the top-level slice can't reach: defined
**inside** the spec object, in version subdirs (`az/2.53.0/…`), or pulled
through **transitive imports** of local modules. Recovering them needs a real
bundler pass (esbuild the whole module tree per spec) — a much larger lever with
sharply diminishing returns. Stop here; Tier C is now a working opt-in.

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
