#!/usr/bin/env bash
# Build the browser version and zip it:  $DIST/lightcraft-web-<version>.zip
#
# Usage: packaging/web/package.sh [--skip-build]
#
# Needs: the wasm32-unknown-unknown target and the wasm-bindgen CLI at the version in Cargo.lock
# (see docs/web.md); `cargo xtask web` builds it. The zip holds a self-contained static site in lightcraft-web-<version>/ that works
# from any URL path and inside an <iframe>. Hosting notes: packaging/web/README.md.
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
HERE="$ROOT/packaging/web"

if [[ "${1:-}" != "--skip-build" ]]; then
  # CI (the shared release workflow installs only trunk): install the wasm-bindgen CLI at the
  # version in Cargo.lock. Locally, `cargo xtask web` says which version to install.
  if [[ -n "${CI:-}" ]]; then
    WASM_BINDGEN_VERSION="$(awk '$0 == "name = \"wasm-bindgen\"" { getline; gsub(/version = |"/, ""); print; exit }' "$ROOT/Cargo.lock")"
    [[ -n "$WASM_BINDGEN_VERSION" ]] || { echo "error: wasm-bindgen not found in Cargo.lock" >&2; exit 1; }
    if ! { command -v wasm-bindgen >/dev/null && wasm-bindgen --version | grep -qF " $WASM_BINDGEN_VERSION"; }; then
      cargo install wasm-bindgen-cli --version "$WASM_BINDGEN_VERSION" --locked
    fi
  fi
  (cd "$ROOT" && cargo xtask web)
fi

SITE="$CARGO_TARGET_DIR/web-site"
[[ -f "$SITE/index.html" ]] || { echo "error: $SITE/index.html missing; run without --skip-build" >&2; exit 1; }
# Guard against Cargo build artifacts leaking into the release (issue #793): cross-compiling writes
# the selected profile's *host* artifacts (build scripts, proc-macros, deps/, build/, .fingerprint/)
# to <target>/<profile>/, so if SITE ever points at a profile directory again the zip would carry
# ~113 MB of build output. Fail loudly instead.
for d in deps build .fingerprint; do
  if [[ -e "$SITE/$d" ]]; then
    echo "error: $SITE/$d exists — $SITE holds Cargo build artifacts, not just the site (issue #793)" >&2
    exit 1
  fi
done
# Paths must be relative so the site works under any prefix.
if grep -Eq '(src|href)="/[^/]' "$SITE/index.html"; then
  echo "error: $SITE/index.html has root-absolute URLs; it would break when served from a sub-path" >&2
  exit 1
fi

NAME="lightcraft-web-$VERSION"
WORK="$CARGO_TARGET_DIR/web-package"
rm -rf "$WORK"
mkdir -p "$WORK/$NAME"
cp -R "$SITE/." "$WORK/$NAME/"
# Sample server configs (MIME type, caching, compression); harmless where unused.
cp "$HERE/_headers" "$HERE/.htaccess" "$WORK/$NAME/"
cp "$HERE/README.md" "$WORK/$NAME/HOSTING.md"
copy_docs "$WORK/$NAME"
rm -f "$DIST/$NAME.zip"
(cd "$WORK" && zip -qr9 "$DIST/$NAME.zip" "$NAME")
echo "wrote $DIST/$NAME.zip"
ls -lh "$DIST/$NAME.zip"
