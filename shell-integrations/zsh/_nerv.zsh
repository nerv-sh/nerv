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
# 1 while the `…loading` one-line hint is on screen (cold spec parse or
# derivation in flight). Cleared the moment rows or settled silence
# arrive, so the stale hint never outlives the load.
typeset -gi __NERV_LOADING_SHOWN=0
# Selection index into the popup. 0 = the "Immediately execute" sentinel
# row (Fig parity): default-highlighted, and Enter on it runs the line as
# typed instead of inserting a suggestion. 1..N index the real items.
# This is why `cd ` / `z ` + Enter execute the command rather than
# injecting the first folder — the user navigates down to pick one.
typeset -gi __NERV_SELECTED=0
# 1 when the popup carries the "Immediately execute" sentinel as row 0
# (segment boundary — browsing). 0 when the user is mid-token filtering,
# where the sentinel is hidden and SELECTED indexes items 1..N directly.
typeset -gi __NERV_HAS_SENTINEL=0
typeset -ga __NERV_ITEMS=()
typeset -gi __NERV_ACTIVE=0
# Rows currently reserved via `zle -R`. Lets show_popup skip re-issuing
# the reservation (and its flicker-inducing blank frame) when a redraw
# keeps the same row count.
typeset -gi __NERV_RESERVED=0
# Cached box dimensions (widest display cells / widest desc cells / any
# wide icon), measured once per item set by __nerv_measure_items so
# show_popup doesn't re-scan all N items on every navigation keystroke.
typeset -gi __NERV_MAXDISP=0 __NERV_MAXDESC=0 __NERV_HASWIDE=0
# Wire rows captured from zsh's own completion by __nerv_compsys_capture,
# for a command no hand-written spec covers.
typeset -ga __NERV_COMPSYS_ROWS=()
# The last capture, reused while the user types on inside the same word
# (__nerv_compsys_rows): where it was taken, the word it was taken for,
# and every row it returned.
typeset -g __NERV_COMPSYS_KEY="" __NERV_COMPSYS_WORD="" __NERV_COMPSYS_SHAPE=""
typeset -ga __NERV_COMPSYS_CACHE=()
# Commands captured once already, and commands too slow to capture on a
# keystroke (both for this shell's lifetime).
typeset -gA __NERV_COMPSYS_SEEN=() __NERV_COMPSYS_SLOW=()

# Tighten KEYTIMEOUT so single-press Esc dismisses the popup
# without zsh's default 0.4s wait for longer escape sequences.
# 1 = 10ms — fast enough for human perception, still safe for
# multi-byte arrow keys on local terminals. User can override
# after the init block.
KEYTIMEOUT=1
typeset -gi __NERV_WIDTH=46

# Popup palette, chosen once per shell. NERV_POPUP_THEME:
#   purple (default) — the bar and the matched letters in nerv's purple,
#     rgb(192,103,222) (tak-cc-statusline's branch color) as its nearest
#     256-color index, 134; chrome in the theme's bright black;
#   native — only the terminal's 16 ANSI colors (bright-magenta reverse bar).
# Item names keep the terminal's default color either way, and chrome
# follows the theme, so light and dark terminals both read right. No true
# color (docs/terminal-compat.md §2).
__nerv_palette() {
  typeset -g __NERV_C_ITEM=$'\e[39m' __NERV_C_BDR=$'\e[90m' \
    __NERV_C_DESC=$'\e[90m' __NERV_C_ICON=$'\e[90m'
  if [[ ${NERV_POPUP_THEME:-purple} == native ]]; then
    typeset -g __NERV_C_SEL=$'\e[0;95;7m' \
      __NERV_C_HL=$'\e[95m' __NERV_C_HLOFF=$'\e[39m' \
      __NERV_C_SHL="" __NERV_C_SHLOFF=""
  else
    # On the bar the name is already white on purple; a matched letter
    # there would be purple on purple, so it gets none.
    typeset -g __NERV_C_SEL=$'\e[0;48;5;134;38;5;255m' \
      __NERV_C_HL=$'\e[38;5;134m' __NERV_C_HLOFF=$'\e[39m' \
      __NERV_C_SHL="" __NERV_C_SHLOFF=""
  fi
}
__nerv_palette

