<p align="center">
  <img src="docs/assets/icon-1024.png" width="132" alt="Nerv">
</p>

<h1 align="center">Nerv</h1>

<p align="center">
  <strong>IDE-grade autocomplete for your terminal.</strong><br>
  No login. No AI. No telemetry. No Electron. Just one small binary and your zsh.
</p>

<p align="center">
  <a href="https://github.com/nerv-sh/nerv/actions/workflows/ci.yml"><img src="https://github.com/nerv-sh/nerv/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/nerv-sh/nerv/releases"><img src="https://img.shields.io/github/v/release/nerv-sh/nerv?include_prereleases" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License"></a>
</p>

**Nerv** (`nerv-sh/nerv`) is an open-source, IDE-style inline autocomplete
for the macOS terminal: a Rust daemon plus a zsh widget that shows the next
token — subcommands, flags, git branches, npm scripts, kubectl resources — as
you type, with descriptions, inside the terminal you already use. It is a successor to
Fig's autocomplete: it runs the Fig completion engine that AWS open-sourced and
the 715 community specs from `withfig/autocomplete`, without Fig's cloud, login,
AI, or Electron app. Apache-2.0, installed with Homebrew.

| | |
|---|---|
| **Category** | Terminal / shell autocomplete (Fig replacement) |
| **Platform** | macOS on Apple Silicon; zsh ≥ 5.8 default, bash and fish via opt-in PTY mode |
| **Install** | `brew install nerv-sh/tap/nerv && eval "$(nerv init zsh)"` |
| **Language** | Rust (`nerv` CLI 2.8 MB + `nervd` daemon 3.8 MB, static) + one zsh script |
| **Specs** | 715 commands converted from `withfig/autocomplete`, 15 MB gzipped, shipped in the package |
| **Latency** | 0.055 ms engine p95 against a 25 ms per-keystroke budget |
| **Network / accounts / telemetry** | None — documented non-goals |
| **License** | Apache-2.0 (engine), MIT (spec corpus) |

Fig gave the terminal IDE-grade autocomplete: type `git ch` and the next token
is just *there*. Then Fig was acquired, folded into Amazon Q, and the
experience got buried under a mandatory Builder ID login, AI chat, and a
multi-hundred-megabyte bundle.

