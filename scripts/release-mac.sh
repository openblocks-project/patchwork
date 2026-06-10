#!/usr/bin/env bash
#
# scripts/release-mac.sh — build a signed, notarized Patchwork.dmg
#
# USAGE
#   scripts/release-mac.sh                    # full release
#   scripts/release-mac.sh --skip-notarize    # signed but not notarized (fast, local-test only)
#   scripts/release-mac.sh --icon path.png    # use a specific icon
#
# ONE-TIME SETUP (do these once, then never again):
#   1. Apple Developer ID Application cert installed in login keychain
#      Verify: security find-identity -v -p codesigning
#
#   2. Store your notarization credentials in keychain (NOT the script):
#      xcrun notarytool store-credentials patchwork-notary \
#          --apple-id YOUR_APPLE_ID@example.com \
#          --team-id W4HN4MBP45 \
#          --password YOUR_APP_SPECIFIC_PASSWORD
#
#   3. Drop your app icon at scripts/icon.png (1024×1024 PNG recommended)

set -euo pipefail

# ── Config (edit these per project, not per release) ────────────
APP_NAME="Patchwork"
BUNDLE_ID="com.openblocks.patchwork"
SIGNING_IDENTITY="Developer ID Application: Allwin Williams (W4HN4MBP45)"
NOTARY_PROFILE="patchwork-notary"
MIN_MACOS="11.0"
EXECUTABLE_NAME="patchwork"  # must match [package].name in Cargo.toml

# ── Parse args ─────────────────────────────────────────────────
ICON_SOURCE=""
SKIP_NOTARIZE=false
while [[ $# -gt 0 ]]; do
    case "$1" in
        --icon)           ICON_SOURCE="$2"; shift 2 ;;
        --skip-notarize)  SKIP_NOTARIZE=true; shift ;;
        -h|--help)
            grep '^#' "$0" | sed 's/^# \?//'
            exit 0
            ;;
        *) echo "Unknown arg: $1" >&2; exit 1 ;;
    esac
done

# Auto-detect icon. Priority:
#   1. --icon CLI argument
#   2. assets/icons/icon.icns  (pre-built multi-resolution — fastest, best quality)
#   3. assets/icons/icon.png   (1024×1024 PNG — script converts on the fly)
#   4. scripts/icon.png        (canonical fallback for projects without assets/)
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"
if [[ -z "$ICON_SOURCE" ]]; then
    for CAND in assets/icons/icon.icns assets/icons/icon.png scripts/icon.png; do
        if [[ -f "$CAND" ]]; then
            ICON_SOURCE="$CAND"
            break
        fi
    done
fi

# ── Paths ──────────────────────────────────────────────────────
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
BUILD_DIR="$PROJECT_ROOT/target/release-bundle"
APP_BUNDLE="$BUILD_DIR/$APP_NAME.app"
DMG_OUT="$BUILD_DIR/$APP_NAME-$VERSION.dmg"

echo ""
echo "═══════════════════════════════════════════════════════════"
echo "  Building $APP_NAME v$VERSION"
echo "  Bundle ID:        $BUNDLE_ID"
echo "  Signing identity: $SIGNING_IDENTITY"
echo "  Icon source:      ${ICON_SOURCE:-(none — generic icon)}"
echo "  Notarize:         $([ "$SKIP_NOTARIZE" = true ] && echo NO || echo YES)"
echo "  Output:           $DMG_OUT"
echo "═══════════════════════════════════════════════════════════"

# ── Sanity check the signing identity exists ───────────────────
if ! security find-identity -v -p codesigning | grep -qF "$SIGNING_IDENTITY"; then
    echo "✗ Signing identity not found in keychain:"
    echo "    $SIGNING_IDENTITY"
    echo ""
    echo "  Available identities:"
    security find-identity -v -p codesigning | sed 's/^/    /'
    exit 1
fi

# ── 1. Build the Rust binary ───────────────────────────────────
echo ""
echo "▶ [1/9] cargo build --release"
cargo build --release --bin "$EXECUTABLE_NAME"

# ── 2. Construct the .app bundle ───────────────────────────────
echo ""
echo "▶ [2/9] Bundling .app structure"
rm -rf "$BUILD_DIR"
mkdir -p "$APP_BUNDLE/Contents/MacOS"
mkdir -p "$APP_BUNDLE/Contents/Resources"
mkdir -p "$APP_BUNDLE/Contents/Frameworks"
cp "target/release/$EXECUTABLE_NAME" "$APP_BUNDLE/Contents/MacOS/"

# Generate Info.plist by substituting placeholders into the template
sed -e "s/__VERSION__/$VERSION/g" \
    -e "s/__BUNDLE_ID__/$BUNDLE_ID/g" \
    -e "s/__APP_NAME__/$APP_NAME/g" \
    -e "s/__EXECUTABLE__/$EXECUTABLE_NAME/g" \
    -e "s/__MIN_MACOS__/$MIN_MACOS/g" \
    scripts/Info.plist.template > "$APP_BUNDLE/Contents/Info.plist"

