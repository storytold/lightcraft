# shellcheck shell=bash
# Shared setup for app-owned native nightly archives; stable packaging does not source this file.

nightly_die() { printf 'error: %s\n' "$*" >&2; exit 2; }

nightly_init() {
  local os="$1" arch="$2" target="$3" date_digits
  [[ "${LIGHTCRAFT_BUILD_CHANNEL:-}" == nightly-v1 ]] || nightly_die 'LIGHTCRAFT_BUILD_CHANNEL must be nightly-v1'
  [[ "${LIGHTCRAFT_VERSION:-}" =~ ^[0-9]+\.[0-9]+\.[0-9]+-nightly\.[0-9]{8}\.[0-9]+\.[0-9]+\.g[0-9a-f]{12}$ ]] || nightly_die 'an exact prepared nightly version is required'
  [[ "${LIGHTCRAFT_BUILD_SHA:-}" =~ ^[0-9a-f]{40}$ ]] || nightly_die 'an exact source commit is required'
  [[ "${LIGHTCRAFT_VERSION##*.g}" == "${LIGHTCRAFT_BUILD_SHA:0:12}" ]] || nightly_die 'version/source commit mismatch'
  [[ "${LIGHTCRAFT_BUILD_DATE:-}" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || nightly_die 'the original run-created UTC date is required'
  date_digits="${LIGHTCRAFT_VERSION#*-nightly.}"
  date_digits="${date_digits%%.*}"
  [[ "${LIGHTCRAFT_BUILD_DATE//-/}" == "$date_digits" ]] || nightly_die 'version/original-run date mismatch'
  [[ "${CARGO_TARGET_DIR:-}" == /* && "$CARGO_TARGET_DIR" != / ]] || nightly_die 'an absolute owned CARGO_TARGET_DIR base is required'
  [[ "${DIST:-}" == /* && "$DIST" != / ]] || nightly_die 'an absolute DIST output directory is required'
  [[ "/$CARGO_TARGET_DIR/" != *'/../'* && "/$DIST/" != *'/../'* ]] || nightly_die 'output paths must not contain parent traversal'
  ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  [[ "$(git -C "$ROOT" rev-parse HEAD)" == "$LIGHTCRAFT_BUILD_SHA" ]] || nightly_die 'prepared source is not the checked-out commit'
  [[ -z "$(git -C "$ROOT" status --porcelain)" ]] || nightly_die 'nightly packaging requires the frozen clean source'
  if [[ -n "${CRAFT_FONTS_DIR:-}" && ! -d "$CRAFT_FONTS_DIR/fonts" ]]; then
    nightly_die 'declared CRAFT_FONTS_DIR has no fonts directory'
  fi
  # env.sh retains the established notice copier. Required values above prohibit its stable fallbacks.
  # shellcheck source=env.sh
  . "$ROOT/packaging/env.sh"
  export CARGO_TARGET_DIR="$CARGO_TARGET_DIR/nightly-v1/$os-$arch"
  NIGHTLY_RUST_TARGET="$target"
  mkdir -p "$CARGO_TARGET_DIR"
  NIGHTLY_WORK="$(mktemp -d "$CARGO_TARGET_DIR/nightly-package.XXXXXX")"
  trap 'rm -rf -- "$NIGHTLY_WORK"' EXIT
}

# Compile only: macOS runs the version query after signing (packaging/macos/nightly-verify.sh).
nightly_compile_gui() {
  # Signing material never reaches compilation (the Windows -CheckBuildEnvironment rule).
  nightly_refuse_signing_env
  (cd "$ROOT" && LIGHTCRAFT_OVERRIDE_VERSION="$VERSION" cargo build --release --locked -p lightcraft --features heif --target "$NIGHTLY_RUST_TARGET")
  NIGHTLY_GUI="$CARGO_TARGET_DIR/$NIGHTLY_RUST_TARGET/release/lightcraft"
  [[ -f "$NIGHTLY_GUI" && ! -L "$NIGHTLY_GUI" ]] || nightly_die 'the owned GUI build did not produce a regular executable'
  [[ "$(git -C "$ROOT" rev-parse HEAD)" == "$LIGHTCRAFT_BUILD_SHA" && -z "$(git -C "$ROOT" status --porcelain)" ]] || nightly_die 'source changed during the build'
}

nightly_build_gui() {
  nightly_compile_gui
  local actual launcher
  # What the binary really prints: the prepared feed identity plus the channel, as build_flavor's
  # version string makes it (apps/lightcraft/src/build_flavor.rs). Both query forms are answered by
  # prepare_launch before any profile or window work, so neither may need a profile on disk.
  actual="$("$NIGHTLY_GUI" --version)" || nightly_die 'side-effect-free nightly version verification failed'
  [[ "$actual" == "LightCraft $VERSION (nightly-v1)" ]] || nightly_die 'the built version/flavor is not the prepared identity'
  launcher="$("$NIGHTLY_GUI" --launch-profile nightly-v1 --profile-root "$NIGHTLY_WORK/profile-query" --version)" \
    || nightly_die 'the Launcher profile version query failed'
  [[ "$launcher" == "$actual" ]] || nightly_die 'the Launcher profile query differs from the plain version query'
  # prepare_launch answers that query before any profile work, so it must leave nothing behind.
  [[ ! -e "$NIGHTLY_WORK/profile-query" ]] || nightly_die 'the Launcher version query created a profile'
  [[ "$(git -C "$ROOT" rev-parse HEAD)" == "$LIGHTCRAFT_BUILD_SHA" && -z "$(git -C "$ROOT" status --porcelain)" ]] || nightly_die 'source changed during the version query'
}

# Every signing name release.yml passes, for every platform, and import-cert.sh's outputs. A step that
# must never run beside signing material (compilation, macOS payload verification) refuses any of them.
NIGHTLY_SIGNING_ENV=(APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD KEYCHAIN_PASSWORD APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID \
  MACOS_SIGN_IDENTITY MACOS_KEYCHAIN WINDOWS_CERTIFICATE WINDOWS_CERTIFICATE_PASSWORD AZURE_TENANT_ID AZURE_CLIENT_ID \
  AZURE_CLIENT_SECRET AZURE_SIGNING_ENDPOINT AZURE_SIGNING_ACCOUNT AZURE_CERT_PROFILE LINUX_SIGNING_KEY)

# Signing material never reaches compilation (the Windows -CheckBuildEnvironment rule).
nightly_refuse_signing_env() {
  local name
  for name in "${NIGHTLY_SIGNING_ENV[@]}"; do
    [[ -z "${!name:-}" ]] || nightly_die "compilation must not receive signing configuration ($name); sign in the separate trusted step"
  done
}

nightly_copy_notices() {
  local destination="$1" required
  for required in README.md LICENSE-MIT LICENSE-APACHE NOTICE; do
    [[ -f "$ROOT/$required" && ! -L "$ROOT/$required" ]] || nightly_die "missing regular app notice: $required"
  done
  copy_docs "$destination"
}

nightly_publish_archive() {
  # Exclusive creation: never replace an existing artifact, even if another writer raced the preflight.
  python3 -I - "$1" "$2" <<'PY'
import os, shutil, sys
source, destination = sys.argv[1:]
created = False
try:
    # Open the input first: a missing source must not leave an empty final artifact.
    with open(source, 'rb') as incoming:
        with open(destination, 'xb') as output:
            created = True
            shutil.copyfileobj(incoming, output, length=1024 * 1024)
            output.flush()
            os.fsync(output.fileno())
except BaseException:
    if created:
        os.unlink(destination)
    raise
PY
  printf 'Nightly artifact: %s\n' "$2"
}
