# _nerv-pty.bash — bash bootstrap for the figterm-style PTY shim.
#
# bash has no ZLE, so inline autocomplete reaches bash ONLY through the
# PTY path (PLAN §5.8 / §6.2). Activated when the user exports
# `NERV_PTY=1` before launching bash:
#
#   - Top-level (no NERV_PTY_SESSION_ID): re-exec the current shell under
#     `nerv-pty`, which spawns a shadow terminal and renders ghost/popup.
#   - Inner shell (NERV_PTY_SESSION_ID set by nerv-pty before it execs):
#     install the figterm-style OSC 697 prompt markers so the shadow
#     terminal can see the edit buffer. Mirrors _nerv-pty.zsh.
#
# Markers use bash's `\[ \]` non-printing brackets (the equivalent of
# zsh's `%{ %}`) so prompt width accounting stays correct.

# ---- Inner shell: install OSC 697 markers ---------------------------------
# nerv-pty sets NERV_PTY_SESSION_ID before it execs us, so this branch runs
# inside the shadow-wrapped shell. Checked FIRST (before the NERV_PTY opt-in
# guard) because the marker path must run whenever we're the wrapped shell,
# regardless of whether NERV_PTY is still exported. Mirrors _nerv-pty.zsh.
if [[ -n "${NERV_PTY_SESSION_ID-}" ]]; then
  # Re-entry guard: PROMPT_COMMAND / sourcing can run this twice.
  if [[ -n "${_NERV_PTY_PROMPT_SET-}" ]]; then
    return 0
  fi
  _NERV_PTY_PROMPT_SET=1

  _nerv_pty_osc() { printf '\033]697;%s\007' "$1"; }

  # NewCmd carries the session id so the shadow term can correlate the
  # prompt with this wrapper instance.
  _NERV_PTY_SP=$'\[\033]697;StartPrompt\007\]'
  _NERV_PTY_EP=$'\[\033]697;EndPrompt\007\]'
  _NERV_PTY_NC=$'\[\033]697;NewCmd='"${NERV_PTY_SESSION_ID}"$'\007\]'

  # precmd-equivalent (runs via PROMPT_COMMAND before each prompt):
  #   1. Emit the shell context markers. `Shell=bash` is REQUIRED — the
  #      shadow term gates its edit-buffer reads on a recognized shell
  #      (can_send_edit_buffer), so without it no ghost/popup ever shows.
  #   2. Wrap PS1 with the Start/End/NewCmd markers bracketing the
  #      editable region. Mirrors _nerv-pty.zsh's precmd.
  #   3. Arm the PreExec gate: mark that a prompt has been shown and that
  #      no command has run yet this cycle.
  __nerv_pty_precmd() {
    _nerv_pty_osc "Shell=bash"
    _nerv_pty_osc "Dir=$PWD"
    _nerv_pty_osc "TTY=$(tty 2>/dev/null)"
    _nerv_pty_osc "PID=$$"
    case "$PS1" in
      *697*) ;; # already wrapped
      *) PS1="${_NERV_PTY_SP}${PS1}${_NERV_PTY_EP}${_NERV_PTY_NC}" ;;
    esac
    # Set the gate LAST so the DEBUG trap firing for precmd's own
    # statements (above) is suppressed.
    _NERV_PTY_PROMPT_SHOWN=1
    _NERV_PTY_PREEXEC_DONE=
  }

  # PreExec via a *gated* DEBUG trap (the figterm "command submitted"
  # signal). A naive trap fires during shell startup — before the first
  # prompt — and wedges the shadow term in "executing" state, killing the
  # ghost. Two guards prevent that:
  #   - _NERV_PTY_PROMPT_SHOWN: stays empty until the first precmd runs,
  #     so nothing fires during rc sourcing / startup.
  #   - _NERV_PTY_PREEXEC_DONE: fire at most once per command line; reset
  #     by precmd. While precmd runs it is still set (from the previous
  #     command), so the trap stays quiet for precmd's own statements.
  _NERV_PTY_PROMPT_SHOWN=
  _NERV_PTY_PREEXEC_DONE=1
  __nerv_pty_debug() {
    [[ -n "${COMP_LINE-}" ]] && return            # tab-completion, not a command
    [[ -z "${_NERV_PTY_PROMPT_SHOWN-}" ]] && return
    [[ -n "${_NERV_PTY_PREEXEC_DONE-}" ]] && return
    _NERV_PTY_PREEXEC_DONE=1
    _nerv_pty_osc PreExec
  }

  # Prepend so we don't clobber a user's existing PROMPT_COMMAND.
  if [[ -n "${PROMPT_COMMAND-}" ]]; then
    PROMPT_COMMAND="__nerv_pty_precmd; ${PROMPT_COMMAND}"
  else
    PROMPT_COMMAND="__nerv_pty_precmd"
  fi
  trap '__nerv_pty_debug' DEBUG

  return 0
fi

# ---- Top-level: re-exec under nerv-pty ------------------------------------
# Only meaningful when the user opted in via NERV_PTY=1.
if [[ -z "${NERV_PTY-}" ]]; then
  return 0 2>/dev/null || exit 0
fi

# Don't shim non-interactive shells (scripts, pipes) — only a real TTY
# benefits and re-execing a script shell would break it.
case "$-" in
  *i*) ;;          # interactive — proceed
  *) return 0 ;;   # non-interactive — leave as-is
esac
[[ -t 1 ]] || return 0

# Locate the nerv-pty binary. Prefer the NERV_PTY_BIN override exported by
# `nerv init bash`, then fall back to PATH.
__NERV_PTY_BIN="${NERV_PTY_BIN:-nerv-pty}"
if ! command -v "$__NERV_PTY_BIN" >/dev/null 2>&1; then
  printf '%s\n' "[nerv] NERV_PTY=1 set but nerv-pty binary not found — falling back to no shim." >&2
  printf '%s\n' "[nerv] Install nerv-pty (it ships with nerv >= 0.2) or unset NERV_PTY." >&2
  return 0
fi

# Autostart nervd before handing over — the shim talks to the same UDS.
# `nerv start` is idempotent (socket probe): quiet no-op when a daemon
# already serves. The CLI lives next to nerv-pty; fall back to PATH.
__NERV_CLI="${__NERV_PTY_BIN%/*}/nerv"
[[ -x "$__NERV_CLI" ]] || __NERV_CLI=nerv
( "$__NERV_CLI" start >/dev/null 2>&1 & ) 2>/dev/null

# Hand control to nerv-pty, which sets NERV_PTY_SESSION_ID and re-execs
# this shell under its shadow terminal.
exec "$__NERV_PTY_BIN" -- "$SHELL"
