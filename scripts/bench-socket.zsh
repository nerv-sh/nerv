#!/usr/bin/env zsh
# bench-socket.zsh — per-keystroke round trip: zsh socket vs forking `nerv`.
#
# Usage: zsh scripts/bench-socket.zsh [N]
# Requires: cargo-built debug binaries (target/debug/nerv, nervd).
#
# Starts an isolated daemon (temp HOME, fixture specs, no history) and asks
# for `git ch` N times each way: once over zsh/net/socket speaking the text
# protocol the widget uses (nerv_engine::wire), once through
# `nerv _complete` the way the fork fallback does. Prints p50/p95 of both.
# The socket client below is the widget's __nerv_sock_* in miniature; the
# e2e (scripts/e2e-zle-history.py) covers the widget's own copy.
emulate -L zsh
setopt err_return
zmodload zsh/datetime zsh/net/socket

local repo=${0:A:h:h}
local nerv=$repo/target/debug/nerv
local N=${1:-200}
[[ -x $nerv ]] || { print -u2 "missing $nerv — run: cargo build -p nerv-cli -p nerv-daemon"; return 2 }

local home=$(mktemp -d)
export HOME=$home NERV_SPECS_DIR=$repo/crates/nerv-engine/tests/fixtures/specs \
       NERV_PATH_SCAN=0 NERV_HISTORY_FILE=- NERV_FRECENCY_FILE=- NERV_MISSES_FILE=-
trap '$nerv stop >/dev/null 2>&1; rm -rf $home' EXIT
$nerv start >/dev/null 2>&1
local sock=$home/Library/Caches/nerv/nervd.sock i
for (( i = 0; i < 50; i++ )); do [[ -S $sock ]] && break; sleep 0.1; done

# p50/p95 of the global `samples` array, in ms. Zero-padded first: zsh's
# numeric sort compares digit runs, so `0.3` would sort before `0.24`.
typeset -ga samples
report() {
  local -a s
  local x
  for x in $samples; do s+=($(printf '%012.4f' $x)); done
  s=(${(o)s})
  printf '%s p50 %.2f ms  p95 %.2f ms  (n=%d)\n' $1 $(( ${s[$(( (${#s} + 1) / 2 ))]} )) \
    $(( ${s[$(( ${#s} * 95 / 100 ))]} )) ${#s}
}

local line='git ch' t0 row fd
zsocket $sock
fd=$REPLY
samples=()
for (( i = 1; i <= N; i++ )); do
  t0=$EPOCHREALTIME
  print -r -u $fd -- "complete"$'\x1f'$i$'\x1f'"$line"$'\x1f'${#line}$'\x1f'$PWD$'\x1f'$'\x1f'"$line"$'\x1f'c
  while IFS= read -r -t 2 -u $fd row; do
    [[ $row == $'\x1f'end$'\t'* ]] && break
  done
  samples+=($(( (EPOCHREALTIME - t0) * 1000 )))
done
exec {fd}<&-
report socket

samples=()
for (( i = 1; i <= N; i++ )); do
  t0=$EPOCHREALTIME
  row=$(NERV_TYPED=$line $nerv _complete --compsys "$line" ${#line})
  samples+=($(( (EPOCHREALTIME - t0) * 1000 )))
done
report fork
