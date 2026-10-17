#!/usr/bin/env bash
# Puts the GitHub CLI on the job's PATH when the runner has none (gpu0's containers, possibly
# mac0/win0/SNAPDRAGON). Unpacked into $RUNNER_TEMP; nothing is installed system-wide.
# Used by .github/workflows/release.yml to upload platform files straight to the draft release.
set -euo pipefail
if command -v gh >/dev/null 2>&1; then gh --version | head -1; exit 0; fi
v="${GH_CLI_VERSION:-2.83.0}"
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) pkg="gh_${v}_linux_amd64" ext=tar.gz ;;
  Linux-aarch64) pkg="gh_${v}_linux_arm64" ext=tar.gz ;;
  Darwin-*) pkg="gh_${v}_macOS_$( [ "$(uname -m)" = arm64 ] && echo arm64 || echo amd64 )" ext=zip ;;
  MINGW*-x86_64|MSYS*-x86_64) pkg="gh_${v}_windows_amd64" ext=zip ;;
  MINGW*-*|MSYS*-*) pkg="gh_${v}_windows_arm64" ext=zip ;;
  *) echo "::error::no GitHub CLI build for $(uname -s)-$(uname -m)"; exit 1 ;;
esac
tmp="$RUNNER_TEMP"
# Git Bash on Windows: work with a POSIX path, hand GITHUB_PATH a Windows one
if command -v cygpath >/dev/null 2>&1; then tmp="$(cygpath -u "$tmp")"; fi
dir="$tmp/gh-$v"
mkdir -p "$dir"
url="https://github.com/cli/cli/releases/download/v${v}/${pkg}.${ext}"
if [ "$ext" = tar.gz ]; then
  curl -fsSL "$url" | tar -xz -C "$dir"
else
  curl -fsSL -o "$dir/gh.zip" "$url"
  unzip -q -o "$dir/gh.zip" -d "$dir"
fi
bin="$(dirname "$(find "$dir" -type f \( -name gh -o -name gh.exe \) -path '*bin*' | head -1)")"
[ -x "$bin/gh" ] || [ -f "$bin/gh.exe" ] || { echo "::error::gh not found in $pkg"; exit 1; }
if command -v cygpath >/dev/null 2>&1; then cygpath -w "$bin" >>"$GITHUB_PATH"; else echo "$bin" >>"$GITHUB_PATH"; fi
"$bin/gh" --version | head -1
