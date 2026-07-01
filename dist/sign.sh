#!/usr/bin/env bash
# sign.sh — Code-sign Atlas.app with hardened runtime
#
# Environment variables (required for signing):
#   ATLAS_SIGNING_IDENTITY  e.g. "Developer ID Application: Your Name (TEAMID)"
#
# If ATLAS_SIGNING_IDENTITY is unset, this script exits 0 without signing.
# The resulting bundle will be unsigned; testers must right-click → Open to
# bypass Gatekeeper on the first launch.
set -euo pipefail
cd "$(dirname "$0")/.."

APP_DIR="target/dist/Atlas.app"

if [ -z "${ATLAS_SIGNING_IDENTITY:-}" ]; then
    echo "Warning: ATLAS_SIGNING_IDENTITY not set — skipping signing (bundle will be unsigned)."
    exit 0
fi

[ -d "${APP_DIR}" ] || { echo "Error: ${APP_DIR} not found — run dist/build-app.sh first" >&2; exit 1; }

echo "==> Signing with identity: ${ATLAS_SIGNING_IDENTITY}"

CODESIGN_FLAGS=(
    --force
    --options runtime
    --timestamp
    --entitlements dist/entitlements.plist
    --sign "${ATLAS_SIGNING_IDENTITY}"
)

# Sign inner binaries first (outside-in is required for --deep to verify cleanly)
echo "--> Signing atlas-indexd..."
codesign "${CODESIGN_FLAGS[@]}" "${APP_DIR}/Contents/MacOS/atlas-indexd"

echo "--> Signing atlas (and bundle)..."
codesign "${CODESIGN_FLAGS[@]}" --deep "${APP_DIR}"

# Verify
echo "--> Verifying signature..."
codesign --verify --deep --strict --verbose=2 "${APP_DIR}"

echo "==> Signing complete."