Nerv digs it back out. The engine is the Rust code AWS preserved and
open-sourced; the specs are [`withfig/autocomplete`](https://github.com/withfig/autocomplete),
compiled to static JSON at build time. What shipped as a desktop app with a
cloud attached is now a local daemon and a zsh widget.

**Press a key, see the next token. That's it.**

<p align="center">
  <img src="docs/assets/demo.gif" width="740" alt="nerv in action: typing git ch shows dim ghost text and a popup with descriptions and argument hints, Right-arrow accepts it, git checkout lists the repository's live branches, selecting one and pressing Enter switches branch, npm run lists the scripts from package.json, and cd lists folders labelled with how many items each holds">
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
- **Never stalls your typing** — the engine answers in well under a
  millisecond against a 25 ms per-keystroke budget, and a slow live completion
  (a `brew` or `docker` shell-out) runs in the background instead of freezing
  the prompt — its result lands on the next keystroke. Lazy-loaded specs,
  bounded memory.
- **Commands without a spec still complete** — if nothing in the bundle or
  your overlay covers a command, Nerv derives a spec from its `--help` output
  once, in the background, and caches it. Typos in the command word
  (`zpeh` → `zeph`) get a one-line *did you mean* correction.
- **Leaves no trace** — `nerv uninstall` removes every file it ever wrote.
  We treat *trace zero* as a release-blocking acceptance criterion.

## Install

Requires **macOS on Apple Silicon** and **zsh ≥ 5.8**.

```sh
brew install nerv-sh/tap/nerv
eval "$(nerv init zsh)"
```

That's the whole install. The `eval` line writes an idempotent, marker-fenced
block into your `~/.zshrc` and activates completion in the current session;
the daemon starts itself on demand, and the 715 completion specs ship inside
the package. Type `git ` — the popup should appear. If it doesn't, run
`nerv doctor`: it checks the shell hook, the daemon (including a stale daemon
left behind by an upgrade — its version mismatching the CLI is reported), the
spec cache, and the schema version, and tells you exactly what's wrong.

> Homebrew installs don't trip Gatekeeper: `brew` doesn't quarantine its
> downloads, and the ad-hoc signature from the Rust toolchain is sufficient on
> Apple Silicon. If you download a release tarball in a browser instead, clear
> the flag once with `xattr -dr com.apple.quarantine <path>`.

## How it works

```text
 zsh ──────────────────────────────┐        ┌─ nervd (daemon) ────────────────┐
 │ ZLE widget (_nerv.zsh)          │  UDS   │ SpecRegistry — lazy, LRU-bound  │
 │   every keystroke:              ├───────►│   specs ship in the package     │
 │   nerv _complete "git ch" 6     │        │   (715 specs, 15 MB gzipped)    │
 │                                 │◄───────┤ generators — git branch,        │
 │ renders ghost + popup           │ 5-field│   npm scripts, … (cached, 800ms │
 │ (raw ANSI, no alternate screen) │  lines │   hard cap, off keystroke path) │
 └─────────────────────────────────┘        │ frecency ranking                │
                                            └─────────────────────────────────┘
```

- **Specs are compiled, not interpreted.** At build time a converter runs the
  TypeScript specs from `withfig/autocomplete` and emits plain JSON. At
  runtime there is no Node and no network — the daemon reads gzipped JSON off
  disk, lazily, with an LRU bound on both entry count and bytes.
- **Dynamic completions run off the keystroke path.** A subprocess
  (`git branch --list`, `cargo metadata`, …) runs on a background thread with a
  hard 800 ms cap and a 5-second cache (60 s for generators measured slower than
  50 ms), so a slow one never freezes the prompt —
  its result lands on the next keystroke. Well-known patterns (package.json, SSH
  config, AWS INI files) are read directly in Rust and never fork; a sandboxed
  QuickJS interpreter (~1 MB) covers the long tail of spec-defined JavaScript
  generators.
- **The widget is plain ZLE.** No alternate screen, no 24-bit color, no PTY
  interposition — just cursor save/restore and line clearing, so it stays
  inside what real terminals reliably support. An opt-in PTY mode
  (`NERV_PTY=1`) exists for bash and fish.

Measured on Apple Silicon (release build, v0.1.15, 2026-09):

| Path | Latency |
|---|---|
| Engine completion, warm (p95) | 0.055 ms |
| IPC round-trip (p95) | 0.052 ms |
| CLI bridge cold start (p95) | 4.07 ms |
| Per-keystroke budget | 25 ms |
| Largest spec (`aws`, 117 MB as JSON) — first keystroke, cold | 3.9 ms (split into 343 lazily loaded subtrees at build time) |

Numbers come from `cargo test --release -p nerv-daemon --test bench_latency`
and `crates/nerv-engine/tests/bench_spec_load.rs`; they are re-measured
before each release.

## What Nerv will never do

Scope is a feature. Nerv has **no** AI, **no** account or login, **no**
telemetry or analytics, **no** runtime network calls, **no** auto-updater,
and **no** webview. The CLI surface is frozen at `init`, `start`, `stop`,
`doctor`, `spec list`, `uninstall` — there is deliberately no `nerv config`.
These are documented non-goals, not a backlog.

## Configuration

One optional file, `~/.config/nerv/nerv.toml`:

```toml
[matching]
mode = "fuzzy"     # default: "prefix"

[derived]
enabled = false    # default: true
```

Prefix matching is the default and the contract: `git co` matches `commit`,
not `checkout` (checkout starts with c-h-e). Fuzzy matching is a deliberate
opt-in and only kicks in from 3 typed characters.

`[derived]` controls the `--help` fallback: when a command has no spec in
any layer, Nerv runs `<command> --help` once (no shell, 1 s timeout, output
capped, cached under `~/Library/Caches/nerv/derived/`) and builds a spec from
it. Set `enabled = false` and nothing is ever spawned. Either setting is read
once at daemon start, so restart after editing (`nerv stop && nerv start`).

### Add your own specs

Drop a spec JSON into `~/.config/nerv/specs/` and it is layered over the
bundled set — one file per command, no rebuild. A file with the same name as
a bundled spec replaces it wholesale (no merge), so this is also how you
patch a bundled spec. [`examples/specs/claude.json`](examples/specs/claude.json)
is a complete example (the `claude` CLI, which upstream never covered):

```sh
mkdir -p ~/.config/nerv/specs
cp examples/specs/claude.json ~/.config/nerv/specs/
nerv stop && nerv start     # once — the dir is watched from then on
nerv doctor                 # → "user specs   1 in ~/.config/nerv/specs"
nerv spec list | grep '\*'  # overlay rows are marked *
```

The format is the engine's plain JSON (`name` / `description` / `subcommands`
/ `options[].names` / `args`), the same as
`crates/nerv-engine/tests/fixtures/specs/`. Edits are picked up on the next
keystroke; a broken file disables only that one command and shows up red in
`nerv doctor`. `nerv uninstall` removes the dir with the rest of
`~/.config/nerv/` unless you pass `--keep-config`.

## Shells and terminals

