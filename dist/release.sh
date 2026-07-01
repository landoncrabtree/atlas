#!/usr/bin/env bash
# release.sh — Top-level release orchestrator
#
# Runs: build-app → sign → build-dmg → notarize
#
# Environment variables (see individual scripts for full docs):
#   VERSION                 Override the bundle version
#   SHORT_VERSION           Override the short version string
#   ATLAS_SIGNING_IDENTITY  Developer ID Application identity (optional)
#   ATLAS_NOTARY_PROFILE    notarytool keychain profile name  (optional)
#
# Artifacts produced in target/dist/:
#   Atlas.app              — signed (or unsigned) app bundle
#   Atlas-<version>.dmg    — distributable disk image
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> Atlas release build"
echo ""

dist/build-app.sh
dist/sign.sh
dist/build-dmg.sh
dist/notarize.sh || echo "Warning: notarization step failed or was skipped."

echo ""
echo "==> Release artifacts:"
ls -lh target/dist/Atlas.app 2>/dev/null || true
ls -lh target/dist/Atlas-*.dmg 2>/dev/null || true
echo ""
echo "Done. Artifacts are in target/dist/"
