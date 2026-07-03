#!/usr/bin/env bash
# build-app.sh — Compile Atlas release binaries and assemble Atlas.app
#
# Environment variables:
#   VERSION        Override the bundle version (default: read from cargo pkgid)
#   SHORT_VERSION  Override the short version string (default: same as VERSION)
#
# Produces: target/dist/Atlas.app
set -euo pipefail
cd "$(dirname "$0")/.."

# ---------------------------------------------------------------------------
# Version detection
# ---------------------------------------------------------------------------
VERSION="${VERSION:-$(cargo pkgid -p atlas-app 2>/dev/null | sed -E 's/.*#(.*)/\1/' || echo "0.0.1")}"
SHORT_VERSION="${SHORT_VERSION:-${VERSION}}"

BUILD_DIR="target/dist"
APP_DIR="${BUILD_DIR}/Atlas.app"

echo "==> Atlas ${VERSION} — assembling macOS app bundle"

# ---------------------------------------------------------------------------
# Compile release binaries
# ---------------------------------------------------------------------------
echo "--> Building release binaries..."
cargo build --release -p atlas-app -p atlas-indexd

# ---------------------------------------------------------------------------
# Bundle skeleton
# ---------------------------------------------------------------------------
echo "--> Assembling ${APP_DIR}..."
rm -rf "${APP_DIR}"
mkdir -p \
    "${APP_DIR}/Contents/MacOS" \
    "${APP_DIR}/Contents/Resources" \
    "${APP_DIR}/Contents/Library/LaunchAgents"

# Binaries
cp "target/release/atlas"        "${APP_DIR}/Contents/MacOS/atlas"
cp "target/release/atlas-indexd" "${APP_DIR}/Contents/MacOS/atlas-indexd"
chmod +x \
    "${APP_DIR}/Contents/MacOS/atlas" \
    "${APP_DIR}/Contents/MacOS/atlas-indexd"

# ---------------------------------------------------------------------------
# Bundle themes as Resources (seeds user theme dir on first launch)
# ---------------------------------------------------------------------------
if [ -d assets/themes ]; then
    mkdir -p "${APP_DIR}/Contents/Resources/themes"
    cp assets/themes/*.toml "${APP_DIR}/Contents/Resources/themes/" 2>/dev/null || true
    echo "    Copied themes → Resources/themes/"
fi

# Bundle keymaps
if [ -d assets/keymaps ]; then
    mkdir -p "${APP_DIR}/Contents/Resources/keymaps"
    cp assets/keymaps/*.toml "${APP_DIR}/Contents/Resources/keymaps/" 2>/dev/null || true
    echo "    Copied keymaps → Resources/keymaps/"
fi

# ---------------------------------------------------------------------------
# App icon
# ---------------------------------------------------------------------------
ICNS_FILE="assets/branding/atlas.icns"

if [ ! -f "${ICNS_FILE}" ]; then
    echo "Error: ${ICNS_FILE} not found — regenerate app icons from assets/branding/atlas.png" >&2
    exit 1
fi

cp "${ICNS_FILE}" "${APP_DIR}/Contents/Resources/atlas.icns"
echo "    Copied atlas.icns → Resources/"

# ---------------------------------------------------------------------------
# Info.plist (substitute template placeholders)
# ---------------------------------------------------------------------------
echo "--> Writing Info.plist..."
sed \
    -e "s/{{VERSION}}/${VERSION}/g" \
    -e "s/{{SHORT_VERSION}}/${SHORT_VERSION}/g" \
    -e "s/{{YEAR}}/$(date +%Y)/g" \
    dist/Info.plist.tmpl > "${APP_DIR}/Contents/Info.plist"

# PkgInfo
printf 'APPL????' > "${APP_DIR}/Contents/PkgInfo"

# ---------------------------------------------------------------------------
# LaunchAgent plist for atlas-indexd
# ---------------------------------------------------------------------------
echo "--> Writing LaunchAgent plist..."
sed \
    -e "s|{{EXE_PATH}}|/Applications/Atlas.app/Contents/MacOS/atlas-indexd|g" \
    -e "s|{{LOG_DIR}}|\$HOME/Library/Logs/Atlas|g" \
    dist/Info.indexd.plist.tmpl \
    > "${APP_DIR}/Contents/Library/LaunchAgents/dev.atlas.atlas-indexd.plist"

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo ""
echo "==> Done: ${APP_DIR}"
du -sh "${APP_DIR}" | awk '{print "    Bundle size: " $1}'