| | Status |
|---|---|
| zsh ≥ 5.8 | **Default path** — native ZLE widget |
| bash, fish | Opt-in PTY shim: `NERV_PTY=1` before `nerv init bash` / `fish` |
| iTerm2, Terminal.app (incl. tmux inside them) | **Guaranteed** — regressions block release |
| WezTerm, Alacritty, kitty | Best-effort |
| Linux, Windows | Not yet — see roadmap |

Details and the exact ANSI contract: [`docs/terminal-compat.md`](./docs/terminal-compat.md).

## How Nerv compares to Fig, Amazon Q, inshellisense and others

Nerv occupies a specific spot: Fig's completion UX and spec corpus, with no
runtime beyond a native binary. The closest projects, and how they differ:

| Project | What it is | How Nerv differs |
|---|---|---|
| **Fig** (discontinued 2024) | Electron desktop app with IDE-style autocomplete; the origin of the spec format | Nerv runs Fig's own engine and specs as a local daemon + zsh widget; no desktop app, no account |
| **Amazon Q Developer CLI** (`aws/amazon-q-developer-cli`) | Fig's successor: autocomplete plus agentic AI chat; requires an AWS Builder ID login | Nerv keeps only the autocomplete half — no login, no AI, no telemetry, a few MB instead of hundreds |
| **inshellisense** (`microsoft/inshellisense`) | Node.js/TypeScript tool that also consumes Fig specs; cross-platform, runs the shell inside a PTY | Nerv is Rust with no Node on the hot path, and the default zsh path is a plain ZLE widget, not a PTY wrapper |
| **carapace** (`carapace-sh/carapace-bin`) | Go multi-shell completion binary with its own spec format, hooked into each shell's native completion system | Nerv is an inline popup with descriptions and live values on every keystroke, not a Tab-triggered completer |
| **zsh-autosuggestions** | History-based grey ghost text | Nerv shows the same history ghost text *and* spec-driven suggestions with descriptions; they coexist |
| **fzf-tab** | Fuzzy picker over zsh's native `compsys` completions on Tab | Nerv does not use `compsys`; it completes as you type from the Fig spec corpus |

Nerv is a good fit if you want Fig back on macOS + zsh with zero cloud. It is
not the right tool if you need Linux or Windows today (see roadmap) or want a
completer for a shell it does not support.

## FAQ

**Is Nerv a Fig replacement?**
Yes, for autocomplete on macOS with zsh. It runs the Fig completion engine
(preserved and open-sourced by AWS) and the `withfig/autocomplete` spec
corpus. It does not reproduce Fig's dashboard, dotfile sync, or team features.

**Does Nerv send any data anywhere?**
No. There are no network calls at runtime, no account, no telemetry, and no
auto-updater. Specs ship inside the Homebrew package. The only files Nerv
writes are under `~/.config/nerv/`, `~/Library/Caches/nerv/`, and
`~/Library/Logs/nerv/`, and `nerv uninstall` removes all of them.

**Does Nerv use AI?**
No. Suggestions come from static completion specs and the output of the
commands themselves (`git branch --list`, `package.json`, `kubectl get`).
This is a documented non-goal, not a missing feature.

**Which commands does Nerv complete?**
715 commands as of v0.1.15 — git, docker, kubectl, helm, aws, gcloud, npm,
yarn, pnpm, cargo, gh, brew, terraform, make, ssh and the rest of the
`withfig/autocomplete` corpus. `nerv spec list` prints the installed set.
Commands outside it get a spec derived from `--help`, and you can add or
override any spec with one JSON file in `~/.config/nerv/specs/`.

**Does Nerv work on Linux or Windows?**
Not yet. v1.0 is macOS on Apple Silicon only; Linux is planned for v1.x,
Windows later.

**Does Nerv work with oh-my-zsh, powerlevel10k, tmux, and zsh-autosuggestions?**
Yes. The widget rebinds its keys after other frameworks load, aligns the
popup under a full-width powerlevel10k prompt, is tested inside tmux, and
shows history ghost text the way zsh-autosuggestions does.

**How fast is it?**
The engine answers in about 0.05 ms at p95 and the whole keystroke path is
held under a 25 ms budget. Slow live completions (a `brew` or `docker`
shell-out) run on a background thread and land on the next keystroke, so
typing is never blocked.

**Does Nerv need Node.js, Python, or a JavaScript runtime?**
No. Specs are converted from TypeScript to JSON at build time. A ~1 MB
sandboxed QuickJS interpreter inside the binary handles the minority of
spec-defined JavaScript generators; nothing external is required.

## Roadmap

- **v1.0** — macOS + zsh, currently in internal dogfooding. Blockers are
  written acceptance criteria, not vibes: latency budget, terminal matrix,
  error UX, trace-zero uninstall.
- **v1.x** — Linux; Windows later. Deeper recovery of the remaining
  JavaScript-closure generators.

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
