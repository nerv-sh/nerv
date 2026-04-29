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

## Install (when v1.0 ships)

```sh
brew install nerv-sh/tap/nerv
eval "$(nerv init zsh)"
```

That's the whole installation. To leave:

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

## Contributing

The project is in M0 spike phase. Issues and discussions are welcome at <https://github.com/nerv-sh/nerv/issues>. Sign-off (DCO) is required on commits.

## License

Apache-2.0. See [`LICENSE`](./LICENSE).

Bundled `withfig/autocomplete` content is MIT-licensed; see [`NOTICE`](./NOTICE).
