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

__nerv_show_popup() {
  local -a items=("$@")
  local count=${#items}
  (( count > 5 )) && count=5
  local W=$__NERV_WIDTH

  local sel_line="${items[$__NERV_SELECTED]}"
  local sel_desc="${sel_line#*	}"; sel_desc="${sel_desc#*	}"

  local hbar=""
  local j; for (( j=0; j<W; j++ )); do hbar+="─"; done

  # --- Build plain-text lines for zle -R (space reservation) ---
  local -a plain=()
  # Use spaces matching the width so zle -R allocates correct space
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

  local i
  for (( i=1; i<=count; i++ )); do
    local line="${items[$i]}"
    local rest="${line#*	}"
    local display="${rest%%	*}"
    local desc="${rest#*	}"
    (( ${#display} > 18 )) && display="${display:0:18}"
    (( ${#desc} > 20 )) && desc="${desc:0:20}"

    local vis_len=$(( 4 + ${#display} + ${#desc} + 2 ))
    local gap=$(( W - vis_len ))
    (( gap < 1 )) && gap=1
    local pad=""; for (( j=0; j<gap; j++ )); do pad+=" "; done

    if (( i == __NERV_SELECTED )); then
      colored+=("  ${SELBG}${BDR}│${SELBG} ${ICON}\$${SELFG} ${display}${pad}${desc} ${BDR}│${R}")
    else
      colored+=("  ${BG}${BDR}│${BG} ${ICON}\$${ITEM} ${display}${pad}${DESC}${desc} ${BDR}│${R}")
    fi
  done

  colored+=("  ${BG}${BDR}├${hbar}┤${R}")

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
  __NERV_ACTIVE=0
  __NERV_SELECTED=1
  __NERV_ITEMS=()
  zle -R ""  # ZLE clears the status lines and fixes cursor
}

__nerv_insert_selected() {
  (( ${#__NERV_ITEMS} == 0 )) && return 1
  local sel_line="${__NERV_ITEMS[$__NERV_SELECTED]}"
  local insertion="${sel_line%%	*}"
  local prefix="${LBUFFER% *}"
  if [[ "$prefix" == "$LBUFFER" ]]; then
    LBUFFER="$insertion "
  else
    LBUFFER="$prefix $insertion "
  fi
  __NERV_PREV_LBUFFER="$LBUFFER"
  __NERV_ACTIVE=0
  __NERV_SELECTED=1
  __NERV_ITEMS=()
  zle -R ""  # clear popup, ZLE redraws with new LBUFFER
  return 0
}

# ---------------------------------------------------------------------------
# Core widget
# ---------------------------------------------------------------------------
__nerv_complete() {
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

# Tab: select if popup, else default completion
__nerv_accept() {
  if (( __NERV_ACTIVE && ${#__NERV_ITEMS} > 0 )); then
    __nerv_insert_selected
  else
    zle expand-or-complete
  fi
}
zle -N __nerv_accept
bindkey '^I' __nerv_accept

# Arrow keys
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
