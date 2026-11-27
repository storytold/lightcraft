#!/usr/bin/env bash
# Build and package LightCraft for Linux (<arch> is x86_64 or aarch64, or riscv64 when cross-compiling):
#
#   $DIST/lightcraft-<version>-linux-<arch>.AppImage  any distro with glibc >= the build host's
#   $DIST/lightcraft-<version>-linux-<arch>.AppImage.zsync  delta updates (needs zsyncmake)
#   $DIST/lightcraft-<version>-linux-<arch>.deb       Debian, Ubuntu, Mint, Pop!_OS, ...
#   $DIST/lightcraft-<version>-linux-<arch>.rpm       Fedora, openSUSE, RHEL, ...
#   $DIST/lightcraft-<version>-linux-<arch>.tar.gz    plain FHS-style tree (bin/, share/)
#
# Usage: packaging/linux/package.sh [--skip-build] [--formats "appimage deb rpm tar"] [--arch-tar-dir DIRECTORY]
# Fonts: use CRAFT_FONTS_DIR or ../craft-fonts; download the pinned checkout if absent.
#
# Needs: cargo; nfpm for deb/rpm (https://nfpm.goreleaser.com); appimagetool for the AppImage
# (downloaded into $CARGO_TARGET_DIR if missing). Build on an old distro (CI: Ubuntu 22.04,
# glibc 2.35) so the binaries run on newer ones. Optional: desktop-file-validate, appstreamcli,
# zsyncmake (the zsync package) for the AppImage's .zsync.
# Cross-compiling (CI: riscv64, tar only): set CROSS_ARCH, CROSS_TARGET (the Rust target),
# CROSS_COMPILE (binutils prefix, for strip) and optionally EMULATOR (runs the CLI smoke test).
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
HERE="$ROOT/packaging/linux"
APP_ID=ai.storyteller.lightcraft

SKIP_BUILD=0
FORMATS="appimage deb rpm tar"
ARCH_TAR_DIR=
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-build) SKIP_BUILD=1; shift ;;
    --formats) FORMATS="$2"; shift 2 ;;
    --arch-tar-dir)
      [[ -n "${2:-}" ]] || { echo "error: --arch-tar-dir needs a directory" >&2; exit 2; }
      ARCH_TAR_DIR="$2"
      shift 2
      ;;
    -h | --help) sed -n '2,17p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

ARCH="${CROSS_ARCH:-$(uname -m)}"
case "$ARCH" in
  riscv64) DEB_ARCH=riscv64 ;;
  x86_64) DEB_ARCH=amd64 ;;
  aarch64 | arm64) ARCH=aarch64; DEB_ARCH=arm64 ;;
  *) echo "unsupported architecture $ARCH" >&2; exit 2 ;;
esac
export LIGHTCRAFT_MAINTAINER="${LIGHTCRAFT_MAINTAINER:-LightCraft maintainers <lightcraft@storyteller.ai>}"
IS_NIGHTLY=0
if [[ "${LIGHTCRAFT_BUILD_CHANNEL:-}" == nightly-v1 || "$VERSION" == *-nightly* ]]; then
  IS_NIGHTLY=1
  export LIGHTCRAFT_BUILD_CHANNEL=nightly-v1
fi
ARTIFACT_VERSION="$VERSION"
if [[ "$IS_NIGHTLY" = 1 && "$VERSION" != *-nightly* ]]; then
  ARTIFACT_VERSION="${VERSION}-nightly"
fi
BASENAME="lightcraft-$ARTIFACT_VERSION-linux-$ARCH"

if [[ -n "$ARCH_TAR_DIR" && "$IS_NIGHTLY" != 1 ]]; then
  echo "error: --arch-tar-dir is only for Nightly builds" >&2
  exit 2
fi

echo "==> LightCraft $VERSION for Linux $ARCH ($FORMATS, channel=${LIGHTCRAFT_BUILD_CHANNEL:-stable})"

if [[ "$SKIP_BUILD" = 0 ]]; then
  # shellcheck source=../fonts.sh
  . "$ROOT/packaging/fonts.sh"
  prepare_fonts
  (cd "$ROOT" && cargo build --release --locked -p lightcraft -p lightcraft-cli --features lightcraft/heif,lightcraft-cli/heif \
    ${CROSS_TARGET:+--target "$CROSS_TARGET"})
fi
BIN="$CARGO_TARGET_DIR/${CROSS_TARGET:+$CROSS_TARGET/}release"
WORK="$CARGO_TARGET_DIR/linux-package"
STAGE="$WORK/root"
rm -rf "$WORK"

