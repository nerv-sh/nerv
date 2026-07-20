#!/usr/bin/env bash
# record-demo.sh — regenerate docs/assets/demo.gif deterministically.
#
# Builds a throwaway HOME + demo project (git repo with branches, a
# package.json with scripts, a few folders), points nerv at the real
# spec cache, then drives `scripts/demo.tape` through VHS. Nothing
# touches the user's own ~/.zshrc, daemon, or frecency store — the
# recording shell runs entirely inside $DEMO_ROOT.
#
# Requires: vhs (brew install vhs), a release build, an installed spec
# cache (~/Library/Caches/nerv/specs) or NERV_SPECS_DIR.
#
#   ./scripts/record-demo.sh
#
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEMO_ROOT="${DEMO_ROOT:-/tmp/nerv-demo}"
NERV_BIN="${NERV_BIN:-$REPO/target/release/nerv}"
SPECS="${NERV_SPECS_DIR:-$HOME/Library/Caches/nerv/specs}"

command -v vhs >/dev/null || { echo "vhs not found — brew install vhs"; exit 1; }
[ -x "$NERV_BIN" ] || { echo "missing $NERV_BIN — cargo build --release -p nerv-cli"; exit 1; }
[ -d "$SPECS" ] || { echo "missing spec cache at $SPECS"; exit 1; }

echo "==> demo workspace: $DEMO_ROOT"
rm -rf "$DEMO_ROOT"
HOME_DIR="$DEMO_ROOT/home"
PROJ="$HOME_DIR/nerv-demo"
mkdir -p "$PROJ"

# --- demo project: real git branches + npm scripts + folders ----------
cd "$PROJ"
mkdir -p src/components src/hooks docs tests
cat > package.json <<'JSON'
{
  "name": "nerv-demo",
  "scripts": {
    "dev": "vite",
    "build": "vite build",
    "test": "vitest run",
    "lint": "eslint ."
  }
}
JSON
echo "# nerv demo" > README.md
git init -q -b main
git config user.email demo@nerv.sh
git config user.name "nerv demo"
git add -A
git commit -qm "initial commit"
# Branches the popup will list under `git checkout `.
for b in feat/inline-ghost fix/popup-flicker release/v1.0; do
  git branch "$b"
done

# --- throwaway HOME: minimal prompt + nerv hook -----------------------
# The inner (`--shell-script`) form loads the widget without writing an
# rc block — the rc IS this file, and we don't want a self-edit mid-source.
cat > "$HOME_DIR/.zshrc" <<ZSHRC
export NERV_SPECS_DIR="$SPECS"
export NERV_FRECENCY_FILE=-          # don't rank on the author's history
PROMPT='%F{magenta}❯%f '
eval "\$("$NERV_BIN" init zsh --shell-script)"
cd "$PROJ"
clear
ZSHRC

echo "==> recording (this spawns a demo daemon under \$DEMO_ROOT)"
cd "$REPO"
HOME="$HOME_DIR" vhs scripts/demo.tape

# The demo shell autostarted a daemon inside the throwaway HOME; stop it
# so it can't linger past the recording.
HOME="$HOME_DIR" "$NERV_BIN" stop >/dev/null 2>&1 || true

echo "==> wrote $(ls -lh "$REPO/docs/assets/demo.gif" | awk '{print $5}') → docs/assets/demo.gif"
