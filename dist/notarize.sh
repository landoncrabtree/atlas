#!/usr/bin/env bash
# notarize.sh — Submit the Atlas DMG to Apple Notary Service and staple the ticket
#
# Environment variables (required for notarization):
#   VERSION              Override the version string (default: read from cargo pkgid)
#   ATLAS_NOTARY_PROFILE Keychain profile name created via:
#                          xcrun notarytool store-credentials <profile> \
#                            --apple-id you@example.com \
#                            --team-id  YOURTEAMID \
#                            --password "@keychain:AC_PASSWORD"
#
# If ATLAS_NOTARY_PROFILE is unset, this script exits 0 without submitting.
# This script is idempotent: re-running it on an already-stapled DMG is safe.
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="${VERSION:-$(cargo pkgid -p atlas-app 2>/dev/null | sed -E 's/.*#(.*)/\1/' || echo "0.0.1")}"

if [ -z "${ATLAS_NOTARY_PROFILE:-}" ]; then
    echo "Warning: ATLAS_NOTARY_PROFILE not set — skipping notarization."
    exit 0
fi

# Find the DMG — prefer exact name, fall back to glob
DMG="target/dist/Atlas-${VERSION}.dmg"
if [ ! -f "${DMG}" ]; then
    DMG="$(ls target/dist/Atlas-*.dmg 2>/dev/null | head -1 || true)"
fi

if [ -z "${DMG}" ] || [ ! -f "${DMG}" ]; then
    echo "Error: DMG not found in target/dist/ — run dist/build-dmg.sh first" >&2
    exit 1
fi

echo "==> Submitting for notarization: ${DMG}"
xcrun notarytool submit "${DMG}" \
    --keychain-profile "${ATLAS_NOTARY_PROFILE}" \
    --wait

echo "--> Stapling ticket..."
xcrun stapler staple "${DMG}"

echo "==> Notarization complete: ${DMG}"