# ---- stage an FHS tree (shared by every format) -------------------------------------------------
install -Dm755 "$BIN/lightcraft" "$STAGE/usr/bin/lightcraft"
install -Dm755 "$BIN/lightcraft-cli" "$STAGE/usr/bin/lightcraft-cli"
"${CROSS_COMPILE:-}strip" "$STAGE/usr/bin/lightcraft" "$STAGE/usr/bin/lightcraft-cli" 2>/dev/null || true
install -Dm644 "$HERE/$APP_ID.desktop" "$STAGE/usr/share/applications/$APP_ID.desktop"
if [[ "$IS_NIGHTLY" = 1 ]]; then
  sed -e 's|^StartupWMClass=.*|StartupWMClass=ai.storyteller.lightcraft.nightly-v1|' \
      "$STAGE/usr/share/applications/$APP_ID.desktop" >"$STAGE/usr/share/applications/$APP_ID.desktop.tmp"
  mv "$STAGE/usr/share/applications/$APP_ID.desktop.tmp" "$STAGE/usr/share/applications/$APP_ID.desktop"
fi
install -Dm644 "$HERE/$APP_ID.mime.xml" "$STAGE/usr/share/mime/packages/$APP_ID.xml"
mkdir -p "$STAGE/usr/share/metainfo"
sed -e "s/@VERSION@/$VERSION/g" -e "s/@DATE@/$LIGHTCRAFT_BUILD_DATE/g" \
  "$HERE/$APP_ID.metainfo.xml.in" >"$STAGE/usr/share/metainfo/$APP_ID.metainfo.xml"
mkdir -p "$STAGE/usr/share/icons"
cp -R "$ROOT/assets/app-icon/hicolor" "$STAGE/usr/share/icons/"
mkdir -p "$STAGE/usr/share/doc/lightcraft"
copy_docs "$STAGE/usr/share/doc/lightcraft"

if command -v desktop-file-validate >/dev/null; then
  desktop-file-validate "$STAGE/usr/share/applications/$APP_ID.desktop"
fi
if command -v appstreamcli >/dev/null; then
  appstreamcli validate --no-net --explain "$STAGE/usr/share/metainfo/$APP_ID.metainfo.xml"
fi

has() {
  local format="$1"
  case " $FORMATS " in *" $format "*) return 0 ;; *) return 1 ;; esac
}

# ---- the Nightly FHS layout (the plain tar and the deb/rpm share it) -----------------------------
# The real binaries move to usr/lib/lightcraft-nightly and usr/bin holds launchers: the GUI starts
# through its launch profile, the headless CLI takes the same profile through its two host
# variables. One layout, so an unpacked tar, a .deb and a .rpm all behave the same.
stage_nightly() {
  local stage="$WORK/nightly-root" icon
  export NFPM_NAME="lightcraft-nightly"
  export NFPM_BIN="lightcraft-nightly"
  export NFPM_CLI="lightcraft-nightly-cli"
  NIGHTLY_DESK_ID="$APP_ID.nightly-v1"
  rm -rf "$stage"
  cp -R "$STAGE" "$stage"
  install -Dm755 "$stage/usr/bin/lightcraft" "$stage/usr/lib/lightcraft-nightly/lightcraft"
  install -Dm755 "$stage/usr/bin/lightcraft-cli" "$stage/usr/lib/lightcraft-nightly/lightcraft-cli"
  # The same launchers the unpacked tar runs (packaging/linux/nightly-*-launch.sh): they resolve the
  # real binary beside themselves, so /usr and an unpacked tar behave alike.
  install -Dm755 "$HERE/nightly-fhs-launch.sh" "$stage/usr/bin/$NFPM_BIN"
  install -Dm755 "$HERE/nightly-fhs-cli-launch.sh" "$stage/usr/bin/$NFPM_CLI"
  rm -f "$stage/usr/bin/lightcraft" "$stage/usr/bin/lightcraft-cli"
  mv "$stage/usr/share/doc/lightcraft" "$stage/usr/share/doc/$NFPM_NAME"
  sed -e "s#^Exec=lightcraft\([[:space:]]\|$\)#Exec=$NFPM_BIN\1#" \
      -e "s|^TryExec=lightcraft$|TryExec=$NFPM_BIN|" \
      -e "s|^Icon=$APP_ID$|Icon=$NIGHTLY_DESK_ID|" \
      -e "s|^StartupWMClass=.*|StartupWMClass=$NIGHTLY_DESK_ID|" \
      -e "s|^Name=LightCraft$|Name=LightCraft Nightly|" \
      -e '/^MimeType=/d' \
      "$stage/usr/share/applications/$APP_ID.desktop" >"$stage/usr/share/applications/$NIGHTLY_DESK_ID.desktop"
  rm -f "$stage/usr/share/applications/$APP_ID.desktop"
  sed -e "s|<id>$APP_ID</id>|<id>$NIGHTLY_DESK_ID</id>|g" \
      -e "s|<name>LightCraft</name>|<name>LightCraft Nightly</name>|g" \
      -e "s|<launchable type=\"desktop-id\">$APP_ID.desktop</launchable>|<launchable type=\"desktop-id\">$NIGHTLY_DESK_ID.desktop</launchable>|g" \
      -e "s|<icon type=\"stock\">$APP_ID</icon>|<icon type=\"stock\">$NIGHTLY_DESK_ID</icon>|g" \
      -e "s|<binary>lightcraft</binary>|<binary>$NFPM_BIN</binary>|g" \
      "$stage/usr/share/metainfo/$APP_ID.metainfo.xml" >"$stage/usr/share/metainfo/$NIGHTLY_DESK_ID.metainfo.xml"
  rm -f "$stage/usr/share/metainfo/$APP_ID.metainfo.xml"
  rm -rf "$stage/usr/share/mime" "$stage/usr/share/icons/hicolor"
  icon="$ROOT/assets/app-icon/lightcraft-nightly.png"
  [[ -f "$icon" ]] || icon="$ROOT/assets/app-icon/hicolor/256x256/apps/$APP_ID.png"
  install -Dm644 "$icon" "$stage/usr/share/icons/hicolor/256x256/apps/$NIGHTLY_DESK_ID.png"
  NIGHTLY_STAGE="$stage"
}
NIGHTLY_STAGE=""
[[ "$IS_NIGHTLY" = 1 ]] && stage_nightly

