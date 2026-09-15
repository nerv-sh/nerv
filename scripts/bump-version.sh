#!/usr/bin/env bash
# Bump the workspace version in Cargo.toml + Cargo.lock.
#
#   scripts/bump-version.sh            # patch +1  (0.1.8 → 0.1.9)
#   scripts/bump-version.sh 0.2.0      # explicit
#
# Prints the new version. No cargo needed: every `nerv-*` package in
# Cargo.lock is a workspace member (no external crate uses that prefix),
# so their `version` lines are rewritten in place. `cargo metadata
# --locked` afterwards proves the lock still matches.
set -euo pipefail
cd "$(dirname "$0")/.."

CURRENT="$(grep -m1 '^version = ' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
if [ $# -ge 1 ]; then
  NEXT="$1"
else
  IFS=. read -r MAJ MIN PAT <<<"$CURRENT"
  NEXT="${MAJ}.${MIN}.$((PAT + 1))"
fi
[[ "$NEXT" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "bad version: $NEXT" >&2; exit 1; }

awk -v cur="$CURRENT" -v nxt="$NEXT" '
  !done && $0 == "version = \"" cur "\"" { print "version = \"" nxt "\""; done = 1; next }
  { print }
' Cargo.toml > Cargo.toml.new
mv Cargo.toml.new Cargo.toml
awk -v cur="$CURRENT" -v nxt="$NEXT" '
  /^name = "nerv-/ { ws = 1; print; next }
  ws && $0 == "version = \"" cur "\"" { print "version = \"" nxt "\""; ws = 0; next }
  { ws = 0; print }
' Cargo.lock > Cargo.lock.new
mv Cargo.lock.new Cargo.lock

if grep -q '^version = "'"$CURRENT"'"' Cargo.toml; then
  echo "Cargo.toml still at $CURRENT" >&2; exit 1
fi
echo "$NEXT"
