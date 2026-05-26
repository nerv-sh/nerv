# M0 (v0.6) absorption + M1 weeks 0–10 head-start

50 commits on `chore/generate-llms-txt`. Closes everything in **PLAN.md §10 M0 산출물** except **M0-8** (Apple Developer ID signing/notarization — blocked on infrastructure, not code). Bonus: a sizeable chunk of M1 weeks 0–10 is already done.

## Summary

- **M0-1 .. M0-7 ✅** — `aws/amazon-q-developer-cli-autocomplete` subtree absorbed; 10 fig crates extracted via `git filter-repo` into `crates/nerv-{pty,term,ipc,proto,integrations,util,settings,os,log,diag}/`; toolchain bumped to **1.85 / edition 2024**; `shell-parser/parser.ts` (20 KB) + `parseArguments.ts` ported to Rust (`nerv-engine::shell_parser` + `::spec_parser`, 298 tests); `loadSpec.ts` → `nerv-engine::spec_loader` + JSON + `build-specs` binary + `complete` pipeline + daemon wire-up.
- **Latency bench (M0-7)** — IPC p95 **0.052 ms**, CLI cold-start p95 **4.07 ms** (16 % of the 25 ms budget). See `crates/nerv-daemon/tests/bench_latency.rs`.
- **CLI 5/5 surface complete** — `nerv init / start / stop / spec list / doctor / uninstall` (uninstall is `uninstall-spec.md §4` 8-step atomic, with `--keep-config`).
- **TS → JSON converter** (`tools/ts-to-json/`, bun-based) — 715 vendor specs converted, 0 failures. Tier A 440 / B 6 / C 246. `loadSpec` depth=1 inlined so nested completions like `aws ec2 <verb>` work.
- **Lazy SpecRegistry + mtime hot-reload** — daemon boot is O(1) (no disk scan); spec replacement is picked up on the next lookup without daemon restart.
- **gzip cache** (`flate2`) — `*.json.gz` auto-detected. 45 MB → 4.5 MB plain, 176 MB → 10 MB at depth=1 (10×). `build-specs --compress` flag.
- **Tier B generator execution** — static shell commands (e.g. `git branch --list`) spawn with 200 ms cap, 5 s TTL + 64-entry LRU, ANSI / git-marker line sanitization. Tier C (closure) stays deferred — closures don't survive JSON serialization (see PLAN §0.2).
- **Error UX** (shell-side) — E1 widget hint, E2 doctor table, E3 zsh<5.8 check, E4 widget conflict detection.
- **Widget polish** — Fig-style Tab cycling, single-item immediate insert, mid-cursor BUFFER+CURSOR atomic insertion, ghost-text clearing, popup auto-size with description truncation. Reasserted Tab binding via `precmd` hook to survive plugin overrides (oh-my-zsh, fzf-tab, zsh-autocomplete).
- **CI** — toolchain pinned to 1.85; new `build-specs-smoke` (plain + gzip against 9 hand-rolled fixtures) and `ts-to-json` (bun convert:one + JSON sanity) jobs.

## Numbers

| Metric | Value |
|---|---|
| Active workspace crates | 16 |
| Workspace tests | 476 passing (5 ignored) |
| `cargo fmt --all --check` | green |
| `cargo clippy --workspace --all-targets -- -D warnings` | green |
| `cargo test --workspace` | green |
| IPC roundtrip p95 | 0.052 ms |
| CLI cold-start p95 | 4.07 ms |
| Specs converted | 715 / 715 |
| Daemon boot (cold cache, 715 specs) | O(1) — first lookup pays |
| Cache footprint (715 specs depth=1, gzip) | 10 MB |

## Hand-rolled fixture pack (9 specs)

`git`, `echo`, `docker`, `kubectl`, `npm`, `cargo`, `gh`, `brew`, `make` — covers 43 integration tests in `nerv-engine::complete`. Goal: regression-proof the parser before depending on 715 generated specs.

## Invariants enforced

All entries in `CLAUDE.md §4` are checked in code:

- `~/Library/Caches/nerv/`, `~/Library/Logs/nerv/`, `~/.config/nerv/` (no `directories` crate; absorbed `fig_util`/`fig_log`/`fig_settings` are re-wired to nerv paths).
- Prefix matching only — no fuzzy code path in v1.0 (M1 opt-in via `~/.config/nerv/nerv.toml [matching] mode = "fuzzy"`).
- Marker block `# >>> nerv >>>` ~ `# <<< nerv <<<` is a fixed string; absorbed `fig_integrations` marker swapped.
- No alternate screen, no true color, no OSC 8/52 (`terminal-compat.md §3` blacklist).
- 5-command CLI surface frozen.
- Zero presence of `fig_api_client` / `fig_auth` / `fig_telemetry*` / `amzn-*` / `semantic_search_client` / `tao` / `wry` in `Cargo.lock` (verified via `cargo tree`).
- `deno_core` not embedded.
- `nerv-pty` is M1 opt-in (`NERV_PTY=1`).

## What's NOT in this PR

- **M0-8** Apple Developer ID signing/notarization (infrastructure dependency).
- **rquickjs Tier C** — permanently deferred (closures aren't JSON-serializable; the static analysis policy in PLAN §0.2 reroutes those specs into Tier B).
- **inotify push-based hot-reload** — current implementation uses mtime stat poll on each lookup (~1 µs).
- **spec depth=2+** loadSpec inlining — works (gzip absorbs the size hit) but per-spec memory cost hasn't been profiled.
- **E5 manifest** — schema/codes not yet defined.

## Test plan

- [x] `cargo fmt --all --check`
- [x] `cargo clippy --workspace --all-targets -- -D warnings`
- [x] `cargo test --workspace`
- [x] Manual: `eval "$(nerv init zsh)"` in fresh zsh session, type `git commi<Tab>` → `git commit` (popup cycles, single-item Tab inserts immediately).
- [x] Manual: `nerv start && nerv doctor && nerv spec list && nerv stop && nerv uninstall` round-trip leaves zero artifacts in `~/Library/Caches/nerv/`, `~/Library/Logs/nerv/`, `~/.config/nerv/`, and `~/.zshrc`.
- [x] Latency bench: `cargo test --release -p nerv-daemon --test bench_latency -- --ignored --nocapture` → IPC p95 < 25 ms.
- [ ] CI workflow runs green on PR (build-specs-smoke + ts-to-json jobs new, untested in CI).
- [ ] M0-8 signing verification — out of scope, separate PR once Apple Developer ID is provisioned.

## File map (high level)

```
crates/
  nerv-cli/                   # 5/5 CLI surface
  nerv-daemon/                # tokio UDS, lazy SpecRegistry → engine
  nerv-engine/                # shell_parser + spec_parser + spec_loader + complete + ranker + bin/build-specs
  nerv-shell/                 # marker block init / strip
  nerv-{pty,term,ipc,proto,integrations,os,util,settings,log,diag}/
                              # M0-2 filter-repo extraction (figterm = M1 opt-in)
shell-integrations/zsh/_nerv.zsh   # ZLE widget (include_str!'d into nerv binary)
tools/ts-to-json/                  # bun-based TS→JSON (715 spec)
vendor/aws-autocomplete/           # M0-1 subtree (Apache+MIT mirror, drift watch)
vendor/withfig-autocomplete/       # subtree, ISC, pin = aef52acf…
.github/workflows/ci.yml           # extended with build-specs-smoke + ts-to-json
```

Refs: PLAN.md v0.6 §10 / CLAUDE.md §3

🤖 Generated with [Claude Code](https://claude.com/claude-code)