# ---- .tar.gz ------------------------------------------------------------------------------------
if has tar; then
  TAR_SRC="${NIGHTLY_STAGE:-$STAGE}"
  mkdir -p "$WORK/tar"
  cp -R "$TAR_SRC/usr" "$WORK/tar/$BASENAME"
  tar -C "$WORK/tar" -czf "$DIST/$BASENAME.tar.gz" "$BASENAME"
  echo "wrote $DIST/$BASENAME.tar.gz"
fi

# The Arch handoff keeps the same basename as the plain tarball in a separate directory. It builds
# its own launchers from the real binaries, so it takes the stable-layout tree, not the Nightly
# usr/bin wrappers; the Nightly desktop, metainfo and icon are added beside them.
if [[ -n "$ARCH_TAR_DIR" ]]; then
  mkdir -p "$ARCH_TAR_DIR"
  if [[ "$(cd "$ARCH_TAR_DIR" && pwd -P)" == "$(cd "$DIST" && pwd -P)" ]]; then
    echo "error: the Arch tar directory must differ from the release directory" >&2
    exit 2
  fi
  bash "$HERE/nightly-arch-tar.sh" "$STAGE/usr" "$WORK/arch-tar" "$ARCH_TAR_DIR" "$BASENAME"
fi

# ---- .deb / .rpm --------------------------------------------------------------------------------
if has deb || has rpm; then
  command -v nfpm >/dev/null || { echo "error: nfpm not found (https://nfpm.goreleaser.com/install/)" >&2; exit 1; }
  export VERSION
  export NFPM_ARCH="$DEB_ARCH"
  # stage_nightly already exported the Nightly names: re-exporting the stable ones here pointed
  # the rendered nfpm.yaml at files the Nightly stage has removed (and named the package
  # `lightcraft`, colliding with stable).
  if [[ -z "$NIGHTLY_STAGE" ]]; then
    export NFPM_NAME="lightcraft"
    export NFPM_BIN="lightcraft"
    export NFPM_CLI="lightcraft-cli"
  fi
  NFPM_STAGE="$STAGE"
  [[ -n "$NIGHTLY_STAGE" ]] && NFPM_STAGE="$NIGHTLY_STAGE"
  # nfpm expands env vars in fields like `version` and `arch`, but not in `contents[].src`.
  DROP_LIB_TREE=
  # shellcheck disable=SC2016
  [[ -d "$NFPM_STAGE/usr/lib" ]] || DROP_LIB_TREE='\|^  - src: \${STAGE}/usr/lib/|,+2d;'
  # shellcheck disable=SC2016
  sed "${DROP_LIB_TREE}s|\${STAGE}|$NFPM_STAGE|g;s|\${NFPM_BIN}|$NFPM_BIN|g;s|\${NFPM_CLI}|$NFPM_CLI|g" "$HERE/nfpm.yaml" >"$WORK/nfpm.yaml"
  for fmt in deb rpm; do
    if has "$fmt"; then (cd "$ROOT" && nfpm package -f "$WORK/nfpm.yaml" -p "$fmt" -t "$DIST/$BASENAME.$fmt"); fi
  done
