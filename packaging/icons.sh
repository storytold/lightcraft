#!/usr/bin/env bash
# Regenerate every app icon file (desktop, iOS, Android) from assets/app-icon/lightcraft.svg.
#
# Needs: resvg (brew install resvg / cargo install resvg). On macOS, iconutil also writes the
# .icns. The outputs are committed, so building and packaging never need these tools.
#
#   packaging/icons.sh
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIR="$ROOT/assets/app-icon"
SVG="$DIR/lightcraft.svg"
ID="ai.storyteller.lightcraft"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

command -v resvg >/dev/null || { echo "error: resvg not found (brew install resvg)" >&2; exit 1; }

# The SVG is the full-bleed 512 tile (rx=112). Windows and Linux use it as is, so the lynx reads at
# 16-48 px. macOS icons follow Apple's grid: an 824/1024 body with a transparent margin, made by
# widening the viewBox (512 / 0.805 = 636, so 62 units each side).
MAC="$TMP/macos.svg"
sed 's/viewBox="0 0 512 512"/viewBox="-62 -62 636 636"/' "$SVG" >"$MAC"
grep -q 'viewBox="-62 -62 636 636"' "$MAC" || { echo "error: unexpected viewBox in $SVG" >&2; exit 1; }

render() { resvg -w "$2" -h "$2" "$1" "$3" </dev/null; }

render "$SVG" 1024 "$DIR/lightcraft-1024.png"
# Runtime window/Dock icon on macOS (embedded by apps/lightcraft/src/main.rs).
render "$MAC" 512 "$DIR/lightcraft-macos-512.png"

# Linux hicolor theme (also the runtime window icon on Windows and Linux: 256x256).
for s in 16 24 32 48 64 128 256 512; do
  mkdir -p "$DIR/hicolor/${s}x${s}/apps"
  render "$SVG" "$s" "$DIR/hicolor/${s}x${s}/apps/$ID.png"
done
mkdir -p "$DIR/hicolor/scalable/apps"
cp "$DIR/lightcraft-small.svg" "$DIR/hicolor/scalable/apps/$ID.svg"

# Windows .ico.
ICO_PNGS=()
for s in 16 20 24 32 40 48 64 128 256; do
  render "$SVG" "$s" "$TMP/ico-$s.png"
  ICO_PNGS+=("$TMP/ico-$s.png")
done
(cd "$ROOT" && cargo run -q -p xtask -- ico "$DIR/lightcraft.ico" "${ICO_PNGS[@]}")

# macOS .icns.
if command -v iconutil >/dev/null; then
  SET="$TMP/lightcraft.iconset"
  mkdir -p "$SET"
  for s in 16 32 128 256 512; do
    render "$MAC" "$s" "$SET/icon_${s}x${s}.png"
    render "$MAC" $((s * 2)) "$SET/icon_${s}x${s}@2x.png"
  done
  iconutil -c icns -o "$DIR/lightcraft.icns" "$SET"
else
  echo "warning: iconutil not found (macOS only); lightcraft.icns not regenerated" >&2
fi

# The art bakes in the tile: the figure stops 6.5 units short of the right and bottom edges and
# follows the rounded corner. Mobile systems mask a square themselves, so these drop the clip and
# the amber field where the system draws its own background.
unclip() { sed -e 's|<defs>.*</defs>||' -e 's| clip-path="url(#t)"||' "$SVG"; }
no_field() { sed 's|<rect width="512" height="512" fill="#f2a516"/>||'; }
view() { sed "s|viewBox=\"0 0 512 512\"|viewBox=\"$1\"|"; }
check() { grep -q "viewBox=\"$2\"" "$1" && ! grep -q 'clip-path' "$1" || { echo "error: unexpected structure in $SVG" >&2; exit 1; }; }

# iOS/iPadOS asset catalog (compiled by apps/lightcraft-mobile/scripts/ios-sim.sh with actool):
# one opaque 1024 square, the art scaled 1.03 from the top left so the body bleeds past the
# system's corner mask. Dark: the figure alone (the system draws the dark field). Tinted: a
# grayscale paper figure on black, which the system tints by luminance.
IOS="$DIR/ios/Assets.xcassets/AppIcon.appiconset"
IOSVIEW="0 0 497 497"
unclip | view "$IOSVIEW" >"$TMP/ios.svg"
no_field <"$TMP/ios.svg" >"$TMP/ios-dark.svg"
sed -e 's/fill="#f2a516"/fill="#000000"/' -e 's/fill="#efe9dc"/fill="#ffffff"/' -e 's/fill="#0b0b0c"/fill="#000000"/' \
  "$TMP/ios.svg" >"$TMP/ios-tinted.svg"
for v in ios ios-dark ios-tinted; do check "$TMP/$v.svg" "$IOSVIEW"; done
grep -q 'fill="#f2a516"' "$TMP/ios-dark.svg" && { echo "error: amber field not removed from $SVG" >&2; exit 1; }
render "$TMP/ios.svg" 1024 "$IOS/AppIcon-1024.png"
render "$TMP/ios-dark.svg" 1024 "$IOS/AppIcon-1024-dark.png"
render "$TMP/ios-tinted.svg" 1024 "$IOS/AppIcon-1024-tinted.png"

# Android adaptive icon (packaged by apps/lightcraft-mobile/scripts/android-apk.sh): the amber field
# is the background colour (values/ic_launcher_background.xml), the figure the 108 dp foreground.
# Scale 0.148 dp per unit about the head (266, 176): the ear tufts stay inside the 72 dp circle
# mask, the right edge sits at the 72 dp viewport's edge (the same sliver as the tile) and the body
# runs off the bottom. Monochrome (themed icons): the paper alone, the line work cut out of it.
RES="$DIR/android/res"
ANDVIEW="-99 -189 730 730"
unclip | no_field | view "$ANDVIEW" >"$TMP/android.svg"
sed 's|<path fill="#0b0b0c"[^>]*/>||' "$TMP/android.svg" >"$TMP/android-mono.svg"
for v in android android-mono; do check "$TMP/$v.svg" "$ANDVIEW"; done
grep -q 'fill="#0b0b0c"' "$TMP/android-mono.svg" && { echo "error: ink not removed from $SVG" >&2; exit 1; }
for d in mdpi:108 hdpi:162 xhdpi:216 xxhdpi:324 xxxhdpi:432; do
  mkdir -p "$RES/mipmap-${d%%:*}"
  render "$TMP/android.svg" "${d##*:}" "$RES/mipmap-${d%%:*}/ic_launcher_foreground.png"
  render "$TMP/android-mono.svg" "${d##*:}" "$RES/mipmap-${d%%:*}/ic_launcher_monochrome.png"
done
echo "icons written to $DIR"