# Mark the letters of `$2` (what is typed) inside `$1` (a row's name) with
# `$3`, going back to the row's color with `$4`. The first way that fits
# wins: `$1` starts with it (prefix mode, and a fuzzy row that also starts
# with it), contains it (zoxide, `substring` args), or holds its letters in
# order (fuzzy, 3+ letters as in `mode_match`). Case-insensitive, like the
# matching. REPLY = `$1` unchanged when nothing fits or `$3` is empty.
__nerv_mark() {
  local t=$1 q=$2 on=$3 off=$4
  REPLY=$t
  [[ -z $q || -z $on ]] && return
  local lt=${(L)t} lq=${(L)q} n=${#q}
  if [[ $lt == ${(b)lq}* ]]; then
    REPLY="$on${t[1,n]}$off${t[n+1,-1]}"
    return
  fi
  local pre=${lt%%${(b)lq}*}
  if [[ $pre != $lt ]]; then
    local a=$(( ${#pre} + 1 ))
    REPLY="${t[1,a-1]}$on${t[a,a+n-1]}$off${t[a+n,-1]}"
    return
  fi
  (( n < 3 )) && return
  local out="" k j=1
  for (( k=1; k<=${#t}; k++ )); do
    if (( j <= n )) && [[ ${lt[k]} == "${lq[j]}" ]]; then
      out+="$on${t[k]}$off"; (( j++ ))
    else
      out+=${t[k]}
    fi
  done
  (( j > n )) && REPLY=$out
}
typeset -gi __NERV_PASTING=0
typeset -gr __NERV_CLEAR_ESC=$'\e7\e[B\e[G\e[J\e8'

# $aliases assoc (used to expand `alias g=git` before the engine call)
# lives in zsh/parameter; interactive shells usually have it, but load
# explicitly so a minimal rc doesn't leave the table missing.
zmodload -F zsh/parameter p:aliases 2>/dev/null
# $EPOCHREALTIME times each compsys capture (__nerv_compsys_rows).
zmodload -F zsh/datetime p:EPOCHREALTIME 2>/dev/null

# Autostart nervd in the background so a fresh install (or a reboot)
# needs no manual `nerv start`. `nerv start` is idempotent — it probes
# the socket and exits quietly when a daemon already serves — so this
# is a cheap no-op on every shell after the first. Backgrounded in a
# subshell: never blocks the prompt, no job-control noise. E1 ("daemon
# not running — run: nerv start") remains the fallback if this fails.
# NERV_AUTOSTART=0 opts out — test harnesses that manage the daemon
# themselves set it so a session's first prompt can observe the
# daemon-down path.
if [[ "${NERV_AUTOSTART:-1}" != "0" ]]; then
  ( "$__NERV_BIN" start >/dev/null 2>&1 & ) 2>/dev/null
fi

__nerv_clear_state_vars() {
  __NERV_ACTIVE=0
  __NERV_SELECTED=0
  __NERV_HAS_SENTINEL=0
  __NERV_RESERVED=0
  __NERV_PAINTED=""
  __NERV_ITEMS=()
}

# The loading hint goes with the popup: any widget path that drops one
# drops the other. (Not in __nerv_clear_state_vars — a trap cannot call
# `zle -M`, and the flag has to survive until a widget can.)
__nerv_reset_state() {
  __nerv_clear_state_vars
  __nerv_clear_loading
  zle -R ""
}

# A resize makes zsh redraw the prompt, which erases the popup rows, but
# the popup state would stay open: the next Down/Tab would steer a popup
# the user can no longer see. Close it. Traps run outside ZLE, so only
# plain variables may change here — no `zle -R`, no widget, no printf.
# An existing WINCH handler (user or plugin, set before us) is chained,
# not replaced — both forms: a TRAPWINCH function and a `trap '…' WINCH`
# list trap (defining TRAPWINCH would silently drop the latter). A
# handler set after us replaces ours (docs/terminal-compat.md §6).
if [[ "${functions[TRAPWINCH]-}" != *__nerv_clear_state_vars* ]]; then
  unfunction __nerv_prev_trapwinch 2>/dev/null
  typeset -g __NERV_PREV_WINCH_LIST=""
  if (( $+functions[TRAPWINCH] )); then
    functions -c TRAPWINCH __nerv_prev_trapwinch
  else
    # `trap` prints the list trap as `trap -- '<cmd>' WINCH`; eval the
    # quoted word back into the plain command string. Not `$(trap)`: a
    # command substitution is a subshell, where zsh has already reset traps.
    # Builtins only (sysopen, zf_rm, `$(<f)`): this runs at every shell
    # start, and forking mktemp/rm cost ~3.9 ms against ~0.3 ms (measured).
    () {
      zmodload -F zsh/system b:sysopen 2>/dev/null || return 0
      zmodload -F zsh/files b:zf_rm 2>/dev/null || return 0
      local tmp="${TMPDIR:-/tmp}/nerv-trap.$$.$RANDOM" fd line
      sysopen -w -o excl,creat -m 600 -u fd "$tmp" 2>/dev/null || return 0
      trap >&$fd
      exec {fd}>&-
      line=${(M)${(f)"$(<$tmp)"}:#trap -- * WINCH}
      zf_rm -f "$tmp"
      [[ -n "$line" ]] && eval "__NERV_PREV_WINCH_LIST=${${line#trap -- }% WINCH}"
    }
  fi
fi
TRAPWINCH() {
  local rc=0
  if (( $+functions[__nerv_prev_trapwinch] )); then
    __nerv_prev_trapwinch "$@"; rc=$?
  elif [[ -n "$__NERV_PREV_WINCH_LIST" ]]; then
    # A list trap's status is ignored by zsh; returning it from this
    # function would read as "interrupted" and drop the typed line.
    eval "$__NERV_PREV_WINCH_LIST"
  fi
  __nerv_clear_state_vars
  # Forget the last buffer so the next redraw re-queries and repaints the
  # popup at the new width (retyping it would otherwise read as unchanged).
  __NERV_PREV_LBUFFER=""
  # A non-zero return from a TRAPNAL function means "interrupted" — keep
  # the chained function's answer.
  return $rc
}

# Scan the item set ONCE to size the box: widest display name (display
# cells, `(m)` flag for CJK width), widest description, and whether any
# row carries a wide (emoji) icon. Cached into globals so show_popup —
# which fires on every navigation keystroke — reads O(1) instead of
# re-scanning all N items (the ~400-subcommand `aws ` lag).
__nerv_measure_items() {
  local i n=${#__NERV_ITEMS} md=0 me=0 hw=0
  for (( i=1; i<=n; i++ )); do
    local rest="${__NERV_ITEMS[$i]#*	}"   # display \t desc \t icon \t replace
    local d="${rest%%	*}"                  # display
    local after="${rest#*	}"               # desc \t icon \t replace
    local desc_full="${after%%	*}"         # desc
    # A path description (zoxide, folders) is shortened from the left in
    # the footer, so it never sets the box width — a 50-cell path under
    # 8-cell names made a box three times wider than its rows.
    local dw=${(m)#d} ew=0
    [[ "$desc_full" == [/~]* ]] || ew=${(m)#desc_full}
    (( dw > md )) && md=$dw
    (( ew > me )) && me=$ew
    if (( ! hw )); then
      local ic="${after#*	}"; [[ "$ic" == "$after" ]] && ic=""
      ic="${ic%%	*}"
      [[ "$ic" == *[^[:ascii:]]* ]] && hw=1
    fi
  done
  (( md > 32 )) && md=32   # hard cap so a long name can't blow the box out
  __NERV_MAXDISP=$md
  __NERV_MAXDESC=$me
  __NERV_HASWIDE=$hw
}

# Cycle across the selectable rows, wrapping. With the sentinel present
# the range is [0(sentinel), 1..N] (0→1→…→N→0); without it, items only
# [1..N] (1→2→…→N→1). Tabbing past the last item lands on the sentinel
# when there is one, else back on the first item.
__nerv_cycle_next() {
  local n=${#__NERV_ITEMS}
  if (( __NERV_HAS_SENTINEL )); then
    (( __NERV_SELECTED = (__NERV_SELECTED + 1) % (n + 1) ))
  else
    (( __NERV_SELECTED = __NERV_SELECTED % n + 1 ))
  fi
}

__nerv_cycle_prev() {
  local n=${#__NERV_ITEMS}
  if (( __NERV_HAS_SENTINEL )); then
    (( __NERV_SELECTED = (__NERV_SELECTED + n) % (n + 1) ))
  else
    (( __NERV_SELECTED = (__NERV_SELECTED + n - 2) % n + 1 ))
  fi
}

# Best-effort on-screen column (1-based) of the input cursor, so the
# popup's top-left sits under it rather than at column 1 — otherwise a
# long prompt strands the box on the far left while the cursor is on the
# right. We compute it from the expanded prompt width plus the typed
# text, instead of a DSR (`ESC[6n`) query: inside a ZLE widget a raw
# `read` competes with the line editor for stdin and the terminal's
# reply leaks into the buffer. Limitation: multi-line / dynamically
# repainted prompts (some powerlevel themes) may be approximate.
# Sets REPLY (no subshell — the keystroke path) and __NERV_PROMPT_W, the
# cells the prompt's last line takes before the typed text.
typeset -gi __NERV_PROMPT_W=0
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
  __NERV_PROMPT_W=${#p}
  if (( col > cols - 20 )); then
    col=$(( 1 + ${#LBUFFER} ))
    __NERV_PROMPT_W=2
    (( col > cols - 20 )) && col=1
  fi
  REPLY=$col
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
  (( REPLY > 8 )) && REPLY=8
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
  # sentinel — no item, and its row already says what Enter does, so the
  # footer shows the keys instead of repeating the label.
  local sel_desc sel_color
  if (( __NERV_SELECTED == 0 )); then
    sel_desc="enter run · tab pick · esc close"
    sel_color=$__NERV_C_DESC
  else
    local sel_line="${items[$__NERV_SELECTED]}"
    sel_desc="${sel_line#*	}"; sel_desc="${sel_desc#*	}"
    sel_desc="${sel_desc%%	*}"  # drop the icon + replace fields
    [[ -n "$HOME" && ( "$sel_desc" == "$HOME" || "$sel_desc" == "$HOME"/* ) ]] \
      && sel_desc="~${sel_desc#$HOME}"
    sel_color=$'\e[39m'
  fi

  # Box dimensions come from __nerv_measure_items (called once when the
  # item set changed) — NOT re-scanned here. show_popup fires on every
  # navigation keystroke; re-measuring all N items each time made a ~400-
  # subcommand `aws ` list lag badly on every arrow press.
  local max_disp=$__NERV_MAXDISP
  local max_desc=$__NERV_MAXDESC
  local has_wide_icon=$__NERV_HASWIDE

  local term_cols=${COLUMNS:-80}
  local cap=$(( term_cols * 8 / 10 ))
  (( cap < 30 )) && cap=30

  # Footer-desc width hint: clamp footer-desc to a generous reach so
  # the popup body width is mainly driven by display names, not by an
  # unusually long description.
  local foot_hint=$max_desc
  (( foot_hint > 60 )) && foot_hint=60

  # Layout: " G " + display + " " — slot is 3 cols (ASCII glyph) or
  # 4 cols (emoji) depending on whether ANY row uses an emoji.
  local body
  if (( has_wide_icon )); then
    body=$(( 5 + max_disp + 1 ))
  else
    body=$(( 4 + max_disp + 1 ))
  fi
  # Footer needs " " + desc + " " + counter + " ", sized for the widest
  # counter (`[N/N]`) so a description that set the width isn't cut short
  # by it. Pick whichever is wider so neither row wraps.
  local cmax="[${total}/${total}]"
  local W=$body
  (( foot_hint + 3 + ${#cmax} > W )) && W=$(( foot_hint + 3 + ${#cmax} ))
  (( W > cap )) && W=$cap
  (( W < __NERV_WIDTH )) && W=$__NERV_WIDTH
  # Terminal-width hard clamp LAST — the fixed minimum above must never
  # push a row past the window edge: painted rows carry 2 leading spaces
  # and the zle -R reservation blanks are W+4 cells, so anything wider
  # than COLUMNS-4 wraps in a small window and tears the box apart.
  local wmax=$(( term_cols - 4 ))
  (( wmax < 8 )) && wmax=8
  (( W > wmax )) && W=$wmax

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
  #
  # Each blank must own a whole screen line. zle -R lists its status strings
  # the way a completion list is drawn — COLUMNATED — so a box-width blank
  # shares its line on a wide window and zsh reserves fewer lines than we
  # asked for. The paint below walks down with ESC[B, which clamps at the
  # bottom margin instead of scrolling, so a short reservation piles every
  # remaining row onto the last screen line and the box collapses to its two
  # borders. COLUMNS-3 leaves room for zsh's inter-column gap, so exactly one
  # column fits at any width. Derivation + regression: e2e-zle-bottom.py.
  local -a plain=()
  local blank_w=$(( term_cols - 3 ))
  (( blank_w < W + 4 )) && blank_w=$(( W + 4 ))
  local blank="${(r:$blank_w:)}"
  # visible items + 4 chrome (top / divider / footer / bottom), + 1 for
  # the sentinel row when present.
  local plain_rows=$(( visible + 4 + __NERV_HAS_SENTINEL ))
  repeat $plain_rows; do plain+=("$blank"); done

  # --- Build colored lines ---
  # Colors come from the palette (__nerv_palette). The box has no
  # background of its own and item names keep the terminal's default
  # color; the selection is a bar across the row, with a `›` marker and
  # a bold name for a terminal that drops the color. Matched letters
  # (__nerv_mark) are the one other accent.
  local R=$'\e[0m'
  local BDR=$__NERV_C_BDR ICON=$__NERV_C_ICON DESC=$__NERV_C_DESC
  local ITEM=$__NERV_C_ITEM
  local SEL=$__NERV_C_SEL SELB=$'\e[1m' SELNB=$'\e[22m'
  # Fig-style arg hint (`cmd [remote] [branch]`): dimmer than the
  # command name. Inside the selection bar it only drops the bold — a
  # grey there would paint a grey patch into the bar.
  local HINT=$__NERV_C_DESC
  # What the rows are matched against: the word being typed, past its
  # last `/` (a path level) or `=` (`--color=`), as the engine splits it.
  local query=${LBUFFER##*[[:space:]]}
  query=${query##*/}; query=${query##*=}

  local -a colored=()

  colored+=("  ${BDR}╭${hbar}╮${R}")

  # Row body width (between the two vertical bars): everything past
  # " │" on the left and " │" on the right. Equals W - 2.
  local row_body=$(( W - 2 ))

  # A narrow window can leave the cached display slot wider than the
  # row body; clamp so the padded name can't push the right border past
  # the box edge (" G " prefix = 2 + glyph slot cells).
  local glyph_slot=1
  (( has_wide_icon )) && glyph_slot=2
  local disp_room=$(( row_body - 2 - glyph_slot ))
  (( disp_room < 1 )) && disp_room=1
  (( max_disp > disp_room )) && max_disp=$disp_room

  # "Immediately execute" sentinel row (Fig parity) — drawn ONLY at a
  # segment boundary (browsing), where it's row 0 and highlighted by
  # default (SELECTED==0). Hidden while the user filters a token. The
  # label names what Enter does from the CURRENT selection — run on the
  # sentinel, insert on an item — so the meaning is visible before the
  # keypress. Width-measured so the right border stays aligned even if
  # the glyph renders as 2 cells.
  if (( __NERV_HAS_SENTINEL )); then
    local sent_act=run
    (( __NERV_SELECTED != 0 )) && sent_act=insert
    local sent_txt="↩ Immediately execute — Enter: $sent_act"
    local sent_w=${(m)#sent_txt}
    # Narrow window: truncate the label on a cell boundary so the
    # sentinel row can't overflow the box either.
    if (( sent_w > row_body - 1 )); then
      sent_txt="${(mr:$(( row_body - 1 )):)sent_txt}"
      sent_w=$(( row_body - 1 ))
    fi
    local sent_pad_n=$(( row_body - 1 - sent_w ))
    (( sent_pad_n < 0 )) && sent_pad_n=0
    local sent_pad=""; repeat $sent_pad_n; do sent_pad+=" "; done
    if (( __NERV_SELECTED == 0 )); then
      colored+=("  ${BDR}│${SEL} ${SELB}${sent_txt}${SELNB}${sent_pad}${R}${BDR}│${R}")
    else
      colored+=("  ${BDR}│${DESC} ${sent_txt}${sent_pad}${BDR}│${R}")
    fi
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
    # A name that does not fit ends in `…`: cut silently, a folder that
    # lost its trailing `/` reads as a file, and two long names that
    # differ only at the end read as one.
    if (( ${(m)#display} > max_disp && max_disp > 1 )); then
      local cut_w=$(( max_disp - 1 ))
      display="${(mr:$cut_w:)display}"
      (( ${(m)#display} > cut_w )) && display="${(mr:$cut_w:)${display%?}}"
      display+="…"
    else
      display="${(mr:$max_disp:)display}"
      (( ${(m)#display} > max_disp )) && display="${(mr:$max_disp:)${display%?}}"
    fi
    # Pull icon (4th field). Empty → blank space (slot reserved
    # for alignment). `$` placeholder previously cluttered cd / ls
    # lists where every row would say `$ foo/` with no signal.
    local trail="${rest#*	}"           # description \t icon \t replace
    local row_icon="${trail#*	}"        # icon \t replace
    [[ "$row_icon" == "$trail" ]] && row_icon=""  # no tab → no icon
    row_icon="${row_icon%%	*}"
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
      __nerv_mark "$dpre" "$query" "$__NERV_C_SHL" "$__NERV_C_SHLOFF"
      colored+=("  ${BDR}│${SEL}›${glyph} ${SELB}${REPLY}${SELNB}${dpost}${row_pad}${R}${BDR}│${R}")
    else
      __nerv_mark "$dpre" "$query" "$__NERV_C_HL" "$__NERV_C_HLOFF"
      colored+=("  ${BDR}│ ${ICON}${glyph}${ITEM} ${REPLY}${HINT}${dpost}${row_pad}${BDR}│${R}")
    fi
  done

  colored+=("  ${BDR}├${hbar}┤${R}")

  # Footer: " desc … [n/total]". The counter shows only when the list
  # runs past the window — it is the one sign that more rows scroll in
  # (Fig had none at all); a list that fits shows every row already.
  # Layout inside `│...│` must equal W-2 cells (matches the body rows
  # above): " " + desc + pad + counter + " ". ASCII: cells == chars.
  # Sentinel selected → the item count alone (`[18]`); an item → its
  # 1-based position (`[3/18]`).
  local counter="" gap=0
  if (( total > visible )); then
    gap=1
    if (( __NERV_SELECTED == 0 )); then
      counter="[${total}]"
    else
      counter="[${__NERV_SELECTED}/${total}]"
    fi
  fi
  # Reserve cells for: leading " ", trailing " ", counter, and one
  # space of gap so a long description never runs into the counter.
  local sel_avail=$(( W - 4 - gap - ${#counter} ))
  (( sel_avail < 0 )) && sel_avail=0
  # Width-aware truncate: a CJK description must be cut on a cell
  # boundary or the counter is pushed past the right border. A path keeps
  # its tail — the folder name is the part that tells rows apart — so it
  # loses its head, at a `/` when one is left (`…/lemon/voucher-wiki`, not
  # `…mon/voucher-wiki`); other text loses its end. When a 2-cell glyph
  # straddles the cut, (m) keeps it whole and overshoots by 1; drop one
  # more character.
  if (( ${(m)#sel_desc} > sel_avail )); then
    local keep=$(( sel_avail - 1 ))
    if (( keep < 1 )); then
      sel_desc="${(mr:$sel_avail:)sel_desc}"
    elif [[ "$sel_desc" == [/~]* ]]; then
      sel_desc="${(ml:$keep:)sel_desc}"
      (( ${(m)#sel_desc} > keep )) && sel_desc="${(ml:$keep:)${sel_desc#?}}"
      [[ "$sel_desc" == ?*/?* ]] && sel_desc="/${sel_desc#*/}"
      sel_desc="…$sel_desc"
    else
      sel_desc="${(mr:$keep:)sel_desc}"
      (( ${(m)#sel_desc} > keep )) && sel_desc="${(mr:$keep:)${sel_desc%?}}"
      sel_desc="$sel_desc…"
    fi
  fi
  local fpad=$(( W - 4 - ${(m)#sel_desc} - ${#counter} ))
  (( fpad < 0 )) && fpad=0
  local fps=""; repeat $fpad; do fps+=" "; done
  colored+=("  ${BDR}│${sel_color} ${sel_desc}${fps}${DESC}${counter} ${BDR}│${R}")

  colored+=("  ${BDR}╰${hbar}╯${R}")

  # Reserve the space with ZLE — but only on first show or when the row
  # count changes. Re-issuing `zle -R` on every keystroke blanks the whole
  # region right before the printf repaints it; that one blank frame per
  # render is the flicker seen while arrowing through a long list. When the
  # reservation already matches, skip to the printf, which overwrites the
  # box in place (each row clears to EOL) — no blank frame.
  if (( ! __NERV_ACTIVE || __NERV_RESERVED != ${#plain} )); then
    zle -R "" "${plain[@]}"
    __NERV_RESERVED=${#plain} __NERV_RESERVED_ROWS=("${plain[@]}")
  fi

  # Anchor the popup's left edge under the input cursor. Clamp so a box
  # near the right edge shifts left to stay on screen. Each colored row
  # carries 2 leading spaces, so the footprint is W + 2 cells; keep one
  # more column of slack (start_col + 2 + W ≤ cols) or the pending-wrap
  # flag scrolls the next row and tears the box.
  __nerv_cursor_col
  local start_col=$REPLY
  (( start_col < 1 )) && start_col=1
  (( start_col + W + 2 > term_cols )) && start_col=$(( term_cols - W - 2 ))
  (( start_col < 1 )) && start_col=1

  # Save cursor, move down + overwrite each row at the anchored column,
  # restore cursor. Stays within the visible screen: MAX_VIS clamps to
  # $LINES so we don't trigger a mid-render scroll that would invalidate
  # the saved cursor position. `ESC[J` after the bottom border clears
  # what a previous, taller box left.
  #
  # The box starts on the line under the cursor even when the edit display
  # goes on below it (text after the cursor, a ghost that wraps): it covers
  # those lines, as Fig's window did. zle reserves its space below the
  # whole display, so the rows the box needs always exist; the covered
  # lines come back when the popup closes (__nerv_hide_popup).
  local move=$'\e[B\e['${start_col}'G'
  local buf=$'\e7\e[B\e['"${start_col}G${colored[1]}"$'\e[K'
  for (( i=2; i<=${#colored}; i++ )); do
    buf+="${move}${colored[$i]}"$'\e[K'
  done
  buf+=$'\e[J\e8'
  printf '%s' "$buf"

  __NERV_ACTIVE=1 __NERV_PAINTED=$buf
  # Widgets that run after this one (zsh-autosuggestions draws its ghost
  # there) change the lines the box covers, and zle's redraw at the end of
  # the key wipes the box on them. Paint once more after that redraw: a
  # `zle -F` handler runs when zle is back to waiting for a key, and
  # /dev/null is readable at once. The brace group keeps `2>/dev/null` off
  # the shell (see __NERV_SOCK_FD).
  if (( ${+__NERV_IDLE_FD} )) || { exec {__NERV_IDLE_FD}</dev/null } 2>/dev/null; then
    zle -F $__NERV_IDLE_FD __nerv_repaint
  fi
}

# Paint the open popup again, as last drawn. `zle -R` first, so the redraw
# zle does after a handler finds nothing left to change.
__nerv_repaint() {
  [[ -n $1 ]] && zle -F $1
  if (( __NERV_ACTIVE )) && [[ -n $__NERV_PAINTED ]]; then
    zle -R "" "${__NERV_RESERVED_ROWS[@]}"
    printf '%s' "$__NERV_PAINTED"
  fi
}

# zsh-autosuggestions in async mode (its default) draws its ghost later
# still, from its own `zle -F` handler. Paint again after it.
__nerv_repaint_after_autosuggest() {
  __nerv_as_async_response "$@"
  local rc=$?
  __nerv_repaint
  return rc
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
  # The clear erases everything under the cursor row, and with text
  # after the cursor that includes the rows a wrapped line continues on.
  # zle believes they are still drawn; only a full redraw brings them back.
  [[ -n $RBUFFER ]] && zle reset-prompt 2>/dev/null
}

__nerv_insert_selected() {
  (( ${#__NERV_ITEMS} == 0 )) && return 1
  local sel_idx=$__NERV_SELECTED
  (( sel_idx < 1 )) && sel_idx=1
  (( sel_idx > ${#__NERV_ITEMS} )) && sel_idx=1
  local sel_line="${__NERV_ITEMS[$sel_idx]}"
  local insertion="${sel_line%%	*}"
  [[ -z "$insertion" ]] && return 1

  # A corrected command word (`zpeh li` → `zeph`) rewrites its own span,
  # not the token under the cursor, and keeps the arguments. It is not a
  # completion the user built on, so there is no frecency to record.
  local REPLY; __nerv_row_span "$sel_line"
  if [[ -n "$REPLY" ]]; then
    local span_start=${REPLY%,*} span_end=${REPLY#*,}
    BUFFER="${BUFFER[1,span_start]}${(q)insertion}${BUFFER[span_end+1,-1]}"
    CURSOR=${#BUFFER}
    (( ${+POSTDISPLAY} )) && POSTDISPLAY=''
    printf '%s' "$__NERV_CLEAR_ESC"
    __NERV_PREV_LBUFFER="$LBUFFER"
    __nerv_reset_state
    zle reset-prompt 2>/dev/null
    zle redisplay 2>/dev/null
    return 0
  fi

  # Compute new BUFFER + CURSOR from scratch. Avoid LBUFFER/RBUFFER
  # split because some plugins (zsh-autosuggestions) wrap those
  # accessors and the assignments don't always propagate.
  local before="$LBUFFER"
  local after="$RBUFFER"

  # Strip trailing partial word from `before` (the word the user was
  # completing). The split has to skip backslash-escaped whitespace or
  # it cuts inside a path we ourselves quoted: `${before% *}` on
  # `cd My\ Fol` yields `cd My\`, mangling the line on the second Tab.
  # Mirrors the engine's tokenizer, which splits by the same rule.
  local pre="" i=1 wstart=1 n=${#before}
  while (( i <= n )); do
    case "${before[i]}" in
      '\') (( i += 2 )) ;;                       # escape swallows the next char
      ' '|$'\t') (( i++ )); wstart=$i ;;         # real break: word starts after it
      *) (( i++ )) ;;
    esac
  done
  (( wstart > 1 )) && pre="${before[1,wstart-1]}"

  # Strip leading partial word from `after` (rest of the same word
  # when cursor is mid-token) — but only when the insertion spells that
  # rest: `che|ckout` + `checkout`. Text that merely touches the cursor
  # is another word (`git commit --am|-m "x"`, the cursor parked at the
  # start of `-m`), and stays.
  local post="$after"
  if [[ -n "$after" && "$after[1]" != ' ' && "$after[1]" != $'\t' ]]; then
    local rest="${after%%[[:space:]]*}"
    if [[ ${(L)insertion} == "${(L)before[wstart,-1]}${(L)rest}"* \
          || ${(L)insertion} == *"${(L)rest}" ]]; then
      post="${after#$rest}"
    fi
  fi

  # Trailing separator: a completed token gets a space so the next arg
  # can be typed — EXCEPT a directory (`apps/`) or a `requiresSeparator`
  # option (`--color=`), where the user drills deeper with no gap. Fig
  # parity: `cd apps/` leaves the cursor flush so the next Tab lists
  # `apps/mobile/…` instead of forcing a backspace.
  local sep=" "
  [[ "$insertion" == */ || "$insertion" == *= ]] && sep=""

  # Shell-quote before splicing: macOS is full of paths with spaces
  # (`Application Support`, `Google Drive`), and a raw insertion turns
  # `cd My Folder/` into two words — zsh reads that as the `cd old new`
  # substitution form and silently lands elsewhere. `(q)` is the
  # backslash flavor, matching what zsh's own completion emits, and it
  # leaves ordinary tokens (`checkout`, `--amend`, `--color=`) untouched.
  #
  # A leading `~/` must stay bare or tilde expansion dies (`(q)` would
  # emit `\~/…`, a literal directory named `~`), so quote only the part
  # after it. Quoting is a per-shell concern and deliberately lives here
  # rather than in the engine, whose `insertion` is also matched against
  # the typed prefix and shared with the bash/fish PTY path.
  local ins_q
  if [[ "$insertion" == '~/'* ]]; then
    ins_q="~/${(q)${insertion#\~/}}"
  else
    ins_q="${(q)insertion}"
  fi

  # Build full BUFFER and place CURSOR right after the insertion+sep.
  BUFFER="${pre}${ins_q}${sep}${post# }"
  CURSOR=$(( ${#pre} + ${#ins_q} + ${#sep} ))

  # Frecency: record the accept in the background so the next
  # completion request can boost it. Fire-and-forget — never
  # block on the IPC, never surface its errors to the user.
  # `${BUFFER%% *}` peels the first word — the spec the user
  # invoked (e.g. `git`, `cd`). Resolve a leading alias the same way
  # the completion request did, so `g checkout` records under `git`
  # (the daemon boosts by the spec name of the line it completed).
  # REPLY is already local (declared for the span check above); a second
  # `local` would make zsh print its current value onto the terminal.
  __nerv_expand_alias_line "$BUFFER"
  local spec_name="${REPLY%% *}"
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

# True when `1` is a word only the shell can resolve — an alias,
# function, builtin or reserved word. The daemon sees none of these.
__nerv_shell_word() {
  (( ${+aliases[$1]} + ${+functions[$1]} + ${+builtins[$1]} \
     + ${reswords[(Ie)$1]} ))
}

# True when the shell itself would run `1`: a shell word or a hashed
# command. A correction row aimed at such a word is always wrong.
__nerv_shell_knows() {
  __nerv_shell_word "$1" || (( ${+commands[$1]} ))
}

# Fifth wire field of a `_complete` row: `start,end`, the character span
# of the line the row rewrites (a corrected command word), or empty for
# the usual "replace the token under the cursor". Sets REPLY.
__nerv_row_span() {
  local -a fields=("${(@ps:\t:)1}")
  REPLY="${fields[5]}"
}

# Expand a leading alias so the engine sees the real command: with
# `alias g=git` the spec lookup for `g push` finds nothing, so rewrite
# the line to `git push` before the IPC call. Sets REPLY to the
# rewritten line (input unchanged when nothing applies). Only plain
# word-list bodies are substituted — a body with shell metacharacters
# (pipes, subshells, quoting, separators) would need a real parse, so
# it's left alone and the engine sees the original line. One level
# deep on purpose: chained aliases resolve on the next request anyway.
__nerv_expand_alias_line() {
  local line="$1"
  REPLY="$line"
  [[ "$line" == *" "* ]] || return 0
  local first="${line%% *}"
  (( ${+aliases[$first]} )) || return 0
  local body="${aliases[$first]}"
  [[ -z "$body" || "$body" == "$first" || "$body" == "$first "* ]] && return 0
  [[ "$body" == *[\;\|\&\<\>\(\)\`\$\"\']* ]] && return 0
  REPLY="${body}${line#"$first"}"
}

# ---------------------------------------------------------------------------
# Shell completion fallback (docs/spec-conversion-policy.md §6.4)
# ---------------------------------------------------------------------------
# A command with no hand-written spec (`_complete` rc 5/6) may still have a
# zsh completion function the CLI installed itself (`_uv`, `_rg`, …). Run
# it the way Tab would, but record what it offers instead of showing it:
# `compadd` is swapped for __nerv_compsys_compadd for the one call, the
# pattern fzf-tab uses. Sets __NERV_COMPSYS_ROWS to wire rows.
__nerv_compsys_capture() {
  __NERV_COMPSYS_ROWS=()
  # No compinit, no completion system: nothing to ask.
  (( ${+functions[_main_complete]} )) || return 1
  local cmd="$1"
  # Only a function written for this command. The generic fallbacks
  # would answer every command with the same file list.
  local fn="${_comps[$cmd]-}"
  [[ -z "$fn" || "$fn" == (_default|_files|_normal) ]] && return 1
  # stderr off: a completion function that errors must not print over the
  # popup. The error is still visible under NERV_DEBUG.
  zle __nerv_compsys 2>/dev/null
  local rc=$?
  if [[ -n "${NERV_DEBUG:-}" ]]; then
    print -r -- "  compsys: $fn rc=$rc rows=${#__NERV_COMPSYS_ROWS}" >> /tmp/nerv-debug.log
  fi
  (( ${#__NERV_COMPSYS_ROWS} ))
}

# `compadd` for the duration of a capture. Filter-only calls (-O/-A/-D)
# add nothing and pass straight through — recording them would list each
# candidate twice. The rest are re-issued unchanged afterwards so the
# completion function sees the return status it expects. Locals carry a
# `__` prefix: `-d desc` names an array in the caller's scope (`_du`), and
# a plain `desc` here would shadow it.
__nerv_compsys_compadd() {
  local -a __hits __dscr
  local __P="" __p="" __S="" __W="" __d="" __isfile=0 __filter=0
  local __a __c __v __k=1 __j
  # zparseopts can't read clusters (`_path_files` passes `-Qf`, others
  # `-qS/`), so walk the options the way compadd does: a letter that
  # takes a value ends its cluster, with the value attached or next.
  while (( __k <= $# )); do
    __a="${@[__k]}"
    [[ "$__a" == -- || "$__a" != -?* ]] && break
    for (( __j = 2; __j <= ${#__a}; __j++ )); do
      __c="${__a[__j]}"
      if [[ "$__c" == [PSpsiIWdJVXxrRMFEDOA] ]]; then
        if (( __j < ${#__a} )); then
          __v="${__a[__j+1,-1]}"
        else
          (( __k++ ))
          __v="${@[__k]}"
        fi
        case "$__c" in
          P) __P="$__v" ;; p) __p="$__v" ;; S) __S="$__v" ;; W) __W="$__v" ;;
          d) __d="$__v" ;; O|A|D) __filter=1 ;;
        esac
        break
      fi
      # `-o` takes an optional order word as the next argument.
      if [[ "$__c" == o && __j -eq ${#__a} \
            && "${@[__k+1]-}" == (match|nosort|numeric|reverse)(,*|) ]]; then
        (( __k++ ))
        break
      fi
      [[ "$__c" == f ]] && __isfile=1
    done
    (( __k++ ))
  done
  if (( __filter )); then
    builtin compadd "$@"
    return
  fi
  [[ -n "$__d" ]] && __dscr=("${(@P)__d}")
  # No status check: -A collects without adding, and then returns
  # non-zero even with hits in hand (`_path_files`, measured).
  builtin compadd -A __hits -D __dscr "$@"
  # nerv replaces the whole token under the cursor, so a candidate zsh
  # splits (`--mode=` + `fast`, `src/` + `main.rs`) is put back together.
  # The widget quotes the insertion itself, so every piece goes in as its
  # plain value: IPREFIX, -P and -p are buffer text, already quoted, and
  # -A hands hits back quoted as they would be inserted — whether zsh
  # quoted them (`odd$'\t'name`) or the caller did and passed -Q
  # (`_path_files`, `My\ File`).
  local __pre="${(Q)IPREFIX}${(Q)__P}${(Q)__p}" __hit __desc __i __suf
  # What zsh itself would append on insert: an explicit -S suffix (not a
  # plain space — the widget adds that), or `/` after a directory when the
  # caller marked the hits as files (-f).
  # -s (a hidden suffix) is left out: it is text already after the cursor
  # in the word, and the popup only opens with the cursor at the end.
  local __s="$__S"
  [[ "$__s" == ' ' ]] && __s=""
  for (( __i = 1; __i <= ${#__hits} && ${#__NERV_COMPSYS_ROWS} < 500; __i++ )); do
    __hit="${(Q)__hits[__i]}"
    __suf="$__s"
    # -W names the directory the hits live in, path typed so far included
    # (`_path_files` passes `subdir/` as both -W and -p); without it they
    # sit under the -p prefix.
    if (( __isfile )) && [[ -z "$__suf" \
          && -d "${${__W:+${(Q)__W%/}/}:-${(Q)__p}}${__hit}" ]]; then
      __suf=/
    fi
    __hit="${__hit//[$'\t\n']/ }"
    # `_describe` pads a display string as `name   -- description`.
    __desc="${__dscr[__i]-}"
    [[ "$__desc" == *' -- '* ]] && __desc="${__desc#* -- }" || __desc=""
    __desc="${__desc//[$'\t\n']/ }"
    __NERV_COMPSYS_ROWS+=("${__pre}${__hit}${__suf}"$'\t'"${__hit}"$'\t'"${__desc}"$'\t\t')
  done
  builtin compadd "$@"
}

# Rows for the line being typed (`1`, alias-expanded), from the cache or a
# fresh capture. The function runs once per word, not once per key: while
# the word under the cursor still starts with the one the last capture was
# taken for, in the same directory, line context and prompt, its rows are
# filtered here instead. A word that changes shape re-captures — a new
# `/` moves into another directory, a new `=` starts an option's value, a
# leading `-` asks for options — and so does a capture cut off at the
# 500-row cap, whose missing tail a filter cannot see (`brew install `).
# A word nothing matches stays empty rather than asking again on every key,
# unless a matcher-list makes zsh match beyond a plain prefix.
# A capture over 300 ms (1000 ms for a command's first, which pays
# autoload and cold caches — `gh pr ` measured 480 ms cold against 60 ms
# warm) marks the command slow, and a slow command is not captured again
# in this shell. Sets __NERV_COMPSYS_ROWS.
__nerv_compsys_rows() {
  __NERV_COMPSYS_ROWS=()
  local word=""
  if [[ "$LBUFFER" != *[[:space:]] ]]; then
    local -a lw=("${(z)LBUFFER}")
    word="${lw[-1]}"
  fi
  local plain="${(Q)word}"
  local key="$PWD"$'\0'"${LBUFFER[1,${#LBUFFER}-${#word}]}"
  # The word's shape: its `/` and `=` count, and whether it is an option.
  # A capture is reused only for a word of the same shape.
  local shape="${plain//[^\/=]/}${${plain[1]}/[^-]/}"
  if [[ "$key" == "$__NERV_COMPSYS_KEY" && "$plain" == "$__NERV_COMPSYS_WORD"* \
        && "$shape" == "$__NERV_COMPSYS_SHAPE" ]] \
     && (( ${#__NERV_COMPSYS_CACHE} < 500 )); then
    __NERV_COMPSYS_ROWS=("${(@M)__NERV_COMPSYS_CACHE:#${(b)plain}*}")
    (( ${#__NERV_COMPSYS_ROWS} )) && return 0
    # A plain prefix is how zsh matches unless the user set a
    # matcher-list (oh-my-zsh sets case-insensitive): then a word the
    # saved rows miss may still match, and only zsh can say.
    # Asked once per word: a capture that came back empty stays empty
    # until the word changes shape.
    local -a matchers
    zstyle -a ':completion:' matcher-list matchers
    (( ${#matchers} && ${#__NERV_COMPSYS_CACHE} )) || return 1
  fi
  # An array first: `${${(z)1}[1]}` on a one-word line is a scalar, and
  # [1] would then take its first character.
  local -a words=("${(z)1}")
  local cmd="${words[1]}"
  (( ${+__NERV_COMPSYS_SLOW[$cmd]} )) && return 1
  local t0=$EPOCHREALTIME
  __nerv_compsys_capture "$cmd"
  local rc=$? ms=$(( (EPOCHREALTIME - t0) * 1000 ))
  if (( ms > (${+__NERV_COMPSYS_SEEN[$cmd]} ? 300 : 1000) )); then
    __NERV_COMPSYS_SLOW[$cmd]=1
  fi
  __NERV_COMPSYS_SEEN[$cmd]=1
  __NERV_COMPSYS_KEY="$key" __NERV_COMPSYS_WORD="$plain" __NERV_COMPSYS_SHAPE="$shape"
  __NERV_COMPSYS_CACHE=("${__NERV_COMPSYS_ROWS[@]}")
  return rc
}

# The completion widget behind __nerv_compsys_capture. Nothing is inserted
# or listed: the popup draws the rows. The restore and both resets sit in
# `always`: an error inside the completion function aborts everything
# after the block, and a `compadd` left defined would hijack the user's
# own Tab.
__nerv_compsys_widget() {
  local saved="${functions[compadd]-}" rc=0
  {
    functions[compadd]="${functions[__nerv_compsys_compadd]}"
    _main_complete
    rc=$?
  } always {
    if [[ -n "$saved" ]]; then
      functions[compadd]="$saved"
    else
      unfunction compadd 2>/dev/null
    fi
    compstate[insert]=''
    compstate[list]=''
  }
  return rc
}
zle -C __nerv_compsys complete-word __nerv_compsys_widget

# ---------------------------------------------------------------------------
# Core widget
# ---------------------------------------------------------------------------
# ---------------------------------------------------------------------------
# Socket transport (docs/history-suggestions.md §6). Each request used to
# fork `nerv` — 12–14 ms a keystroke, 5 of them the fork alone. zsh can
# speak to the daemon itself: zsh/net/socket opens the UDS, one line of
# `\x1f`-separated fields goes out, rows come back ended by
# `\x1fend\t<seq>\t<code>` (nerv_engine::wire). The connection stays open
# for the shell's life; a failure closes it and that request takes the
# fork path, and the next one reconnects. NERV_SOCKET=0 forces the fork
# path. Command text never reaches any argv this way.
# ---------------------------------------------------------------------------
typeset -gi __NERV_SOCK_FD=0 __NERV_SEQ=0 __NERV_SOCK_OK=0
typeset -ga __NERV_SOCK_LINES=()
zmodload zsh/net/socket 2>/dev/null && __NERV_SOCK_OK=1

__nerv_sock_close() {
  # The brace group matters: a bare `exec {fd}<&- 2>/dev/null` has no
  # command, so zsh applies `2>/dev/null` to the shell itself — for good.
  (( __NERV_SOCK_FD )) && { exec {__NERV_SOCK_FD}<&- } 2>/dev/null
  __NERV_SOCK_FD=0
}

# Send one request line built from "$@" (verb first). Returns 1 when the
# socket path is off or broken.
__nerv_sock_send() {
  (( __NERV_SOCK_OK )) && [[ ${NERV_SOCKET:-1} != 0 ]] || return 1
  if (( ! __NERV_SOCK_FD )); then
    zsocket "$HOME/Library/Caches/nerv/nervd.sock" 2>/dev/null || return 1
    __NERV_SOCK_FD=$REPLY
  fi
  local -a fields
  local f
  for f in "$@"; do
    f=${f//\\/\\\\}
    f=${f//$'\n'/\\n}
    fields+=("${f//$'\x1f'/\\u}")
  done
  print -r -u $__NERV_SOCK_FD -- "${(pj:\x1f:)fields}" 2>/dev/null && return 0
  __nerv_sock_close
  return 1
}

# Read one reply into __NERV_SOCK_LINES, its exit code into REPLY. $1 is
# the seq to wait for — the end line of a reply the widget already gave up
# on is skipped with its rows. $2 bounds each read, in seconds. A JSON
# line means a daemon from before the text protocol: the socket path is
# turned off for this shell.
__nerv_sock_read() {
  local seq=$1 line rest
  __NERV_SOCK_LINES=()
  while :; do
    if ! IFS= read -r -t $2 -u $__NERV_SOCK_FD line; then
      __nerv_sock_close
      return 1
    fi
    case $line in
      $'\x1f'end$'\t'*)
        rest=${line#*$'\t'}
        if [[ ${rest%%$'\t'*} == "$seq" ]]; then
          REPLY=${rest##*$'\t'}
          return 0
        fi
        __NERV_SOCK_LINES=() ;;
      '{"kind":'*)
        # A JSON reply: only a daemon from before the text protocol sends
        # one to a text request. No completion row starts this way.
        __nerv_sock_close
        __NERV_SOCK_OK=0
        return 1 ;;
      *) __NERV_SOCK_LINES+=("$line") ;;
    esac
  done
}

# One request and its reply: "$1" = read bound, the rest = fields after
# the verb's seq. Sets __NERV_SOCK_LINES and REPLY (the exit code).
__nerv_sock_call() {
  local bound=$1 verb=$2
  shift 2
  (( ++__NERV_SEQ ))
  __nerv_sock_send "$verb" $__NERV_SEQ "$@" || return 1
  __nerv_sock_read $__NERV_SEQ $bound
}

# Take the `…loading` hint off the screen once anything else answers.
__nerv_clear_loading() {
  (( __NERV_LOADING_SHOWN )) || return 0
  __NERV_LOADING_SHOWN=0
  zle -M ""
}

__nerv_complete() {
  if [[ -n "${NERV_DEBUG:-}" ]]; then
    print -r -- "[$(date +%H:%M:%S.%N)] complete LBUFFER=[$LBUFFER] PREV=[$__NERV_PREV_LBUFFER] ACTIVE=$__NERV_ACTIVE ITEMS=${#__NERV_ITEMS} CURSOR=$CURSOR" >> /tmp/nerv-debug.log
  fi
  (( __NERV_PASTING )) && return
  # Mid-line completion: the engine completes `line[..cursor]`
  # (`complete_in` + `before_cursor` char handling), so the popup stays
  # up when the cursor sits in the middle of the buffer. The request
  # sends LBUFFER (exactly the text before the cursor) with its length
  # as the char cursor; the insert path splices the remainder back
  # (`__nerv_insert_selected` strips the partial word from RBUFFER).
  # Quoted strings stay empty via the engine's `cursor_in_open_quote`.
  [[ "$LBUFFER" == "$__NERV_PREV_LBUFFER" ]] && return
  __NERV_PREV_LBUFFER="$LBUFFER"
  __NERV_SELECTED=0
  __NERV_RANKED_FOR=$LBUFFER __NERV_RANKED="" __NERV_PREFILLED=""

  if [[ -z "${LBUFFER// /}" ]]; then
    __nerv_clear_loading
    __nerv_hide_popup
    # Back to an empty line: the prompt's prediction returns. Written even
    # when zsh-autosuggestions owns the ghost: it never paints an empty
    # line, and its wrapper does not fetch over one.
    [[ -z $BUFFER ]] && (( ! __NERV_YIELD )) && POSTDISPLAY=$__NERV_PREDICTED
    return
  fi

  # History inline suggestion (autosuggestions / Fig) applies whether or
  # not the spec engine has anything. The daemon's ranked history answers
  # it; zsh's own `$history` only where no ranking came back — a bare
  # command word, a failed request, a reply without a ghost row
  # (docs/history-suggestions.md §3). That scan walks the whole history,
  # so it runs only there.
  local REPLY hist_ghost=""

  # A bare command name (no space yet) goes through the same request:
  # the engine answers it from its command-name list — `doc` → `docker`
  # — and returns nothing once the name is complete, so Enter on a
  # finished `git` still runs git. History keeps its priority over the
  # popup's ghost either way (`pwd` → ` pbcopy`).
  #
  # A bare token naming a shell alias, function, builtin or reserved word
  # is already a finished command. The daemon can't see any of them, so
  # its exact-name guard won't fire and it would keep offering longer
  # names or a correction — Enter on a complete `k` (alias k=kubectl)
  # would swap the line for `kubectl`, and `export` for `expo`. Leading
  # whitespace is stripped first: ` k` (the HIST_IGNORE_SPACE habit) is
  # the same command to the engine, whose tokenizer skips it too.
  local bare="${LBUFFER#"${LBUFFER%%[^[:space:]]*}"}"
  if [[ "$bare" != *[[:space:]]* ]] \
     && __nerv_shell_word "$bare"; then
    __nerv_hide_popup
    __nerv_history_ghost
    __nerv_ghost "$REPLY"
    return
  fi

  # Resolve a leading alias (g=git, cat=bat) so the spec lookup hits.
  # A line with no space is left alone by the expansion below.
  # LBUFFER is exactly the text before the cursor, so the cursor sits
  # at the end of whatever line we send — use the expanded length,
  # not $CURSOR (the expansion only touches the first word, which is
  # fully inside LBUFFER whenever LBUFFER holds a space).
  __nerv_expand_alias_line "$LBUFFER"
  local send_line="$REPLY"

  local resp
  # NERV_COMPSYS=0 turns the shell-completion fallback off: no flag, so
  # the exit codes stay the pre-fallback ones.
  local -a compsys_flag=(--compsys)
  [[ "${NERV_COMPSYS:-1}" == 0 ]] && compsys_flag=()
  local rc
  # The socket first; a cold spec parse can take a while, so its read
  # bound is generous. The fork path answers the same rows.
  if __nerv_sock_call 2 complete "$send_line" ${#send_line} "${PWD:A}" \
       "$__NERV_PREV_CMD" "$LBUFFER" "${compsys_flag:+c}"; then
    resp=${(pj:\n:)__NERV_SOCK_LINES}
    rc=$REPLY
  else
    # NERV_PREV / NERV_TYPED feed the ranked ghost and stay out of argv.
    # NERV_TYPED is also this widget's "I read the ghost row" flag.
    resp=$(NERV_PREV="$__NERV_PREV_CMD" NERV_TYPED="$LBUFFER" \
           "$__NERV_BIN" _complete $compsys_flag "$send_line" ${#send_line} 2>/dev/null)
    rc=$?
  fi
  # rc 4-6 are successes (`complete_exit_code` in nerv-cli): 4 = the typed
  # token already names one of the candidates (see the default selection
  # below), 5 = no hand-written spec covers the command, 6 = both.
  # rc 7 = the spec is still loading (cold first keystroke): no rows,
  # but not a miss either — the widget shows its one-line loading hint.
  local token_complete=0 unspecced=0 loading=0
  case $rc in
    4) token_complete=1; rc=0 ;;
    5) unspecced=1; rc=0 ;;
    6) token_complete=1; unspecced=1; rc=0 ;;
    7) loading=1; rc=0 ;;
  esac
  if (( rc != 0 )); then
    # The history ghost never needed the engine, so a dead or
    # mismatched daemon must not cost the user that too. Assigning
    # unconditionally also clears the previous keystroke's ghost.
    __nerv_history_ghost
    __nerv_ghost "$REPLY"
    if [[ -n "${NERV_DEBUG:-}" ]]; then
      print -r -- "  complete: BIN call FAILED rc=$rc" >> /tmp/nerv-debug.log
    fi
    # rc 3 = E5 spec schema mismatch (daemon up, cache wrong version);
    # rc 7 never reaches here (folded to loading above);
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
    __nerv_clear_loading
    return
  fi
  if [[ -n "${NERV_DEBUG:-}" ]]; then
    print -r -- "  complete: got $(print -r -- "$resp" | wc -l | tr -d ' ') lines" >> /tmp/nerv-debug.log
  fi

  local -a rlines=("${(@f)resp}")
  rlines=("${(@)rlines:#}")

  # First row `\x1fghost\t<command>`: the daemon ranked the history. Its
  # answer wins over `$history`, "nothing" included — an absent row means
  # it had no history to rank, and the `$history` ghost stands.
  if [[ "${rlines[1]}" == $'\x1f'ghost$'\t'* ]]; then
    local ranked="${rlines[1]#*$'\t'}"
    rlines=("${(@)rlines[2,-1]}")
    if (( ${#ranked} > ${#LBUFFER} )) && [[ "${ranked[1,${#LBUFFER}]}" == "$LBUFFER" ]]; then
      hist_ghost="${ranked[${#LBUFFER}+1,-1]}"
      __NERV_RANKED=$ranked
    fi
  else
    __nerv_history_ghost
    hist_ghost=$REPLY
  fi

  # A command-word correction comes back alone — the engine returns it as
  # the whole answer (`complete.rs`, the `registry.lookup` miss branch),
  # which is why one row is the only shape checked here. Drop it when the
  # word is one the shell runs itself — alias, function, builtin
  # (`export` is two edits from the `expo` spec), reserved word or hashed
  # command — or when the alias expansion above changed the line, since
  # the span indexes the line that was sent.
  if (( ${#rlines} == 1 )); then
    __nerv_row_span "${rlines[1]}"
    if [[ -n "$REPLY" ]]; then
      local fix_start=${REPLY%,*} fix_end=${REPLY#*,}
      if [[ "$send_line" != "$LBUFFER" ]] \
         || __nerv_shell_knows "${LBUFFER[fix_start+1,fix_end]}"; then
        rlines=()
      fi
    fi
  fi

  # No hand-written spec: rows zsh's own completion function offers beat
  # the ones scraped from `--help`. Nothing captured keeps those.
  if (( unspecced )) && __nerv_compsys_rows "$send_line"; then
    rlines=("${__NERV_COMPSYS_ROWS[@]}")
    # rc 6's "typed token is complete" described the rows just replaced.
    token_complete=0
  fi

  if (( ${#rlines} == 0 )); then
    # No spec completions, but a history suggestion may still apply.
    __nerv_hide_popup
    [[ -n "$hist_ghost" ]] && __nerv_ghost "$hist_ghost"
    # Cold first keystroke: the spec parse or derivation lands on the
    # next key. A grey one-line hint tells "loading" apart from a
    # settled miss (same channel as the E1/E5 hints). Shown on every
    # loading keystroke, cleared the moment anything else arrives —
    # rows overwrite the status themselves, settled silence clears it.
    # `zle -M`, not `zle -R`: a status line is gone when the widget
    # returns, a message stays until the next one.
    if (( loading )); then
      zle -M "…loading"
      __NERV_LOADING_SHOWN=1
    else
      __nerv_clear_loading
    fi
    return
  fi
  __nerv_clear_loading

  # Shell-name rows arrive with the engine's generic description. An
  # alias knows its expansion locally ($aliases is already loaded for
  # line expansion) — paint `alias → body` instead.
  local __sn_i __sn_ins tab=$'\t'
  for (( __sn_i = 1; __sn_i <= ${#rlines}; __sn_i++ )); do
    __sn_ins="${rlines[__sn_i]%%$tab*}"
    (( ${+aliases[$__sn_ins]} )) || continue
    local -a __sn_fields=("${(@ps:\t:)rlines[__sn_i]}")
    # Only rows the engine sourced from the shell-name registry — a spec
    # row that happens to share the name (`git gr` → `grep`) keeps its own.
    if [[ "${__sn_fields[3]}" == "shell function" ]]; then
      __sn_fields[3]="alias → ${aliases[$__sn_ins]}"
      rlines[__sn_i]="${(pj:\t:)__sn_fields}"
    fi
  done
  # The ghost and the popup must not disagree: when the history ghost
  # continues the word being typed into one of the rows (`yarn we` +
  # ghost `b:start` → `web:start`), that row goes first, so the selection
  # Enter and Tab act on is the one the grey text shows. The ghost's next
  # word is compared to each row's insertion; a ghost that starts with a
  # space finished the word already and names no row.
  if [[ -n "$hist_ghost" && "$hist_ghost" != [[:space:]]* ]]; then
    local want="${LBUFFER##*[[:space:]]}${hist_ghost%%[[:space:]]*}" __gi
    for (( __gi = 2; __gi <= ${#rlines}; __gi++ )); do
      if [[ "${rlines[__gi]%%$tab*}" == "$want" ]]; then
        rlines=("${rlines[__gi]}" "${(@)rlines[1,__gi-1]}" "${(@)rlines[__gi+1,-1]}")
        break
      fi
    done
  fi
  __NERV_ITEMS=("${rlines[@]}")
  __nerv_measure_items   # size the box once; show_popup reads the cache
  # The "Immediately execute" sentinel shows ONLY at a segment boundary —
  # LBUFFER ends in whitespace or `/` (`z `, `cd apps/`), i.e. the user is
  # browsing, not filtering. There it's the default selection so Enter
  # runs the command. Once the user types into a token (`z ad`), the
  # sentinel disappears entirely and the first real match is highlighted,
  # so Tab/Enter picks it. Mirrors the ghost's mid-token gate.
  #
  # Two exceptions, both about what Enter should mean:
  # - A lone command-word correction (`dokcer ⎵`) is selected even after
  #   the space: running the typo can only end in "command not found".
  # - A token typed in full (`pnpm dev`, rc 4) keeps the sentinel even
  #   mid-token: the rows left only extend it (`dev:web`), and Enter on a
  #   finished name means "run it", not "insert something longer".
  local lone_fix=0
  if (( ${#rlines} == 1 )); then
    __nerv_row_span "${rlines[1]}"
    [[ -n "$REPLY" ]] && lone_fix=1
  fi
  if (( token_complete )) \
     || { (( ! lone_fix )) \
          && [[ "$LBUFFER" == *' ' || "$LBUFFER" == *$'\t' || "$LBUFFER" == */ ]]; }; then
    __NERV_HAS_SENTINEL=1
    __NERV_SELECTED=0
  elif (( lone_fix )) && [[ "$LBUFFER" == *' ' || "$LBUFFER" == *$'\t' || "$LBUFFER" == */ ]]; then
    __NERV_HAS_SENTINEL=1
    __NERV_SELECTED=1
  else
    __NERV_HAS_SENTINEL=0
    __NERV_SELECTED=1
  fi
  if [[ -n "$hist_ghost" ]]; then
    __nerv_ghost "$hist_ghost"
  else
    __nerv_set_ghost
  fi
  # zsh-autosuggestions clears its ghost while this widget runs and draws
  # it after, so the box would be laid out for a one-line edit display and
  # then covered by a ghost that wraps. Put the ghost it is about to draw
  # (the ranked line nerv hands it, or the same `$history` match) in place
  # first, styled as the plugin styles it (__nerv_paint_ghost).
  # With no ranked line the plugin falls through to its own strategies;
  # its `history` one is this same `$history` scan. Only when the plugin
  # will draw — loaded for real, not disabled, the buffer under its size
  # cap (`_zsh_autosuggest_modify`) — or the ghost put here would stay.
  local max=${ZSH_AUTOSUGGEST_BUFFER_MAX_SIZE-}
  if (( __NERV_GHOST_OFF && ! __NERV_YIELD && $+functions[_zsh_autosuggest_fetch] \
        && ! ${+_ZSH_AUTOSUGGEST_DISABLED} )) \
     && [[ -z $RBUFFER ]] && [[ -z $max || ${#BUFFER} -le $max ]]; then
    local pre=$hist_ghost
    if [[ -z $pre ]] && (( ${${(@)=ZSH_AUTOSUGGEST_STRATEGY}[(Ie)history]} )); then
      __nerv_history_ghost force
      pre=$REPLY
    fi
    POSTDISPLAY=$pre
    __NERV_PREFILLED=$pre
  fi
  __nerv_show_popup "${rlines[@]}"
}

# Inline ghost text (POSTDISPLAY), unless another plugin owns it.
# zsh-autosuggestions paints the same slot from its own widget wrappers;
# two writers overwrite each other on every key, so with it loaded nerv
# stops writing the slot while typing and hands its ranking to the plugin
# instead, as the `nerv` strategy (docs/history-suggestions.md §7).
# Decided at the first prompt, not when this file is sourced: a plugin
# manager may load it after nerv.
typeset -gi __NERV_GHOST_OFF=0 __NERV_GHOST_CHECKED=0
# NERV_AUTOSUGGEST=0 with the plugin loaded: nerv shows only its popup, as
# in 0.1.19 — no ranking handed over, no empty-prompt prediction either.
typeset -gi __NERV_YIELD=0
# The ghost __nerv_complete put in place for zsh-autosuggestions before
# painting the popup (see there); styled here until the plugin redraws it.
typeset -g __NERV_PREFILLED=""
# The last paint and the rows reserved for it, for
# __nerv_repaint_after_autosuggest.
typeset -g __NERV_PAINTED=""
typeset -ga __NERV_RESERVED_ROWS=()
# POSTDISPLAY is drawn after the whole buffer, so with text after the
# cursor a ghost would read as glued to that text: none is shown.
__nerv_ghost() {
  (( __NERV_GHOST_OFF )) && return
  [[ -n $RBUFFER ]] && POSTDISPLAY='' || POSTDISPLAY=$1
}
__nerv_ghost_owner() {
  (( __NERV_GHOST_CHECKED )) && return
  __NERV_GHOST_CHECKED=1
  # What was found, for the one-time notice and `nerv doctor` (paths.rs
  # AUTOSUGGEST_SEEN_NAME). Removed once the plugin is gone.
  local seen=$HOME/Library/Caches/nerv/autosuggest-seen mode
  # Defined for the life of the shell: MANUAL_REBIND removes the precmd
  # hook, not the function.
  if (( ! ${+functions[_zsh_autosuggest_start]} )); then
    [[ -e $seen ]] && rm -f -- $seen
    return
  fi
  __NERV_GHOST_OFF=1
  if (( $+functions[_zsh_autosuggest_async_response] \
        && ! $+functions[__nerv_as_async_response] )); then
    functions -c _zsh_autosuggest_async_response __nerv_as_async_response
    functions -c __nerv_repaint_after_autosuggest _zsh_autosuggest_async_response
  fi
  if [[ ${NERV_AUTOSUGGEST:-1} == 0 ]]; then
    mode=yield __NERV_YIELD=1
  else
    mode=strategy
    # First, so the ranking answers before the user's own strategies; they
    # still answer whenever it has nothing.
    # Split like the plugin does (`${=…}`): a user may have set a string.
    local -a strategies=(${=ZSH_AUTOSUGGEST_STRATEGY})
    ZSH_AUTOSUGGEST_STRATEGY=(nerv ${strategies:#nerv})
  fi
  # Said once per install, and again only when the mode changes; the file
  # is not rewritten otherwise, so a shell start costs one read.
  local was
  [[ -r $seen ]] && IFS= read -r was < $seen
  [[ $was == $mode ]] && return
  if [[ $mode == yield ]]; then
    print -ru2 -- "[nerv] zsh-autosuggestions is loaded: it keeps the inline ghost text, nerv shows only its popup."
  else
    print -ru2 -- "[nerv] zsh-autosuggestions is loaded: its inline ghost now comes from nerv's ranked history (NERV_AUTOSUGGEST=0 to turn off)."
  fi
  # Without the folder (no daemon has run yet) the notice would repeat.
  { mkdir -p -- ${seen:h} && print -r -- $mode >| $seen } 2>/dev/null
}

# The ghost the daemon ranked for the line as typed at the last request,
# for the `nerv` strategy. The plugin's widget wrappers fetch right after
# nerv's widget has run, and its async child is forked then too, so the
# value is this keystroke's. `__NERV_RANKED_FOR` names the line it belongs
# to: a buffer changed by a widget that does not ask the daemon (a paste,
# history recall) never gets a stale ranking. Empty when nothing was ranked
# — no history, no daemon, a shell word (alias, function, builtin) — and
# the next strategy
# answers.
typeset -g __NERV_RANKED_FOR="" __NERV_RANKED=""
_zsh_autosuggest_strategy_nerv() {
  [[ $1 == "$__NERV_RANKED_FOR" && -n $__NERV_RANKED ]] || return
  typeset -g suggestion=$__NERV_RANKED
}

# Most recent history command that strictly extends the current buffer,
# set in REPLY as the not-yet-typed remainder — the zsh-autosuggestions
# / Fig grey inline. `(r)` reverse-subscripts $history (iterated newest
# first) by a literal-prefix pattern; `(b)` escapes glob metacharacters
# so a buffer containing `[`, `*`, etc. still matches literally. The
# remainder is sliced by length (not `#`) so those metacharacters can't
# over-strip. REPLY (no `$(...)` subshell) keeps this off the fork path
# — it runs on the keystroke path.
__nerv_history_ghost() {
  emulate -L zsh
  REPLY=''
  # Another plugin owns the ghost: the scan's answer would be discarded —
  # unless `force`, to know what that plugin is about to draw.
  (( __NERV_GHOST_OFF )) && [[ $1 != force ]] && return
  [[ -z "$LBUFFER" ]] && return
  local match="${history[(r)${(b)LBUFFER}*]}"
  [[ -z "$match" || "$match" == "$LBUFFER" ]] && return
  REPLY="${match[${#LBUFFER}+1,-1]}"
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
  (( __NERV_GHOST_OFF )) && return
  POSTDISPLAY=''
  (( ${#__NERV_ITEMS} == 0 )) && return
  [[ -n $RBUFFER ]] && return
  # Bail when nothing typed yet for the current word — keeps the
  # prompt line quiet while the user surveys the popup.
  [[ "$LBUFFER" == *' ' || "$LBUFFER" == *$'\t' ]] && return
  local top="${__NERV_ITEMS[1]}"
  local top_ins="${top%%	*}"
  [[ -z "$top_ins" ]] && return
  # A correction replaces the command word; its text never continues the
  # token being typed, even when it happens to share a first letter.
  local REPLY; __nerv_row_span "$top"
  [[ -n "$REPLY" ]] && return
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

__nerv_space() {
  LBUFFER+=" "
  __nerv_complete
  # zsh-autosuggestions does not wrap `_`-named widgets, so its ghost for
  # the line before the space would stay on: ask it for a new one.
  (( __NERV_GHOST_OFF )) && { POSTDISPLAY=''; zle autosuggest-fetch 2>/dev/null; }
  return 0
}
zle -N __nerv_space
bindkey ' ' __nerv_space

zle -A backward-delete-char __nerv_orig_backward_delete_char 2>/dev/null
__nerv_backward_delete() { zle __nerv_orig_backward_delete_char "$@"; __nerv_complete; }
zle -N backward-delete-char __nerv_backward_delete

# Insert the highlight, then re-run completion so the next level pops up
# at once (a subcommand's flags, a flag's values, a folder's contents) —
# the user shouldn't have to type a throwaway character to see the next
# step. Bypasses the dedup guard. No-op popup-wise when nothing follows.
# Single owner for the insert + dedup-bypass + requery idiom (Enter and
# Tab share it).
__nerv_insert_and_requery() {
  __nerv_insert_selected
  __NERV_PREV_LBUFFER=$'\x00'
  __nerv_complete
}

# Enter: select if popup, else execute
__nerv_line_finish() {
  # Enter inserts the highlighted item ONLY when a real item is selected
  # (SELECTED >= 1) — and never runs it, not even a directory: an
  # arrowed-to `apps/` + Enter lands `cd apps/` on the line and stays,
  # so a mis-highlight costs nothing (a second Enter runs the line).
  # On the default "Immediately execute" sentinel (SELECTED == 0) — or
  # with no popup — Enter runs the line as typed. This is the Fig model:
  # `cd ` / `z ` + Enter execute the command; navigate to pick one.
  if (( __NERV_ACTIVE && __NERV_SELECTED >= 1 && ${#__NERV_ITEMS} > 0 )); then
    # Insert-only, then re-query so the next level pops up at once
    # (same chain as Tab's accept path below). Bypass the dedup guard.
    # Non-directory items behave exactly as before; directories used to
    # insert AND run here — that immediate `cd` on a mis-highlight is
    # what insert-only removes. Exceptions stay: a lone command-word
    # correction rewrites its span (no frecency either way), and a
    # fully-typed token (rc 4) keeps the sentinel default, so Enter on
    # it still runs what was typed.
    __nerv_insert_and_requery
  else
    (( __NERV_ACTIVE )) && { __NERV_ACTIVE=0; zle -R ""; }
    # A stale `…loading` hint must not outlive the line it belonged to.
    __nerv_clear_loading
    __NERV_PREV_LBUFFER=""
    __NERV_SELECTED=0
    __NERV_ITEMS=()
    # An unaccepted ghost is not part of the command: keep it out of the
    # scrollback line zsh leaves behind.
    POSTDISPLAY=''
    zle .accept-line
  fi
}
zle -N accept-line __nerv_line_finish

# Tab: accept the highlighted item — insert it. When the sentinel is
# selected (browsing, SELECTED==0), Tab instead dives into the list
# (moves to the first item) so a second Tab accepts it. Use arrows /
# Shift-Tab to move the highlight without accepting. Outside a popup:
# defer to zsh's expand-or-complete.
#
# Only checks __NERV_ITEMS, NOT __NERV_ACTIVE: cursor-movement keys
# don't clear ITEMS but may leave ACTIVE stale, and we'd rather accept
# than appear no-op.
__nerv_accept() {
  if (( ${#__NERV_ITEMS} > 0 )); then
    if (( __NERV_SELECTED >= 1 )); then
      __nerv_insert_and_requery
    else
      __nerv_cycle_next
      __nerv_show_popup "${__NERV_ITEMS[@]}"
    fi
  else
    # No live popup. The user pressed Tab expecting completion to fire
    # (universal shell habit), or a prior keystroke's popup desynced away.
    # Re-run the query once — bypass the dedup guard so an unchanged
    # LBUFFER still re-completes — and only defer to zsh's
    # expand-or-complete when nerv genuinely has nothing (filenames, etc).
    __NERV_PREV_LBUFFER=$'\x00'
    __nerv_complete
    (( ${#__NERV_ITEMS} > 0 )) || zle expand-or-complete
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
    __NERV_PREV_LBUFFER=$LBUFFER
  fi
}
zle -N __nerv_select_down

__nerv_select_up() {
  if (( __NERV_ACTIVE && ${#__NERV_ITEMS} > 0 )) && [[ -n "$BUFFER" ]]; then
    __nerv_cycle_prev
    __nerv_show_popup "${__NERV_ITEMS[@]}"
  else
    zle up-line-or-history
    __NERV_PREV_LBUFFER=$LBUFFER
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
    # Floor at the sentinel (0) when present so Page-Up can return to
    # "Immediately execute"; otherwise floor at the first item (1).
    local floor=$(( __NERV_HAS_SENTINEL ? 0 : 1 ))
    (( __NERV_SELECTED < floor )) && __NERV_SELECTED=$floor
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
  # A zsh-autosuggestions ghost is accepted by its own forward-char wrapper.
  if (( ! __NERV_GHOST_OFF )) && [[ -n "$POSTDISPLAY" && -z "$RBUFFER" ]]; then
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

# Cursor moves re-query through the pre-redraw hook below, which fires
# on every ZLE redraw — no need to bind individual movement keys (left,
# ctrl-a, home, …). The engine completes `line[..cursor]`, so the popup
# tracks the cursor into the middle of the buffer.
#
# Cost control: `__nerv_complete` opens with the LBUFFER dedup guard, so
# a redraw that changed nothing (repaints, selection navigation, ghost
# styling) returns before any IPC. A query fires only when LBUFFER
# differs from the last completed one — exactly cursor moves and
# unwrapped edits (a kill, a paste). Typing already completed via
# `self-insert` first, so its redraw is a no-op here.
#
# A line brought back from the history is the exception: it is a whole
# command, not a word being typed. A popup over it would take the next
# Up (cycling rows instead of older history) and Enter (inserting a row
# instead of running the line). nerv's own arrow widgets mark the
# recalled line seen; the pattern below covers history widgets nerv
# does not wrap (Ctrl-R, fzf, atuin, the *-search family).

__nerv_pre_redraw() {
  # __nerv_complete's first check (`LBUFFER == PREV → return`) is the
  # whole gate: edits complete in their own widgets, repaints change
  # nothing, and only a moved cursor (or an unwrapped edit) gets a
  # fresh query. Dismiss/Esc mark the current LBUFFER seen
  # (`PREV=LBUFFER`, not a popup) so a dismissed line stays dismissed.
  if [[ $LBUFFER != "$__NERV_PREV_LBUFFER" \
        && $LASTWIDGET == *(history|-or-search|beginning-search|atuin)* ]]; then
    __nerv_hide_popup
    __NERV_PREV_LBUFFER=$LBUFFER
  fi
  __nerv_complete

  # Paint the inline ghost (POSTDISPLAY) a muted grey — like fish /
  # zsh-autosuggestions — so the not-yet-typed completion reads as a
  # dim hint, not live input. Without this the ghost inherits the
  # terminal's default foreground and looks identical to what the user
  # typed. Re-synced every redraw off the current POSTDISPLAY: strip
  # our previous entry (matched by the memo tag so we never clobber a
  # highlight another plugin added), then re-add if a ghost is showing.
  # POSTDISPLAY chars occupy buffer positions ${#BUFFER}..+len.
  # The prediction belongs to the empty line. A widget nerv does not wrap
  # (Up-arrow history recall, a paste) can fill the buffer without going
  # through __nerv_complete, and Right-arrow would then glue the two
  # together (`echo prepecho next-thing`).
  if [[ -n $BUFFER && -n $POSTDISPLAY && $POSTDISPLAY == "$__NERV_PREDICTED" ]]; then
    POSTDISPLAY=''
  fi
  __nerv_paint_ghost
}
__nerv_paint_ghost() {
  region_highlight=(${region_highlight:#*memo=nerv_ghost*})
  # With zsh-autosuggestions the plugin styles its own ghost; the
  # prediction on the empty line is nerv's, and no plugin widget has run
  # yet to style it.
  if [[ -n "$POSTDISPLAY" ]] && { (( ! __NERV_GHOST_OFF )) \
       || [[ -z $BUFFER && $POSTDISPLAY == "$__NERV_PREDICTED" ]] \
       || [[ $POSTDISPLAY == "$__NERV_PREFILLED" ]]; }; then
    # The plugin's style when it draws the rest, so a prediction typed
    # through keeps one colour.
    local style=fg=242
    (( __NERV_GHOST_OFF )) && style=${ZSH_AUTOSUGGEST_HIGHLIGHT_STYLE:-fg=8}
    region_highlight+=("${#BUFFER} $(( ${#BUFFER} + ${#POSTDISPLAY} )) $style, memo=nerv_ghost")
  fi
}
# Chain into the pre-redraw hook via add-zle-hook-widget instead of
# `zle -N zle-line-pre-redraw` — the latter REPLACES the special widget,
# clobbering zsh-syntax-highlighting's own pre-redraw hook so command
# text loses its colour (valid-command green → default white). Hooking
# lets both run; registering after other plugins means our ghost paint
# lands on top of their region_highlight rather than being overwritten.
zle -N __nerv_pre_redraw
if autoload -Uz add-zle-hook-widget 2>/dev/null && \
   add-zle-hook-widget line-pre-redraw __nerv_pre_redraw 2>/dev/null; then
  :
else
  # Fallback for a zsh without add-zle-hook-widget (< 5.3): bind
  # directly. Rare on our 5.8+ floor, but keep the ghost working.
  zle -N zle-line-pre-redraw __nerv_pre_redraw
fi

# Empty-prompt prediction (docs/history-suggestions.md §4): the command
# that usually follows the one just run, as ghost text on the fresh line.
# One `_predict` per prompt; clearing the line back to empty shows the
# same prediction again without asking. NERV_PREDICT=0 turns it off.
typeset -g __NERV_PREDICTED=""
__nerv_line_init() {
  __NERV_PREDICTED=""
  [[ ${NERV_PREDICT:-1} == 0 || -z $__NERV_PREV_CMD || -n $BUFFER ]] && return
  (( __NERV_YIELD )) && return
  local row rc
  # Same 150 ms bound as `_predict`: the prompt waits on this.
  if __nerv_sock_call 0.15 predict "$__NERV_PREV_CMD" "${PWD:A}"; then
    row=${__NERV_SOCK_LINES[1]}
    rc=$REPLY
  else
    row=$(NERV_PREV="$__NERV_PREV_CMD" "$__NERV_BIN" _predict 2>/dev/null)
    rc=$?
  fi
  if (( rc )); then
    # A daemon error, or no answer within _predict's 150 ms bound: no
    # prediction either way. Under NERV_DEBUG it is logged, not guessed at.
    [[ -n "${NERV_DEBUG:-}" ]] && print -r -- "  predict: FAILED rc=$rc" >> /tmp/nerv-debug.log
    return
  fi
  [[ $row == $'\x1f'ghost$'\t'?* ]] || return
  __NERV_PREDICTED="${row#*$'\t'}"
  POSTDISPLAY="$__NERV_PREDICTED"
  # line-pre-redraw ran before this hook, so the grey is painted here.
  __nerv_paint_ghost
}
zle -N __nerv_line_init
# Registered like line-pre-redraw below: hooked when add-zle-hook-widget
# exists (so another plugin's line-init keeps running), bound directly
# otherwise.
if autoload -Uz add-zle-hook-widget 2>/dev/null && \
   add-zle-hook-widget line-init __nerv_line_init 2>/dev/null; then
  :
else
  zle -N zle-line-init __nerv_line_init
fi

__nerv_dismiss() { __nerv_hide_popup; __NERV_PREV_LBUFFER="$LBUFFER"; POSTDISPLAY=''; __NERV_PREDICTED=""; }
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
    # Mark seen, not unseen: the pre-redraw hook re-queries any LBUFFER
    # it hasn't completed, so blanking PREV here would reopen the popup
    # on the next repaint.
    __NERV_PREV_LBUFFER="$LBUFFER"
    POSTDISPLAY=''
  elif [[ -n $POSTDISPLAY && $POSTDISPLAY == "$__NERV_PREDICTED" ]]; then
    # A showing prediction is dismissed for this line, like ^G.
    POSTDISPLAY='' __NERV_PREDICTED=''
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
  __NERV_LOADING_SHOWN=0
  # A new prompt means the last command may have changed what completes
  # (a new branch, a killed process): nothing captured before it is reused.
  # The ranked ghost was ranked after a different previous command.
  __NERV_COMPSYS_KEY="" __NERV_RANKED_FOR="" __NERV_RANKED=""
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
  __nerv_claim self-insert          __nerv_self_insert
  __nerv_claim backward-delete-char __nerv_backward_delete
  __nerv_claim accept-line          __nerv_line_finish
}

# Bind widget $1 to ours ($2) unless zsh-autosuggestions already wraps
# ours: its wrapper calls ours first, then refreshes its ghost, which is
# how both run. It wraps at the first prompt and, unless
# ZSH_AUTOSUGGEST_MANUAL_REBIND is set, on every prompt after
# (`_zsh_autosuggest_start`), keeping the original as
# `autosuggest-orig-<n>-<widget>`; taking the
# widget back would leave the plugin's ghost frozen while typing.
__nerv_claim() {
  local cur=${widgets[$1]}
  if [[ $cur == user:_zsh_autosuggest_bound_* ]]; then
    local n=${${cur#user:_zsh_autosuggest_bound_}%%_*}
    [[ ${widgets[${ZSH_AUTOSUGGEST_ORIGINAL_WIDGET_PREFIX:-autosuggest-orig-}$n-$1]} == user:$2 ]] && return
  fi
  zle -N $1 $2 2>/dev/null
}
autoload -Uz add-zsh-hook 2>/dev/null && {
  add-zsh-hook precmd __nerv_ghost_owner
  add-zsh-hook precmd __nerv_precmd_reset
  add-zsh-hook precmd __nerv_rebind
  add-zsh-hook precmd __nerv_register_shell_names
  add-zsh-hook preexec __nerv_preexec
  add-zsh-hook precmd __nerv_precmd_record
}

# Command history behind the history-ranked ghost (docs/history-suggestions.md).
# preexec keeps the line as typed ($1, leading whitespace intact) and
# zsh's alias-expanded form ($3); precmd sends both with the exit status
# and the command before it. The directory is the one the command was
# typed in, read at preexec: `cd sub/` belongs to where you typed it,
# not to sub/. What zsh itself would not remember never
# leaves the shell: a leading space under hist_ignore_space, or a match
# for HISTORY_IGNORE. Such a command also breaks the chain — it is not
# kept as the next command's predecessor, and no adjacency is invented
# across it. Command text travels on stdin, never argv (`ps` shows argv
# to every user). `&!` disowns, so no job notice lands on the prompt.
typeset -g __NERV_LAST_CMD="" __NERV_LAST_EXP="" __NERV_LAST_CWD="" __NERV_PREV_CMD=""
typeset -gi __NERV_IMPORT_TRIED=0
__nerv_history_ignored() {
  # No history file (never set, `unset HISTFILE`, `fc -p`, /dev/null):
  # zsh keeps nothing of this session on disk, so neither does nerv.
  [[ -z $HISTFILE || $HISTFILE == /dev/null ]] && return 0
  [[ -o hist_ignore_space && $1 == [[:space:]]* ]] && return 0
  [[ -n $HISTORY_IGNORE && $1 == ${~HISTORY_IGNORE} ]] && return 0
  return 1
}
__nerv_preexec() {
  # The command about to run would inherit the socket (zsocket descriptors
  # stay open across exec), and a long-running one would hold the daemon
  # connection for its lifetime. The next request reconnects.
  __nerv_sock_close
  # $1 is empty when the history mechanism is off; without the typed
  # line the ignore rules cannot be applied, so nothing is recorded.
  if [[ -z $1 ]] || __nerv_history_ignored "$1"; then
    __NERV_LAST_CMD="" __NERV_PREV_CMD=""
    return
  fi
  __NERV_LAST_CMD=$1 __NERV_LAST_EXP=$3 __NERV_LAST_CWD=${PWD:A}
}
__nerv_precmd_record() {
  local -i ec=$?
  local hist="${NERV_HISTORY_FILE:-$HOME/Library/Caches/nerv/history.tsv}"
  [[ $hist == - ]] && return
  if (( ! __NERV_IMPORT_TRIED )); then
    __NERV_IMPORT_TRIED=1
    # First prompt with no history yet: seed it from zsh's own file once.
    if [[ ! -e $hist && -n $HISTFILE && -r $HISTFILE ]]; then
      "$__NERV_BIN" _import-history "$HISTFILE" >/dev/null 2>&1 &!
    fi
  fi
  [[ -n $__NERV_LAST_CMD ]] || return
  local cmd=${__NERV_LAST_CMD%$'\n'}
  # No reply to wait for: the socket write is the whole cost. It runs in
  # this shell, not a background job, so the connection it opens is the one
  # the keystrokes reuse. The daemon caps a line at 256 KiB, in bytes; a
  # character is at most 4 bytes on the wire, escaped or UTF-8, so past
  # 60,000 characters the command goes through `_record-cmd` instead, which
  # appends the file itself if need be. A daemon from before the text protocol drops a socket record (the
  # widget notices at the next read and forks from then on) — one row, once.
  if (( ${#cmd} + ${#__NERV_LAST_EXP} + ${#__NERV_PREV_CMD} >= 60000 )) \
     || ! __nerv_sock_send record "$cmd" "$__NERV_LAST_EXP" "$__NERV_LAST_CWD" $ec "$__NERV_PREV_CMD"; then
    print -rN -- "$cmd" "$__NERV_LAST_EXP" "$__NERV_PREV_CMD" \
      | "$__NERV_BIN" _record-cmd --cwd "$__NERV_LAST_CWD" --exit $ec >/dev/null 2>&1 &!
  fi
  __NERV_PREV_CMD=$cmd
  __NERV_LAST_CMD=""
}

# Register this session's function·alias names with the daemon so they
# surface as first-token candidates (docs: plan slice 02). Fires on
# `precmd`, never at sourcing: `nerv start` is still backgrounding the
# daemon then, and plugins like p10k define their functions after nerv
# is sourced (same reason __nerv_rebind runs late). The transfer runs
# asynchronously — prompt blocking 0 — and the names go to the daemon's
# memory only; nothing here writes a file.
# The names live in the daemon's memory, so a restarted daemon (the fix
# `nerv doctor` itself suggests) has none. The hook therefore stays
# attached and keys the registration on the daemon's pid: each precmd
# reads the pid file with the `read` builtin (no fork) and re-sends when
# it names a daemon this shell hasn't registered with. No pid file =
# no daemon, so nothing is sent. A failed transfer is NOT swallowed:
# the pid stays unregistered and the next precmd retries.
# The transfer is a process substitution, not a `&` job: an interactive
# shell announces every job it reaps (`[1]  + done …`) at the next
# prompt, even one started under `nomonitor`. Its exit status comes back
# as a line on the substitution's fd instead of through `wait`.
typeset -gi __NERV_SN_FD=0 __NERV_SN_DPID=0 __NERV_SN_TRY=0
__nerv_register_shell_names() {
  if (( __NERV_SN_FD )); then
    local st
    read -t 0 -u $__NERV_SN_FD st 2>/dev/null || return  # still in flight
    exec {__NERV_SN_FD}<&-
    __NERV_SN_FD=0
    [[ "$st" == 0 ]] && __NERV_SN_DPID=$__NERV_SN_TRY
  fi
  local dpid
  { read -r dpid < "${NERV_PID:-$HOME/Library/Caches/nerv/nervd.pid}" } 2>/dev/null
  [[ "$dpid" == <1-> ]] || return        # daemon not up (yet)
  (( dpid == __NERV_SN_DPID )) && return  # this daemon already has them
  local -a snames
  # Aliases first: the 2000 cap below trims from the end, and a shell
  # with hundreds of plugin functions would otherwise drop every alias.
  snames=( "${(@k)aliases[@]}" "${(@k)functions[@]}" )
  snames=( "${(@)snames:#[._]*}" )          # internals: _foo, .foo
  (( ${#snames} )) || { add-zsh-hook -d precmd __nerv_register_shell_names; return }
  (( ${#snames} > 2000 )) && snames=( "${(@)snames[1,2000]}" )
  __NERV_SN_TRY=$dpid
  exec {__NERV_SN_FD}< <(
    print -rC1 -- "${snames[@]}" | "$__NERV_BIN" _shell-names >/dev/null 2>&1
    print -r -- $?
  )
}
