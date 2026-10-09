#!/usr/bin/env bash
# Regenerate every app icon file from brand.toml's icon_svg (brand/icon.svg), into assets/app-icon/
# under neutral names (app.ico, app.icns, app-1024.png, app-macos-512.png, hicolor/<size>/apps/app.png).
# Packaging renames the hicolor icons to the brand's app id (packaging/env.sh install_icons).
#
# Needs: resvg (brew install resvg / cargo install resvg). On macOS, iconutil also writes the
# .icns. The outputs are committed, so building and packaging never need these tools.
#
#   packaging/icons.sh
set -euo pipefail
# shellcheck source=env.sh
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"
DIR="$ROOT/assets/app-icon"
SVG="$ROOT/$BRAND_ICON_SVG"
[ -f "$SVG" ] || { echo "error: $SVG (brand.toml icon_svg) not found" >&2; exit 1; }
ID="app"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

if command -v resvg >/dev/null; then
  render() { resvg -w "$2" -h "$2" "$1" "$3" </dev/null; }
elif command -v rsvg-convert >/dev/null; then
  render() { rsvg-convert -w "$2" -h "$2" -o "$3" "$1" </dev/null; }
else
  echo "error: resvg (or rsvg-convert) not found (brew install resvg)" >&2
  exit 1
fi

# The SVG is expected to be a full-bleed square tile (viewBox="0 0 N N"). Windows and Linux use it as is, so the lynx reads at
# 16-48 px. macOS icons follow Apple's grid: an 824/1024 body with a transparent margin, made by
# widening the viewBox (512 / 0.805 = 636, so 62 units each side).
MAC="$TMP/macos.svg"
N="$(sed -n 's/.*viewBox="0 0 \([0-9]*\) \1".*/\1/p' "$SVG" | head -n 1)"
[ -n "$N" ] || { echo "error: $SVG needs a square viewBox=\"0 0 N N\"" >&2; exit 1; }
M=$(((N * 1000 / 805 - N) / 2)) # margin each side
sed "s/viewBox=\"0 0 $N $N\"/viewBox=\"-$M -$M $((N + 2 * M)) $((N + 2 * M))\"/" "$SVG" >"$MAC"

render "$SVG" 1024 "$DIR/app-1024.png"
# Runtime window/Dock icon on macOS (embedded by apps/app/src/main.rs).
render "$MAC" 512 "$DIR/app-macos-512.png"

# Linux hicolor theme (also the runtime window icon on Windows and Linux: 256x256).
for s in 16 24 32 48 64 128 256 512; do
  mkdir -p "$DIR/hicolor/${s}x${s}/apps"
  render "$SVG" "$s" "$DIR/hicolor/${s}x${s}/apps/$ID.png"
done
mkdir -p "$DIR/hicolor/scalable/apps"
cp "$SVG" "$DIR/hicolor/scalable/apps/$ID.svg"

# Windows .ico.
ICO_PNGS=()
for s in 16 20 24 32 40 48 64 128 256; do
  render "$SVG" "$s" "$TMP/ico-$s.png"
  ICO_PNGS+=("$TMP/ico-$s.png")
done
(cd "$ROOT" && cargo run -q -p xtask -- ico "$DIR/app.ico" "${ICO_PNGS[@]}")

# macOS .icns.
if command -v iconutil >/dev/null; then
  SET="$TMP/app.iconset"
  mkdir -p "$SET"
  for s in 16 32 128 256 512; do
    render "$MAC" "$s" "$SET/icon_${s}x${s}.png"
    render "$MAC" $((s * 2)) "$SET/icon_${s}x${s}@2x.png"
  done
  iconutil -c icns -o "$DIR/app.icns" "$SET"
elif python3 -c 'import PIL' 2>/dev/null; then
  # Pillow writes every ICNS size from one large image
  render "$MAC" 1024 "$TMP/icns-1024.png"
  python3 -c 'import sys; from PIL import Image; Image.open(sys.argv[1]).save(sys.argv[2], format="ICNS")' \
    "$TMP/icns-1024.png" "$DIR/app.icns"
else
  echo "warning: iconutil (macOS) or python3 Pillow not found; app.icns not regenerated" >&2
fi
echo "icons written to $DIR"
