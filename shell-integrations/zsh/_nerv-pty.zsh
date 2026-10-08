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

# Only meaningful when the user opted in via NERV_PTY=1 (mirrors the
# bash/fish top-level guards). The `nerv init` emitter sources this file
# instead of `_nerv.zsh` in that case, but a direct source without the
# opt-in must stay a no-op — never re-exec a shell the user didn't ask
# to wrap. Checked AFTER the inner-shell branch above: the marker path
# must run whenever we're the wrapped shell, regardless of NERV_PTY.
if [[ -z "${NERV_PTY-}" ]]; then
  return 0
fi

# Don't shim non-interactive shells (scripts, pipes) — only a real
# interactive shell benefits, and re-execing a script shell would
# break it. Mirrors the bash `$-` and fish `status is-interactive`
# checks.
[[ -o interactive ]] || return 0

# Locate the nerv-pty binary. Prefer NERV_PTY_BIN override (used
# by `nerv init zsh --shell-script` when nerv was launched from a
# non-PATH location like a CI worktree). Fall back to PATH lookup.
typeset -g __NERV_PTY_BIN="${NERV_PTY_BIN:-nerv-pty}"
if ! command -v "$__NERV_PTY_BIN" >/dev/null 2>&1; then
  print -u2 -- "[nerv] NERV_PTY=1 but nerv-pty not found — falling back to no shim (unset NERV_PTY or install nerv-pty)."
  return 0
fi

# Autostart nervd before handing over — the shim's ghost/popup talk to
# the same UDS. `nerv start` is idempotent (socket probe), so this is a
# quiet no-op when a daemon already serves. The CLI lives next to
# nerv-pty; fall back to PATH. NERV_AUTOSTART=0 opts out, same as
# _nerv.zsh.
if [[ "${NERV_AUTOSTART:-1}" != "0" ]]; then
  typeset -g __NERV_CLI="${__NERV_PTY_BIN:h}/nerv"
  [[ -x "$__NERV_CLI" ]] || __NERV_CLI=nerv
  ( "$__NERV_CLI" start >/dev/null 2>&1 & ) 2>/dev/null
fi

# Hand the shell over to nerv-pty. The wrapper opens a PTY, sets
# NERV_PTY_SESSION_ID, and execs zsh again under itself — that
# inner zsh hits the re-entry guard above and skips this block.
# Re-exec zsh, not the login `$SHELL`: a bash-login user who runs
# `zsh` must land back in zsh. zsh exposes no path to its own binary,
# so keep `$SHELL` when it is a zsh and otherwise resolve zsh on PATH.
typeset -g __NERV_SELF_SHELL="$SHELL"
[[ "${SHELL:t}" == zsh ]] || __NERV_SELF_SHELL="${commands[zsh]:-zsh}"
exec "$__NERV_PTY_BIN" -- "$__NERV_SELF_SHELL" "$@"
