# Dogfooding — M1 10-week beta checkpoint

> PLAN §10: *"10주차 베타 체크포인트: 내부 dogfooding 2주."* This doc is the
> playbook for that window — what to run, what to watch, how to log, and the
> bar we have to clear before tagging v1.0.

## 0. Goal

Run nerv as the **only** autocomplete on a real daily-driver shell for two
weeks and surface every paper-cut that the e2e harnesses can't — latency
under real load, spec gaps on commands you actually type, popup jitter on a
real terminal, frecency that ranks wrong, prompt-shift on PTY mode.

The harnesses prove the machine works in isolation. Dogfooding proves it
*disappears* — the Fig bar is "press a key, see the next token, never think
about it."

## 1. Exit criteria (all must hold to leave the window)

| # | Criterion | How to check |
|---|---|---|
| 1 | 2 weeks of daily use, primary shell, no fall-back to bare shell | self-report log below |
| 2 | Zero crashes of `nervd` or the shell integration | `nerv doctor` daily; daemon PID stable |
| 3 | p95 keystroke→render still < 25 ms under real spec cache | `cargo test --release -p nerv-daemon --test bench_latency -- --ignored --nocapture` |
| 4 | No prompt corruption / leftover ANSI after `nerv uninstall` | trace-zero check (uninstall-spec §6) |
| 5 | Every logged P0/P1 either fixed or explicitly deferred with rationale | feedback table triaged |
| 6 | At least the top-20 commands you type have working completion | coverage notes below |

## 2. Setup — make nerv the daily driver

This is *not* the isolated harness (`e2e-isolated.sh`); dogfooding means your
real shell. It is fully reversible (`nerv uninstall`).

```sh
# 1. Build + install the 715-spec gzipped cache from this checkout.
cargo build --release -p nerv-cli -p nerv-daemon
cd tools/ts-to-json && bun install && bun run convert:all && cd -
cargo run --release -p nerv-engine --bin build-specs -- \
    --input crates/nerv-engine/tests/fixtures/converted/ \
    --output ~/Library/Caches/nerv/specs/ --compress

# 2. Wire your real ~/.zshrc (zsh) — keep the printed block.
target/release/nerv init zsh >> ~/.zshrc      # review before sourcing
exec zsh

# 3. (bash/fish dogfooders) opt into the PTY path instead:
#    export NERV_PTY=1
#    bash:  eval "$(nerv init bash)"      (POSIX eval)
#    fish:  nerv init fish | source       (fish syntax, not eval)

# 4. Start the daemon and verify.
nerv start && nerv doctor
```

Put `~/Library/Caches/nerv/specs/` on a real cache (gzipped, ~10 MB) so you
exercise the lazy-load + hot-reload path, not the hand-rolled fixtures.

## 3. Daily checklist (~2 min)

Run once a day, ideally mid-session when the cache is warm:

- [ ] `nerv doctor` — all rows green (zsh / hook / daemon / specs / schema).
- [ ] Type a 3-deep subcommand you use (`git`, `docker`, `kubectl`, `aws`) —
      completion + ghost + popup all appear, no lag.
- [ ] `cd <Tab>` in a big dir — popup paginates, `[k/N]` counter correct.
- [ ] Accept a ghost (Right-arrow at EOL) — it lands, frecency floats it next time.
- [ ] No stray `^[[…m` / box chrome left on the line after Esc or accept.
- [ ] Note any command where completion was **wrong** or **missing** → §4.

## 4. Feedback capture

Log here as you go — one row per paper-cut. Keep it terse; a repro beats prose.
Severity: **P0** blocks daily use / corrupts the prompt · **P1** wrong or
missing completion on a common command · **P2** cosmetic / rare.

| Date | Sev | Shell+Term | What happened | Repro | Status |
|------|-----|-----------|---------------|-------|--------|
| | | | | | |

When a row is real and reproducible, file it at
<https://github.com/nerv-sh/nerv/issues> and link the issue in *Status*.

## 5. Known limitations — do **not** re-report

These are by design (PLAN §4 비목표 / current scope). Logging them is noise:

- No AI / natural-language → command. Intentional, permanent.
- JS-closure dynamic generators don't run in the default build (Tier C is
  opt-in `--features quickjs`, and even then 0% e2e recovery today — see
  `docs/findings/tier-c-quickjs-e2e.md`). Static generators *do* work.
- bash/fish need `NERV_PTY=1`; there is no ZLE-equivalent for them.
- macOS Gatekeeper warns on first launch until M0-8 signing lands.
- No Linux / Windows. No `nerv config` command (edit the TOML directly).

## 6. Wrap-up

At the end of the 2 weeks, triage the §4 table against the §1 exit criteria.
If all six hold and the P0/P1 queue is drained, the 10-week checkpoint passes
→ proceed to PLAN §10 weeks 11–14 (Homebrew tap public, KPI CI, v1.0).
Otherwise, extend the window and re-run until green.
