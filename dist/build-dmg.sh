#!/usr/bin/env bash
# build-dmg.sh — Package Atlas.app into a distributable DMG
#
# Environment variables:
#   VERSION  Override the version string (default: read from cargo pkgid)
#
# Prerequisites: dist/build-app.sh must have been run first.
# Produces: target/dist/Atlas-<VERSION>.dmg
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="${VERSION:-$(cargo pkgid -p atlas-app 2>/dev/null | sed -E 's/.*#(.*)/\1/' || echo "0.0.1")}"
APP_DIR="target/dist/Atlas.app"
DMG_PATH="target/dist/Atlas-${VERSION}.dmg"

[ -d "${APP_DIR}" ] || { echo "Error: ${APP_DIR} not found — run dist/build-app.sh first" >&2; exit 1; }

echo "==> Building DMG: ${DMG_PATH}"

# Create a temporary staging directory
STAGE="target/dist/.dmg-stage"
rm -rf "${STAGE}"
mkdir -p "${STAGE}"

# Symlink to /Applications for drag-install UX
ln -s /Applications "${STAGE}/Applications"

# Copy the app bundle
cp -R "${APP_DIR}" "${STAGE}/Atlas.app"

# Create the compressed DMG
hdiutil create \
    -volname "Atlas ${VERSION}" \
    -srcfolder "${STAGE}" \
    -ov \
    -format UDZO \
    "${DMG_PATH}"

rm -rf "${STAGE}"

echo "==> Wrote ${DMG_PATH}"
du -sh "${DMG_PATH}" | awk '{print "    DMG size: " $1}'
