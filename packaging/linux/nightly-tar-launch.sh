#!/bin/sh
# LightCraft Nightly as unpacked from its tar: start the GUI in the Nightly profile, so a download
# unzipped by hand is as isolated as the AppImage, the deb/rpm and the Flatpak.
#
#   ./lightcraft [files…]        opens the Nightly library under $XDG_DATA_HOME/lightcraft-nightly
#   ./lightcraft-bin [files…]    the binary itself (stable's settings folder unless given a profile)
set -eu
here="$(dirname "$0")"
case "${1:-}" in
  --version | -V | --help | -h) exec "$here/lightcraft-bin" "$@" ;;
esac
profile="${XDG_DATA_HOME:-${HOME:?HOME is required}/.local/share}/lightcraft-nightly"
exec "$here/lightcraft-bin" --launch-profile nightly-v1 --profile-root "$profile" -- "$@"
