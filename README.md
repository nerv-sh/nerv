# Nerv

> Inline shell autocomplete for macOS zsh — without the AWS login.

Nerv is a single-binary recreation of [Fig](https://fig.io)'s killer feature: real-time, IDE-like autocomplete for `git`, `docker`, `kubectl`, `npm`, and 50+ other CLIs. After Fig was acquired by Amazon and folded into Amazon Q (`q`), the experience got buried under mandatory Builder ID login, AI chat, agents, and a multi-hundred-megabyte bundle.

Nerv brings back the original promise: **press a key, see the next token. That's it.**

## Status

**Pre-alpha** — scaffolding phase. See [`PLAN.md`](./PLAN.md) (v0.5) for the full design and [`docs/`](./docs/) for the acceptance criteria written before the code.

## What it is

- **macOS + zsh** only (v1.0). bash, fish, Linux land in v1.x.
- **Static Fig spec compatibility** — uses the [`withfig/autocomplete`](https://github.com/withfig/autocomplete) repository (MIT) at build time. No runtime JS engine.
- **Single binary**, installed via Homebrew. No Node, no daemon manager, no cloud.
- **Apache-2.0 licensed**, no telemetry, no auth.

## What it isn't (v1.0)

- AI / natural-language to command (this is intentional — see [`PLAN.md`](./PLAN.md) §10)
- Dynamic completions like `git checkout <branch>` (deferred to v1.1; v1.0 shows a hint pointing to the right shell command)
- Available on Linux, Windows, or in shells other than zsh
- Configurable via a `nerv config` command (edit TOML directly)

## Install

Alpha releases ship as signed-pending ARM-only tarballs via Homebrew tap:

```sh
brew tap nerv-sh/tap
brew install nerv
eval "$(nerv init zsh)"
nerv start
```

`brew services start nerv` keeps `nervd` alive across restarts (optional).
Run `nerv doctor` to verify the install.

> macOS Gatekeeper will warn on first launch — Apple Developer ID signing
> arrives in a later alpha (M0-8). Until then, allow the binary in
> System Settings → Privacy & Security.

To leave:

```sh
nerv uninstall
```

It removes the `~/.zshrc` block, the daemon, caches, and configs. We treat *trace zero* as a release-blocking acceptance criterion — see [`docs/uninstall-spec.md`](./docs/uninstall-spec.md).

## Repository layout

```
crates/
  nerv-cli/        # `nerv` binary (clap subcommands)
  nerv-daemon/     # `nervd` background process (tokio + UDS)
  nerv-engine/     # parser, ranking, IPC types
  nerv-shell/      # zsh init script generator
build/
  spec-transpile/  # withfig TS specs -> JSON build tool (swc)
shell-integrations/zsh/_nerv.zsh   # ZLE widget
specs-prebuilt/                    # build artifacts (release only, not committed)
vendor/withfig-autocomplete/       # git subtree (MIT, version-pinned)
docs/                              # acceptance criteria written before the code
```

## Documents

The acceptance criteria are intentionally written before any production code, so the implementation has a target instead of a vibe:

- [`PLAN.md`](./PLAN.md) — product plan, scope, roadmap (v0.5)
- [`docs/uninstall-spec.md`](./docs/uninstall-spec.md) — the trace-zero uninstall contract
- [`docs/error-states.md`](./docs/error-states.md) — five auto-detected error UX cases
- [`docs/terminal-compat.md`](./docs/terminal-compat.md) — guaranteed and best-effort terminals
- [`docs/first-5-min.md`](./docs/first-5-min.md) — install + 12-step usage scenario
- [`docs/spec-conversion-policy.md`](./docs/spec-conversion-policy.md) — TS spec → static JSON policy

## Try it locally (isolated, reversible)

The repo ships an end-to-end smoke harness that spawns a clean zsh session — no oh-my-zsh, no Amazon Q, no plugins from your real `~/.zshrc` — so you can confirm nerv works on your machine without touching your real shell config.

```sh
./scripts/e2e-isolated.sh
```

What it does (printed step by step):

1. Builds `nerv` + `nervd` in release mode (skipped if up-to-date; set `NERV_SKIP_BUILD=1` to skip).
2. Installs the 715-spec gzipped cache into `~/Library/Caches/nerv/specs/` (skipped if already present; set `NERV_SKIP_SPECS=1` to skip).
3. Restarts the daemon so it picks up the latest binary + specs.
4. Writes a minimal `.zshrc` into `/tmp/nerv-test/` (no plugins, no Q).
5. `exec`s into the test shell with `ZDOTDIR` pointed at the isolated dir, prints the scenario checklist.

Inside the isolated shell, the scenarios to try are:

| Command | Expected |
|---|---|
| `cd <Tab>` | Folder list, folders only, 10 visible + `[k/N]` counter when overflowing. |
| `cd t<Tab>` | Folders starting with `t`. |
| `cd <Tab> ↓↓↓` | Arrows cycle, wrap at top/bottom. |
| `git co<Tab>` | `checkout`, `commit` (prefix match — `co` is not "starts with c+o" of `checkout`). |
| `git commit --<Tab>` | Long options only. |
| `yarn <Tab>` in a dir with `package.json` | Subcommands **+** scripts merged. |
| `yarn run <Tab>` | Scripts only. |
| `z <Tab>` | zsh-z / zoxide history (if `~/.z` or zoxide installed). |
| Top suggestion in dim grey after cursor | Inline ghost text (Right-Arrow accepts at end of line). |
| Repeat-pick same item | On the next request it floats to the top (frecency). |

Exit with `exit` (or close the tab). Your real `~/.zshrc` is untouched. The `/tmp/nerv-test/` dir can be deleted at any time.

## Contributing

The project is in M0 spike phase. Issues and discussions are welcome at <https://github.com/nerv-sh/nerv/issues>. Sign-off (DCO) is required on commits.

## License

Apache-2.0. See [`LICENSE`](./LICENSE).

Bundled `withfig/autocomplete` content is MIT-licensed; see [`NOTICE`](./NOTICE).
