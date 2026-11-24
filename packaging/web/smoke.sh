#!/usr/bin/env bash
# Headless-browser smoke test of the browser version (plan item 5.6): build it, serve it, load it in
# headless Chromium and check that the app boots without console errors.
#
# Usage: packaging/web/smoke.sh [--skip-build] [--port N]
#
# Needs: what `cargo xtask web` needs (wasm32-unknown-unknown target, wasm-bindgen CLI at the
# Cargo.lock version; docs/web.md) plus a Chromium-family browser on PATH (`chromium`,
# `chromium-browser`, `google-chrome` or `google-chrome-stable`), or its path in $CHROME. No Node,
# no WebDriver: Chromium's own `--headless --dump-dom` and `--enable-logging=stderr` are enough.
#
# Pass: the loading placeholder (#app_loading) is gone (eframe started), the page shows no
# "failed to start" or crash overlay, and the browser logged no console error.
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"

BUILD=1
PORT=18731
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-build) BUILD=0 ;;
    --port) PORT="$2"; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

CHROME="${CHROME:-}"
if [[ -z "$CHROME" ]]; then
  for c in chromium chromium-browser google-chrome google-chrome-stable; do
    if command -v "$c" >/dev/null; then CHROME="$c"; break; fi
  done
fi
[[ -n "$CHROME" ]] || { echo "error: no Chromium found; install one (e.g. apt install chromium) or set CHROME" >&2; exit 1; }

cd "$ROOT"
if [[ "$BUILD" == 1 ]]; then
  cargo xtask web --dev
fi
[[ -f "$CARGO_TARGET_DIR/web-site/index.html" ]] || { echo "error: no web build in $CARGO_TARGET_DIR/web-site" >&2; exit 1; }

OUT="$(mktemp -d)"
cleanup() { [[ -n "${SERVER:-}" ]] && kill "$SERVER" 2>/dev/null; rm -rf "$OUT"; }
trap cleanup EXIT

# the xtask server sends the COOP/COEP headers the render workers need; --dev skips the rebuild
# only when the bundle is current, which it is right after the build above
cargo xtask web --dev --serve "$PORT" >"$OUT/server.log" 2>&1 &
SERVER=$!
for _ in $(seq 1 600); do
  if (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null; then break; fi
  kill -0 "$SERVER" 2>/dev/null || { cat "$OUT/server.log" >&2; echo "error: web server exited" >&2; exit 1; }
  sleep 1
done

# store=memory: no browser storage and no one-tab lock, so a fresh profile boots the demo library
URL="http://127.0.0.1:$PORT/index.html?store=memory"
timeout 180 "$CHROME" --headless=new --no-sandbox --disable-gpu-sandbox --no-first-run \
  --user-data-dir="$OUT/profile" --use-angle=swiftshader --enable-unsafe-swiftshader \
  --enable-logging=stderr --v=0 --virtual-time-budget=60000 \
  --dump-dom "$URL" >"$OUT/dom.html" 2>"$OUT/chrome.log" || true

fail=0
if grep -q 'id="app_loading"' "$OUT/dom.html"; then
  echo "FAIL: the app did not start (loading placeholder still shown)"; fail=1
fi
if grep -qiE 'failed to start|stopped after an internal error|panicked' "$OUT/dom.html"; then
  echo "FAIL: the page shows a start-up failure or crash"; fail=1
fi
if ! grep -q 'app_canvas' "$OUT/dom.html"; then
  echo "FAIL: no page loaded"; fail=1
fi
if grep -E 'ERROR:CONSOLE|Uncaught' "$OUT/chrome.log"; then
  echo "FAIL: console errors (above)"; fail=1
fi
if [[ "$fail" == 1 ]]; then
  echo "--- page ---"; head -c 4000 "$OUT/dom.html"; echo
  echo "--- browser log (tail) ---"; tail -n 60 "$OUT/chrome.log"
  exit 1
fi
echo "web smoke: app booted in headless Chromium without console errors"
