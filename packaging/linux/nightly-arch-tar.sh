#!/usr/bin/env bash
# Stage the LightCraft Nightly-only handoff for the Arch packager.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
source_tree="$1"
work="$2"
destination="$3"
basename="$4"
stage="$work/$basename"

mkdir -p "$work"
cp -R "$source_tree" "$stage"
mkdir "$stage/bin/.lightcraft-nightly"
mv "$stage/bin/lightcraft" "$stage/bin/.lightcraft-nightly/lightcraft"
mv "$stage/bin/lightcraft-cli" "$stage/bin/.lightcraft-nightly/lightcraft-cli"
cp "$here/nightly-arch-launch.sh" "$stage/bin/lightcraft"
cp "$here/nightly-arch-launch.sh" "$stage/bin/lightcraft-cli"
chmod 755 "$stage/bin/lightcraft" "$stage/bin/lightcraft-cli"

# The packager installs this tree as it is, so the Nightly desktop entry, metainfo and icon are
# named for the Nightly channel here: leaving the stable ids would make them rename three files
# and would collide with a stable install's menu entry.
app_id=ai.storyteller.lightcraft
nightly_id="$app_id.nightly-v1"
if [ -f "$stage/share/applications/$app_id.desktop" ]; then
  # The packager renames the launchers to lightcraft-nightly(-cli) when it installs this tree, so the
  # menu entry must name the installed command: `Exec=lightcraft` would start stable beside it.
  sed -e 's#^Exec=lightcraft\([[:space:]]\|$\)#Exec=lightcraft-nightly\1#' \
      -e 's#^TryExec=lightcraft$#TryExec=lightcraft-nightly#' \
      -e "s|^Icon=$app_id$|Icon=$nightly_id|" \
      -e "s|^StartupWMClass=.*|StartupWMClass=$nightly_id|" \
      -e "s|^Name=LightCraft$|Name=LightCraft Nightly|" \
      -e '/^MimeType=/d' \
      "$stage/share/applications/$app_id.desktop" >"$stage/share/applications/$nightly_id.desktop"
  rm -f "$stage/share/applications/$app_id.desktop"
fi
if [ -f "$stage/share/metainfo/$app_id.metainfo.xml" ]; then
  sed -e "s|<id>$app_id</id>|<id>$nightly_id</id>|g" \
      -e "s|<name>LightCraft</name>|<name>LightCraft Nightly</name>|g" \
      -e "s|<launchable type=\"desktop-id\">$app_id.desktop</launchable>|<launchable type=\"desktop-id\">$nightly_id.desktop</launchable>|g" \
      -e "s|<icon type=\"stock\">$app_id</icon>|<icon type=\"stock\">$nightly_id</icon>|g" \
      -e "s|<binary>lightcraft</binary>|<binary>lightcraft-nightly</binary>|g" \
      "$stage/share/metainfo/$app_id.metainfo.xml" >"$stage/share/metainfo/$nightly_id.metainfo.xml"
  rm -f "$stage/share/metainfo/$app_id.metainfo.xml"
fi
if [ -d "$stage/share/icons/hicolor" ]; then
  nightly_icon="$here/../../assets/app-icon/lightcraft-nightly.png"
  if [ -f "$nightly_icon" ]; then
    rm -rf "$stage/share/icons/hicolor"
    install -Dm644 "$nightly_icon" "$stage/share/icons/hicolor/256x256/apps/$nightly_id.png"
  fi
fi
tar -C "$work" -czf "$destination/$basename.tar.gz" "$basename"
echo "wrote $destination/$basename.tar.gz"
