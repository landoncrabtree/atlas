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
# Icon (best-effort — generate placeholder if artwork is missing)
# ---------------------------------------------------------------------------
ICONSET_DIR="dist/icons/atlas.iconset"
ICNS_FILE="dist/icons/atlas.icns"

mkdir -p "${ICONSET_DIR}"

if [ ! -f "${ICNS_FILE}" ]; then
    echo "--> Generating placeholder icon..."
    if command -v python3 &>/dev/null; then
        # Generate solid-color PNG files at all required sizes
        python3 - <<'PYEOF'
import struct, zlib, os

def make_png(w, h, r=70, g=130, b=200):
    """Return bytes for a solid-color RGBA PNG."""
    def chunk(tag, data):
        c = zlib.crc32(tag + data) & 0xFFFFFFFF
        return struct.pack('>I', len(data)) + tag + data + struct.pack('>I', c)
    sig = b'\x89PNG\r\n\x1a\n'
    ihdr = chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0))
    row = bytes([0] + [r, g, b] * w)
    raw = b''.join(row for _ in range(h))
    idat = chunk(b'IDAT', zlib.compress(raw))
    iend = chunk(b'IEND', b'')
    return sig + ihdr + idat + iend

iconset = "dist/icons/atlas.iconset"
os.makedirs(iconset, exist_ok=True)
for size in [16, 32, 64, 128, 256, 512, 1024]:
    png = make_png(size, size)
    with open(f"{iconset}/icon_{size}x{size}.png", "wb") as f:
        f.write(png)
    if size <= 512:
        with open(f"{iconset}/icon_{size}x{size}@2x.png", "wb") as f:
            f.write(make_png(size * 2, size * 2))
print("  Generated placeholder PNGs in dist/icons/atlas.iconset/")
PYEOF
        if command -v iconutil &>/dev/null; then
            iconutil -c icns "${ICONSET_DIR}" -o "${ICNS_FILE}"
            echo "    Created ${ICNS_FILE}"
        else
            echo "    iconutil not found — skipping .icns creation (bundle will have no icon)"
        fi
    else
        echo "    python3 not found — skipping icon generation (bundle will have no icon)"
    fi
fi

if [ -f "${ICNS_FILE}" ]; then
    cp "${ICNS_FILE}" "${APP_DIR}/Contents/Resources/atlas.icns"
    echo "    Copied atlas.icns → Resources/"
else
    echo "    Warning: no atlas.icns available; bundle has no icon"
fi

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
