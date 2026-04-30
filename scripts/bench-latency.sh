#!/usr/bin/env zsh
# bench-latency.sh — Measure nerv _complete round-trip latency.
#
# Usage:
#   1. Start nervd in another terminal:  cargo run -p nerv-daemon
#   2. Run this script:                  zsh scripts/bench-latency.sh
#
# Reports p50 and p95 over N iterations.
# Target: p95 < 25 ms (PLAN.md §10 M0-1).

set -euo pipefail
zmodload zsh/datetime

NERV_BIN="${NERV_BIN:-cargo run -p nerv-cli --}"
N="${1:-100}"

echo "Benchmarking nerv _complete (N=$N)..."
echo "Binary: $NERV_BIN"
echo ""

# Warm up (first run compiles if using cargo run)
eval "$NERV_BIN _complete 'git ' 4" >/dev/null 2>&1 || {
  echo "ERROR: nerv _complete failed. Is nervd running?" >&2
  exit 1
}

typeset -a latencies

for (( i=1; i<=N; i++ )); do
  local t0=$EPOCHREALTIME
  eval "$NERV_BIN _complete 'git ' 4" >/dev/null 2>&1
  local t1=$EPOCHREALTIME
  latencies+=( $(( (t1 - t0) * 1000.0 )) )
done

# Sort ascending
latencies=(${(on)latencies})

local p50_idx=$(( N / 2 ))
local p95_idx=$(( N * 95 / 100 ))
(( p50_idx < 1 )) && p50_idx=1
(( p95_idx < 1 )) && p95_idx=1

local p50=${latencies[$p50_idx]}
local p95=${latencies[$p95_idx]}
local min=${latencies[1]}
local max=${latencies[$N]}

printf "Results (N=%d):\n" "$N"
printf "  min:  %6.2f ms\n" "$min"
printf "  p50:  %6.2f ms\n" "$p50"
printf "  p95:  %6.2f ms\n" "$p95"
printf "  max:  %6.2f ms\n" "$max"
echo ""

if (( p95 < 25.0 )); then
  echo "PASS: p95 (${p95}ms) < 25ms target"
else
  echo "FAIL: p95 (${p95}ms) >= 25ms target"
  exit 1
fi
