#!/usr/bin/env bash
# Build the browser version and zip it:  $DIST/<binary>-web-<version>.zip (<binary> from brand.toml)
#
# Usage: packaging/web/package.sh [--skip-build]
#
# Needs: the wasm32-unknown-unknown target and the wasm-bindgen CLI at the version in Cargo.lock
# (see docs/web.md); `cargo xtask web` builds it. The zip holds a self-contained static site in <binary>-web-<version>/ that works
# from any URL path and inside an <iframe>. Hosting notes: packaging/web/HOSTING.md.in.
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
HERE="$ROOT/packaging/web"

if [ "${1:-}" != "--skip-build" ]; then
  (cd "$ROOT" && cargo xtask web)
fi

SITE="$CARGO_TARGET_DIR/web-site"
[ -f "$SITE/index.html" ] || { echo "error: $SITE/index.html missing; run without --skip-build" >&2; exit 1; }
# Guard against Cargo build artifacts leaking into the release (issue #793): cross-compiling writes
# the selected profile's *host* artifacts (build scripts, proc-macros, deps/, build/, .fingerprint/)
# to <target>/<profile>/, so if SITE ever points at a profile directory again the zip would carry
# ~113 MB of build output. Fail loudly instead.
for d in deps build .fingerprint; do
  if [ -e "$SITE/$d" ]; then
    echo "error: $SITE/$d exists — $SITE holds Cargo build artifacts, not just the site (issue #793)" >&2
    exit 1
  fi
done
# Paths must be relative so the site works under any prefix.
if grep -Eq '(src|href)="/[^/]' "$SITE/index.html"; then
  echo "error: $SITE/index.html has root-absolute URLs; it would break when served from a sub-path" >&2
  exit 1
fi

NAME="$BRAND_BINARY-web-$VERSION"
WORK="$CARGO_TARGET_DIR/web-package"
rm -rf "$WORK"
mkdir -p "$WORK/$NAME"
cp -R "$SITE/." "$WORK/$NAME/"
# Sample server configs (MIME type, caching, compression); harmless where unused.
cp "$HERE/_headers" "$HERE/.htaccess" "$WORK/$NAME/"
brand_render "$HERE/HOSTING.md.in" "$WORK/$NAME/HOSTING.md"
copy_docs "$WORK/$NAME"
rm -f "$DIST/$NAME.zip"
if command -v zip >/dev/null; then
  (cd "$WORK" && zip -qr9 "$DIST/$NAME.zip" "$NAME")
else
  # no zip (e.g. a minimal build host): Python's zipfile, deflated
  (cd "$WORK" && python3 - "$DIST/$NAME.zip" "$NAME" <<'PY'
import os, sys, zipfile
out, top = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
    for d, _, files in os.walk(top):
        z.write(d)
        for f in sorted(files):
            z.write(os.path.join(d, f))
PY
  )
fi
echo "wrote $DIST/$NAME.zip"
ls -lh "$DIST/$NAME.zip"
