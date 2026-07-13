# Nerv

> [Fig](https://fig.io)'s inline shell autocomplete, resurrected.
> No login. No AI. No telemetry. No Electron. One small binary and your zsh.

[![CI](https://github.com/nerv-sh/nerv/actions/workflows/ci.yml/badge.svg)](https://github.com/nerv-sh/nerv/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/nerv-sh/nerv?include_prereleases)](https://github.com/nerv-sh/nerv/releases)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Fig gave the terminal IDE-grade autocomplete: type `git ch` and the next token
is just *there*. Then Fig was acquired, folded into Amazon Q, and the
experience got buried under a mandatory Builder ID login, AI chat, and a
multi-hundred-megabyte bundle.

Nerv digs it back out. It runs the actual Fig completion engine — the Rust
codebase AWS preserved and open-sourced — plus the 1,400+ community-maintained
completion specs from [`withfig/autocomplete`](https://github.com/withfig/autocomplete),
compiled to static JSON at build time. What shipped as a desktop app with a
cloud attached is now a local daemon and a zsh widget.

**Press a key, see the next token. That's it.**

<p align="center">
  <img src="docs/assets/demo.gif" width="740" alt="nerv in action: typing git che shows ghost text and a popup, Right-arrow accepts, live git branches complete from the repo, and npm run lists package.json scripts">
</p>

## Highlights

- **715 CLI specs** — `git`, `docker`, `kubectl`, `aws`, `npm`, `cargo`, `gh`,
  `brew`, `terraform`, … converted from `withfig/autocomplete` at build time.
- **Live values, not just flags** — git branches, npm scripts, kubectl
  namespaces and resources, AWS profiles and resource IDs, SSH hosts, make
  targets, man pages, zoxide history. Recovered natively in Rust; no Node, no
  JS runtime on the hot path.
- **Inline ghost text** — your most recent matching history command (like
  zsh-autosuggestions), falling back to the top spec suggestion. Right-arrow
  accepts.
- **Frecency ranking** — things you actually pick float to the top.
- **Understands your shell** — completes through `alias g=git`, behind
  `sudo` / `env` / `watch`, and on the last segment of compound lines
  (`git pull && git ch<Tab>`). Flags you already typed aren't offered twice.
- **Fast** — sub-millisecond engine responses against a 25 ms per-keystroke
  budget. The daemon lazy-loads specs and keeps memory bounded.
- **Leaves no trace** — `nerv uninstall` removes every file it ever wrote.
  We treat *trace zero* as a release-blocking acceptance criterion.

## Install

Requires **macOS on Apple Silicon** and **zsh ≥ 5.8**.

```sh
brew tap nerv-sh/tap
brew install nerv
eval "$(nerv init zsh)"
nerv start
```

Type `git ` in a new prompt — the popup should appear. If it doesn't, run
`nerv doctor`: it checks the shell hook, the daemon, the spec cache, and the
schema version, and tells you exactly what's wrong.

`brew services start nerv` keeps the daemon alive across reboots (optional —
`nerv start` is enough for a session).

> Homebrew installs don't trip Gatekeeper: `brew` doesn't quarantine its
> downloads, and the ad-hoc signature from the Rust toolchain is sufficient on
> Apple Silicon. If you download a release tarball in a browser instead, clear
> the flag once with `xattr -dr com.apple.quarantine <path>`.

## Using it

There is nothing to learn beyond five keys:

| Key | Action |
|---|---|
| `Tab` / `↓` | Next suggestion (wraps). Accepting a flag that takes a value chains straight into its value list. |
| `Shift-Tab` / `↑` | Previous suggestion |
| `→` (at end of line) | Accept the inline ghost |
| `Enter` | On a suggestion: insert it. On the `↩ Immediately execute` row: run the line as typed. |
| `Esc` / `Ctrl-G` | Dismiss the popup |

`PageUp` / `PageDown` jump through long lists; the `[k/N]` footer counts them.

Everything else is automatic:

```sh
❯ git ch<Tab>            # checkout / cherry / cherry-pick
❯ git checkout <Tab>     # your actual branches, checkout order
❯ npm run <Tab>          # scripts from the nearest package.json
❯ kubectl -n <Tab>       # live namespaces
❯ aws --profile <Tab>    # profiles from ~/.aws/config
❯ ssh <Tab>              # hosts from known_hosts + ssh config
❯ z <Tab>                # zoxide / zsh-z directory history
❯ sudo docker r<Tab>     # wrappers are looked through
❯ g ch<Tab>              # so are your aliases (alias g=git)
```

## How it works

```text
 zsh ──────────────────────────────┐        ┌─ nervd (daemon) ────────────────┐
 │ ZLE widget (_nerv.zsh)          │  UDS   │ SpecRegistry — lazy, LRU-bound  │
 │   every keystroke:              ├───────►│   ~/Library/Caches/nerv/specs/  │
 │   nerv _complete "git ch" 6     │        │   (715 specs, ~10 MB gzipped)   │
 │                                 │◄───────┤ generators — git branch,        │
 │ renders ghost + popup           │ 4-field│   npm scripts, … (cached, 800ms │
 │ (raw ANSI, no alternate screen) │  lines │   hard cap, run in parallel)    │
 └─────────────────────────────────┘        │ frecency ranking                │
                                            └─────────────────────────────────┘
```

- **Specs are compiled, not interpreted.** At build time a converter runs the
  TypeScript specs from `withfig/autocomplete` and emits plain JSON. At
  runtime there is no Node and no network — the daemon reads gzipped JSON off
  disk, lazily, with an LRU bound on both entry count and bytes.
- **Dynamic completions run as plain subprocesses** (`git branch --list`,
  `cargo metadata`, …) with a hard 800 ms timeout and a 5-second cache, or as
  pure-Rust readers for well-known patterns (package.json, SSH config, AWS
  INI files) that never fork at all. A sandboxed QuickJS interpreter (~1 MB)
  covers the long tail of spec-defined JavaScript generators.
- **The widget is plain ZLE.** No alternate screen, no 24-bit color, no PTY
  interposition — just cursor save/restore and line clearing, so it stays
  inside what real terminals reliably support. An opt-in PTY mode
  (`NERV_PTY=1`) exists for bash and fish.

Measured on Apple Silicon (release build):

| Path | Latency |
|---|---|
| Engine completion, warm (p95) | 0.055 ms |
| IPC round-trip (p95) | 0.052 ms |
| Per-keystroke budget | 25 ms |
| Largest spec (aws) cold parse — once, then cached | ~210 ms |

## What Nerv will never do

Scope is a feature. Nerv has **no** AI, **no** account or login, **no**
telemetry or analytics, **no** runtime network calls, **no** auto-updater,
and **no** webview. The CLI surface is frozen at `init`, `start`, `stop`,
`doctor`, `spec list`, `uninstall` — there is deliberately no `nerv config`.
These are documented non-goals ([`PLAN.md`](./PLAN.md) §4), not a backlog.

## Configuration

One optional file, `~/.config/nerv/nerv.toml`:

```toml
[matching]
mode = "fuzzy"   # default: "prefix"
```

Prefix matching is the default and the contract: `git co` matches `commit`,
not `checkout` (checkout starts with c-h-e). Fuzzy matching is a deliberate
opt-in and only kicks in from 3 typed characters. Restart the daemon after
editing (`nerv stop && nerv start`).

## Shells and terminals

| | Status |
|---|---|
| zsh ≥ 5.8 | **Default path** — native ZLE widget |
| bash, fish | Opt-in PTY shim: `NERV_PTY=1` before `nerv init bash` / `fish` |
| iTerm2, Terminal.app (incl. tmux inside them) | **Guaranteed** — regressions block release |
| WezTerm, Alacritty, kitty | Best-effort |
| Linux, Windows | Not yet — see roadmap |

Details and the exact ANSI contract: [`docs/terminal-compat.md`](./docs/terminal-compat.md).

## Turn it off

Stop the daemon — completions go quiet, everything stays installed:

```sh
nerv stop
brew services stop nerv   # only if you started it as a service
```

To keep it out of new shells entirely, comment out the `eval` line inside the
`# >>> nerv >>>` block in `~/.zshrc`, then `exec zsh`. Re-running
`nerv init zsh` turns it back on.

To leave for good:

```sh
nerv uninstall            # --keep-config preserves ~/.config/nerv/
```

It removes the `~/.zshrc` block, the daemon, caches, logs, and config, and is
verified against a written contract: [`docs/uninstall-spec.md`](./docs/uninstall-spec.md).

## Alternatives

Different projects make different trade-offs — pick what fits:

- **Amazon Q CLI** — the official successor; same engine ancestry, but
  requires a Builder ID login and ships the AI/telemetry stack Nerv exists to
  avoid.
- **[inshellisense](https://github.com/microsoft/inshellisense)** — Microsoft's
  take on the same spec set; TypeScript/Node, cross-platform.
- **[carapace](https://github.com/carapace-sh/carapace-bin)** — Go, huge
  cross-shell coverage, its own spec format; integrates with the shell's
  native completion system rather than an inline popup.
- **[zsh-autosuggestions](https://github.com/zsh-users/zsh-autosuggestions)** —
  history-only inline ghost. Nerv includes that behavior and adds the
  spec-driven popup on top.

## Roadmap

- **v1.0** — macOS + zsh, currently in internal dogfooding. Blockers are
  written acceptance criteria, not vibes: latency budget, terminal matrix,
  error UX, trace-zero uninstall.
- **v1.x** — Linux; Windows later. Deeper recovery of the remaining
  JavaScript-closure generators.

## Development

```sh
git clone https://github.com/nerv-sh/nerv && cd nerv

cargo test --workspace              # 700+ tests
cargo clippy --workspace --all-targets -- -D warnings
./scripts/e2e-isolated.sh           # isolated zsh smoke session (safe: own ZDOTDIR)

# Regenerate the spec cache from vendored TypeScript (needs bun)
cd tools/ts-to-json && bun install && bun run convert:all
```

Layout, briefly: `crates/nerv-cli` (the `nerv` binary), `crates/nerv-daemon`
(`nervd`), `crates/nerv-engine` (parser, spec loader, ranking, IPC),
`shell-integrations/` (the ZLE widget + PTY bootstraps), `tools/ts-to-json/`
(spec converter), `vendor/` (pinned upstream subtrees — never edited
directly).

One engineering habit worth stealing: **the acceptance criteria were written
before the code**, and changing behavior requires changing the document in
the same PR:

- [`PLAN.md`](./PLAN.md) — product plan, scope, roadmap
- [`docs/uninstall-spec.md`](./docs/uninstall-spec.md) — the trace-zero uninstall contract
- [`docs/error-states.md`](./docs/error-states.md) — five auto-detected error UX cases
- [`docs/terminal-compat.md`](./docs/terminal-compat.md) — guaranteed vs best-effort terminals
- [`docs/first-5-min.md`](./docs/first-5-min.md) — the install + first-5-minutes scenario
- [`docs/spec-conversion-policy.md`](./docs/spec-conversion-policy.md) — TS → static JSON policy
- [`docs/dogfood.md`](./docs/dogfood.md) — the dogfooding playbook

## Contributing

Issues and discussions welcome at
[github.com/nerv-sh/nerv/issues](https://github.com/nerv-sh/nerv/issues).
Commits require a DCO sign-off (`git commit -s`). Completion behavior bugs
are the most valuable reports during the alpha — a one-liner with the exact
input and what you expected is enough.

## License

Apache-2.0 — see [`LICENSE`](./LICENSE).

Nerv stands on two upstream projects, gratefully:
[`withfig/autocomplete`](https://github.com/withfig/autocomplete) (the spec
corpus, MIT) and
[`aws/amazon-q-developer-cli-autocomplete`](https://github.com/aws/amazon-q-developer-cli-autocomplete)
(the preserved Fig engine, Apache-2.0 + MIT). See [`NOTICE`](./NOTICE).
