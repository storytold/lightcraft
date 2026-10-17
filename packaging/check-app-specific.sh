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
