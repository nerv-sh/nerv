#!/usr/bin/env zsh
# _nerv-pty.zsh — opt-in figterm-style PTY shim.
#
# Activated when the user exports `NERV_PTY=1` before launching
# their terminal. The `nerv init zsh` emitter sources this file
# instead of `_nerv.zsh` in that case.
#
# Contract (PLAN.md §5.8 / CLAUDE.md §4 invariant):
#   - ZLE widget (_nerv.zsh) and this PTY shim are mutually
#     exclusive. Loading order is enforced by the cmd_init
#     emitter; _nerv.zsh also self-skips when NERV_PTY=1.
#   - Re-entry guard: if this script has already run in the
#     current shell, return early. The PTY wrapper re-execs zsh
#     under itself; the inner shell must not loop forever.
#   - NERV_PTY_SESSION_ID is set by the PTY wrapper before exec.
#     Its presence is the signal that we are the inner shell
#     and should NOT re-spawn ourselves.
#
# M1 scope: this script is the scaffold. The PTY wrapper binary
# (nerv-pty) does the heavy lifting — spawning a slave shell,
# intercepting input, talking to nervd. The shim emitted here
# only handles the bootstrap: detect, exec wrapper, restore TTY
# on exit.

# Re-entry guard. Set by nerv-pty before it execs us.
if [[ -n "${NERV_PTY_SESSION_ID-}" ]]; then
  return 0
fi

# Never run PTY shim under `eval` non-interactively (CI, scripts).
if [[ ! -t 0 || ! -t 1 ]]; then
  return 0
fi

# Locate the nerv-pty binary. Prefer NERV_PTY_BIN override (used
# by `nerv init zsh --shell-script` when nerv was launched from a
# non-PATH location like a CI worktree). Fall back to PATH lookup.
typeset -g __NERV_PTY_BIN="${NERV_PTY_BIN:-nerv-pty}"
if ! command -v "$__NERV_PTY_BIN" >/dev/null 2>&1; then
  print -u2 -- "[nerv] NERV_PTY=1 set but nerv-pty binary not found — falling back to no shim."
  print -u2 -- "[nerv] Install nerv-pty (it ships with nerv >= 0.2) or unset NERV_PTY."
  return 0
fi

# Hand the shell over to nerv-pty. The wrapper opens a PTY, sets
# NERV_PTY_SESSION_ID, and execs zsh again under itself — that
# inner zsh hits the re-entry guard above and skips this block.
exec "$__NERV_PTY_BIN" -- "$SHELL" "$@"
