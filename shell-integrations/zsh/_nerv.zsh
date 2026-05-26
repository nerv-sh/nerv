#!/usr/bin/env zsh
# _nerv.zsh — Nerv ZLE widget for inline autocomplete.
#
# Hybrid: zle -R manages space & cursor, raw ANSI overwrites colors.
# 1. zle -R "" "plain1" "plain2" ... → ZLE creates space, positions cursor
# 2. \e7 saves cursor (now correct)
# 3. \e[B to status area, overwrite with colored text
# 4. \e8 restores cursor

if (( ${+__NERV_LOADED} )); then return 0; fi
typeset -g __NERV_LOADED=1
typeset -g __NERV_BIN="${NERV_BIN:-nerv}"
typeset -g __NERV_PREV_LBUFFER=""
typeset -gi __NERV_E1_SHOWN=0
typeset -gi __NERV_SELECTED=1
typeset -ga __NERV_ITEMS=()
typeset -gi __NERV_ACTIVE=0
typeset -gi __NERV_WIDTH=46
typeset -gi __NERV_PASTING=0

__nerv_show_popup() {
  local -a items=("$@")
  local total=${#items}
  # Max visible rows. 10 is a reasonable default — taller popups
  # eat too much vertical real estate; cycling slides the window.
  local MAX_VIS=10
  local visible=$total
  (( visible > MAX_VIS )) && visible=$MAX_VIS

  # Slide the rendered window so the selected item stays visible.
  # When the user cycles Tab past the bottom of the window, scroll.
  local start=1
  if (( total > visible )); then
    start=$(( __NERV_SELECTED - visible + 1 ))
    (( start < 1 )) && start=1
    local last_possible_start=$(( total - visible + 1 ))
    (( start > last_possible_start )) && start=$last_possible_start
  fi
  local end=$(( start + visible - 1 ))

  local sel_line="${items[$__NERV_SELECTED]}"
  local sel_desc="${sel_line#*	}"; sel_desc="${sel_desc#*	}"

  # Auto-size: measure max display + max desc across ALL items
  # (not just the window), so window-sliding doesn't reshape the
  # popup width every tick.
  local i max_disp=0 max_desc=0
  for (( i=1; i<=total; i++ )); do
    local line="${items[$i]}"
    local rest="${line#*	}"
    local d="${rest%%	*}"
    local desc_full="${rest#*	}"
    (( ${#d} > max_disp )) && max_disp=${#d}
    (( ${#desc_full} > max_desc )) && max_desc=${#desc_full}
  done

  # Hard caps so a long description doesn't blow the popup off-screen.
  (( max_disp > 32 )) && max_disp=32
  local term_cols=${COLUMNS:-80}
  local cap=$(( term_cols * 8 / 10 ))
  (( cap < 30 )) && cap=30

  # Footer-desc width hint: clamp footer-desc to a generous reach so
  # the popup body width is mainly driven by display names, not by an
  # unusually long description.
  local foot_hint=$max_desc
  (( foot_hint > 60 )) && foot_hint=60

  # Layout: " $ "(4) + display + " "(1)
  local body=$(( 4 + max_disp + 1 ))
  # Footer needs " " + desc + " " (= foot_hint + 2). Pick whichever
  # is wider so neither row wraps.
  local W=$body
  (( foot_hint + 2 > W )) && W=$(( foot_hint + 2 ))
  (( W > cap )) && W=$cap
  (( W < __NERV_WIDTH )) && W=$__NERV_WIDTH

  # hbar fills the cells BETWEEN the corner glyphs (╭…╮ / ├…┤ /
  # ╰…╯). Each border row is W cells total; corners take 2 cells,
  # so hbar must be W-2. Off-by-2 bug here previously made the box
  # 2 columns wider than the item rows, so the right `│` of rows
  # appeared visually clipped against the wider border above.
  local hbar=""
  local hbar_n=$(( W - 2 ))
  (( hbar_n < 0 )) && hbar_n=0
  local j; for (( j=0; j<hbar_n; j++ )); do hbar+="─"; done

  # --- Build plain-text lines for zle -R (space reservation) ---
  local -a plain=()
  local blank=""
  for (( j=0; j<W+4; j++ )); do blank+=" "; done
  local plain_rows=$(( visible + 4 ))
  for (( j=0; j<plain_rows; j++ )); do plain+=("$blank"); done

  # --- Build colored lines ---
  local R=$'\e[0m'
  local BG=$'\e[48;5;236m' BDR=$'\e[38;5;240m'
  local ITEM=$'\e[38;5;252m' DESC=$'\e[38;5;244m'
  local SELBG=$'\e[48;5;62m' SELFG=$'\e[38;5;255m\e[1m'
  local ICON=$'\e[38;5;141m'

  local -a colored=()

  colored+=("  ${BG}${BDR}╭${hbar}╮${R}")

  # Row body width (between the two vertical bars): everything past
  # " │" on the left and " │" on the right. Equals W - 2.
  local row_body=$(( W - 2 ))

  for (( i=start; i<=end; i++ )); do
    local line="${items[$i]}"
    local rest="${line#*	}"
    local display="${rest%%	*}"
    (( ${#display} > max_disp )) && display="${display:0:$max_disp}"

    # Layout inside one row: " $ display<padding>"
    # ($ icon takes 2 cols including space after.) Right-pad with
    # spaces to fill row_body so the right vertical bar lines up.
    local visible_chars=$(( 3 + ${#display} ))  # " $ " + display
    local pad_n=$(( row_body - visible_chars ))
    (( pad_n < 0 )) && pad_n=0
    local row_pad=""
    for (( j=0; j<pad_n; j++ )); do row_pad+=" "; done

    if (( i == __NERV_SELECTED )); then
      colored+=("  ${SELBG}${BDR}│${SELBG} ${ICON}\$${SELFG} ${display}${row_pad}${BDR}│${R}")
    else
      colored+=("  ${BG}${BDR}│${BG} ${ICON}\$${ITEM} ${display}${row_pad}${BDR}│${R}")
    fi
  done

  colored+=("  ${BG}${BDR}├${hbar}┤${R}")

  # Footer: " desc … [n/total]" — right-side counter shows the
  # current position within the full list so users know there's
  # more below / above when the window is sliding.
  # Content inside `│...│` must equal W-2 cells (matches the
  # body rows above). Layout: " " + desc + pad + counter + " ".
  local counter=""
  (( total > visible )) && counter="[${__NERV_SELECTED}/${total}]"
  # Reserve cells for: leading " ", trailing " ", counter.
  local sel_avail=$(( W - 4 - ${#counter} ))
  (( sel_avail < 0 )) && sel_avail=0
  (( ${#sel_desc} > sel_avail )) && sel_desc="${sel_desc:0:$sel_avail}"
  local fpad=$(( W - 4 - ${#sel_desc} - ${#counter} ))
  (( fpad < 0 )) && fpad=0
  local fps=""; for (( j=0; j<fpad; j++ )); do fps+=" "; done
  colored+=("  ${BG}${BDR}│${DESC} ${sel_desc}${fps}${counter} ${BDR}│${R}")

  colored+=("  ${BG}${BDR}╰${hbar}╯${R}")

  # Step 1: ZLE creates space and positions cursor correctly
  zle -R "" "${plain[@]}"

  # Step 2-4: Save cursor, overwrite with colors, restore cursor
  local buf=$'\e7'
  for (( i=1; i<=${#colored}; i++ )); do
    buf+=$'\e[B\e[G'"${colored[$i]}"$'\e[K'
  done
  buf+=$'\e8'
  printf '%s' "$buf"

  __NERV_ACTIVE=1
}

__nerv_hide_popup() {
  POSTDISPLAY=''
  (( ! __NERV_ACTIVE )) && return
  # Clear raw ANSI remnants, then let ZLE clean up status lines
  printf '%s' $'\e7\e[B\e[G\e[J\e8'
  __NERV_ACTIVE=0
  __NERV_SELECTED=1
  __NERV_ITEMS=()
  zle -R ""
}

__nerv_insert_selected() {
  (( ${#__NERV_ITEMS} == 0 )) && return 1
  local sel_idx=$__NERV_SELECTED
  (( sel_idx < 1 )) && sel_idx=1
  (( sel_idx > ${#__NERV_ITEMS} )) && sel_idx=1
  local sel_line="${__NERV_ITEMS[$sel_idx]}"
  local insertion="${sel_line%%	*}"
  [[ -z "$insertion" ]] && return 1

  # Compute new BUFFER + CURSOR from scratch. Avoid LBUFFER/RBUFFER
  # split because some plugins (zsh-autosuggestions) wrap those
  # accessors and the assignments don't always propagate.
  local before="$LBUFFER"
  local after="$RBUFFER"

  # Strip trailing partial word from `before` (the word the user
  # was completing).
  local pre
  if [[ "$before" == *' '* ]]; then
    pre="${before% *} "
  else
    pre=""
  fi

  # Strip leading partial word from `after` (rest of the same word
  # when cursor is mid-token).
  local post="$after"
  if [[ -n "$after" && "$after[1]" != ' ' && "$after[1]" != $'\t' ]]; then
    local rest="${after%%[[:space:]]*}"
    post="${after#$rest}"
  fi

  # Build full BUFFER and place CURSOR right after the insertion+space.
  BUFFER="${pre}${insertion} ${post# }"
  CURSOR=$(( ${#pre} + ${#insertion} + 1 ))

  # Best-effort: clear zsh-autosuggestions ghost overlay.
  if (( ${+POSTDISPLAY} )); then
    POSTDISPLAY=''
  fi

  # Clear popup area + reset internal state.
  printf '%s' $'\e7\e[B\e[G\e[J\e8'
  __NERV_PREV_LBUFFER="$LBUFFER"
  __NERV_ACTIVE=0
  __NERV_SELECTED=1
  __NERV_ITEMS=()
  zle -R ""
  zle reset-prompt 2>/dev/null
  zle redisplay 2>/dev/null
  return 0
}

# ---------------------------------------------------------------------------
# Core widget
# ---------------------------------------------------------------------------
__nerv_complete() {
  (( __NERV_PASTING )) && return
  [[ "$LBUFFER" == "$__NERV_PREV_LBUFFER" ]] && return
  __NERV_PREV_LBUFFER="$LBUFFER"
  __NERV_SELECTED=1

  [[ -z "${LBUFFER// /}" ]] && { __nerv_hide_popup; return; }
  [[ "$LBUFFER" != *" "* ]] && { __nerv_hide_popup; return; }

  local resp
  resp=$("$__NERV_BIN" _complete "$LBUFFER" $CURSOR 2>/dev/null) || {
    if (( ! __NERV_E1_SHOWN )); then
      __NERV_E1_SHOWN=1
      zle -R "[nerv] daemon not running — run: nerv start"
      __NERV_ACTIVE=1
    fi
    return
  }

  local -a rlines=("${(@f)resp}")
  rlines=("${(@)rlines:#}")
  (( ${#rlines} == 0 )) && { __nerv_hide_popup; return; }

  __NERV_ITEMS=("${rlines[@]}")
  __nerv_set_ghost
  __nerv_show_popup "${rlines[@]}"
}

# Set POSTDISPLAY to the trailing portion of the top suggestion that
# the user hasn't typed yet. Accepted with Right-Arrow at end of
# buffer. Cleared on every other widget that mutates the buffer.
__nerv_set_ghost() {
  POSTDISPLAY=''
  (( ${#__NERV_ITEMS} == 0 )) && return
  local top="${__NERV_ITEMS[1]}"
  local top_ins="${top%%	*}"
  [[ -z "$top_ins" ]] && return
  # Current word = last whitespace-separated token of LBUFFER.
  local prefix="${LBUFFER##* }"
  # Ghost only when top insertion extends the current word — never
  # for sideways matches (alias completions, fuzzy-style hits).
  [[ "$top_ins" == "$prefix"* ]] || return
  [[ "$top_ins" == "$prefix" ]] && return
  POSTDISPLAY="${top_ins#$prefix}"
}
zle -N __nerv_complete

# ---------------------------------------------------------------------------
# Hooks
# ---------------------------------------------------------------------------
zle -A self-insert __nerv_orig_self_insert 2>/dev/null
__nerv_self_insert() { zle __nerv_orig_self_insert "$@"; __nerv_complete; }
zle -N self-insert __nerv_self_insert

__nerv_space() { LBUFFER+=" "; __nerv_complete; }
zle -N __nerv_space
bindkey ' ' __nerv_space

zle -A backward-delete-char __nerv_orig_backward_delete_char 2>/dev/null
__nerv_backward_delete() { zle __nerv_orig_backward_delete_char "$@"; __nerv_complete; }
zle -N backward-delete-char __nerv_backward_delete

# Enter: select if popup, else execute
__nerv_line_finish() {
  if (( __NERV_ACTIVE && ${#__NERV_ITEMS} > 0 )); then
    __nerv_insert_selected
  else
    (( __NERV_ACTIVE )) && { __NERV_ACTIVE=0; zle -R ""; }
    __NERV_PREV_LBUFFER=""
    __NERV_SELECTED=1
    __NERV_ITEMS=()
    zle .accept-line
  fi
}
zle -N accept-line __nerv_line_finish

# Tab: cycle DOWN through popup items (Fig-style). Enter accepts.
# Single-item popup short-circuits — cycling 1→1 is useless, so we
# insert immediately (the user clearly wants that one suggestion).
# Outside popup: defer to zsh's expand-or-complete.
#
# Only checks __NERV_ITEMS, NOT __NERV_ACTIVE: cursor-movement
# keys don't clear ITEMS but may leave ACTIVE stale, and we'd
# rather accept than appear no-op.
__nerv_accept() {
  if (( ${#__NERV_ITEMS} > 0 )); then
    if (( ${#__NERV_ITEMS} == 1 )); then
      __nerv_insert_selected
      return
    fi
    local max=${#__NERV_ITEMS}
    if (( __NERV_SELECTED < max )); then
      (( __NERV_SELECTED++ ))
    else
      __NERV_SELECTED=1  # cycle wrap
    fi
    __nerv_show_popup "${__NERV_ITEMS[@]}"
  else
    zle expand-or-complete
  fi
}
zle -N __nerv_accept
bindkey '^I' __nerv_accept

# Shift-Tab: cycle UP. Falls back to reverse-menu-complete outside Nerv.
__nerv_accept_back() {
  if (( __NERV_ACTIVE && ${#__NERV_ITEMS} > 0 )); then
    local max=${#__NERV_ITEMS}
    if (( __NERV_SELECTED > 1 )); then
      (( __NERV_SELECTED-- ))
    else
      __NERV_SELECTED=$max  # cycle wrap
    fi
    __nerv_show_popup "${__NERV_ITEMS[@]}"
  else
    zle reverse-menu-complete 2>/dev/null || zle expand-or-complete
  fi
}
zle -N __nerv_accept_back
bindkey '^[[Z' __nerv_accept_back

# Arrow keys: cycle through items (down at last → first, up at
# first → last). Matches user expectation; differs from old Fig
# behavior (which stopped at edges) per direct user feedback.
__nerv_select_down() {
  if (( __NERV_ACTIVE && ${#__NERV_ITEMS} > 0 )); then
    local max=${#__NERV_ITEMS}
    if (( __NERV_SELECTED < max )); then
      (( __NERV_SELECTED++ ))
    else
      __NERV_SELECTED=1
    fi
    __nerv_show_popup "${__NERV_ITEMS[@]}"
  else
    zle down-line-or-history
  fi
}
zle -N __nerv_select_down

__nerv_select_up() {
  if (( __NERV_ACTIVE && ${#__NERV_ITEMS} > 0 )); then
    local max=${#__NERV_ITEMS}
    if (( __NERV_SELECTED > 1 )); then
      (( __NERV_SELECTED-- ))
    else
      __NERV_SELECTED=$max
    fi
    __nerv_show_popup "${__NERV_ITEMS[@]}"
  else
    zle up-line-or-history
  fi
}
zle -N __nerv_select_up

bindkey $'\e[B' __nerv_select_down
bindkey $'\eOB' __nerv_select_down
bindkey $'\e[A' __nerv_select_up
bindkey $'\eOA' __nerv_select_up

# Right-Arrow: accept ghost text (POSTDISPLAY) when at end of line.
# Falls back to plain forward-char in the middle of the buffer or
# when there's no ghost — matches user expectation for cursor
# movement inside an existing edit.
__nerv_accept_ghost() {
  if (( ${+POSTDISPLAY} )) && [[ -n "$POSTDISPLAY" ]] && [[ -z "$RBUFFER" ]]; then
    LBUFFER+="$POSTDISPLAY"
    POSTDISPLAY=''
    __NERV_PREV_LBUFFER="$LBUFFER"
    __nerv_hide_popup
  else
    zle forward-char
  fi
}
zle -N __nerv_accept_ghost
bindkey $'\e[C' __nerv_accept_ghost
bindkey $'\eOC' __nerv_accept_ghost

__nerv_dismiss() { __nerv_hide_popup; __NERV_PREV_LBUFFER=""; POSTDISPLAY=''; }
zle -N __nerv_dismiss
bindkey '^G' __nerv_dismiss

# Bracketed paste: suppress completions during paste
__nerv_bracketed_paste() {
  __NERV_PASTING=1
  zle .bracketed-paste "$@"
  __NERV_PASTING=0
  __NERV_PREV_LBUFFER="$LBUFFER"
}
zle -N bracketed-paste __nerv_bracketed_paste

# ---------------------------------------------------------------------------
# Late re-binding via precmd hook — defends against plugins that bind ^I
# AFTER us (fzf-tab, zsh-autocomplete, oh-my-zsh complete-on-tab).
# Runs every prompt; cheap idempotent reassert.
# ---------------------------------------------------------------------------
__nerv_rebind() {
  # Tab / Shift-Tab / Ctrl-G — last writer wins; reassert ours.
  bindkey -M main '^I'    __nerv_accept       2>/dev/null
  bindkey -M main '^[[Z'  __nerv_accept_back  2>/dev/null
  bindkey -M main '^G'    __nerv_dismiss      2>/dev/null
  bindkey -M main ' '     __nerv_space        2>/dev/null

  # zle widgets that competing inline-suggestion plugins (Amazon Q,
  # Fig, fzf-tab, zsh-autosuggestions) re-bind during their own
  # post-init. The last `zle -N self-insert <name>` wins, so we
  # re-claim every prompt.
  zle -N self-insert          __nerv_self_insert      2>/dev/null
  zle -N backward-delete-char __nerv_backward_delete  2>/dev/null
  zle -N accept-line          __nerv_line_finish      2>/dev/null
}
autoload -Uz add-zsh-hook 2>/dev/null && add-zsh-hook precmd __nerv_rebind
