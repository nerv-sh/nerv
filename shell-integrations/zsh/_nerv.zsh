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
# NERV_PTY=1 opts the user into the figterm-style PTY shim
# (PLAN.md §5.8 / CLAUDE.md §4 invariant: ZLE widget and PTY shim
# are mutually exclusive). When set, skip ZLE setup entirely so
# the PTY wrapper owns input. M1; default v1.0 path stays ZLE.
if [[ -n "${NERV_PTY-}" ]]; then
  return 0
fi
typeset -g __NERV_LOADED=1
typeset -g __NERV_BIN="${NERV_BIN:-nerv}"
typeset -g __NERV_PREV_LBUFFER=""
typeset -gi __NERV_E1_SHOWN=0
typeset -gi __NERV_E5_SHOWN=0
# Selection index into the popup. 0 = the "Immediately execute" sentinel
# row (Fig parity): default-highlighted, and Enter on it runs the line as
# typed instead of inserting a suggestion. 1..N index the real items.
# This is why `cd ` / `z ` + Enter execute the command rather than
# injecting the first folder — the user navigates down to pick one.
typeset -gi __NERV_SELECTED=0
typeset -ga __NERV_ITEMS=()
typeset -gi __NERV_ACTIVE=0

# Tighten KEYTIMEOUT so single-press Esc dismisses the popup
# without zsh's default 0.4s wait for longer escape sequences.
# 1 = 10ms — fast enough for human perception, still safe for
# multi-byte arrow keys on local terminals. User can override
# after the init block.
KEYTIMEOUT=1
typeset -gi __NERV_WIDTH=46
typeset -gi __NERV_PASTING=0
typeset -gr __NERV_CLEAR_ESC=$'\e7\e[B\e[G\e[J\e8'

__nerv_reset_state() {
  __NERV_ACTIVE=0
  __NERV_SELECTED=0
  __NERV_ITEMS=()
  zle -R ""
}

