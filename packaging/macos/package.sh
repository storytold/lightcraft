#!/usr/bin/env bash
# Build, sign and (optionally) notarize the macOS release artifacts:
#
#   $DIST/<binary>-<version>-macos-<arch>.dmg          <display_name>.app on a drag-to-Applications DMG
# (names from brand.toml). The headless CLI is inside the app, Contents/MacOS/<cli_binary>, and the
# craft-fonts faces are files in Contents/Resources/fonts/, one copy read by both programs.
#
# Usage: packaging/macos/package.sh [--arch universal|arm64|x64] [--skip-build]
#   arm64 (Apple silicon) and x64 (Intel) hold one build each; universal holds both. The names are
#   arm64 and x64, never aarch64 and x86_64: earlier builds take any `-macos-universal`,
#   `-macos-aarch64` or `-macos-x86_64` image as theirs and pick the smallest (issue #1228).
# Fonts: use CRAFT_FONTS_DIR or ../craft-fonts; download the pinned checkout if absent.
#
# Signing (env):
#   MACOS_SIGN_IDENTITY   codesign identity (name or SHA-1). Default "-" = ad-hoc (local testing;
#                         Gatekeeper will reject the result on other Macs). In CI, import-cert.sh sets it.
#   MACOS_KEYCHAIN        keychain holding the identity (optional)
# Notarization (env; all three needed, and a real identity):
#   APPLE_ID, APPLE_PASSWORD (app-specific password), APPLE_TEAM_ID
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
HERE="$ROOT/packaging/macos"

ARCH=universal
SKIP_BUILD=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --arch) ARCH="$2"; shift 2 ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    -h | --help) sed -n '2,19p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
# LIPO_ARCHS: what `lipo -archs` must print for each binary, sorted
case "$ARCH" in
  universal) TARGETS=(aarch64-apple-darwin x86_64-apple-darwin) LIPO_ARCHS="arm64 x86_64" ;;
  arm64) TARGETS=(aarch64-apple-darwin) LIPO_ARCHS="arm64" ;;
  x64) TARGETS=(x86_64-apple-darwin) LIPO_ARCHS="x86_64" ;;
  *) echo "unknown --arch $ARCH (universal, arm64 or x64)" >&2; exit 2 ;;
esac

# Keep in sync with LSMinimumSystemVersion in Info.plist.in.
export MACOSX_DEPLOYMENT_TARGET=11.0
IDENTITY="${MACOS_SIGN_IDENTITY:--}"
WORK="$CARGO_TARGET_DIR/macos-package"
NAME="$BRAND_DISPLAY_NAME"
CLI="$BRAND_CLI_BINARY"
APP="$WORK/$NAME.app"
DMG="$DIST/$BRAND_BINARY-$VERSION-macos-$ARCH.dmg"

NOTARIZE=0
if [[ "$IDENTITY" = "-" ]]; then
  warn "macOS: ad-hoc signing (no MACOS_SIGN_IDENTITY); artifacts are not notarized"
elif [[ -n "${APPLE_ID:-}" ]] && [[ -n "${APPLE_PASSWORD:-}" ]] && [[ -n "${APPLE_TEAM_ID:-}" ]]; then
  NOTARIZE=1
else
  warn "macOS: APPLE_ID / APPLE_PASSWORD / APPLE_TEAM_ID incomplete; signed but not notarized"
fi

echo "==> $NAME $VERSION for macOS ($ARCH), identity: $IDENTITY, notarize: $NOTARIZE"

# ---- build -------------------------------------------------------------------------------------
# shellcheck source=../fonts.sh
. "$ROOT/packaging/fonts.sh"
# also with --skip-build: the bundle takes its fonts from the checkout
prepare_fonts
if [[ "$SKIP_BUILD" = 0 ]]; then
  # The fonts go into the bundle as files (below), not into each binary: the app and the CLI share them.
  export CRAFT_FONTS_EMBED=0
  args=()
  for t in "${TARGETS[@]}"; do args+=(--target "$t"); done
  (cd "$ROOT" && cargo build --release --locked -p lcc-app -p lcc-cli --features lcc-app/heif,lcc-cli/heif "${args[@]}")
