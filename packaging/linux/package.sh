#!/usr/bin/env bash
# Build and package the app for Linux (<arch> is x86_64 or aarch64; <binary> and every other name
# come from brand.toml):
#
#   $DIST/<binary>-<version>-linux-<arch>.AppImage  any distro with glibc >= the build host's
#   $DIST/<binary>-<version>-linux-<arch>.AppImage.zsync  delta updates (needs zsyncmake)
#   $DIST/<binary>-<version>-linux-<arch>.deb       Debian, Ubuntu, Mint, Pop!_OS, ...
#   $DIST/<binary>-<version>-linux-<arch>.rpm       Fedora, openSUSE, RHEL, ...
#   $DIST/<binary>-<version>-linux-<arch>.tar.gz    plain FHS-style tree (bin/, share/)
#
# Usage: packaging/linux/package.sh [--skip-build] [--formats "appimage deb rpm tar"]
#
# Needs: cargo; nfpm for deb/rpm (https://nfpm.goreleaser.com); appimagetool for the AppImage
# (downloaded into $CARGO_TARGET_DIR if missing). Build on an old distro (CI: Ubuntu 22.04,
# glibc 2.35) so the binaries run on newer ones. Optional: desktop-file-validate, appstreamcli,
# zsyncmake (the zsync package) for the AppImage's .zsync.
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
HERE="$ROOT/packaging/linux"
APP_ID=$BRAND_APP_ID
APP=$BRAND_BINARY
CLI=$BRAND_CLI_BINARY

SKIP_BUILD=0
FORMATS="appimage deb rpm tar"
while [ $# -gt 0 ]; do
  case "$1" in
    --skip-build) SKIP_BUILD=1; shift ;;
    --formats) FORMATS="$2"; shift 2 ;;
    -h | --help) sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

ARCH="$(uname -m)"
case "$ARCH" in
  x86_64) DEB_ARCH=amd64 ;;
  aarch64 | arm64) ARCH=aarch64; DEB_ARCH=arm64 ;;
  *) echo "unsupported architecture $ARCH" >&2; exit 2 ;;
esac
export PACKAGE_MAINTAINER="${PACKAGE_MAINTAINER:-$BRAND_VENDOR <noreply@example.invalid>}"
BASENAME="$APP-$VERSION-linux-$ARCH"

echo "==> $BRAND_DISPLAY_NAME $VERSION for Linux $ARCH ($FORMATS)"

if [ "$SKIP_BUILD" = 0 ]; then
  (cd "$ROOT" && cargo build --release --locked -p dac-app -p dac-cli --features dac-app/heif,dac-cli/heif)
fi
BIN="$CARGO_TARGET_DIR/release"
WORK="$CARGO_TARGET_DIR/linux-package"
STAGE="$WORK/root"
rm -rf "$WORK"

# ---- stage an FHS tree (shared by every format) -------------------------------------------------
# The cargo binaries have neutral names (app, app-cli); packages carry the brand's.
stage_binaries "$BIN" "$STAGE/usr/bin"
strip "$STAGE/usr/bin/$APP" "$STAGE/usr/bin/$CLI" 2>/dev/null || true
brand_render "$HERE/{app_id}.desktop.in" "$STAGE/usr/share/applications/$APP_ID.desktop"
brand_render "$HERE/{app_id}.mime.xml.in" "$STAGE/usr/share/mime/packages/$APP_ID.xml"
brand_render "$HERE/{app_id}.metainfo.xml.in" "$STAGE/usr/share/metainfo/$APP_ID.metainfo.xml"
brand_render "$HERE/70-{app_id}-ptp.rules.in" "$STAGE/usr/lib/udev/rules.d/70-$APP_ID-ptp.rules" # tethering: docs/tethering.md
install_icons "$STAGE/usr/share/icons"
mkdir -p "$STAGE/usr/share/doc/$APP"
copy_docs "$STAGE/usr/share/doc/$APP"

if command -v desktop-file-validate >/dev/null; then
  desktop-file-validate "$STAGE/usr/share/applications/$APP_ID.desktop"
fi
if command -v appstreamcli >/dev/null; then
  appstreamcli validate --no-net --explain "$STAGE/usr/share/metainfo/$APP_ID.metainfo.xml"
fi

has() { case " $FORMATS " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }

# ---- .tar.gz ------------------------------------------------------------------------------------
if has tar; then
  mkdir -p "$WORK/tar"
  cp -R "$STAGE/usr" "$WORK/tar/$BASENAME"
  tar -C "$WORK/tar" -czf "$DIST/$BASENAME.tar.gz" "$BASENAME"
  echo "wrote $DIST/$BASENAME.tar.gz"
fi

# ---- .deb / .rpm --------------------------------------------------------------------------------
if has deb || has rpm; then
  command -v nfpm >/dev/null || { echo "error: nfpm not found (https://nfpm.goreleaser.com/install/)" >&2; exit 1; }
  export VERSION
  export NFPM_ARCH="$DEB_ARCH"
  # nfpm expands env vars in fields like `version` and `arch`, but not in `contents[].src`.
  brand_render "$HERE/nfpm.yaml.in" "$WORK/nfpm.brand.yaml"
  sed "s|\${STAGE}|$STAGE|g" "$WORK/nfpm.brand.yaml" >"$WORK/nfpm.yaml"
  for fmt in deb rpm; do
    if has "$fmt"; then (cd "$ROOT" && nfpm package -f "$WORK/nfpm.yaml" -p "$fmt" -t "$DIST/$BASENAME.$fmt"); fi
  done
fi

# ---- AppImage -----------------------------------------------------------------------------------
if has appimage; then
  APPDIR="$WORK/$APP.AppDir"
  cp -R "$STAGE" "$APPDIR"
  mv "$APPDIR/usr/share/doc" "$WORK/doc-unused"
  ln -s "usr/bin/$APP" "$APPDIR/AppRun"
  cp "$STAGE/usr/share/applications/$APP_ID.desktop" "$APPDIR/$APP_ID.desktop"
  cp "$STAGE/usr/share/icons/hicolor/256x256/apps/$APP_ID.png" "$APPDIR/$APP_ID.png"
  ln -s "$APP_ID.png" "$APPDIR/.DirIcon"

  TOOL="${APPIMAGETOOL:-$(command -v appimagetool || true)}"
  if [ -z "$TOOL" ]; then
    TOOL="$CARGO_TARGET_DIR/appimagetool-$ARCH.AppImage"
    if [ ! -x "$TOOL" ]; then
      curl -fsSL -o "$TOOL" "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage"
      chmod +x "$TOOL"
    fi
  fi
  # Absolute, because appimagetool runs in $DIST below (CARGO_TARGET_DIR or APPIMAGETOOL may be
  # relative).
  OUT="$(cd "$DIST" && pwd)/$BASENAME.AppImage"
  APPDIR="$(cd "$APPDIR" && pwd)"
  TOOL="$(cd "$(dirname "$TOOL")" && pwd)/$(basename "$TOOL")"
  # A .zsync left by an earlier run would hide a missing zsyncmake and describe another file.
  rm -f "$OUT.zsync"
  # Update information: AppImageUpdate, AppImageLauncher and the like read it from the file and
  # fetch only the blocks that changed in a newer release, through the .zsync published next to
  # each AppImage on GitHub Releases. `latest` is the newest published release that is not a
  # pre-release. Builds point at the releases of GITHUB_REPOSITORY, else of brand.toml's
  # repository when that is on GitHub; with neither, the AppImage has no update information.
  REPO="${GITHUB_REPOSITORY:-}"
  case "$BRAND_REPOSITORY" in https://github.com/*/*) REPO="${REPO:-${BRAND_REPOSITORY#https://github.com/}}" ;; esac
  UPDATE_ARGS=()
  if [ -n "$REPO" ]; then
    UPDATE_ARGS=(-u "gh-releases-zsync|${REPO%%/*}|${REPO#*/}|latest|$APP-*-linux-$ARCH.AppImage.zsync")
  fi
  # Extract-and-run: works without FUSE (containers, CI). The output embeds the static runtime,
  # so users don't need libfuse2 either. With zsyncmake on the host (CI installs the zsync
  # package) appimagetool also writes the .zsync, into its working directory, hence the cd.
  (cd "$DIST" && ARCH="$ARCH" APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --no-appstream ${UPDATE_ARGS[@]+"${UPDATE_ARGS[@]}"} "$APPDIR" "$OUT")
  echo "wrote $OUT"
  if [ -s "$OUT.zsync" ]; then
    echo "wrote $OUT.zsync"
  else
    warn "zsyncmake not found, so $OUT.zsync was not written; AppImage delta updates need it"
  fi
fi

"$STAGE/usr/bin/$CLI" --version
echo "==> done"
ls -lh "$DIST"