# Cycle across [sentinel(0), item1, …, itemN], wrapping. next:
# 0→1→…→N→0; prev: 0→N→…→1→0. The sentinel is one of the stops, so
# tabbing past the last item lands back on "Immediately execute".
__nerv_cycle_next() {
  local n=${#__NERV_ITEMS}
  (( __NERV_SELECTED = (__NERV_SELECTED + 1) % (n + 1) ))
}

__nerv_cycle_prev() {
  local n=${#__NERV_ITEMS}
  (( __NERV_SELECTED = (__NERV_SELECTED + n) % (n + 1) ))
}

# Best-effort on-screen column (1-based) of the input cursor, so the
# popup's top-left sits under it rather than at column 1 — otherwise a
# long prompt strands the box on the far left while the cursor is on the
# right. We compute it from the expanded prompt width plus the typed
# text, instead of a DSR (`ESC[6n`) query: inside a ZLE widget a raw
# `read` competes with the line editor for stdin and the terminal's
# reply leaks into the buffer. Limitation: multi-line / dynamically
# repainted prompts (some powerlevel themes) may be approximate.
__nerv_cursor_col() {
  emulate -L zsh
  setopt local_options extended_glob
  # Expand the prompt the way zsh would render it, strip CSI colour
  # sequences, and keep only the last screen line.
  local p=${(%)PS1}
  p=${p//$'\e'\[[0-9;?]#[a-zA-Z]/}
  p=${p##*$'\n'}
  # Anchor the box's left border under the typed text. We subtract from
  # the cursor cell so the box edge sits beneath the input rather than a
  # column or two to its right (the leading " " inside the box accounts
  # for the rest of the visual offset).
  local col=$(( ${#p} + ${#LBUFFER} - 1 ))
  (( col < 1 )) && col=1
  # Guard against prompts whose %-expansion doesn't yield a clean
  # last-line width. powerlevel10k (and similar) draw a full-width
  # filler bar (`···· 16:41`) whose padding inflates ${#p} to ≈COLUMNS,
  # which slams the box against the right edge (observed: cursor at
  # col 6, box at col ~190). When the estimate lands implausibly far
  # right — beyond what the typed text alone could justify — the
  # heuristic is defeated; fall back to anchoring from the left under
  # the typed text (assumes a short input-line prompt, true for the
  # multiline themes that trigger this). A left box is always usable;
  # a right-clamped one is not.
  local cols=${COLUMNS:-80}
  if (( col > cols - 20 )); then
    col=$(( 1 + ${#LBUFFER} ))
    (( col > cols - 20 )) && col=1
  fi
  print -r -- $col
}

# Max visible rows — tight enough that the popup never runs past
# the bottom of the screen (the printf save/restore dance dies
# if the terminal scrolls mid-render). Each rendered row uses
# ~1 terminal row + 4 chrome rows (top + divider + footer +
# bottom). Reserve 7 lines so the popup (visible + 4 chrome) leaves
# room for a prompt that wrapped to two lines plus the cursor row —
# on a short window a 1-line reserve overflows by one and tears the
# bottom of the box. Sets REPLY (no subshell — this runs on the
# keystroke path). Shared by the renderer and the PageUp/PageDown
# jump so a "page" always equals what's on screen.
__nerv_max_vis() {
  local term_lines=${LINES:-24}
  # -8, not -7: the box now carries an extra "Immediately execute"
  # sentinel row on top of the item window + 4 chrome rows.
  REPLY=$(( term_lines - 8 ))
  (( REPLY > 10 )) && REPLY=10
  (( REPLY < 3 )) && REPLY=3
}

__nerv_show_popup() {
  local -a items=("$@")
  local total=${#items}
  local REPLY; __nerv_max_vis
  local MAX_VIS=$REPLY
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
  # All 4 fields tab-separated; icon may be empty. SELECTED==0 is the
  # sentinel — no item, so the footer shows the "Immediately execute"
  # blurb instead of a per-item description.
  local sel_desc
  if (( __NERV_SELECTED == 0 )); then
    sel_desc="Immediately execute"
  else
    local sel_line="${items[$__NERV_SELECTED]}"
    sel_desc="${sel_line#*	}"; sel_desc="${sel_desc#*	}"
    sel_desc="${sel_desc%%	*}"  # drop trailing icon field
  fi

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
    # Measure DISPLAY cells, not code points: CJK glyphs are 2 cells,
    # so ${#d} undercounts and the right border drifts. The `m` flag
    # makes the length operator count East Asian width.
    local dw=${(m)#d} ew=${(m)#desc_full}
    (( dw > max_disp )) && max_disp=$dw
    (( ew > max_desc )) && max_desc=$ew
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

  # Pre-scan items: if any row carries an emoji icon (4th field
  # contains a non-ASCII byte), the glyph occupies 2 cells. Reserve
  # an extra column in body so the right border doesn't clip.
  local has_wide_icon=0
  for (( i=1; i<=total; i++ )); do
    local _l="${items[$i]}"
    local _t="${_l#*	}"; _t="${_t#*	}"
    local _ic="${_t#*	}"; [[ "$_ic" == "$_t" ]] && _ic=""
    [[ "$_ic" == *[^[:ascii:]]* ]] && { has_wide_icon=1; break; }
  done
  # Layout: " G " + display + " " — slot is 3 cols (ASCII glyph) or
  # 4 cols (emoji) depending on whether ANY row uses an emoji.
  local body
  if (( has_wide_icon )); then
    body=$(( 5 + max_disp + 1 ))
  else
    body=$(( 4 + max_disp + 1 ))
  fi
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
  # visible items + sentinel row + 4 chrome (top / divider / footer /
  # bottom).
  local plain_rows=$(( visible + 5 ))
  repeat $plain_rows; do plain+=("$blank"); done

  # --- Build colored lines ---
  local R=$'\e[0m'
  local BG=$'\e[48;5;236m' BDR=$'\e[38;5;240m'
  local ITEM=$'\e[38;5;252m' DESC=$'\e[38;5;244m'
  local SELBG=$'\e[48;5;62m' SELFG=$'\e[38;5;255m\e[1m'
  local ICON=$'\e[38;5;141m'
  # Fig-style arg hint (`cmd [remote] [branch]`): dimmer than the
  # command name. HINT for normal rows, HINTSEL drops bold + uses light
  # grey so it stays legible on the selected-row accent background.
  local HINT=$'\e[38;5;244m' HINTSEL=$'\e[22m\e[38;5;250m'

  local -a colored=()

  colored+=("  ${BG}${BDR}╭${hbar}╮${R}")

  # Row body width (between the two vertical bars): everything past
  # " │" on the left and " │" on the right. Equals W - 2.
  local row_body=$(( W - 2 ))

  # "Immediately execute" sentinel row (Fig parity), always drawn first
  # and highlighted by default (SELECTED==0). Content = `↩` + label,
  # width-measured so the right border stays aligned even if the glyph
  # renders as 2 cells. Enter here runs the line as typed.
  local sent_txt="↩ Immediately execute"
  local sent_w=${(m)#sent_txt}
  local sent_pad_n=$(( row_body - 1 - sent_w ))
  (( sent_pad_n < 0 )) && sent_pad_n=0
  local sent_pad=""; repeat $sent_pad_n; do sent_pad+=" "; done
  if (( __NERV_SELECTED == 0 )); then
    colored+=("  ${SELBG}${BDR}│${SELBG} ${SELFG}${sent_txt}${sent_pad}${BDR}│${R}")
  else
    colored+=("  ${BG}${BDR}│${DESC} ${sent_txt}${sent_pad}${BDR}│${R}")
  fi

  for (( i=start; i<=end; i++ )); do
    local line="${items[$i]}"
    local rest="${line#*	}"
    local display="${rest%%	*}"
    # Width-aware truncate AND right-pad to exactly max_disp display
    # cells. (mr) counts East Asian width via the m flag, so a CJK name
    # that overflows is cut on a cell boundary and shorter names fill
    # the slot — both keep the right border aligned. When a 2-cell glyph
    # straddles the boundary, (mr) keeps the whole glyph and overshoots
    # by 1; drop it and re-pad so the slot is exactly max_disp cells.
    display="${(mr:$max_disp:)display}"
    (( ${(m)#display} > max_disp )) && display="${(mr:$max_disp:)${display%?}}"
    # Pull icon (4th field). Empty → blank space (slot reserved
    # for alignment). `$` placeholder previously cluttered cd / ls
    # lists where every row would say `$ foo/` with no signal.
    local trail="${rest#*	}"           # description + tab + icon
    local row_icon="${trail#*	}"        # everything after description tab
    [[ "$row_icon" == "$trail" ]] && row_icon=""  # no tab → no icon
    local glyph=" "
    [[ -n "$row_icon" ]] && glyph="$row_icon"

    # Glyph slot width (display cells, not bytes). Emoji = 2 cells,
    # ASCII/space = 1. When the popup has ANY emoji row, ascii rows
    # need an extra trailing space so display columns align.
    local glyph_w=1
    [[ "$glyph" == *[^[:ascii:]]* ]] && glyph_w=2
    if (( has_wide_icon && glyph_w == 1 )); then
      glyph="$glyph "
      glyph_w=2
    fi
    local visible_chars=$(( 2 + glyph_w + max_disp ))  # " G " + padded display
    local pad_n=$(( row_body - visible_chars ))
    (( pad_n < 0 )) && pad_n=0
    local row_pad=""
    repeat $pad_n; do row_pad+=" "; done

    # Split off a trailing Fig-style arg hint so it renders dimmer than
    # the command name. The hint begins at the first " [" or " <" (from
    # the engine's arg_hint); subcommand names never contain those, so
    # the split is unambiguous. dpre = name, dpost = hint (+ any pad the
    # width truncation folded in). No hint → dpost empty → unchanged.
    local dpre="$display" dpost=""
    local b1="${display%%' ['*}" b2="${display%%' <'*}"
    local boundary="$display"
    [[ "$b1" != "$display" && ${#b1} -lt ${#boundary} ]] && boundary="$b1"
    [[ "$b2" != "$display" && ${#b2} -lt ${#boundary} ]] && boundary="$b2"
    if [[ "$boundary" != "$display" ]]; then
      dpre="$boundary"
      dpost="${display[${#boundary}+1,-1]}"
    fi

    if (( i == __NERV_SELECTED )); then
      colored+=("  ${SELBG}${BDR}│${SELBG} ${ICON}${glyph}${SELFG} ${dpre}${HINTSEL}${dpost}${row_pad}${BDR}│${R}")
    else
      colored+=("  ${BG}${BDR}│${BG} ${ICON}${glyph}${ITEM} ${dpre}${HINT}${dpost}${row_pad}${BDR}│${R}")
    fi
  done

  colored+=("  ${BG}${BDR}├${hbar}┤${R}")

  # Footer: " desc … [n/total]" — right-side counter ALWAYS shown
  # so users can see Tab cycle progression at a glance, even when
  # the whole list fits in one window. Layout inside `│...│` must
  # equal W-2 cells (matches the body rows above):
  # " " + desc + pad + counter + " ".
  # ASCII: cells == chars. Sentinel selected → show the item count
  # alone (`[8]`); an item → its 1-based position (`[3/8]`).
  local counter
  if (( __NERV_SELECTED == 0 )); then
    counter="[${total}]"
  else
    counter="[${__NERV_SELECTED}/${total}]"
  fi
  # Reserve cells for: leading " ", trailing " ", counter.
  local sel_avail=$(( W - 4 - ${#counter} ))
  (( sel_avail < 0 )) && sel_avail=0
  # Width-aware truncate: a CJK description must be cut on a cell
  # boundary or the counter is pushed past the right border.
  sel_desc="${(mr:$sel_avail:)sel_desc}"
  (( ${(m)#sel_desc} > sel_avail )) && sel_desc="${(mr:$sel_avail:)${sel_desc%?}}"
  local fpad=$(( W - 4 - ${(m)#sel_desc} - ${#counter} ))
  (( fpad < 0 )) && fpad=0
  local fps=""; repeat $fpad; do fps+=" "; done
  colored+=("  ${BG}${BDR}│${DESC} ${sel_desc}${fps}${counter} ${BDR}│${R}")

  colored+=("  ${BG}${BDR}╰${hbar}╯${R}")

  # Step 1: ZLE creates space (plain blanks) and positions cursor.
  zle -R "" "${plain[@]}"

  # Anchor the popup's left edge under the input cursor. Query the
  # cursor column AFTER `zle -R` so it reflects the input line. Clamp
  # so a box near the right edge shifts left to stay on screen.
  #
  # Each colored row is prefixed with 2 leading spaces (see the
  # "  ${BG}…" rows below), so the painted footprint is W + 2 cells, not
  # W. Reserve one more column on top of that: writing into the very last
  # cell arms the terminal's pending-wrap flag, and the next row's
  # cursor-down then scrolls — drifting every following row one line low
  # and tearing the box into the alternating "│ … │" / margin-"│"
  # fragments seen with long prompts. So keep start_col + 2 + W ≤ cols.
  local start_col=$(__nerv_cursor_col)
  (( start_col < 1 )) && start_col=1
  (( start_col + W + 2 > term_cols )) && start_col=$(( term_cols - W - 2 ))
  (( start_col < 1 )) && start_col=1

  # Step 2-4: save cursor, move down + overwrite with colored
  # content per row at the anchored column, restore cursor. Works as
  # long as the popup stays within the visible screen — MAX_VIS above
  # clamps to $LINES so we don't trigger a mid-render scroll that
  # would invalidate the saved cursor pos.
  local move=$'\e[B\e['${start_col}'G'
  local buf=$'\e7'
  for (( i=1; i<=${#colored}; i++ )); do
    buf+="${move}${colored[$i]}"$'\e[K'
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
    print -r -- "[$(date +%H:%M:%S.%N)] complete LBUFFER=[$LBUFFER] PREV=[$__NERV_PREV_LBUFFER] ACTIVE=$__NERV_ACTIVE ITEMS=${#__NERV_ITEMS} CURSOR=$CURSOR" >> /tmp/nerv-debug.log
  fi
  (( __NERV_PASTING )) && return
  # Popup only when cursor is at the end of the buffer (LBUFFER ==
  # full BUFFER, i.e. RBUFFER empty). Fig behavior: typing →
  # popup; left-arrow into the middle → popup hides; right-arrow
  # back to the end → popup reappears.
  if [[ -n "$RBUFFER" ]]; then
    __nerv_hide_popup
    __NERV_PREV_LBUFFER=""
    return
  fi
  [[ "$LBUFFER" == "$__NERV_PREV_LBUFFER" ]] && return
  __NERV_PREV_LBUFFER="$LBUFFER"
  __NERV_SELECTED=0

  [[ -z "${LBUFFER// /}" ]] && { __nerv_hide_popup; return; }
  [[ "$LBUFFER" != *" "* ]] && { __nerv_hide_popup; return; }

  local resp
  resp=$("$__NERV_BIN" _complete "$LBUFFER" $CURSOR 2>/dev/null)
  local rc=$?
  if (( rc != 0 )); then
    if [[ -n "${NERV_DEBUG:-}" ]]; then
      print -r -- "  complete: BIN call FAILED rc=$rc" >> /tmp/nerv-debug.log
    fi
    # rc 3 = E5 spec schema mismatch (daemon up, cache wrong version);
    # any other non-zero = E1 daemon not reachable.
    if (( rc == 3 )); then
      if (( ! __NERV_E5_SHOWN )); then
        __NERV_E5_SHOWN=1
        zle -R "[nerv] spec mismatch — run: brew reinstall nerv"
        __NERV_ACTIVE=1
      fi
    elif (( ! __NERV_E1_SHOWN )); then
      __NERV_E1_SHOWN=1
      zle -R "[nerv] daemon not running — run: nerv start"
      __NERV_ACTIVE=1
    fi
    return
  fi
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
  # Enter inserts the highlighted item ONLY when a real item is selected
  # (SELECTED >= 1). On the default "Immediately execute" sentinel
  # (SELECTED == 0) — or with no popup — Enter runs the line as typed.
  # This is the Fig model: `cd ` / `z ` + Enter execute the command;
  # navigate down to a folder first to insert one.
  if (( __NERV_ACTIVE && __NERV_SELECTED >= 1 && ${#__NERV_ITEMS} > 0 )); then
    __nerv_insert_selected
  else
    (( __NERV_ACTIVE )) && { __NERV_ACTIVE=0; zle -R ""; }
    __NERV_PREV_LBUFFER=""
    __NERV_SELECTED=0
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
# Arrows navigate the popup only when there's actually a line being
# completed. On an empty buffer they must reach zsh's history search —
# stale popup state (a `cd ` list left active after the previous command)
# would otherwise re-draw the old popup on a blank prompt when the user
# just wanted history. The `[[ -n "$BUFFER" ]]` guard is the belt; the
# precmd reset below is the suspenders.
__nerv_select_down() {
  if (( __NERV_ACTIVE && ${#__NERV_ITEMS} > 0 )) && [[ -n "$BUFFER" ]]; then
    __nerv_cycle_next
    __nerv_show_popup "${__NERV_ITEMS[@]}"
  else
    zle down-line-or-history
  fi
}
zle -N __nerv_select_down

__nerv_select_up() {
  if (( __NERV_ACTIVE && ${#__NERV_ITEMS} > 0 )) && [[ -n "$BUFFER" ]]; then
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

# PageDown / PageUp: jump the selection by one visible window (the
# same MAX_VIS the renderer uses) and clamp at the edges — page keys
# page, they don't wrap (single-step Tab/arrow keep their wrap).
# Outside the popup they fall back to history movement, mirroring
# the arrow-key fallback above.
__nerv_page_down() {
  if (( __NERV_ACTIVE && ${#__NERV_ITEMS} > 0 )) && [[ -n "$BUFFER" ]]; then
    local REPLY; __nerv_max_vis
    local max=${#__NERV_ITEMS}
    (( __NERV_SELECTED += REPLY ))
    (( __NERV_SELECTED > max )) && __NERV_SELECTED=$max
    __nerv_show_popup "${__NERV_ITEMS[@]}"
  else
    zle down-line-or-history
  fi
}
zle -N __nerv_page_down

__nerv_page_up() {
  if (( __NERV_ACTIVE && ${#__NERV_ITEMS} > 0 )) && [[ -n "$BUFFER" ]]; then
    local REPLY; __nerv_max_vis
    (( __NERV_SELECTED -= REPLY ))
    # Floor at the sentinel (0), not the first item — Page-Up should be
    # able to return to "Immediately execute".
    (( __NERV_SELECTED < 0 )) && __NERV_SELECTED=0
    __nerv_show_popup "${__NERV_ITEMS[@]}"
  else
    zle up-line-or-history
  fi
}
zle -N __nerv_page_up

bindkey $'\e[5~' __nerv_page_up
bindkey $'\e[6~' __nerv_page_down

# Right-Arrow: accept ghost text (POSTDISPLAY) when at end of line.
# Falls back to plain forward-char in the middle of the buffer or
# when there's no ghost — matches user expectation for cursor
# movement inside an existing edit. After forward-char, re-trigger
# __nerv_complete so the popup reappears when the cursor lands back
# at the end of the buffer.
__nerv_accept_ghost() {
  if (( ${+POSTDISPLAY} )) && [[ -n "$POSTDISPLAY" ]] && [[ -z "$RBUFFER" ]]; then
    LBUFFER+="$POSTDISPLAY"
    POSTDISPLAY=''
    __NERV_PREV_LBUFFER="$LBUFFER"
    __nerv_hide_popup
  else
    zle forward-char
    __nerv_complete
  fi
}
zle -N __nerv_accept_ghost
bindkey $'\e[C' __nerv_accept_ghost
bindkey $'\eOC' __nerv_accept_ghost

# Cursor-position state tracking. The pre-redraw hook below fires
# on every ZLE redraw and uses this to detect transitions between
# "at end of buffer" and "in the middle" without having to bind
# every individual movement key (left, ctrl-a, home, etc.).
typeset -gi __NERV_LAST_AT_END=1

__nerv_pre_redraw() {
  local at_end=0
  [[ -z "$RBUFFER" ]] && at_end=1
  # Only act on state TRANSITIONS to avoid running the heavy
  # __nerv_complete path on every redraw.
  if (( at_end != __NERV_LAST_AT_END )); then
    __NERV_LAST_AT_END=$at_end
    if (( ! at_end )); then
      __nerv_hide_popup
      __NERV_PREV_LBUFFER=""
    else
      # Cursor returned to end → re-trigger completion query.
      __NERV_PREV_LBUFFER=""
      __nerv_complete
    fi
  fi

  # Paint the inline ghost (POSTDISPLAY) a muted grey — like fish /
  # zsh-autosuggestions — so the not-yet-typed completion reads as a
  # dim hint, not live input. Without this the ghost inherits the
  # terminal's default foreground and looks identical to what the user
  # typed. Re-synced every redraw off the current POSTDISPLAY: strip
  # our previous entry (matched by the memo tag so we never clobber a
  # highlight another plugin added), then re-add if a ghost is showing.
  # POSTDISPLAY chars occupy buffer positions ${#BUFFER}..+len.
  region_highlight=(${region_highlight:#*memo=nerv_ghost*})
  if [[ -n "$POSTDISPLAY" ]]; then
    region_highlight+=("${#BUFFER} $(( ${#BUFFER} + ${#POSTDISPLAY} )) fg=242, memo=nerv_ghost")
  fi
}
zle -N zle-line-pre-redraw __nerv_pre_redraw

__nerv_dismiss() { __nerv_hide_popup; __NERV_PREV_LBUFFER=""; POSTDISPLAY=''; }
zle -N __nerv_dismiss
bindkey '^G' __nerv_dismiss

# Esc closes an active popup. When no popup is up, falls through to
# zsh's usual Esc handling (send-break — same as the unbound default).
# KEYTIMEOUT is set to 1 (10ms) at the top of this file so the
# bare-Esc binding fires instantly without waiting for a longer
# escape sequence like `\e[A`.
__nerv_escape() {
  if (( __NERV_ACTIVE )); then
    __nerv_hide_popup
    __NERV_PREV_LBUFFER=""
    POSTDISPLAY=''
  else
    zle .send-break 2>/dev/null || true
  fi
}
zle -N __nerv_escape
bindkey '\e' __nerv_escape

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
# Clear popup state at the start of every new prompt. Without this the
# globals survive a command / Ctrl-C, so a `cd ` list left active stays
# "active" on the next, blank prompt — and an arrow / Page key would
# re-draw that stale popup instead of reaching history. (Plain var
# reset only — no `zle -R`, which is invalid outside a widget.)
__nerv_precmd_reset() {
  __NERV_ACTIVE=0
  __NERV_ITEMS=()
  __NERV_SELECTED=0
  __NERV_PREV_LBUFFER=""
}

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
autoload -Uz add-zsh-hook 2>/dev/null && {
  add-zsh-hook precmd __nerv_precmd_reset
  add-zsh-hook precmd __nerv_rebind
}
