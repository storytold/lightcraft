#!/usr/bin/env bash
# Validate the staged Arch launcher without building or opening LightCraft.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
stage="$work/source"
dst="$work/dist"
base=lightcraft-0.0.1-nightly.1-linux-x86_64
mkdir -p "$stage/bin" "$dst" "$work/extract"

# A local argv/env recorder stands in for the compiled binaries. It never opens a UI.
cat >"$stage/bin/lightcraft" <<'RECORDER'
#!/bin/sh
printf 'CONFIG=<%s>\n' "${LIGHTCRAFT_CONFIG_DIR-<unset>}"
for arg in "$@"; do
  printf '<%s>\n' "$arg"
done
RECORDER
chmod 755 "$stage/bin/lightcraft"
cp "$stage/bin/lightcraft" "$stage/bin/lightcraft-cli"

bash "$HERE/nightly-arch-tar.sh" "$stage" "$work/stage" "$dst" "$base"
tar -xzf "$dst/$base.tar.gz" -C "$work/extract"
bin="$work/extract/$base/bin"
# Simulate the existing Arch bin-tree copy and public launcher rename.
mkdir -p "$work/installed"
cp -R "$bin" "$work/installed/bin"
bin="$work/installed/bin"
mv "$bin/lightcraft" "$bin/lightcraft-nightly"
mv "$bin/lightcraft-cli" "$bin/lightcraft-nightly-cli"

test ! -e "$stage/bin/.lightcraft-nightly"
cmp "$stage/bin/lightcraft" "$bin/.lightcraft-nightly/lightcraft"
cmp "$stage/bin/lightcraft-cli" "$bin/lightcraft-nightly-cli"
export HOME="$work/home with space"
unset XDG_DATA_HOME LIGHTCRAFT_CONFIG_DIR
expect() {
  expected="$1"
  shift
  actual=$(PATH="$bin:$PATH" lightcraft-nightly "$@")
  if [[ "$actual" != "$expected" ]]; then
    printf 'expected:\n%s\nactual:\n%s\n' "$expected" "$actual" >&2
    exit 1
  fi
}
expect "CONFIG=<$HOME/.local/share/lightcraft-nightly>"
export XDG_DATA_HOME="$work/xdg with space"
expect "$(printf 'CONFIG=<%s>\n<%s>\n<%s>\n' "$XDG_DATA_HOME/lightcraft-nightly" 'photo with spaces.dng' '--other-name.dng')" \
  'photo with spaces.dng' '--other-name.dng'
expect "$(printf 'CONFIG=<<unset>>\n<--version>\n')" --version
expect "$(printf 'CONFIG=<<unset>>\n<-V>\n')" -V
LIGHTCRAFT_CONFIG_DIR="$work/explicit with space" expect "$(printf 'CONFIG=<%s>\n<%s>\n' "$work/explicit with space" sample.dng)" \
  sample.dng

echo "PASS: Arch Nightly handoff isolates the profile and preserves launch arguments"
