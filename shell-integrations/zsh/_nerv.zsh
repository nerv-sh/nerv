#!/usr/bin/env zsh
# _nerv.zsh — Nerv ZLE widget for inline autocomplete.
#
# Loaded via `eval "$(nerv init zsh --shell-script)"`. The marker block in
# the user's ~/.zshrc invokes that command (see docs/uninstall-spec.md §3).
#
# This file is the M0-1 starting point. It will:
#   1. Connect to nervd over UDS.
#   2. Send {method:"complete", line:$LBUFFER, cursor:$CURSOR} on each redraw.
#   3. Render the response as an inline ANSI popup beneath the input line
#      (terminal-compat.md §4 — no alternate screen, raw cursor save/restore).
#
# Implementation budget: ≤ 30 lines of meaningful zsh + a tiny coproc helper.
# Anything more belongs in nervd, not here.

# Guard against being sourced twice.
if (( ${+__NERV_LOADED} )); then
  return 0
fi
typeset -g __NERV_LOADED=1

# Path to the nerv binary (set by nerv-cli at init time).
typeset -g __NERV_BIN="${NERV_BIN:-/opt/homebrew/bin/nerv}"

# Socket path (matches nerv-daemon::cache_dir + SOCKET_NAME).
typeset -g __NERV_SOCK="${HOME}/Library/Caches/nerv/nervd.sock"

# Debounce: suppress duplicate dynamic-hint messages (PLAN.md §5.1).
typeset -g __NERV_LAST_HINT_AT=0

# ZLE widget — fires on every line redraw.
__nerv_complete() {
  # Stub. M0-1 PoC will:
  #   - read $LBUFFER and $CURSOR
  #   - write a JSON request to $__NERV_SOCK
  #   - read the response, render it
  #   - never block the user's input loop
  zle -R
}
zle -N __nerv_complete

# Hook into ZLE's pre-redraw event.
__nerv_zle_pre_redraw() {
  __nerv_complete
}
zle -N zle-line-pre-redraw __nerv_zle_pre_redraw 2>/dev/null || true

# Tab key — accept the current top suggestion (M0-1 wires this up).
# Esc — close the inline popup.
# ? on a focused suggestion — show description (PLAN.md §5.2).
#
# Bindings live in `nerv-cli init zsh --shell-script` so they can be
# disabled via config without editing this file.
