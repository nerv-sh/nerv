#!/usr/bin/env zsh
# bench-init.zsh — interactive zsh start-up: eval'd init vs the cached script.
#
# Usage: zsh scripts/bench-init.zsh [N]
# Requires: cargo-built debug binaries (target/debug/nerv).
#
# Two ZDOTDIRs under a temp HOME: one whose .zshrc runs
# `eval "$(nerv init zsh --shell-script)"` (the old block), one with the
# block `nerv init zsh` writes today (source the cache, eval only when it
# is stale). Each `zsh -i -c exit` is timed N times; prints p50 of both
# and "cached init faster" when the cache wins. The daemon is not started:
# only shell start-up is measured.
emulate -L zsh
zmodload zsh/datetime

local repo=${0:A:h:h}
local nerv=$repo/target/debug/nerv
local N=${1:-20}
[[ -x $nerv ]] || { print -u2 "missing $nerv — run: cargo build -p nerv-cli"; return 2 }

local home=$(mktemp -d)
trap 'rm -rf $home' EXIT
local old=$home/old new=$home/new
mkdir -p $old $new
print -r -- "eval \"\$($nerv init zsh --shell-script)\"" > $old/.zshrc
# `nerv init zsh` installs today's block into $HOME/.zshrc.
HOME=$new NERV_AUTOSTART=0 $nerv init zsh >/dev/null 2>&1
# One run writes the cache.
HOME=$home ZDOTDIR=$new NERV_AUTOSTART=0 zsh -i -c exit >/dev/null 2>&1
[[ -r $home/Library/Caches/nerv/init.zsh ]] || { print -u2 "no cache written"; return 1 }

local -a samples
p50() {
  local -a s
  local x
  for x in $samples; do s+=($(printf '%012.4f' $x)); done
  s=(${(o)s})
  REPLY=$(( ${s[$(( (${#s} + 1) / 2 ))]} ))
}
run() {
  local zdot=$1 i t0
  samples=()
  for (( i = 0; i < N; i++ )); do
    t0=$EPOCHREALTIME
    HOME=$home ZDOTDIR=$zdot NERV_AUTOSTART=0 zsh -i -c exit >/dev/null 2>&1
    samples+=($(( (EPOCHREALTIME - t0) * 1000 )))
  done
  p50
}
run $old; local eval_ms=$REPLY
run $new; local cached_ms=$REPLY
printf 'eval p50 %.1f ms  cached p50 %.1f ms  (n=%d)\n' $eval_ms $cached_ms $N
(( cached_ms < eval_ms )) && print "cached init faster"
