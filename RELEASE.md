# Releasing Patchwork (macOS)

A signed, notarized `.dmg` you can share anywhere — your website, a Discord, GitHub
Releases. Users double-click to mount, drag to Applications, and run without Gatekeeper
warnings.

## One-time setup

You only do these once. After that, every release is `scripts/release-mac.sh` and ~5 min.

### 1. Apple Developer ID Application certificate

Already done if you can run:

```bash
security find-identity -v -p codesigning
```

and see `Developer ID Application: Allwin Williams (W4HN4MBP45)` as a valid identity.
(If not, see the cert-creation flow in the project chat history.)

### 2. App-specific password for notarization

Apple won't let you upload to their notarization service with your real password —
you need an app-specific one.

1. Go to <https://account.apple.com/account/manage> → **Sign-In and Security** →
   **App-Specific Passwords** → **Generate**
2. Label it `Patchwork notarization`
3. Apple shows you a password like `abcd-efgh-ijkl-mnop`. **Copy it now** — Apple
   won't show it again.
4. Store it in your macOS Keychain so the script can use it without you typing it
   every release:

   ```bash
   xcrun notarytool store-credentials patchwork-notary \
       --apple-id YOUR_APPLE_ID@example.com \
       --team-id W4HN4MBP45 \
       --password abcd-efgh-ijkl-mnop
   ```

   This creates a `patchwork-notary` profile inside your keychain. The build script
   references the profile by name; the password itself never lives on disk.

### 3. App icon

Drop your icon at `scripts/icon.png` (the script picks it up automatically),
or pass `--icon /path/to/icon.png` to the build script.

- **Recommended:** 1024×1024 PNG. The script auto-generates every smaller variant
  needed for Retina, Dock, Finder, etc.
- Smaller PNGs work but will look fuzzy at Retina sizes.

### 4. System tools (one-line check)

```bash
which xcrun hdiutil iconutil sips codesign install_name_tool
```

All ship with macOS / Xcode Command Line Tools — should all return paths. If `xcrun`
is missing, install Xcode Command Line Tools: `xcode-select --install`.

## Releasing

```bash
scripts/release-mac.sh
```

What happens (3–10 min depending on cargo cache + Apple's notarization queue):

| # | Step | Time |
|---|------|------|
| 1 | `cargo build --release` | ~30 s (cached) – 5 min (fresh) |
| 2 | Build `.app` bundle, copy ONNX runtime, generate Info.plist | <5 s |
| 3 | Convert icon → `.icns` | <5 s |
| 4 | Sign with hardened runtime + entitlements | ~5 s |
| 5 | Verify signature | <5 s |
| 6 | Submit to Apple's notarization service, wait | 1–5 min |
| 7 | Staple notarization ticket to `.app` | <5 s |
| 8 | Build `.dmg`, sign it | ~10 s |
| 9 | Notarize + staple the `.dmg` | 1–5 min |

Output: `target/release-bundle/Patchwork-X.Y.Z.dmg`

## Variants

```bash
# Fast iteration — skip Apple's notarization service.
# Output is signed but Gatekeeper will warn on a fresh Mac.
# Useful when testing the bundle layout locally before a real release.
scripts/release-mac.sh --skip-notarize

# Use a different icon for one build
scripts/release-mac.sh --icon path/to/different-icon.png
```

## Updating the version

Just bump `version = "X.Y.Z"` in `Cargo.toml`. The build script reads it. No other edits.

## Verifying the result

After the script finishes, sanity-check that Gatekeeper is happy:

```bash
APP="target/release-bundle/Patchwork.app"
DMG="target/release-bundle/Patchwork-$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2).dmg"

# Should both print "accepted" with a "source=Notarized Developer ID" line
spctl --assess --type open --context context:primary-signature -v "$APP"
spctl --assess --type open --context context:primary-signature -v "$DMG"
```

Mount the `.dmg`, drag the app to Applications, double-click. No warning dialog →
notarization is working correctly.

## When things break

| Symptom | Likely cause | Fix |
|---|---|---|
| `security find-identity` shows 0 | Cert expired, revoked, or not trusted | Re-issue at developer.apple.com |
| Notarization fails with "code signature is not valid" | A dylib inside the bundle isn't signed | Look at the notarization log; the script signs `Contents/Frameworks/*.dylib` but if a nested dep has its own bundle structure it may need extra coverage |
| Notarization stuck "in progress" >15 min | Apple's queue lags occasionally | Wait, or check status: `xcrun notarytool history --keychain-profile patchwork-notary` |
| `xcrun: error: invalid active developer path` | Xcode Command Line Tools missing | `xcode-select --install` |
| Camera permission denied at runtime | `NSCameraUsageDescription` stripped | Check `Info.plist.template` is present and unmodified |
| Audio permission denied | `NSMicrophoneUsageDescription` stripped | Same as above |

## What's NOT bundled (users need separately)

Patchwork shells out to a few external tools that aren't included in the `.dmg`.
Document these on your download page or first-launch screen:

- **`ffmpeg`** — used by Video In (Camera / URL / File / Screen sources). Users install
  via `brew install ffmpeg`.
- **`yt-dlp`** — used by Video In's URL mode for YouTube / Vimeo. `brew install yt-dlp`.
- **NDI runtime** — used by NDI source (optional). Download from ndi.video.

Bundling these properly is a separate v1.1 task (would significantly increase the `.dmg`
size and add per-tool signing).

## Files in this release pipeline

| File | Purpose |
|------|---------|
| `scripts/release-mac.sh` | The build script. Run it. |
| `scripts/entitlements.plist` | Hardened-runtime entitlements (library validation off for CLAP/RustPlugin, JIT capability for future Cranelift work). |
| `scripts/Info.plist.template` | Bundle metadata + macOS permission prompts. Placeholders get substituted at build time. |
| `scripts/icon.png` | Your app icon (you provide). |
| `RELEASE.md` | This file. |
