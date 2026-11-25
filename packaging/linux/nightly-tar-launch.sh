#!/bin/sh
# LightCraft Nightly as unpacked from its tar: start the GUI in the Nightly profile, so a download
# unzipped by hand is as isolated as the AppImage, the deb/rpm and the Flatpak.
#
#   ./lightcraft [files…]        opens the Nightly library under $XDG_DATA_HOME/lightcraft-nightly
#   ./lightcraft-bin [files…]    the binary itself (stable's settings folder unless given a profile)
set -eu
self="$(readlink -f "$0")"
here="$(dirname "$self")"
# Never fall back to a relative path: an unresolvable $0 is a broken install, not a guess.
[ -n "$self" ] && [ "${here#/}" != "$here" ] || { echo "error: cannot resolve $0" >&2; exit 2; }
case "${1:-}" in
  # The Launcher's own profile form is passed on untouched (its profile root wins over this
  # default), and the queries are answered without any profile.
  --launch-profile | --profile-root | --version | -V | --help | -h) exec "$here/lightcraft-bin" "$@" ;;
esac
profile="${XDG_DATA_HOME:-${HOME:?HOME is required}/.local/share}/lightcraft-nightly"
exec "$here/lightcraft-bin" --launch-profile nightly-v1 --profile-root "$profile" -- "$@"