fi

# ---- AppImage -----------------------------------------------------------------------------------
if has appimage; then
  APPIMAGETOOL_VERSION=1.9.1
  case "$ARCH" in
    x86_64) APPIMAGETOOL_SHA256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0 ;;
    aarch64) APPIMAGETOOL_SHA256=f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158 ;;
    *) APPIMAGETOOL_SHA256='' ;;
  esac
  # The runtime appimagetool embeds in every AppImage; unpinned, it would fetch type2-runtime's
  # moving `continuous` build and embed it unchecked.
  RUNTIME_VERSION=20251108
  case "$ARCH" in
    x86_64) RUNTIME_SHA256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d ;;
    aarch64) RUNTIME_SHA256=00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444 ;;
    *) RUNTIME_SHA256='' ;;
  esac
  APPDIR="$WORK/LightCraft.AppDir"
  cp -R "$STAGE" "$APPDIR"
  if [[ "$IS_NIGHTLY" = 1 ]]; then
    APPIMAGE_ID="$APP_ID.nightly-v1"
    cat >"$APPDIR/AppRun" <<'APPRUN'
#!/bin/sh
set -eu
HERE="$(dirname "$(readlink -f "$0")")"
# A real script (not the stable symlink), so an AppImage launch selects the Nightly profile even
# when the desktop entry's Exec is bypassed; APPIMAGE/APPDIR stay untouched for the app.
case "${1:-}" in
  --launch-profile | --profile-root | --version | -V | --help | -h) exec "$HERE/usr/bin/lightcraft" "$@" ;;
