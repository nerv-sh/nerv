#!/usr/bin/env bash
# M0-8: Developer ID sign + notarize pre-validation e2e (PLAN §10 M0-8).
#
# Builds a bare `fn main(){}` binary, signs it with the Developer ID
# Application certificate (hardened runtime + secure timestamp), submits
# it to Apple notary service, and verifies the result — proving the
# whole release-signing pipeline works before wiring it into release.yml.
#
# Usage:
#   ./scripts/sign-notarize-e2e.sh                 # sign + notarize + assess
#   NERV_SKIP_NOTARIZE=1 ./scripts/sign-notarize-e2e.sh   # sign-only (no creds needed)
#
# Notarization credentials (either one):
#   A. keychain profile:  xcrun notarytool store-credentials nerv-notary \
#        --key <AuthKey_XXX.p8> --key-id <KEY_ID> --issuer <ISSUER_UUID>
#      (or --apple-id <id> --team-id CH8U9VM6SR --password <app-specific>)
#   B. env vars: NOTARY_KEY (p8 path) + NOTARY_KEY_ID + NOTARY_ISSUER
set -euo pipefail

IDENTITY="${NERV_SIGN_IDENTITY:-Developer ID Application: Lemon Cloud Co., Ltd. (CH8U9VM6SR)}"
PROFILE="${NERV_NOTARY_PROFILE:-nerv-notary}"
WORK="$(mktemp -d /tmp/nerv-m08.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

log() { printf '[m0-8] %s\n' "$*"; }

# --- 1. Build the empty binary -------------------------------------------
log "building fn main(){} release binary"
mkdir -p "$WORK/src"
cat > "$WORK/Cargo.toml" <<'EOF'
[package]
name = "nerv-m08-probe"
version = "0.0.0"
edition = "2024"

[[bin]]
name = "nerv-m08-probe"
path = "src/main.rs"

[workspace]
EOF
echo 'fn main() {}' > "$WORK/src/main.rs"
cargo build --release --manifest-path "$WORK/Cargo.toml" --target-dir "$WORK/target" -q
BIN="$WORK/target/release/nerv-m08-probe"
[[ -x "$BIN" ]] || { log "FAIL: probe binary missing"; exit 1; }

# --- 2. Sign: hardened runtime + secure timestamp -------------------------
log "codesign with: $IDENTITY"
codesign --force --options runtime --timestamp --sign "$IDENTITY" "$BIN"

log "codesign verify (strict)"
codesign --verify --strict --verbose=2 "$BIN"
codesign --display --verbose=2 "$BIN" 2>&1 | grep -E 'Authority=Developer ID Application' \
    || { log "FAIL: not signed by a Developer ID Application cert"; exit 1; }
log "sign OK"

if [[ "${NERV_SKIP_NOTARIZE:-0}" = "1" ]]; then
    log "NERV_SKIP_NOTARIZE=1 — stopping after sign (steps 3-5 skipped)"
    exit 0
fi

# --- 3. Zip + submit to notary service ------------------------------------
ZIP="$WORK/nerv-m08-probe.zip"
/usr/bin/ditto -c -k --keepParent "$BIN" "$ZIP"

NOTARY_ARGS=()
if [[ -n "${NOTARY_KEY:-}" ]]; then
    NOTARY_ARGS=(--key "$NOTARY_KEY" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER")
    log "notarytool submit (API key ${NOTARY_KEY_ID})"
else
    NOTARY_ARGS=(--keychain-profile "$PROFILE")
    log "notarytool submit (keychain profile '$PROFILE')"
fi

SUBMIT_OUT="$(xcrun notarytool submit "$ZIP" "${NOTARY_ARGS[@]}" --wait --output-format json)"
STATUS="$(printf '%s' "$SUBMIT_OUT" | /usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin)["status"])')"
SUBMISSION_ID="$(printf '%s' "$SUBMIT_OUT" | /usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
log "submission $SUBMISSION_ID -> $STATUS"

if [[ "$STATUS" != "Accepted" ]]; then
    log "FAIL: notarization not Accepted — fetching log"
    xcrun notarytool log "$SUBMISSION_ID" "${NOTARY_ARGS[@]}" || true
    exit 1
fi

# --- 4. Gatekeeper assessment ----------------------------------------------
# Standalone CLI binaries can't be stapled (no ticket slot); Gatekeeper
# fetches the ticket online. `spctl` only assesses bundles/installers, so
# the authoritative binary-level check is the notary status above plus a
# quarantined-execution smoke test.
log "quarantine + execute smoke test"
QBIN="$WORK/quarantined-probe"
cp "$BIN" "$QBIN"
xattr -w com.apple.quarantine "0083;$(printf '%x' "$(date +%s)");e2e;$(uuidgen)" "$QBIN"
"$QBIN" && log "quarantined signed binary executed cleanly"

log "PASS — M0-8 sign + notarize e2e green"
