# Finding — aws generator recovery ceiling = 60% Rust-native, rest is rquickjs-bound

**Date**: 2026-06-13
**Branch**: chore/restore-ci
**Status**: ✅ **investigated — no clean Rust-native win remains**. The aws tail
beyond the recovered 60% is genuinely blocked on a JS runtime (rquickjs, which is
0% e2e today — see `tier-c-quickjs-e2e.md`). Recorded so the "recover more aws"
idea is not re-investigated from scratch. Confirms (does not revise) CLAUDE.md §3.

## TL;DR

`aws.json` carries **1844 generator instances**. **1095 (60%) already run in the
default build**; the remaining **749 (40%) are Tier C** and there is **no clean
recognizer left to write** — the static-extractable shapes are all recovered, and
what remains is bespoke token-dependent closures.

## How it was measured

Walked every `generators` array in `crates/nerv-engine/tests/fixtures/converted/aws.json`
and bucketed by generator `type` + whether it is runnable:

| `type` | count | runs in default build? |
|--------|-------|------------------------|
| `template` | 415 | ✅ engine spawns the static command |
| `script_with_json_path` | 591 | ✅ spawn + `JSON.parse`[parent_key]→id_field |
| `aws_list` | 89 | ✅ token-aware `aws <svc> <verb> [flag val]*` + JSON extract |
| `script` (empty `script:[]`) | 624 | ❌ no static command captured |
| `custom` (has `source`) | 125 | ❌ quickjs-only (`source` captured, 0% e2e) |

- **Runs**: 415 + 591 + 89 = **1095 (60%)**
- **Tier C**: 624 + 125 = **749 (40%)**

## Why the 624 `script:[]` cannot be recovered Rust-native

The 624 are **not** a converter bug and **not** static commands the converter
dropped. Two facts settle it:

1. **Static-array scripts already recover.** A generator like ec2
   `instances: { script: ["aws","ec2","describe-instances","--query",
   "Reservations[*].Instances[].InstanceId"], postProcess: postProcessAWS }`
   has an `Array` `script`, so `convertOneGenerator` captures it and (since the
   postProcess isn't the `postPrecessGenerator(out,parentKey,idField)` signature)
   emits it as a **`template`** (convert.ts line ~683). These are in the 415.

2. **The 624 come from only ~17 distinct closures.** `grep -rn "script: ("
   vendor/withfig-autocomplete/src/aws*` finds **17** function-form scripts total.
   The 624 is the post-walk *instance* count — those ~17 generator objects are
   referenced across many subcommand args (one `instances` generator reused by
   dozens of `--instance-id` slots, etc.).

   `tryResolveScriptFn` runs each with stub `tokens=[]`; these closures depend on
   `tokens[tokens.length-1]` (what the user typed) and do real control flow, so
   they return `undefined`/`[]` → `script:[]`. Example (s3.ts):

   ```js
   script: (tokens) => {
     const whatHasUserTyped = tokens[tokens.length - 1];
     const baseLsCommand = ["aws", "s3", "ls"];
     // ... lastIndexOf("/"), prefix strip, conditional return undefined ...
   }
   ```

   These are **bespoke** (filesystem `ls`, `s3://` prefix handling, slash-path
   splitting, conditional early returns) — not the uniform token→argv template
   that `aws_list`/`detectAwsListCustom` recognises. A recognizer per closure ≈
   hand-porting each one; ~17 distinct, low marginal value (each = one service's
   path completion), high per-closure effort.

## Options considered

| Option | Verdict |
|--------|---------|
| New recognizer for the 624 (like `aws_list`) | ❌ no shared signature — 17 bespoke closures |
| Hand-port the ~17 s3/path closures to Rust | ⚠️ possible, high effort / low value; deferred |
| Capture `script`-fn source so quickjs can try them | ⚠️ only helps once quickjs works (0% e2e); not now |
| Accept 60% and move on | ✅ chosen — record this finding |

## Bottom line

The recovered 60% covers the common read-verbs (describe-*/list-*/get-*) that
make up the bulk of real `aws` completion. The 40% tail is genuinely
JS-runtime-bound; re-open only alongside the quickjs root-cause fixes in
`tier-c-quickjs-e2e.md` (Promise drain + `__awaiter` shim + shell host binding).
