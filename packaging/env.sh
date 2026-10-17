# shellcheck shell=bash
# Shared setup for the packaging scripts. Source it: `. "$(dirname "$0")/../env.sh"`.
#
# Exports:
#   ROOT                    workspace root
#   VERSION                 [workspace.package] version from Cargo.toml (override: PACKAGE_VERSION)
#   DIST                    output directory for release artifacts (default: $ROOT/dist/release)
#   BUILD_SHA               git commit of the build
#   BUILD_DATE              UTC build date, YYYY-MM-DD
#   CARGO_TARGET_DIR        cargo's target dir (default: $ROOT/target)
#   BRAND_<KEY>             every string in brand.toml's [product], [identity] and [stable], upper-cased:
#                           BRAND_DISPLAY_NAME, BRAND_BINARY, BRAND_CLI_BINARY, BRAND_APP_ID, BRAND_ENV_PREFIX, …
#                           ($BRAND_FILE, relative to ROOT, picks another brand file)
#
# Functions: brand_render IN OUT (fill a *.in template's {{key}} placeholders), stage_binaries DIR OUT
# (copy the neutral `app`/`app-cli` binaries to their brand names), warn, copy_docs, sha256.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export ROOT

# The version lives in exactly one place: `[workspace.package] version` in the root Cargo.toml.
# (`cargo xtask version` prints the same thing; awk avoids compiling xtask here.)
workspace_version() {
  awk '
    /^\[/ { in_pkg = ($0 == "[workspace.package]") ; next }
    in_pkg && $1 == "version" { gsub(/[" ]/, "", $3); print $3; exit }
  ' "$ROOT/Cargo.toml"
}

VERSION="${PACKAGE_VERSION:-$(workspace_version)}"
if [ -z "$VERSION" ]; then
  echo "error: could not read [workspace.package] version from $ROOT/Cargo.toml" >&2
  exit 1
fi
export VERSION

DIST="${DIST:-$ROOT/dist/release}"
mkdir -p "$DIST"
export DIST

if [ -z "${BUILD_SHA:-}" ]; then
  BUILD_SHA="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || true)"
fi
export BUILD_SHA
export BUILD_DATE="${BUILD_DATE:-$(date -u +%Y-%m-%d)}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"

# ---- brand.toml ---------------------------------------------------------------------------------
# The product name lives only in brand.toml (see crates/brand). This reads the same subset of TOML:
# `key = "string"` lines under [product], [identity] and [stable] become BRAND_<KEY>.
BRAND_FILE_PATH="$ROOT/${BRAND_FILE:-brand.toml}"
case "${BRAND_FILE:-}" in /*) BRAND_FILE_PATH="$BRAND_FILE" ;; esac
[ -f "$BRAND_FILE_PATH" ] || { echo "error: brand file $BRAND_FILE_PATH not found" >&2; exit 1; }
brand_vars() {
  awk '
    /^[ \t]*\[/ { s = $0; gsub(/[][ \t]/, "", s); next }
    s != "product" && s != "identity" && s != "stable" { next }
    /^[ \t]*[A-Za-z0-9_]+[ \t]*=[ \t]*"/ {
      key = $0; sub(/^[ \t]*/, "", key); sub(/[ \t]*=.*/, "", key)
      val = $0; sub(/^[^"]*"/, "", val)
      out = ""
      while (length(val) > 0) {
        c = substr(val, 1, 1)
        if (c == "\\") { out = out substr(val, 2, 1); val = substr(val, 3); continue }
        if (c == "\"") break
        out = out c; val = substr(val, 2)
      }
      print key "\t" out
    }
  ' "$BRAND_FILE_PATH"
}
while IFS="$(printf '\t')" read -r key val; do
  upper="$(printf '%s' "$key" | tr '[:lower:]' '[:upper:]')"
  export "BRAND_$upper=$val"
done <<EOF_BRAND
$(brand_vars)
EOF_BRAND
for required in DISPLAY_NAME BINARY CLI_BINARY APP_ID ENV_PREFIX VENDOR HOMEPAGE REPOSITORY TAGLINE; do
  eval "v=\${BRAND_$required:-}"
  # shellcheck disable=SC2154
  [ -n "$v" ] || { echo "error: $BRAND_FILE_PATH has no $(printf '%s' "$required" | tr '[:upper:]' '[:lower:]')" >&2; exit 1; }
done

# Escape a value for the right-hand side of a sed s||| expression.
sed_escape() { printf '%s' "$1" | sed -e 's/[\\|&]/\\&/g'; }

# Render a *.in template: every {{key}} from brand.toml, plus {{version}}, {{short_version}},
# {{date}} and {{build_sha}}. An unknown {{key}} left over is an error.
brand_render() {
  local in="$1" out="$2" args=() key val
  while IFS="$(printf '\t')" read -r key val; do
    args+=(-e "s|{{$key}}|$(sed_escape "$val")|g")
  done <<EOF_RENDER
$(brand_vars)
app	${BRAND_DISPLAY_NAME}
version	${VERSION}
short_version	${VERSION%%-*}
date	${BUILD_DATE}
build_sha	${BUILD_SHA:-unknown}
EOF_RENDER
  mkdir -p "$(dirname "$out")"
  sed "${args[@]}" "$in" >"$out"
  if grep -n '{{[a-z0-9_]*}}' "$out" >&2; then
    echo "error: unknown placeholder(s) left in $out (from $in)" >&2
    exit 1
  fi
}

# Copy the neutral cargo binaries (app, app-cli) from DIR to OUT under their brand names.
stage_binaries() {
  local dir="$1" out="$2" ext="${3:-}"
  mkdir -p "$out"
  cp "$dir/app$ext" "$out/$BRAND_BINARY$ext"
  cp "$dir/app-cli$ext" "$out/$BRAND_CLI_BINARY$ext"
}

# Emit a GitHub Actions warning (plain stderr outside Actions).
warn() {
  if [ -n "${GITHUB_ACTIONS:-}" ]; then echo "::warning::$*"; else echo "warning: $*" >&2; fi
}

# Copy licence and readme files that exist into a package directory (plus the craft-fonts licences
# when building with CRAFT_FONTS_DIR).
copy_docs() {
  local dest="$1" f
  for f in README.md LICENSE LICENSE-MIT LICENSE-APACHE COPYRIGHT NOTICE; do
    if [ -f "$ROOT/$f" ]; then cp "$ROOT/$f" "$dest/"; fi
  done
  copy_font_licences "$dest"
}

# Builds made with CRAFT_FONTS_DIR (storytold/craft-fonts, an optional build input) embed its fonts;
# their OFL licences go with the package as OFL-<family-dir>.txt. Nothing to do without it.
copy_font_licences() {
  local dest="$1" lic family
  [ -n "${CRAFT_FONTS_DIR:-}" ] || return 0
  for lic in "$CRAFT_FONTS_DIR"/fonts/*/OFL.txt; do
    [ -f "$lic" ] || continue
    family="$(basename "$(dirname "$lic")")"
    cp "$lic" "$dest/OFL-$family.txt"
  done
}

# Portable SHA-256 of a file (prints just the hash).
sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# Copy the app icons (assets/app-icon/hicolor/<size>/apps/*) into DEST/hicolor/<size>/apps/, named
# after the brand's app id, as the desktop entry and AppStream metadata expect.
install_icons() {
  local dest="$1" f size
  for f in "$ROOT"/assets/app-icon/hicolor/*/apps/*; do
    [ -f "$f" ] || continue
    size="$(basename "$(dirname "$(dirname "$f")")")"
    mkdir -p "$dest/hicolor/$size/apps"
    cp "$f" "$dest/hicolor/$size/apps/$BRAND_APP_ID.${f##*.}"
  done
}
