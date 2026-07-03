#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"
install -Dm755 "$SCRIPT_DIR/bin/atlas" "$PREFIX/bin/atlas"
for size in 16 22 24 32 48 64 96 128 192 256 512; do
  install -Dm644 "$SCRIPT_DIR/share/icons/hicolor/${size}x${size}/apps/atlas.png" \
    "$PREFIX/share/icons/hicolor/${size}x${size}/apps/atlas.png"
done
install -Dm644 "$SCRIPT_DIR/share/applications/atlas.desktop" "$PREFIX/share/applications/atlas.desktop"
update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
gtk-update-icon-cache -q "$PREFIX/share/icons/hicolor" 2>/dev/null || true
echo "Atlas installed to $PREFIX"
