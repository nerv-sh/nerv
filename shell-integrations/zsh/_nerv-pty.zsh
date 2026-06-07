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

# Inner shell. NERV_PTY_SESSION_ID is set by nerv-pty before it execs
# us, so we must NOT re-spawn the wrapper. Instead this is where we
# install the figterm-style OSC 697 prompt markers that let the
# wrapper's shadow terminal locate the command line:
#
#   StartPrompt .. EndPrompt   bound the prompt region (so prompt text
#                              is excluded from the edit buffer)
#   NewCmd=<session>           marks a fresh command line
#   Shell=zsh / Dir=<pwd>      shell context (Shell gates completion)
#   PreExec                    a command started running (preview off)
#
# Without these the shadow terminal can't tell the prompt from the
# typed command and inline completion never fires. Ordering mirrors the
# upstream figterm zsh integration (NewCmd trails EndPrompt at the end
# of the prompt, right before user input).
if [[ -n "${NERV_PTY_SESSION_ID-}" ]]; then
  # Guard against double-wrapping the prompt if sourced more than once.
  if [[ -n "${_NERV_PTY_PROMPT_SET-}" ]]; then
    return 0
  fi
  typeset -g _NERV_PTY_PROMPT_SET=1

  autoload -Uz add-zsh-hook

  _nerv_pty_osc() { printf '\033]697;'"$1"'\007' "${@:2}"; }

  _nerv_pty_preexec() { _nerv_pty_osc PreExec; }
  _nerv_pty_precmd() {
    _nerv_pty_osc Shell=zsh
    _nerv_pty_osc "Dir=%s" "$PWD"
    _nerv_pty_osc "TTY=%s" "$TTY"
    _nerv_pty_osc "PID=%d" "$$"
  }
  add-zsh-hook preexec _nerv_pty_preexec
  add-zsh-hook precmd _nerv_pty_precmd

  typeset -g _NERV_PTY_SP=$'\033]697;StartPrompt\007'
  typeset -g _NERV_PTY_EP=$'\033]697;EndPrompt\007'
  typeset -g _NERV_PTY_NC=$'\033]697;NewCmd='"${NERV_PTY_SESSION_ID}"$'\007'
  if [[ -n "${PROMPT+x}" ]]; then
    PROMPT="%{${_NERV_PTY_SP}%}${PROMPT}%{${_NERV_PTY_EP}${_NERV_PTY_NC}%}"
  else
    PS1="%{${_NERV_PTY_SP}%}${PS1}%{${_NERV_PTY_EP}${_NERV_PTY_NC}%}"
  fi

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
