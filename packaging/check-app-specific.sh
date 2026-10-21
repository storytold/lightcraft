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

# Winget release workflow (issue #487): the generated manifests and the PR against
# winget-pkgs only exist on GitHub, so assert the workflow's contract instead.
python3 - <<'PYEOF'
import re
import yaml

with open(".github/workflows/winget.yml", encoding="utf-8") as source:
    text = source.read()
wf = yaml.safe_load(text)
trigger = wf.get("on") or wf.get(True)  # PyYAML parses `on:` as the boolean True
assert trigger["release"]["types"] == ["released"], \
    "winget must publish stable releases only (no prereleases, no drafts)"
step = wf["jobs"]["winget"]["steps"][-1]
uses = step["uses"]
# Third-party action: pinned by commit, never a moving tag.
assert re.fullmatch(r"vedantmgoyal9/winget-releaser@[0-9a-f]{40}", uses), uses
assert re.search(r"winget-releaser@[0-9a-f]{40} # v\d+$", text, re.M), "pin comment"
assert step["with"]["identifier"] == "ArtCraft.LightCraft", step["with"]
assert step["with"]["token"] == "${{ secrets.WINGET_TOKEN }}", step["with"]
# Only the Windows MSI installers are winget installers (no portable zips, DMGs, ...).
assert step["with"]["installers-regex"] == r"\.msi$", step["with"]
print("winget workflow ok:", uses)
PYEOF
