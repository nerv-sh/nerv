#!/usr/bin/env zsh
# _nerv.zsh — Nerv ZLE widget for inline autocomplete.
#
# Hybrid: `zle -R` reserves space with plain blank lines, raw ANSI
# (printf with \e7 / \e[B / \e8) overwrites them with colored
# content. `zle -R` doesn't interpret ANSI in its line arguments
# (the escapes get quoted and rendered as literal `^[[…m`), so
# the colored content has to go through printf separately.
# MAX_VIS is capped to $LINES so the popup never spills past the
# bottom of the screen — the save-restore dance breaks when the
# terminal scrolls mid-render.

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
typeset -gr __NERV_CLEAR_ESC=$'\e7\e[B\e[G\e[J\e8'

__nerv_reset_state() {
  __NERV_ACTIVE=0
  __NERV_SELECTED=1
  __NERV_ITEMS=()
  zle -R ""
}

__nerv_cycle_next() {
  local max=${#__NERV_ITEMS}
  if (( __NERV_SELECTED < max )); then
    (( __NERV_SELECTED++ ))
  else
    __NERV_SELECTED=1
  fi
}

__nerv_cycle_prev() {
  local max=${#__NERV_ITEMS}
  if (( __NERV_SELECTED > 1 )); then
    (( __NERV_SELECTED-- ))
  else
    __NERV_SELECTED=$max
  fi
}

__nerv_show_popup() {
  local -a items=("$@")
  local total=${#items}
  # Max visible rows — tight enough that the popup never runs past
  # the bottom of the screen (the printf save/restore dance dies
  # if the terminal scrolls mid-render). Each rendered row uses
  # ~1 terminal row + 4 chrome rows (top + divider + footer +
  # bottom), so leave 6 lines of headroom.
  local term_lines=${LINES:-24}
  local MAX_VIS=$(( term_lines - 6 ))
  (( MAX_VIS > 10 )) && MAX_VIS=10
  (( MAX_VIS < 3 )) && MAX_VIS=3
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

  # Wire format from `nerv _complete`:
  #   insertion \t display \t description \t icon
  # All 4 fields tab-separated; icon may be empty.
  local sel_line="${items[$__NERV_SELECTED]}"
  local sel_desc="${sel_line#*	}"; sel_desc="${sel_desc#*	}"
  sel_desc="${sel_desc%%	*}"  # drop trailing icon field

  # Auto-size: measure max display + max desc across ALL items
  # (not just the window), so window-sliding doesn't reshape the
  # popup width every tick.
  local i max_disp=0 max_desc=0
  for (( i=1; i<=total; i++ )); do
    local line="${items[$i]}"
    local rest="${line#*	}"
    local d="${rest%%	*}"
    local desc_full="${rest#*	}"
    desc_full="${desc_full%%	*}"  # drop icon field
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
  repeat $hbar_n; do hbar+="─"; done

  # --- Build plain-text lines for zle -R (space reservation) ---
  # zle -R doesn't interpret ANSI in its args, so we reserve space
  # with blanks first and overwrite with colored content via printf.
  local -a plain=()
  local blank=""
  repeat $(( W + 4 )); do blank+=" "; done
  local plain_rows=$(( visible + 4 ))
  repeat $plain_rows; do plain+=("$blank"); done

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
    # Pull icon (4th field). Empty → blank space (slot reserved
    # for alignment). `$` placeholder previously cluttered cd / ls
    # lists where every row would say `$ foo/` with no signal.
    local trail="${rest#*	}"           # description + tab + icon
    local row_icon="${trail#*	}"        # everything after description tab
    [[ "$row_icon" == "$trail" ]] && row_icon=""  # no tab → no icon
    local glyph=" "
    [[ -n "$row_icon" ]] && glyph="$row_icon"

    # Layout inside one row: " <glyph> display<padding>"
    # (slot takes 3 cols including bracketing spaces.) Right-pad
    # with spaces to fill row_body so the right vertical bar lines
    # up. Glyph width is approximated as 1 col; emojis can be 2
    # wide and may bleed one cell — accepted MVP tradeoff.
    local visible_chars=$(( 3 + ${#display} ))  # " G " + display
    local pad_n=$(( row_body - visible_chars ))
    (( pad_n < 0 )) && pad_n=0
    local row_pad=""
    repeat $pad_n; do row_pad+=" "; done

    if (( i == __NERV_SELECTED )); then
      colored+=("  ${SELBG}${BDR}│${SELBG} ${ICON}${glyph}${SELFG} ${display}${row_pad}${BDR}│${R}")
    else
      colored+=("  ${BG}${BDR}│${BG} ${ICON}${glyph}${ITEM} ${display}${row_pad}${BDR}│${R}")
    fi
  done

  colored+=("  ${BG}${BDR}├${hbar}┤${R}")

  # Footer: " desc … [n/total]" — right-side counter ALWAYS shown
  # so users can see Tab cycle progression at a glance, even when
  # the whole list fits in one window. Layout inside `│...│` must
  # equal W-2 cells (matches the body rows above):
  # " " + desc + pad + counter + " ".
  local counter="[${__NERV_SELECTED}/${total}]"
  # Reserve cells for: leading " ", trailing " ", counter.
  local sel_avail=$(( W - 4 - ${#counter} ))
  (( sel_avail < 0 )) && sel_avail=0
  (( ${#sel_desc} > sel_avail )) && sel_desc="${sel_desc:0:$sel_avail}"
  local fpad=$(( W - 4 - ${#sel_desc} - ${#counter} ))
  (( fpad < 0 )) && fpad=0
  local fps=""; repeat $fpad; do fps+=" "; done
  colored+=("  ${BG}${BDR}│${DESC} ${sel_desc}${fps}${counter} ${BDR}│${R}")

  colored+=("  ${BG}${BDR}╰${hbar}╯${R}")

  # Step 1: ZLE creates space (plain blanks) and positions cursor.
  zle -R "" "${plain[@]}"
  # Step 2-4: save cursor, move down + overwrite with colored
  # content per row, restore cursor. Works as long as the popup
  # stays within the visible screen — MAX_VIS above clamps to
  # $LINES so we don't trigger a mid-render scroll that would
  # invalidate the saved cursor pos.
  local buf=$'\e7'
  for (( i=1; i<=${#colored}; i++ )); do
    buf+=$'\e[B\e[G'"${colored[$i]}"$'\e[K'
  done
  buf+=$'\e8'
  printf '%s' "$buf"

  __NERV_ACTIVE=1
}

__nerv_hide_popup() {
  if [[ -n "${NERV_DEBUG:-}" ]]; then
    print -r -- "  hide_popup ACTIVE=$__NERV_ACTIVE" >> /tmp/nerv-debug.log
  fi
  POSTDISPLAY=''
  (( ! __NERV_ACTIVE )) && return
  # Clear the raw-ANSI overlay we painted in show_popup BEFORE
  # zle -R releases the status lines, otherwise the colored
  # remnants persist where the blank lines used to be.
  printf '%s' "$__NERV_CLEAR_ESC"
  __nerv_reset_state
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

  # Frecency: record the accept in the background so the next
  # completion request can boost it. Fire-and-forget — never
  # block on the IPC, never surface its errors to the user.
  # `${BUFFER%% *}` peels the first word — the spec the user
  # invoked (e.g. `git`, `cd`).
  local spec_name="${BUFFER%% *}"
  if [[ -n "$spec_name" && -n "$insertion" ]]; then
    ( "$__NERV_BIN" _record "$spec_name" "$insertion" >/dev/null 2>&1 & ) >/dev/null 2>&1
  fi

  # Best-effort: clear zsh-autosuggestions ghost overlay.
  if (( ${+POSTDISPLAY} )); then
    POSTDISPLAY=''
  fi

  # Clear the raw-ANSI popup overlay, then reset internal state.
  # reset-prompt + redisplay force ZLE to repaint the prompt now
  # that BUFFER/CURSOR have moved.
  printf '%s' "$__NERV_CLEAR_ESC"
  __NERV_PREV_LBUFFER="$LBUFFER"
  __nerv_reset_state
  zle reset-prompt 2>/dev/null
  zle redisplay 2>/dev/null
  return 0
}

# ---------------------------------------------------------------------------
# Core widget
# ---------------------------------------------------------------------------
__nerv_complete() {
  if [[ -n "${NERV_DEBUG:-}" ]]; then
    print -r -- "[$(date +%H:%M:%S.%N)] complete LBUFFER=[$LBUFFER] PREV=[$__NERV_PREV_LBUFFER] ACTIVE=$__NERV_ACTIVE ITEMS=${#__NERV_ITEMS}" >> /tmp/nerv-debug.log
  fi
  (( __NERV_PASTING )) && return
  [[ "$LBUFFER" == "$__NERV_PREV_LBUFFER" ]] && return
  __NERV_PREV_LBUFFER="$LBUFFER"
  __NERV_SELECTED=1

  [[ -z "${LBUFFER// /}" ]] && { __nerv_hide_popup; return; }
  [[ "$LBUFFER" != *" "* ]] && { __nerv_hide_popup; return; }

  local resp
  resp=$("$__NERV_BIN" _complete "$LBUFFER" $CURSOR 2>/dev/null) || {
    if [[ -n "${NERV_DEBUG:-}" ]]; then
      print -r -- "  complete: BIN call FAILED" >> /tmp/nerv-debug.log
    fi
    if (( ! __NERV_E1_SHOWN )); then
      __NERV_E1_SHOWN=1
      zle -R "[nerv] daemon not running — run: nerv start"
      __NERV_ACTIVE=1
    fi
    return
  }
  if [[ -n "${NERV_DEBUG:-}" ]]; then
    print -r -- "  complete: got $(print -r -- "$resp" | wc -l | tr -d ' ') lines" >> /tmp/nerv-debug.log
  fi

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
#
# Only shown when the user is actively mid-token (LBUFFER doesn't end
# in whitespace). Browsing the popup right after typing a space (e.g.
# `cd ` then looking at all folders) shouldn't smear a stray
# suggestion onto the cursor line — that looks like the cursor
# teleported into a new word.
__nerv_set_ghost() {
  POSTDISPLAY=''
  (( ${#__NERV_ITEMS} == 0 )) && return
  # Bail when nothing typed yet for the current word — keeps the
  # prompt line quiet while the user surveys the popup.
  [[ "$LBUFFER" == *' ' || "$LBUFFER" == *$'\t' ]] && return
  local top="${__NERV_ITEMS[1]}"
  local top_ins="${top%%	*}"
  [[ -z "$top_ins" ]] && return
  # Current word = last whitespace-separated token of LBUFFER.
  local prefix="${LBUFFER##* }"
  [[ -z "$prefix" ]] && return
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
    __nerv_cycle_next
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
    __nerv_cycle_prev
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
    __nerv_cycle_next
    __nerv_show_popup "${__NERV_ITEMS[@]}"
  else
    zle down-line-or-history
  fi
}
zle -N __nerv_select_down

__nerv_select_up() {
  if (( __NERV_ACTIVE && ${#__NERV_ITEMS} > 0 )); then
    __nerv_cycle_prev
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