esac
profile="${XDG_DATA_HOME:-${HOME:?HOME is required}/.local/share}/lightcraft-nightly"
exec "$HERE/usr/bin/lightcraft" --launch-profile nightly-v1 --profile-root "$profile" -- "$@"
APPRUN
    chmod 755 "$APPDIR/AppRun"
    sed -e "s|^Icon=$APP_ID$|Icon=$APPIMAGE_ID|" \
        -e "s|^Name=LightCraft$|Name=LightCraft Nightly|" \
        -e '/^MimeType=/d' \
        "$APPDIR/usr/share/applications/$APP_ID.desktop" >"$APPDIR/usr/share/applications/$APPIMAGE_ID.desktop"
    rm -f "$APPDIR/usr/share/applications/$APP_ID.desktop"
    cp "$APPDIR/usr/share/applications/$APPIMAGE_ID.desktop" "$APPDIR/$APPIMAGE_ID.desktop"
    rm -rf "$APPDIR/usr/share/icons/hicolor"
    NIGHTLY_ICON="$ROOT/assets/app-icon/lightcraft-nightly.png"
    [[ -f "$NIGHTLY_ICON" ]] || NIGHTLY_ICON="$ROOT/assets/app-icon/hicolor/256x256/apps/$APP_ID.png"
    install -Dm644 "$NIGHTLY_ICON" \
      "$APPDIR/usr/share/icons/hicolor/256x256/apps/$APPIMAGE_ID.png"
    cp "$NIGHTLY_ICON" "$APPDIR/$APPIMAGE_ID.png"
    ln -s "$APPIMAGE_ID.png" "$APPDIR/.DirIcon"
  else
    ln -s usr/bin/lightcraft "$APPDIR/AppRun"
    cp "$HERE/$APP_ID.desktop" "$APPDIR/$APP_ID.desktop"
    cp "$ROOT/assets/app-icon/hicolor/256x256/apps/$APP_ID.png" "$APPDIR/$APP_ID.png"
    ln -s "$APP_ID.png" "$APPDIR/.DirIcon"
  fi

  TOOL="${APPIMAGETOOL:-$(command -v appimagetool || true)}"
  if [[ -z "$TOOL" ]]; then
    if [[ -z "$APPIMAGETOOL_SHA256" ]]; then
      echo "no pinned appimagetool for $ARCH; set APPIMAGETOOL or put appimagetool on PATH" >&2
      exit 2
    fi
    TOOL="$CARGO_TARGET_DIR/appimagetool-$APPIMAGETOOL_VERSION-$ARCH.AppImage"
    if [[ ! -x "$TOOL" ]]; then
      download="$TOOL.download"
      curl --proto '=https' -fsSL -o "$download" \
        "https://github.com/AppImage/appimagetool/releases/download/$APPIMAGETOOL_VERSION/appimagetool-$ARCH.AppImage"
      printf '%s  %s\n' "$APPIMAGETOOL_SHA256" "$download" | sha256sum -c -
      chmod 755 "$download"
      mv "$download" "$TOOL"
    fi
    printf '%s  %s\n' "$APPIMAGETOOL_SHA256" "$TOOL" | sha256sum -c -
  fi
  if [[ -z "$RUNTIME_SHA256" ]]; then
    echo "no pinned AppImage runtime for $ARCH" >&2
    exit 2
  fi
  RUNTIME="$CARGO_TARGET_DIR/appimage-runtime-$RUNTIME_VERSION-$ARCH"
  if [[ ! -f "$RUNTIME" ]]; then
    download="$RUNTIME.download"
    curl --proto '=https' -fsSL -o "$download" \
      "https://github.com/AppImage/type2-runtime/releases/download/$RUNTIME_VERSION/runtime-$ARCH"
    printf '%s  %s\n' "$RUNTIME_SHA256" "$download" | sha256sum -c -
    mv "$download" "$RUNTIME"
  fi
  printf '%s  %s\n' "$RUNTIME_SHA256" "$RUNTIME" | sha256sum -c -
  # Absolute, because appimagetool runs in $DIST below (CARGO_TARGET_DIR or APPIMAGETOOL may be
  # relative).
  OUT="$(cd "$DIST" && pwd)/$BASENAME.AppImage"
  APPDIR="$(cd "$APPDIR" && pwd)"
  TOOL="$(cd "$(dirname "$TOOL")" && pwd)/$(basename "$TOOL")"
  RUNTIME="$(cd "$(dirname "$RUNTIME")" && pwd)/$(basename "$RUNTIME")"
  # A .zsync left by an earlier run would hide a missing zsyncmake and describe another file.
  rm -f "$OUT.zsync"
  # Update information: AppImageUpdate, AppImageLauncher and the like read it from the file and
  # fetch only the blocks that changed in a newer release, through the .zsync published next to
  # each AppImage on GitHub Releases. `latest` is the newest published release that is not a
  # pre-release. A fork's builds point at its own releases through GITHUB_REPOSITORY.
  REPO="${GITHUB_REPOSITORY:-storytold/lightcraft}"
  UPDATE_ARGS=()
  if [[ "$IS_NIGHTLY" = 0 ]]; then
    UPDATE_INFO="gh-releases-zsync|${REPO%%/*}|${REPO#*/}|latest|lightcraft-*-linux-$ARCH.AppImage.zsync"
    UPDATE_ARGS=(-u "$UPDATE_INFO")
  else
    # Nightly releases are GitHub pre-releases (`latest` only selects stable releases), so point
    # zsync update metadata at pre-release tags matching `-nightly` rather than stable `latest`.
    UPDATE_INFO="gh-releases-zsync|${REPO%%/*}|${REPO#*/}|latest-pre|lightcraft-*-nightly*-linux-$ARCH.AppImage.zsync"
    UPDATE_ARGS=(-u "$UPDATE_INFO")
  fi
  # Extract-and-run: works without FUSE (containers, CI). The output embeds the static runtime,
  # so users don't need libfuse2 either. With zsyncmake on the host (CI installs the zsync
  # package) appimagetool also writes the .zsync, into its working directory, hence the cd.
  (cd "$DIST" && ARCH="$ARCH" APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --no-appstream --runtime-file "$RUNTIME" "${UPDATE_ARGS[@]}" "$APPDIR" "$OUT")
  echo "wrote $OUT"
  if [[ -s "$OUT.zsync" ]]; then
    echo "wrote $OUT.zsync"
  else
    warn "zsyncmake not found, so $OUT.zsync was not written; AppImage delta updates need it"
  fi
fi

if [[ -z "${CROSS_TARGET:-}" ]]; then
  "$STAGE/usr/bin/lightcraft-cli" --version
elif [[ -n "${EMULATOR:-}" ]]; then
  "$EMULATOR" "$STAGE/usr/bin/lightcraft-cli" --version
fi
echo "==> done"
ls -lh "$DIST"