fi

rm -rf "$WORK"
mkdir -p "$WORK/bin"
# neutral cargo names (app, app-cli) -> brand names
for pair in "app:$BRAND_BINARY" "app-cli:$CLI"; do
  bin="${pair%%:*}" out="${pair#*:}"
  inputs=()
  for t in "${TARGETS[@]}"; do inputs+=("$CARGO_TARGET_DIR/$t/release/$bin"); done
  lipo -create -output "$WORK/bin/$out" "${inputs[@]}"
  lipo -info "$WORK/bin/$out"
  archs="$(lipo -archs "$WORK/bin/$out" | tr ' ' '\n' | sort | xargs)"
  if [[ "$archs" != "$LIPO_ARCHS" ]]; then
    echo "error: $out holds '$archs', expected '$LIPO_ARCHS' for --arch $ARCH" >&2
    exit 1
  fi
done

# ---- signing helpers ---------------------------------------------------------------------------
sign() {
  # Hardened runtime + secure timestamp for a real identity; ad-hoc can't be timestamped.
  local ts=(--timestamp)
  [[ "$IDENTITY" = "-" ]] && ts=(--timestamp=none)
  local kc=()
  [[ -n "${MACOS_KEYCHAIN:-}" ]] && kc=(--keychain "$MACOS_KEYCHAIN")
  codesign --force --sign "$IDENTITY" ${kc[@]+"${kc[@]}"} "${ts[@]}" "$@"
}

notarize() {
  local file="$1" out id status
  echo "==> notarizing $(basename "$file") (this can take a few minutes)"
  out="$(xcrun notarytool submit "$file" --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" \
    --team-id "$APPLE_TEAM_ID" --wait --timeout 1h --output-format json)" || true
  echo "$out"
  id="$(printf '%s' "$out" | plutil -extract id raw -o - - 2>/dev/null || true)"
  status="$(printf '%s' "$out" | plutil -extract status raw -o - - 2>/dev/null || true)"
  if [[ "$status" != "Accepted" ]]; then
    if [[ -n "$id" ]]; then
      xcrun notarytool log "$id" --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID" || true
    fi
    echo "error: notarization of $(basename "$file") failed (status: ${status:-unknown})" >&2
    exit 1
  fi
}

