#!/bin/sh
set -eu

real_bin="$(dirname "$0")/.lightcraft-nightly/lightcraft"
if [ "${1:-}" = "--version" ] || [ "${1:-}" = "-V" ] || [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
  exec "$real_bin" "$@"
fi
if [ -z "${LIGHTCRAFT_CONFIG_DIR+x}" ]; then
  profile_base="${XDG_DATA_HOME:-${HOME:?HOME is required}/.local/share}"
  export LIGHTCRAFT_CONFIG_DIR="$profile_base/lightcraft-nightly"
fi
exec "$real_bin" "$@"
