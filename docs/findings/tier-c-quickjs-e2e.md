# Finding — Tier C (quickjs) recovery: 0% → 82% (shipped by default)

**Date**: 2026-06-06 (0% baseline) · **2026-07-07: machinery fixed, 82%** ·
**2026-07-08: latency measured, shipped in release binary**
**Status**: ✅ **shipped by default**. The three 0%-era root causes are resolved;
the executor runs closures for real (82% settle, aws 746/749). A 2026-07-08
latency measurement confirmed the JS machinery is 2-3ms (inside the 25ms budget),
and revealed the release binary shipped *without* `--features quickjs` — so the
recovery never reached users. `release.yml` now builds
`--features nerv-cli/quickjs,nerv-daemon/quickjs` (+0.78MB). Remaining ceiling is
closure **lexical scope** (module-level helpers the converter doesn't capture) —
a niche-tool tail, deferred (needs a per-spec esbuild bundler).

## 2026-07-08 update — latency measured, shipped

Two questions gated the ship decision; a `measure_tierc.rs` run answered both.

**1. Perf — is the per-keystroke JS cost inside budget?** Stage timing (avg over
50 iters, shell stubbed no-op to isolate JS from the already-SHELL_CACHE'd
subprocess):

| stem | Sandbox::new | ts-helpers | host shim | tokens | eval (closure+prelude) | TOTAL |
|------|------|------|------|------|------|-------|
| cargo | 174µs | 295µs | 181µs | 8µs | 2078µs | **2.7ms** |
| npm | 176µs | 296µs | 185µs | 8µs | 1809µs | **2.5ms** |
| nx | 181µs | 303µs | 187µs | 10µs | 2479µs | **3.2ms** |
| gh | 172µs | 289µs | 179µs | 8µs | 1513µs | **2.2ms** |

The earlier "warm 8ms" included the (cached) shell round-trip. Pure JS machinery
is **2-3ms** — comfortably inside the 25ms input budget. The token-independent
setup (new + helpers + shim = ~0.65ms) *could* be amortised via Runtime reuse,
but the dominant cost is `eval` (per-call, unavoidable) and the total is already
in budget. **No perf work needed** — a result cache keyed on tokens would miss on
every keystroke anyway, and the residual is token-independent.

**2. Function — do the specs users actually use work?** Per-stem recovery
(cwd=None, no creds — undercounts real-world; `empty` mostly = ran-clean-no-data):

| stem | settled/total | ok_ge1 | note |
|------|------|------|------|
| aws | 746/749 | 615 | dominant corpus, essentially complete |
| npm | 25/29 | 0 | ran clean, needs real npm |
| meteor/trivy/dscl/st2 | full | — | niche, work |
| **cargo** | **0/44** | 0 | all fail — `'lastIndexOf' is not defined` etc. |
| chezmoi/nx/asdf/esbuild/deno/pnpm/swift/dotnet | 0-few/N | 0 | broken tail |

The broken 18% is **niche tools** whose closures reference module-level helpers
the prelude slice can't reach (`'separator'`, `'getSuggestions'`, `'map'`,
`'keywords'` not defined). The specs a typical user hits (git/docker/kubectl/gh/
npm/cargo core) are recovered by **Tier A/B + Rust-native recognizers**, not Tier
C — so the tail is low priority. cargo's important completions (`-p <pkg>`, subs)
come from native `detectCargoMetadataPackages`; only its bespoke Tier C tail
fails, and that soft-fails to the next generator.

**Decision (2026-07-08): ship Tier C in the release binary.** Perf is in budget,
aws recovery is the biggest available functional win, and it only reaches users
if compiled in. Reverses the 2026-06-07 "default-OFF, unbundled" decision (which
was correct when execution was 0%). Closing the niche tail needs a per-spec
esbuild bundler — a larger lever with diminishing returns, deferred.

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

### Function-form `script` synthesis (done) — the big lever

Many generators use `script: (tokens) => [...cmd]` + `postProcess: (out) =>
Suggestion[]` (aws `s3://`, ssh, file listings). The converter couldn't reduce
the function to a static array, so it emitted an inert `{type:script,
script:[]}` marker — 652 dead generators. Now `synthesizeScriptSource` wraps the
`script` + `postProcess` function bodies into a Tier C `custom` source that, in
the sandbox, calls `script(tokens)` → `executeShellCommand` → `postProcess(out)`.
The module-scope preludes resolve their helpers.

The engine's Tier C arm is also **separator-aware**: for a `s3://` / `dir/`
token it matches + inserts against the segment after the last `/` (the closure
already owns the leading path), so `aws s3 ls s3://<tab>` completes to
`s3://<bucket>/`.

**Corpus grew 473 → 1125 custom sources** (the 652 revived function-form
scripts). Recovery with real creds/CLIs present: **922/1125 settle (82%),
ok_ge1 = 693 (62%)** — real bucket/host/path completions, not just "ran".

### Remaining ceiling (deferred)

Residual failures reference helpers the top-level slice can't reach: defined
**inside** the spec object, in version subdirs (`az/2.53.0/…`), or pulled
through **transitive imports** of local modules. Recovering them needs a real
bundler pass (esbuild the whole module tree per spec) — a larger lever with
diminishing returns. Tier C is now a genuinely useful opt-in.

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