# ---- <display_name>.app ----------------------------------------------------------------------------
echo "==> assembling $APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
# Executable and icon carry the display name (CFBundleExecutable / CFBundleIconFile).
cp "$WORK/bin/$BRAND_BINARY" "$APP/Contents/MacOS/$NAME"
ICNS="$(find "$ROOT/assets/app-icon" -maxdepth 1 -name '*.icns' | sort | head -n 1)"
cp "$ICNS" "$APP/Contents/Resources/$NAME.icns"
brand_render "$HERE/Info.plist.in" "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist"
printf 'APPL????' >"$APP/Contents/PkgInfo"
# The CLI beside the app: signed and updated with it (the updater replaces the whole bundle).
cp "$WORK/bin/$CLI" "$APP/Contents/MacOS/$CLI"
# The craft-fonts faces, once, as the checkout lays them out (fonts/manifest.txt, each file and its
# licence at the path the manifest gives), checked against the manifest's checksums. The binaries
# read them from here (lcc_engine::fonts::craft_fonts) and must not embed them.
trim() {
  local s="$1"
  s="${s#"${s%%[![:space:]]*}"}"
  printf '%s' "${s%"${s##*[![:space:]]}"}"
}
copy_fonts() {
  local src="$1" dest="$2" line file lfile sum n=0
  mkdir -p "$dest/fonts"
  cp "$src/fonts/manifest.txt" "$dest/fonts/manifest.txt"
  while IFS= read -r line || [[ -n "$line" ]]; do
    [[ "$line" =~ ^[[:space:]]*(#|$) ]] && continue
    # family | style | file | scripts | licence | licence file | sha256 | source
    IFS='|' read -r _ _ file _ _ lfile sum _ <<<"$line"
    file="$(trim "$file")" lfile="$(trim "$lfile")" sum="$(trim "$sum")"
    for f in "$file" "$lfile"; do
      case "$f" in "" | /* | *..*) echo "error: fonts/manifest.txt: unexpected path '$f'" >&2; return 1 ;; esac
      mkdir -p "$dest/$(dirname "$f")"
      cp "$src/$f" "$dest/$f"
    done
    if [[ "$(shasum -a 256 "$dest/$file" | cut -d' ' -f1)" != "$sum" ]]; then
      echo "error: $file does not match its checksum in fonts/manifest.txt" >&2
      return 1
    fi
    n=$((n + 1))
  done <"$src/fonts/manifest.txt"
  [[ "$n" -gt 0 ]] || { echo "error: no fonts listed in $src/fonts/manifest.txt" >&2; return 1; }
  echo "==> $n craft-fonts faces in $dest/fonts"
}
# A binary that embeds a face (a build without CRAFT_FONTS_EMBED=0, e.g. --skip-build over an older
# build) would carry a second copy: refuse it. Looks for 4 KiB from the middle of each face.
check_no_embedded_fonts() {
  local fonts="$1"; shift
  perl -e '
    my ($fonts, @bins) = @ARGV;
    open(my $m, "<", "$fonts/fonts/manifest.txt") or die "manifest: $!";
    my @chunks;
    while (<$m>) {
      next if /^\s*(#|$)/;
      my @f = map { s/^\s+|\s+$//gr } split /\|/;
      open(my $fh, "<:raw", "$fonts/$f[2]") or die "$f[2]: $!";
      local $/; my $b = <$fh>;
      push @chunks, [$f[2], substr($b, int(length($b) / 2), 4096)];
    }
    my $bad = 0;
    for my $bin (@bins) {
      open(my $fh, "<:raw", $bin) or die "$bin: $!";
      local $/; my $b = <$fh>;
      for my $c (@chunks) {
        if (index($b, $c->[1]) >= 0) { print STDERR "error: $bin embeds $c->[0]\n"; $bad = 1; }
      }
    }
    exit $bad;
  ' "$fonts" "$@"
}
copy_fonts "$CRAFT_FONTS_DIR" "$APP/Contents/Resources"
check_no_embedded_fonts "$APP/Contents/Resources" "$APP/Contents/MacOS/$NAME" "$APP/Contents/MacOS/$CLI"
# Licences (and the craft-fonts font licences when built with CRAFT_FONTS_DIR) inside the bundle.
mkdir -p "$APP/Contents/Resources/Licenses"
copy_docs "$APP/Contents/Resources/Licenses"

# Remove inherited build-time extended attributes BEFORE code signing.
# Never repair, re-sign or strip metadata from the published release after signing.
xattr -cr "$APP"

# Sign inside-out: nested code first, then the bundle itself (no --deep on the final signature).
# The nested code is the main executable and the CLI; frameworks/helpers would be signed here too.
# The CLI has the app's entitlements: `--opt addToPhotos=true` / `app.export {addToPhotos}` send
# Apple Events to Photos through osascript too (macOS asks about the app that started the CLI,
# usually the terminal).
sign --options runtime --entitlements "$HERE/entitlements.plist" "$APP/Contents/MacOS/$CLI"
sign --options runtime --entitlements "$HERE/entitlements.plist" "$APP/Contents/MacOS/$NAME"
sign --options runtime --entitlements "$HERE/entitlements.plist" "$APP"
codesign --verify --strict --deep --verbose=2 "$APP"

if [[ "$NOTARIZE" = 1 ]]; then
  ditto -c -k --keepParent "$APP" "$WORK/app-notarize.zip"
  notarize "$WORK/app-notarize.zip"
  xcrun stapler staple "$APP"
  xcrun stapler validate "$APP"
  spctl --assess --type execute -vvv "$APP"
fi

# ---- DMG ---------------------------------------------------------------------------------------
echo "==> building $DMG"
STAGE="$WORK/dmg"
mkdir -p "$STAGE"
ditto "$APP" "$STAGE/$NAME.app"
# While [legacy] display_names is non-empty in the brand file, add a hidden `<name>.app` copy under each
# earlier name: the updater of a build released under that name looks for its own name in the image
# (fork #411). Restore the block from 0a528340e when the next rename needs it.
ln -s /Applications "$STAGE/Applications"
# Finder window layout: background, icon size and positions (packaging/macos/dmg/README.md).
mkdir -p "$STAGE/.background"
cp "$HERE/dmg/background.tiff" "$STAGE/.background/background.tiff"
cp "$HERE/dmg/dmg-layout.DS_Store" "$STAGE/.DS_Store"
rm -f "$DMG"
# hdiutil makehybrid -hfs injects FinderInfo metadata into the signed app.
# create -srcfolder preserves the signed app and the existing .DS_Store layout.
# hdiutil may transiently return "Resource busy" on CI; retry boundedly.
hdiutil_retry() {
  local verb="$1" attempt
  for attempt in 1 2 3 4 5; do
    if hdiutil "$@"; then
      return 0
    fi
    if [[ "$attempt" = 5 ]]; then
      echo "error: hdiutil $verb failed after five attempts" >&2
      return 1
    fi
    warn "hdiutil $verb attempt $attempt failed; retrying"
    sleep $((attempt * 10))
  done
}
hdiutil_retry create -srcfolder "$STAGE" -volname "$NAME" -fs HFS+ -format UDZO \
  -imagekey zlib-level=9 -ov "$DMG"
sign "$DMG"
codesign --verify --strict --verbose=2 "$DMG"

# Verification must use the FINISHED image (and a normal ditto-installed copy).
# Signatures of the DMG container alone do not validate the bundle in the image.
verify_dmg_app() {
  local mount="$WORK/dmg-verify-mount" copy="$WORK/dmg-verify-copy" rc=0
  mkdir -p "$mount" "$copy"
  if ! hdiutil_retry attach -readonly -nobrowse -noautoopen -mountpoint "$mount" "$DMG" >/dev/null; then
    echo "error: unable to mount finished $DMG" >&2
    hdiutil detach "$mount" >/dev/null 2>&1 || true
    return 1
  fi
  echo "==> strict codesign of $NAME.app in finished DMG"
  codesign --verify --deep --strict --verbose=2 "$mount/$NAME.app" || rc=1
  if [[ "$rc" = 0 ]]; then
    ditto "$mount/$NAME.app" "$copy/$NAME.app" || rc=1
  fi
  if [[ "$rc" = 0 ]]; then
    echo "==> strict codesign of $NAME.app after ditto install-copy"
    codesign --verify --deep --strict --verbose=2 "$copy/$NAME.app" || rc=1
  fi
  if [[ "$rc" = 0 ]] && [[ "$NOTARIZE" = 1 ]]; then
    spctl --assess --type execute -vvv "$copy/$NAME.app" || rc=1
  fi
  if [[ "$rc" != 0 ]]; then
    echo "error: published-image app verification failed; reject $DMG" >&2
    xattr -lr "$mount/$NAME.app" >&2 || true
  fi
  if ! hdiutil_retry detach "$mount" >/dev/null; then
    echo "error: could not detach verification mount" >&2
    rc=1
  fi
  rm -rf "$copy"
  return "$rc"
}
verify_dmg_app
if [[ "$NOTARIZE" = 1 ]]; then
  notarize "$DMG"
  xcrun stapler staple "$DMG"
  xcrun stapler validate "$DMG"
  spctl --assess --type open --context context:primary-signature -vvv "$DMG"
fi

# only where this Mac runs it natively: an Intel build on Apple silicon needs Rosetta, which a
# build machine needn't have
if [[ " $LIPO_ARCHS " == *" $(uname -m) "* ]]; then
  "$APP/Contents/MacOS/$CLI" --version
else
  echo "==> not running the $ARCH CLI on this $(uname -m) Mac"
fi
echo "==> done"
ls -lh "$DMG"
