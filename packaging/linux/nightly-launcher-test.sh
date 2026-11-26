#!/usr/bin/env bash
# Validate the Nightly launchers without building or opening LightCraft: the unpacked tar's
# `lightcraft` (craft-common-actions' rendered template, GUI in `lightcraft-bin`) and the
# deb/rpm/FHS `lightcraft-nightly` (GUI in lib/lightcraft-nightly). A recorder stands in for both
# binaries and prints its argv and env.
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

# ---- the unpacked tar: the shared template as lightcraft (GUI in lightcraft-bin) ----------------
tar_dir="$work/tar"
mkdir -p "$tar_dir"
cp "$work/recorder" "$tar_dir/lightcraft-bin"
install -m755 "$HERE/nightly-tar-launch.sh" "$tar_dir/lightcraft"
identity="ai.storyteller.lightcraft.nightly"
profile="$HOME/.local/share/$identity"

# The shared template, rendered for LightCraft: a hand-unpacked tar opens the channel's own profile
# whatever the working directory is, and every argument follows the profile tokens.
cd "$work"
expect "$(printf 'CONFIG=<<unset>>\nLIBRARY=<<unset>>\n<--launch-profile>\n<nightly-v1>\n<--profile-root>\n<%s>\n<-->\n<photo with spaces.dng>\n<--other-name.dng>\n' "$profile")" \
  "$("$tar_dir/lightcraft" 'photo with spaces.dng' '--other-name.dng')"
export XDG_DATA_HOME="$work/xdg with space"
profile="$XDG_DATA_HOME/$identity"
expect "$(printf 'CONFIG=<<unset>>\nLIBRARY=<<unset>>\n<--launch-profile>\n<nightly-v1>\n<--profile-root>\n<%s>\n<-->\n<sample.dng>\n' "$profile")" \
  "$("$tar_dir/lightcraft" sample.dng)"
unset XDG_DATA_HOME
profile="$HOME/.local/share/$identity"

# A version query is asked in the Launcher's own form, so the binary answers before any profile.
expect "$(printf 'CONFIG=<<unset>>\nLIBRARY=<<unset>>\n<--launch-profile>\n<nightly-v1>\n<--profile-root>\n<%s>\n<--version>\n' "$profile")" \
  "$("$tar_dir/lightcraft" --version)"
expect "$(printf 'CONFIG=<<unset>>\nLIBRARY=<<unset>>\n<--launch-profile>\n<nightly-v1>\n<--profile-root>\n<%s>\n<--version>\n' "$profile")" \
  "$("$tar_dir/lightcraft" -V)"

# The ArtCraft Launcher names its own profile and per-install root: the pair goes to the GUI unchanged.
expect "$(printf 'CONFIG=<<unset>>\nLIBRARY=<<unset>>\n<--launch-profile>\n<nightly-v1>\n<--profile-root>\n<%s>\n<-->\n<photo.dng>\n' "$work/launcher profile with space")" \
  "$("$tar_dir/lightcraft" --launch-profile nightly-v1 --profile-root "$work/launcher profile with space" -- photo.dng)"

# The bytes are the shared template rendered for this app, not a local script.
expected="dbb1dff470eaff5ecfe1cf03cc43459d4d8796a0e8875fc0bea9563e30deba59"
actual="$(sha256sum "$HERE/nightly-tar-launch.sh" | cut -d' ' -f1)"
[[ "$actual" == "$expected" ]] || { printf 'nightly-tar-launch.sh is %s, not the rendered template %s\n' "$actual" "$expected" >&2; exit 1; }

# ---- the deb/rpm and the unpacked FHS tar: bin/lightcraft-nightly + lib/lightcraft-nightly -------
fhs="$work/fhs"
mkdir -p "$fhs/bin" "$fhs/lib/lightcraft-nightly"
cp "$work/recorder" "$fhs/lib/lightcraft-nightly/lightcraft"
cp "$work/recorder" "$fhs/lib/lightcraft-nightly/lightcraft-cli"
install -m755 "$HERE/nightly-fhs-launch.sh" "$fhs/bin/lightcraft-nightly"
install -m755 "$HERE/nightly-fhs-cli-launch.sh" "$fhs/bin/lightcraft-nightly-cli"
# The distro formats keep their own profile folder, so they are not the tar's identity.
fhs_profile="$HOME/.local/share/lightcraft-nightly"

# The real binary is resolved beside the launcher, so an unpacked tree works anywhere.
expect "$(printf 'CONFIG=<<unset>>\nLIBRARY=<<unset>>\n<--launch-profile>\n<nightly-v1>\n<--profile-root>\n<%s>\n<-->\n<photo with spaces.dng>\n' "$fhs_profile")" \
  "$("$fhs/bin/lightcraft-nightly" 'photo with spaces.dng')"
expect "$(printf 'CONFIG=<<unset>>\nLIBRARY=<<unset>>\n<--launch-profile>\n<nightly-v1>\n<--profile-root>\n</launcher/profiles/lightcraft>\n<--version>\n')" \
  "$("$fhs/bin/lightcraft-nightly" --launch-profile nightly-v1 --profile-root /launcher/profiles/lightcraft --version)"
expect "$(printf 'CONFIG=<<unset>>\nLIBRARY=<<unset>>\n<--version>\n')" "$("$fhs/bin/lightcraft-nightly" --version)"

# The headless CLI takes the same profile through its two host variables; explicit values win.
expect "$(printf 'CONFIG=<%s>\nLIBRARY=<%s>\n<calibrate>\n' "$fhs_profile/Settings" "$fhs_profile/Library")" \
  "$("$fhs/bin/lightcraft-nightly-cli" calibrate)"
expect "$(printf 'CONFIG=<%s>\nLIBRARY=<%s>\n<calibrate>\n' "$work/cli settings" "$work/cli library")" \
  "$(LIGHTCRAFT_CONFIG_DIR="$work/cli settings" LIGHTCRAFT_LIBRARY="$work/cli library" "$fhs/bin/lightcraft-nightly-cli" calibrate)"

# Neither launcher writes anything by itself (no folder, no file).
test ! -e "$profile"
test ! -e "$fhs_profile"
test ! -e "$HOME/Pictures"

echo "PASS: the Nightly tar launcher is the shared template and the FHS launchers isolate the profile"
