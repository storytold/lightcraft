#!/usr/bin/env bash
# App-specific installer and package assertions retained alongside shared packaging lint.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

for want in '<UIRef Id="LightcraftUI" />' 'Value="InstallDirDlg"' '<Property Id="WIXUI_INSTALLDIR" Value="INSTALLFOLDER" />' \
    'Key="Software\LightCraft" Name="InstallFolder"' 'Value="LaunchLightcraft"'; do
  grep -qF "$want" packaging/windows/lightcraft.wxs || { echo "::error::lightcraft.wxs lost: $want"; exit 1; }
done
grep -qF '<DialogRef Id="ExitDialog" />' packaging/windows/lightcraft.wxs \
  || { echo "::error::ExitDialog is not referenced; installs and removes end without a completion dialog"; exit 1; }

python3 - <<'PYEOF'
import yaml
manifests = []
for name in ("ai.storyteller.lightcraft.yml", "ai.storyteller.lightcraft.bundle.yml"):
    with open("packaging/linux/flatpak/" + name) as source:
        manifest = yaml.safe_load(source)
    assert manifest["id"] == "ai.storyteller.lightcraft" and manifest["command"] == "lightcraft", name
    manifests.append(manifest)
for key in ("runtime", "runtime-version", "sdk", "finish-args"):
    assert manifests[0][key] == manifests[1][key], f"Flatpak manifests disagree on {key}"
print("LightCraft package identity ok")
PYEOF

# The macOS bundle must declare only file types LightCraft can open. It used to carry PhotoCraft's
# `.pcraft` document type and imported PSB/QOI declarations, so Launch Services typed PhotoCraft
# documents as LightCraft's and `.pcraft` had three claimant apps (issue #1951). plistlib parses the
# template as a property list too, which is the syntax check package.sh's `plutil -lint` repeats on
# the substituted file.
python3 - <<'PYEOF'
import plistlib
with open("packaging/macos/Info.plist.in", "rb") as source:
    plist = plistlib.load(source)
for key, value in {
    "CFBundleIdentifier": "ai.storyteller.lightcraft",
    "CFBundleExecutable": "LightCraft",
    "CFBundleIconFile": "LightCraft",
    "LSMinimumSystemVersion": "11.0",
}.items():
    assert plist.get(key) == value, f"Info.plist {key}: {plist.get(key)!r}, expected {value!r}"
for key in ("CFBundleShortVersionString", "CFBundleVersion", "LightCraftVersion", "LightCraftBuildCommit"):
    assert plist.get(key), f"Info.plist lost {key}"
declared = set()
for section in ("UTExportedTypeDeclarations", "UTImportedTypeDeclarations"):
    for entry in plist.get(section, []):
        tags = entry.get("UTTypeTagSpecification", {})
        declared.update(tags.get("public.filename-extension", []))
        declared.update(tags.get("public.mime-type", []))
        declared.add(entry.get("UTTypeIdentifier", ""))
unsupported = {name.lower() for name in declared} & {
    "pcraft",
    "psb",
    "qoi",
    "application/x-lightcraft",
    "ai.storyteller.lightcraft.document",
    "ai.storyteller.lightcraft.qoi",
    "com.adobe.photoshop-large-image",
}
assert not unsupported, f"Info.plist declares types LightCraft cannot open: {sorted(unsupported)}"
print("LightCraft macOS bundle types ok")
PYEOF

# FreeBSD aarch64 is cross-compiled in the x86_64 release VM (craft-common-actions#39). With
# CARGO_BUILD_TARGET set, packaging/freebsd/package.sh must take the binaries from
# target/<triple>/release, name the archive after the target, and check the binaries with file(1)
# instead of running them. An amd64 FreeBSD host is faked with a uname shim.
fb="$(mktemp -d)"
trap 'rm -rf "$fb"' EXIT
mkdir -p "$fb/shim" "$fb/target/aarch64-unknown-freebsd/release"
cat >"$fb/shim/uname" <<'SHEOF'
#!/bin/sh
case "$1" in -m) echo amd64 ;; -s) echo FreeBSD ;; *) exec /usr/bin/uname "$@" ;; esac
SHEOF
chmod 755 "$fb/shim/uname"
# Just an ELF header: 64-bit, little-endian, FreeBSD ABI, aarch64, of the given ELF type
# (1 relocatable, 2 executable), written to each path given. A shared library can't be faked
# this way: with no dynamic section, file(1) calls an executable-mode one a PIE.
freebsd_elf() {
  python3 - "$@" <<'PYEOF'
import os, struct, sys
header = b"\x7fELF" + bytes([2, 1, 1, 9]) + bytes(8)
header += struct.pack("<HHIQQQIHHHHHH", int(sys.argv[1]), 183, 1, 0, 0, 0, 0, 64, 0, 0, 0, 0, 0)
for path in sys.argv[2:]:
    with open(path, "wb") as out:
        out.write(header)
    os.chmod(path, 0o755)
PYEOF
}
rel="$fb/target/aarch64-unknown-freebsd/release"
freebsd_elf 2 "$rel/lightcraft" "$rel/lightcraft-cli"
freebsd_package() {
  PATH="$fb/shim:$PATH" CARGO_TARGET_DIR="$fb/target" DIST="$fb/dist" bash packaging/freebsd/package.sh --skip-build
}
log="$(CARGO_BUILD_TARGET=aarch64-unknown-freebsd freebsd_package 2>&1)" \
  || { echo "$log"; echo "::error::FreeBSD package.sh failed to package an aarch64 cross build"; exit 1; }
ls "$fb/dist"/lightcraft-*-freebsd-aarch64.tar.gz >/dev/null \
  || { echo "$log"; echo "::error::FreeBSD package.sh did not name the cross build's archive freebsd-aarch64"; exit 1; }
grep -q 'skipping --version' <<<"$log" \
  || { echo "$log"; echo "::error::FreeBSD package.sh ran an aarch64 binary on an x86_64 host"; exit 1; }
# Refused as a target (exit 2 and the message), not just failing later on missing binaries.
status=0
log="$(CARGO_BUILD_TARGET=aarch64-unknown-linux-gnu freebsd_package 2>&1)" || status=$?
if [ "$status" != 2 ] || ! grep -qF 'is not a supported FreeBSD target' <<<"$log"; then
  echo "$log"; echo "::error::FreeBSD package.sh did not refuse a Linux target (exit $status)"; exit 1
fi
freebsd_elf 1 "$rel/lightcraft-cli"
if CARGO_BUILD_TARGET=aarch64-unknown-freebsd freebsd_package >/dev/null 2>&1; then
  echo "::error::FreeBSD package.sh packaged an ELF object file as an aarch64 executable"; exit 1
fi
printf '#!/bin/sh\n' >"$rel/lightcraft-cli"
if CARGO_BUILD_TARGET=aarch64-unknown-freebsd freebsd_package >/dev/null 2>&1; then
  echo "::error::FreeBSD package.sh packaged a cross build whose binary is not a FreeBSD aarch64 executable"; exit 1
fi
echo "LightCraft FreeBSD aarch64 cross packaging ok"
