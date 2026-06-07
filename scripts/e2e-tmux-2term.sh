#!/usr/bin/env bash
# e2e-tmux-2term.sh — M1 4-week checkpoint: tmux + 2-terminal regression.
#
# Proves the daemon serves completions correctly to two concurrent
# clients running in separate tmux panes with *different* working
# directories, and survives a burst of concurrent queries. This is the
# "tmux + 2터미널 회귀" acceptance criterion (CLAUDE.md §7.3).
#
# What it guards:
#   1. cwd-aware IPC — `Request::Complete.cwd` is the CLIENT's cwd, not
#      the daemon's (CLAUDE.md §4 invariant). Pane A sits in a dir with
#      a package.json whose scripts must surface; pane B sits in a bare
#      dir where those same scripts must NOT surface. One daemon, two
#      cwds, no cross-talk.
#   2. Concurrency — a burst of simultaneous `_complete` calls all
#      return correct results with the daemon still alive afterwards
#      (no panic, no serialized-stream corruption).
#
# Fully isolated: a throwaway $HOME means its own socket / spec cache /
# frecency file. The user's real daemon is never touched.
#
# Usage:   ./scripts/e2e-tmux-2term.sh
# Env:     NERV_SKIP_BUILD=1   reuse an existing release build
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$REPO_ROOT/target/release/nerv"
SPEC_SRC="$REPO_ROOT/crates/nerv-engine/tests/fixtures/converted"
TMUX_BIN="$(command -v tmux || true)"
SESSION="nerv-e2e-2term"

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
dim()  { printf '\033[2m%s\033[0m\n' "$*"; }
pass() { printf '\033[32m%s\033[0m\n' "$*"; }
fail() { printf '\033[31m%s\033[0m\n' "$*" >&2; exit 1; }
skip() { printf '\033[33m%s\033[0m\n' "$*" >&2; exit 0; }

[ -n "$TMUX_BIN" ] || skip "[skip] tmux not installed — cannot run the 2-pane regression."

# 1. Build.
if [ -z "${NERV_SKIP_BUILD:-}" ]; then
  bold "[1/6] Building nerv + nervd (release) …"
  (cd "$REPO_ROOT" && cargo build --release -q -p nerv-cli -p nerv-daemon)
fi
[ -x "$BIN" ] || fail "binary missing: $BIN (drop NERV_SKIP_BUILD)"

# 2. Isolated HOME → isolated socket + spec cache + frecency.
TEST_HOME="$(mktemp -d "${TMPDIR:-/tmp}/nerv-2term.XXXXXX")"
SPEC_DST="$TEST_HOME/Library/Caches/nerv/specs"
mkdir -p "$SPEC_DST"
cleanup() {
  HOME="$TEST_HOME" "$BIN" stop >/dev/null 2>&1 || true
  "$TMUX_BIN" kill-session -t "$SESSION" >/dev/null 2>&1 || true
  rm -rf "$TEST_HOME"
}
trap cleanup EXIT

# 3. Install specs. Need the npm package.json-scripts generator, which
#    only the converted cache carries — skip cleanly without it.
if [ -d "$SPEC_SRC" ] && [ -n "$(ls -A "$SPEC_SRC" 2>/dev/null)" ]; then
  bold "[2/6] Installing converted spec cache (gzip) …"
  (cd "$REPO_ROOT" && cargo run --release -q -p nerv-engine --bin build-specs -- \
    --input "$SPEC_SRC" --output "$SPEC_DST" --only npm --only git --compress)
else
  skip "[skip] converted specs absent at $SPEC_SRC — run \`cd tools/ts-to-json && bun run convert:all\`."
fi

# 4. Two working dirs: A has marker scripts, B is bare.
DIR_A="$TEST_HOME/proj-a"
DIR_B="$TEST_HOME/proj-b"
mkdir -p "$DIR_A" "$DIR_B"
cat > "$DIR_A/package.json" <<'JSON'
{ "name": "proj-a", "scripts": { "e2e_marker_alpha": "true", "e2e_marker_beta": "true" } }
JSON

