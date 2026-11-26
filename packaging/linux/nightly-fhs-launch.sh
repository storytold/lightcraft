#!/bin/sh
# The Nightly GUI of a .deb, a .rpm or an unpacked FHS tar (`usr/bin/lightcraft-nightly`). The
# launch profile selects the profile, so settings, models, logs, the library lock and the default
# library all sit under $profile. The real binary is resolved beside this launcher, so the same file
# works installed in /usr and unpacked anywhere.
set -eu
self="$(readlink -f "$0")"
here="$(dirname "$self")"
# Never fall back to a relative path: an unresolvable $0 is a broken install, not a guess.
[ -n "$self" ] && [ "${here#/}" != "$here" ] || { echo "error: cannot resolve $0" >&2; exit 2; }
real="$here/../lib/lightcraft-nightly/lightcraft"
case "${1:-}" in
  # The Launcher's own profile form is passed on untouched, and the queries need no profile.
  --launch-profile | --profile-root | --version | -V | --help | -h) exec "$real" "$@" ;;
esac
profile="${XDG_DATA_HOME:-${HOME:?HOME is required}/.local/share}/lightcraft-nightly"
exec "$real" --launch-profile nightly-v1 --profile-root "$profile" -- "$@"
