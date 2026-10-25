# shellcheck shell=bash
# Font setup for local packaging; sourced after packaging/env.sh.
prepare_fonts() {
  local font_dir="${CRAFT_FONTS_DIR:-$ROOT/../craft-fonts}"
  # Relative overrides are documented relative to the workspace, regardless of the caller's cwd.
  case "$font_dir" in
    /*) ;;
    *) font_dir="$ROOT/$font_dir" ;;
  esac

  if [[ -z "${CRAFT_FONTS_DIR:-}" ]] && [[ ! -e "$font_dir" ]]; then
    # Same revision as release.yml. Never update or reset an existing checkout.
    local revision=3d9ab3a132ed0eff457e424226a435a901da30d3
    echo "==> downloading craft-fonts ($revision) to $font_dir"
    if ! git clone --no-checkout https://github.com/storytold/craft-fonts "$font_dir" ||
       ! git -C "$font_dir" checkout --detach "$revision"; then
      echo "error: could not prepare craft-fonts; provide a complete checkout with CRAFT_FONTS_DIR" >&2
      return 1
    fi
  fi

  if [[ ! -f "$font_dir/fonts/manifest.txt" ]]; then
    echo "error: missing $font_dir/fonts/manifest.txt; set CRAFT_FONTS_DIR to a complete craft-fonts checkout" >&2
    return 1
  fi
  CRAFT_FONTS_DIR="$(cd "$font_dir" && pwd)" || return 1
  export CRAFT_FONTS_DIR
  # Fail the build if required faces are missing, rather than ship empty glyph boxes.
  export CRAFT_FONTS_REQUIRED=1
  echo "==> embedding fonts from $CRAFT_FONTS_DIR"
}