# 5. Start the daemon (isolated) and drive two tmux panes.
bold "[3/6] Starting daemon in isolated HOME …"
HOME="$TEST_HOME" "$BIN" start
sleep 1

OUT_A="$TEST_HOME/out-a.txt"
OUT_B="$TEST_HOME/out-b.txt"

bold "[4/6] Spawning tmux session with two panes (different cwds) …"
"$TMUX_BIN" kill-session -t "$SESSION" >/dev/null 2>&1 || true
# Capture pane ids (%N) directly — these are config-independent, unlike
# window/pane indices which depend on the user's base-index setting.
PANE_A="$("$TMUX_BIN" new-session -d -s "$SESSION" -x 200 -y 50 -c "$DIR_A" \
  -P -F '#{pane_id}')"
PANE_B="$("$TMUX_BIN" split-window -t "$PANE_A" -c "$DIR_B" -P -F '#{pane_id}')"
# Each pane runs the bridge with its own cwd.
"$TMUX_BIN" send-keys -t "$PANE_A" \
  "HOME='$TEST_HOME' '$BIN' _complete 'npm run ' 8 > '$OUT_A'; echo DONE_A >> '$OUT_A'" Enter
"$TMUX_BIN" send-keys -t "$PANE_B" \
  "HOME='$TEST_HOME' '$BIN' _complete 'npm run ' 8 > '$OUT_B'; echo DONE_B >> '$OUT_B'" Enter

# Wait for both panes to finish.
for _ in $(seq 1 50); do
  if grep -q DONE_A "$OUT_A" 2>/dev/null && grep -q DONE_B "$OUT_B" 2>/dev/null; then
    break
  fi
  sleep 0.2
done

bold "[5/6] Asserting cwd-aware isolation across panes …"
grep -q DONE_A "$OUT_A" || fail "pane A never completed"
grep -q DONE_B "$OUT_B" || fail "pane B never completed"

if grep -q "e2e_marker_alpha" "$OUT_A"; then
  pass "  pane A (has package.json) sees e2e_marker_alpha ✓"
else
  dim "  pane A output:"; sed 's/^/    /' "$OUT_A" >&2
  fail "pane A did NOT surface package.json scripts — cwd not honored"
fi

if grep -q "e2e_marker_alpha" "$OUT_B"; then
  dim "  pane B output:"; sed 's/^/    /' "$OUT_B" >&2
  fail "pane B (bare dir) LEAKED pane A's scripts — daemon used wrong cwd"
else
  pass "  pane B (bare dir) correctly has no marker scripts ✓"
fi

# 6. Concurrency burst: 20 simultaneous queries, all must resolve.
bold "[6/6] Concurrency burst — 20 simultaneous queries …"
BURST_DIR="$TEST_HOME/burst"
mkdir -p "$BURST_DIR"
pids=()
for i in $(seq 1 20); do
  ( HOME="$TEST_HOME" "$BIN" _complete "git c" 5 > "$BURST_DIR/$i.txt" 2>&1 ) &
  pids+=($!)
done
for p in "${pids[@]}"; do wait "$p" || fail "a concurrent client exited nonzero"; done

bad=0
for i in $(seq 1 20); do
  grep -q "checkout" "$BURST_DIR/$i.txt" || bad=$((bad + 1))
done
[ "$bad" -eq 0 ] || fail "$bad/20 concurrent queries missing 'checkout'"
pass "  20/20 concurrent queries returned 'checkout' ✓"

# Daemon must still be alive after the burst.
HOME="$TEST_HOME" "$BIN" _complete "git s" 5 | grep -q "status" \
  || fail "daemon unresponsive after concurrency burst"
pass "  daemon healthy after burst ✓"

bold "PASS — tmux 2-pane cwd isolation + concurrency regression"
