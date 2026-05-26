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
  local count=${#items}
  (( count > 5 )) && count=5

  local sel_line="${items[$__NERV_SELECTED]}"
  local sel_desc="${sel_line#*	}"; sel_desc="${sel_desc#*	}"

  # Auto-size: measure max display + max desc across visible items.
  local i max_disp=0 max_desc=0
  for (( i=1; i<=count; i++ )); do
    local line="${items[$i]}"
    local rest="${line#*	}"
    local d="${rest%%	*}"
    local desc_full="${rest#*	}"
    (( ${#d} > max_disp )) && max_disp=${#d}
    (( ${#desc_full} > max_desc )) && max_desc=${#desc_full}
  done

  # Hard caps so a long description doesn't blow the popup off-screen.
  (( max_disp > 24 )) && max_disp=24
  local term_cols=${COLUMNS:-80}
  local cap=$(( term_cols * 8 / 10 ))
  (( cap < 30 )) && cap=30

  # Layout: " $ "(4) + display + gap(2) + desc + " "(1)
  local W=$(( 4 + max_disp + 2 + max_desc + 1 ))
  (( W > cap )) && W=$cap
  (( W < __NERV_WIDTH )) && W=$__NERV_WIDTH

  # desc gets whatever's left after the display column.
  local desc_avail=$(( W - 4 - max_disp - 2 - 1 ))
  (( desc_avail < 0 )) && desc_avail=0

  local hbar=""
  local j; for (( j=0; j<W; j++ )); do hbar+="─"; done

  # --- Build plain-text lines for zle -R (space reservation) ---
  local -a plain=()
  local blank=""
  for (( j=0; j<W+4; j++ )); do blank+=" "; done
  local total=$(( count + 4 ))
  for (( j=0; j<total; j++ )); do plain+=("$blank"); done

  # --- Build colored lines ---
  local R=$'\e[0m'
  local BG=$'\e[48;5;236m' BDR=$'\e[38;5;240m'
  local ITEM=$'\e[38;5;252m' DESC=$'\e[38;5;244m'
  local SELBG=$'\e[48;5;62m' SELFG=$'\e[38;5;255m\e[1m'
  local ICON=$'\e[38;5;141m'

  local -a colored=()

  colored+=("  ${BG}${BDR}╭${hbar}╮${R}")

  for (( i=1; i<=count; i++ )); do
    local line="${items[$i]}"
    local rest="${line#*	}"
    local display="${rest%%	*}"
    local desc="${rest#*	}"
    (( ${#display} > max_disp )) && display="${display:0:$max_disp}"
    (( ${#desc} > desc_avail )) && desc="${desc:0:$desc_avail}"

    # Right-pad display to max_disp so descriptions align across rows.
    local disp_pad=""
    local disp_gap=$(( max_disp - ${#display} ))
    (( disp_gap > 0 )) && for (( j=0; j<disp_gap; j++ )); do disp_pad+=" "; done

    # Right-pad desc to desc_avail so the right border lines up.
    local desc_pad=""
    local d_gap=$(( desc_avail - ${#desc} ))
    (( d_gap > 0 )) && for (( j=0; j<d_gap; j++ )); do desc_pad+=" "; done

    if (( i == __NERV_SELECTED )); then
      colored+=("  ${SELBG}${BDR}│${SELBG} ${ICON}\$${SELFG} ${display}${disp_pad}  ${desc}${desc_pad} ${BDR}│${R}")
    else
      colored+=("  ${BG}${BDR}│${BG} ${ICON}\$${ITEM} ${display}${disp_pad}  ${DESC}${desc}${desc_pad} ${BDR}│${R}")
    fi
  done

  colored+=("  ${BG}${BDR}├${hbar}┤${R}")

  # Footer shows the full selected description, line-wrapped via truncation
  # to W-2 (leading + trailing space).
  local sel_avail=$(( W - 2 ))
  (( ${#sel_desc} > sel_avail )) && sel_desc="${sel_desc:0:$sel_avail}"
  local fvis=" ${sel_desc} "
  local fpad=$(( W - ${#fvis} ))
  (( fpad < 0 )) && fpad=0
  local fps=""; for (( j=0; j<fpad; j++ )); do fps+=" "; done
  colored+=("  ${BG}${BDR}│${DESC}${fvis}${fps}${BDR}│${R}")

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
  local sel_line="${__NERV_ITEMS[$__NERV_SELECTED]}"
  local insertion="${sel_line%%	*}"

  # LBUFFER side: replace the trailing partial word with the insertion.
  local prefix="${LBUFFER% *}"
  if [[ "$prefix" == "$LBUFFER" ]]; then
    LBUFFER="$insertion "
  else
    LBUFFER="$prefix $insertion "
  fi

  # RBUFFER side: when cursor is mid-token, swallow the leading
  # partial word so we don't end up with `git commit▮it -m ...`.
  # The first run of non-whitespace bytes is the rest of the word
  # the user was completing — anything from the first whitespace
  # onward is preserved.
  if [[ -n "$RBUFFER" && "${RBUFFER[1]}" != ' ' && "${RBUFFER[1]}" != $'\t' ]]; then
    local rest_word="${RBUFFER%%[[:space:]]*}"
    RBUFFER="${RBUFFER#$rest_word}"
  fi

  # Clear ghost text from zsh-autosuggestions (if loaded) so the
  # user sees the actual line buffer, not a stale overlay.
  unset POSTDISPLAY 2>/dev/null
  typeset -g POSTDISPLAY=''

  # Clear raw ANSI popup first
  printf '%s' $'\e7\e[B\e[G\e[J\e8'
  __NERV_PREV_LBUFFER="$LBUFFER"
  __NERV_ACTIVE=0
  __NERV_SELECTED=1
  __NERV_ITEMS=()
  zle -R ""
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
  __nerv_show_popup "${rlines[@]}"
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
__nerv_accept() {
  if (( __NERV_ACTIVE && ${#__NERV_ITEMS} > 0 )); then
    if (( ${#__NERV_ITEMS} == 1 )); then
      __nerv_insert_selected
      return
    fi
    local max=${#__NERV_ITEMS}; (( max > 5 )) && max=5
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
    local max=${#__NERV_ITEMS}; (( max > 5 )) && max=5
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

# Arrow keys: same navigation, NO cycle wrap (matches Fig — arrows
# stop at the edges, Tab cycles).
__nerv_select_down() {
  if (( __NERV_ACTIVE && ${#__NERV_ITEMS} > 0 )); then
    local max=${#__NERV_ITEMS}; (( max > 5 )) && max=5
    (( __NERV_SELECTED < max )) && { (( __NERV_SELECTED++ )); __nerv_show_popup "${__NERV_ITEMS[@]}"; }
  else
    zle down-line-or-history
  fi
}
zle -N __nerv_select_down

__nerv_select_up() {
  if (( __NERV_ACTIVE && ${#__NERV_ITEMS} > 0 )); then
    (( __NERV_SELECTED > 1 )) && { (( __NERV_SELECTED-- )); __nerv_show_popup "${__NERV_ITEMS[@]}"; }
  else
    zle up-line-or-history
  fi
}
zle -N __nerv_select_up

bindkey $'\e[B' __nerv_select_down
bindkey $'\eOB' __nerv_select_down
bindkey $'\e[A' __nerv_select_up
bindkey $'\eOA' __nerv_select_up

__nerv_dismiss() { __nerv_hide_popup; __NERV_PREV_LBUFFER=""; }
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
