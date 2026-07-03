# Atlas — macOS Packaging Scripts

This directory contains everything needed to build, sign, notarize, and package Atlas for macOS distribution.

## Quick start

```bash
# From the repo root:
dist/release.sh
```

Produces:
- `target/dist/Atlas.app` — signed (or unsigned) app bundle
- `target/dist/Atlas-<version>.dmg` — distributable disk image

## Scripts

| Script | Purpose |
|--------|---------|
| `build-app.sh` | Compiles release binaries, assembles `Atlas.app` bundle |
| `sign.sh` | Code-signs all binaries with hardened runtime |
| `build-dmg.sh` | Creates a compressed DMG from the signed `.app` |
| `notarize.sh` | Submits DMG to Apple notarization service and staples the ticket |
| `release.sh` | Runs all steps in order |

## Environment variables

| Variable | Required for | Description |
|----------|-------------|-------------|
| `VERSION` | optional | Override the version string (default: read from `cargo pkgid`) |
| `SHORT_VERSION` | optional | Override the short version (default: same as `VERSION`) |
| `ATLAS_SIGNING_IDENTITY` | signing | `Developer ID Application: Name (TEAMID)` from your keychain |
| `ATLAS_NOTARY_PROFILE` | notarization | Keychain profile created via `xcrun notarytool store-credentials` |

If `ATLAS_SIGNING_IDENTITY` is unset, `sign.sh` exits 0 without signing — the bundle will be unsigned and testers need to right-click → Open to bypass Gatekeeper.

If `ATLAS_NOTARY_PROFILE` is unset, `notarize.sh` exits 0 without submitting — the DMG will not have a notarization ticket stapled.

## Signing setup (one-time)

```bash
# Store your Apple ID credentials in the keychain under a profile name:
xcrun notarytool store-credentials "atlas-notary" \
    --apple-id "you@example.com" \
    --team-id  "YOURTEAMID" \
    --password "@keychain:AC_PASSWORD"   # or use app-specific password

# Then:
export ATLAS_SIGNING_IDENTITY="Developer ID Application: Your Name (TEAMID)"
export ATLAS_NOTARY_PROFILE="atlas-notary"
dist/release.sh
```

## Templates

- `Info.plist.tmpl` — `Atlas.app/Contents/Info.plist`; placeholders `{{VERSION}}`, `{{SHORT_VERSION}}`, `{{YEAR}}`
- `Info.indexd.plist.tmpl` — `Atlas.app/Contents/Library/LaunchAgents/dev.atlas.atlas-indexd.plist`; placeholders `{{EXE_PATH}}`, `{{LOG_DIR}}`

## Icons

`build-app.sh` copies the checked-in app icon from `assets/branding/atlas.icns`
to `Atlas.app/Contents/Resources/atlas.icns`. The bundle template references it
with `CFBundleIconFile=atlas`.

Regenerate macOS, Windows, and Linux icon derivatives from
`assets/branding/atlas.png` with `sips`, `iconutil`, and ImageMagick.

## Out of scope (planned for v0.2)

- Sparkle auto-updater integration
- Linux `.deb` / `.rpm` / AppImage
- Windows MSI / MSIX
