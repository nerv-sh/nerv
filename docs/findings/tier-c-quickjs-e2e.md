# Finding — Tier C (quickjs) e2e recovery rate = 0%

**Date**: 2026-06-06 (decision recorded 2026-06-07)
**Branch**: feat/m1-batch-v3
**Status**: ✅ **decided — defer**. `--features quickjs` scaffold stays, but
production ships with it **OFF / not bundled** (opt-in only). Re-open when the
root causes below are addressed. Recorded in PLAN §0.2 (JS generator) + CLAUDE.md §3.

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
