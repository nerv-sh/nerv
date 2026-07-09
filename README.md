# Nerv

> Inline shell autocomplete for macOS zsh — without the AWS login.

Nerv is a single-binary recreation of [Fig](https://fig.io)'s killer feature: real-time, IDE-like autocomplete for `git`, `docker`, `kubectl`, `npm`, and 50+ other CLIs. After Fig was acquired by Amazon and folded into Amazon Q (`q`), the experience got buried under mandatory Builder ID login, AI chat, agents, and a multi-hundred-megabyte bundle.

Nerv brings back the original promise: **press a key, see the next token. That's it.**

## Status

**Alpha** — the M1 4-week checkpoint passed: a 715-spec completion engine, a ZLE widget for zsh, an opt-in PTY path for bash/fish, inline ghost text + popup, frecency ranking, and trace-zero uninstall all work end to end. All eight M0 deliverables are done (M0-8 settled as Homebrew-only distribution — no Developer ID notarization needed); a 2-week internal dogfooding window remains before v1.0. See [`PLAN.md`](./PLAN.md) (v0.6) for the full design and [`docs/`](./docs/) for the acceptance criteria written before the code.

## What it is

- **macOS** (v1.0). **zsh** uses a native ZLE widget; **bash** and **fish** work through an opt-in PTY shim (`NERV_PTY=1`). Linux and Windows are v1.x.
- **Static Fig spec compatibility** — uses the [`withfig/autocomplete`](https://github.com/withfig/autocomplete) repository (MIT) at build time. No runtime JS engine in the default build (a sandboxed `rquickjs` Tier C path exists only behind an opt-in feature flag).
- **Single binary**, installed via Homebrew. No Node, no daemon manager, no cloud.
- **Apache-2.0 licensed**, no telemetry, no auth.

## What it isn't (v1.0)

- AI / natural-language to command (this is intentional — see [`PLAN.md`](./PLAN.md) §4)
- Arbitrary dynamic completions backed by JavaScript closures (deferred — these need a JS runtime). Static shell-command generators *do* work: `git checkout <branch>`, `npm run <script>`, `cd <dir>`, `z <history>`, and well-known patterns (kubectl, docker, gh, aws) are recovered natively in Rust without a JS engine.
- Available on Linux or Windows
- Configurable via a `nerv config` command (edit `~/.config/nerv/nerv.toml` directly)

## Install

Alpha releases ship as ARM-only tarballs via Homebrew tap:

```sh
brew tap nerv-sh/tap
brew install nerv
eval "$(nerv init zsh)"
nerv start
```

`brew services start nerv` keeps `nervd` alive across restarts (optional).
Run `nerv doctor` to verify the install.

> Installing through Homebrew does **not** trip macOS Gatekeeper — `brew`
> doesn't quarantine what it downloads, and the ad-hoc signature the Rust
> toolchain applies is enough to run on Apple Silicon. Developer ID
> notarization is intentionally out of scope for v1.0 (PLAN §10 M0-8).
> If you instead download a release tarball directly in a browser,
> Gatekeeper will warn once — clear the quarantine flag with
> `xattr -dr com.apple.quarantine <path-to-nerv>`.

## Turn it off

Stop the daemon — completions go quiet, everything stays installed:

```sh
nerv stop
brew services stop nerv   # only if you started it as a service
```

The widget is still loaded, so the next keystroke prints a one-line hint that
the daemon isn't running. To keep it out of new shells entirely, comment out
the `eval` line inside the `# >>> nerv >>>` block in `~/.zshrc`, then
`exec zsh`. Uncomment to re-enable. Re-running `nerv init zsh` rewrites that
block and turns it back on.

To leave for good:

```sh
nerv uninstall
```

It removes the `~/.zshrc` block, the daemon, caches, and configs. We treat *trace zero* as a release-blocking acceptance criterion — see [`docs/uninstall-spec.md`](./docs/uninstall-spec.md).

## Repository layout

```
crates/
  nerv-cli/        # `nerv` binary (clap, 5 subcommands + hidden _complete IPC bridge)
  nerv-daemon/     # `nervd` background process (tokio + UDS, lazy SpecRegistry)
  nerv-engine/     # parser, spec loader, ranking, IPC types, schema gate
  nerv-shell/      # zsh init-script / marker-block generator
  nerv-pty/        # figterm-derived PTY shim (opt-in, NERV_PTY=1)
  nerv-term/       # alacritty-derived shadow terminal
  nerv-{ipc,proto,integrations,os,util,settings,log,diag}/  # absorbed Fig crates (brand-stripped)
shell-integrations/
  zsh/_nerv.zsh           # ZLE widget (default zsh path)
  {zsh,bash,fish}/_nerv-pty.*  # PTY bootstraps (NERV_PTY=1)
tools/ts-to-json/                  # bun-based withfig TS spec -> JSON converter
packaging/homebrew/nerv.rb         # Homebrew Formula template (auto-bumped on release)
vendor/withfig-autocomplete/       # git subtree (MIT, version-pinned)
vendor/aws-autocomplete/           # git subtree (Apache+MIT, absorbed Fig engine)
docs/                              # acceptance criteria written before the code
```

## Documents

The acceptance criteria are intentionally written before any production code, so the implementation has a target instead of a vibe:

- [`PLAN.md`](./PLAN.md) — product plan, scope, roadmap (v0.6)
- [`docs/uninstall-spec.md`](./docs/uninstall-spec.md) — the trace-zero uninstall contract
- [`docs/error-states.md`](./docs/error-states.md) — five auto-detected error UX cases
- [`docs/terminal-compat.md`](./docs/terminal-compat.md) — guaranteed and best-effort terminals
- [`docs/first-5-min.md`](./docs/first-5-min.md) — install + 12-step usage scenario
- [`docs/spec-conversion-policy.md`](./docs/spec-conversion-policy.md) — TS spec → static JSON policy
- [`docs/dogfood.md`](./docs/dogfood.md) — the M1 10-week internal dogfooding playbook

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

The project is in the M1 alpha / dogfooding phase. Issues and discussions are welcome at <https://github.com/nerv-sh/nerv/issues>. Sign-off (DCO) is required on commits.

## License

Apache-2.0. See [`LICENSE`](./LICENSE).

Bundled `withfig/autocomplete` content is MIT-licensed; see [`NOTICE`](./NOTICE).
