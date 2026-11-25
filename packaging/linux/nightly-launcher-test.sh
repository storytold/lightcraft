#!/usr/bin/env bash
# Validate the Nightly launchers without building or opening LightCraft: the unpacked tar's
# `lightcraft` (GUI in `lightcraft-bin`) and the deb/rpm/FHS `lightcraft-nightly` (GUI in
# lib/lightcraft-nightly). A recorder stands in for both binaries and prints its argv and env.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cat >"$work/recorder" <<'RECORDER'
#!/bin/sh
printf 'CONFIG=<%s>\n' "${LIGHTCRAFT_CONFIG_DIR-<unset>}"
printf 'LIBRARY=<%s>\n' "${LIGHTCRAFT_LIBRARY-<unset>}"
for arg in "$@"; do
  printf '<%s>\n' "$arg"
done
RECORDER
chmod 755 "$work/recorder"

expect() {
  local expected="$1" actual="$2"
  if [[ "$actual" != "$expected" ]]; then
    printf 'expected:\n%s\nactual:\n%s\n' "$expected" "$actual" >&2
    exit 1
  fi
}

unset XDG_DATA_HOME LIGHTCRAFT_CONFIG_DIR LIGHTCRAFT_LIBRARY
export HOME="$work/home with space"

# ---- the unpacked tar: lightcraft (launcher) + lightcraft-bin (the GUI) --------------------------
tar_dir="$work/tar"
mkdir -p "$tar_dir"
cp "$work/recorder" "$tar_dir/lightcraft-bin"
install -m755 "$HERE/nightly-tar-launch.sh" "$tar_dir/lightcraft"
profile="$HOME/.local/share/lightcraft-nightly"

# A hand-unpacked tar opens its own Nightly profile, whatever the working directory is.
cd "$work"
expect "$(printf 'CONFIG=<<unset>>\nLIBRARY=<<unset>>\n<--launch-profile>\n<nightly-v1>\n<--profile-root>\n<%s>\n<-->\n<photo with spaces.dng>\n<--other-name.dng>\n' "$profile")" \
  "$("$tar_dir/lightcraft" 'photo with spaces.dng' '--other-name.dng')"
export XDG_DATA_HOME="$work/xdg with space"
profile="$XDG_DATA_HOME/lightcraft-nightly"
expect "$(printf 'CONFIG=<<unset>>\nLIBRARY=<<unset>>\n<--launch-profile>\n<nightly-v1>\n<--profile-root>\n<%s>\n<-->\n<sample.dng>\n' "$profile")" \
  "$("$tar_dir/lightcraft" sample.dng)"
unset XDG_DATA_HOME
profile="$HOME/.local/share/lightcraft-nightly"

# The Launcher's own form arrives already shaped: its profile root wins and nothing is added.
expect "$(printf 'CONFIG=<<unset>>\nLIBRARY=<<unset>>\n<--launch-profile>\n<nightly-v1>\n<--profile-root>\n</launcher/profiles/lightcraft>\n<--version>\n')" \
  "$("$tar_dir/lightcraft" --launch-profile nightly-v1 --profile-root /launcher/profiles/lightcraft --version)"
expect "$(printf 'CONFIG=<<unset>>\nLIBRARY=<<unset>>\n<--profile-root>\n<%s>\n<-->\n<photo.dng>\n' "$work/explicit root")" \
  "$("$tar_dir/lightcraft" --profile-root "$work/explicit root" -- photo.dng)"
# The version query is answered by the binary itself, with no profile tokens.
expect "$(printf 'CONFIG=<<unset>>\nLIBRARY=<<unset>>\n<--version>\n')" "$("$tar_dir/lightcraft" --version)"

# ---- the deb/rpm and the unpacked FHS tar: bin/lightcraft-nightly + lib/lightcraft-nightly -------
fhs="$work/fhs"
mkdir -p "$fhs/bin" "$fhs/lib/lightcraft-nightly"
cp "$work/recorder" "$fhs/lib/lightcraft-nightly/lightcraft"
cp "$work/recorder" "$fhs/lib/lightcraft-nightly/lightcraft-cli"
install -m755 "$HERE/nightly-fhs-launch.sh" "$fhs/bin/lightcraft-nightly"
install -m755 "$HERE/nightly-fhs-cli-launch.sh" "$fhs/bin/lightcraft-nightly-cli"

# The real binary is resolved beside the launcher, so an unpacked tree works anywhere.
expect "$(printf 'CONFIG=<<unset>>\nLIBRARY=<<unset>>\n<--launch-profile>\n<nightly-v1>\n<--profile-root>\n<%s>\n<-->\n<photo with spaces.dng>\n' "$profile")" \
  "$("$fhs/bin/lightcraft-nightly" 'photo with spaces.dng')"
expect "$(printf 'CONFIG=<<unset>>\nLIBRARY=<<unset>>\n<--launch-profile>\n<nightly-v1>\n<--profile-root>\n</launcher/profiles/lightcraft>\n<--version>\n')" \
  "$("$fhs/bin/lightcraft-nightly" --launch-profile nightly-v1 --profile-root /launcher/profiles/lightcraft --version)"
expect "$(printf 'CONFIG=<<unset>>\nLIBRARY=<<unset>>\n<--version>\n')" "$("$fhs/bin/lightcraft-nightly" --version)"

# The headless CLI takes the same profile through its two host variables; explicit values win.
expect "$(printf 'CONFIG=<%s>\nLIBRARY=<%s>\n<calibrate>\n' "$profile/Settings" "$profile/Library")" \
  "$("$fhs/bin/lightcraft-nightly-cli" calibrate)"
expect "$(printf 'CONFIG=<%s>\nLIBRARY=<%s>\n<calibrate>\n' "$work/cli settings" "$work/cli library")" \
  "$(LIGHTCRAFT_CONFIG_DIR="$work/cli settings" LIGHTCRAFT_LIBRARY="$work/cli library" "$fhs/bin/lightcraft-nightly-cli" calibrate)"

# Neither launcher writes anything by itself (no folder, no file).
test ! -e "$profile"
test ! -e "$HOME/Pictures"

echo "PASS: the Nightly tar and FHS launchers isolate the profile and pass the Launcher protocol through"
