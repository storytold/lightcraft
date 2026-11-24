#!/bin/sh
set -eu

# The Arch packager copies this file over bin/lightcraft (and bin/lightcraft-cli) and moves the
# real binaries into bin/.lightcraft-nightly/. Neither writes outside the Nightly profile.
profile="${XDG_DATA_HOME:-${HOME:?HOME is required}/.local/share}/lightcraft-nightly"
case "$(basename "$0")" in
  *-cli)
    real_bin="$(dirname "$0")/.lightcraft-nightly/lightcraft-cli"
    if [ "${1:-}" = "--version" ] || [ "${1:-}" = "-V" ] || [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
      exec "$real_bin" "$@"
    fi
    # The headless CLI has no Launcher protocol: the same profile through its two host variables.
    if [ -z "${LIGHTCRAFT_CONFIG_DIR+x}" ]; then export LIGHTCRAFT_CONFIG_DIR="$profile/Settings"; fi
    if [ -z "${LIGHTCRAFT_LIBRARY+x}" ]; then export LIGHTCRAFT_LIBRARY="$profile/Library"; fi
    exec "$real_bin" "$@"
    ;;
esac
real_bin="$(dirname "$0")/.lightcraft-nightly/lightcraft"
if [ "${1:-}" = "--version" ] || [ "${1:-}" = "-V" ] || [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
  exec "$real_bin" "$@"
fi
exec "$real_bin" --launch-profile nightly-v1 --profile-root "$profile" -- "$@"