# ── 3. Bundle ONNX runtime dylib alongside the binary (if present) ──
#   The `ort` crate vendors libonnxruntime.dylib into the build output.
#   For a standalone .app, it must live in Contents/Frameworks/ and the
#   binary's rpath must point there.
ORT_DYLIB=$(find target/release/build -name 'libonnxruntime*.dylib' 2>/dev/null | head -1 || true)
if [[ -n "$ORT_DYLIB" ]]; then
    echo "    → bundling $ORT_DYLIB"
    cp "$ORT_DYLIB" "$APP_BUNDLE/Contents/Frameworks/"
    # Add @executable_path/../Frameworks to the binary's runtime search path
    install_name_tool -add_rpath "@executable_path/../Frameworks" \
        "$APP_BUNDLE/Contents/MacOS/$EXECUTABLE_NAME" 2>/dev/null || true
fi

# ── 4. Place icon ──────────────────────────────────────────────
#   Use existing .icns directly when available (fastest, highest quality);
#   convert from PNG via iconutil when only a PNG is provided.
if [[ -n "$ICON_SOURCE" && -f "$ICON_SOURCE" ]]; then
    if [[ "$ICON_SOURCE" == *.icns ]]; then
        echo ""
        echo "▶ [3/9] Copying existing .icns ($ICON_SOURCE)"
        cp "$ICON_SOURCE" "$APP_BUNDLE/Contents/Resources/AppIcon.icns"
    else
        echo ""
        echo "▶ [3/9] Generating .icns from $ICON_SOURCE"
        ICONSET="$BUILD_DIR/AppIcon.iconset"
        mkdir -p "$ICONSET"
        # Standard iconset variants — macOS picks the right one per display density.
        for SIZE in 16 32 128 256 512; do
            DBL=$(( SIZE * 2 ))
            sips -z $SIZE $SIZE "$ICON_SOURCE" --out "$ICONSET/icon_${SIZE}x${SIZE}.png"     >/dev/null
            sips -z $DBL  $DBL  "$ICON_SOURCE" --out "$ICONSET/icon_${SIZE}x${SIZE}@2x.png"  >/dev/null
        done
        sips -z 1024 1024 "$ICON_SOURCE" --out "$ICONSET/icon_512x512@2x.png" >/dev/null
        iconutil -c icns "$ICONSET" -o "$APP_BUNDLE/Contents/Resources/AppIcon.icns"
        rm -rf "$ICONSET"
    fi
else
    echo ""
    echo "▶ [3/9] No icon — bundle gets the generic application icon."
    # Strip the icon reference from Info.plist so macOS uses default
    /usr/libexec/PlistBuddy -c "Delete :CFBundleIconFile" "$APP_BUNDLE/Contents/Info.plist" 2>/dev/null || true
fi

# ── 5. Codesign (hardened runtime + entitlements + timestamp) ─────
echo ""
echo "▶ [4/9] codesigning the bundle"
# Sign nested dylibs first (deep sign sometimes misses these)
find "$APP_BUNDLE/Contents/Frameworks" -name '*.dylib' -exec \
    codesign --force --options=runtime --timestamp \
        --sign "$SIGNING_IDENTITY" {} \;
# Sign the main executable + bundle
codesign --deep --force \
    --options=runtime \
    --entitlements scripts/entitlements.plist \
    --sign "$SIGNING_IDENTITY" \
    --timestamp \
    "$APP_BUNDLE"

# ── 6. Verify signature ────────────────────────────────────────
echo ""
echo "▶ [5/9] Verifying signature"
codesign --verify --deep --strict --verbose=2 "$APP_BUNDLE"

# ── 7. Notarize the .app ───────────────────────────────────────
if [[ "$SKIP_NOTARIZE" == "true" ]]; then
    echo ""
    echo "▶ [6/9] Skipping notarization (--skip-notarize)"
else
    echo ""
    echo "▶ [6/9] Notarizing .app (Apple's service usually takes 1–5 min)"
    ZIP_TMP="$BUILD_DIR/$APP_NAME.zip"
    ditto -c -k --keepParent "$APP_BUNDLE" "$ZIP_TMP"
    xcrun notarytool submit "$ZIP_TMP" \
        --keychain-profile "$NOTARY_PROFILE" \
        --wait
    rm -f "$ZIP_TMP"

    # ── 8. Staple the ticket onto the .app ─────────────────────
    echo ""
    echo "▶ [7/9] Stapling notarization ticket to .app"
    xcrun stapler staple "$APP_BUNDLE"
fi

# ── 9. Build the DMG ───────────────────────────────────────────
echo ""
echo "▶ [8/9] Building .dmg"
rm -f "$DMG_OUT"
hdiutil create \
    -volname "$APP_NAME" \
    -srcfolder "$APP_BUNDLE" \
    -ov -format UDZO \
    "$DMG_OUT"

# Sign the DMG itself (Gatekeeper checks this)
codesign --sign "$SIGNING_IDENTITY" --timestamp "$DMG_OUT"

# ── 10. Notarize the DMG too (best practice) ───────────────────
if [[ "$SKIP_NOTARIZE" != "true" ]]; then
    echo ""
    echo "▶ [9/9] Notarizing the .dmg"
    xcrun notarytool submit "$DMG_OUT" \
        --keychain-profile "$NOTARY_PROFILE" \
        --wait
    xcrun stapler staple "$DMG_OUT"
fi

# ── Done ───────────────────────────────────────────────────────
SIZE=$(du -h "$DMG_OUT" | cut -f1)
echo ""
echo "═══════════════════════════════════════════════════════════"
echo "  ✓ Done"
echo "  $DMG_OUT  ($SIZE)"
echo ""
echo "  Verify with Gatekeeper:"
echo "    spctl --assess --type open --context context:primary-signature -v \"$APP_BUNDLE\""
echo "═══════════════════════════════════════════════════════════"
