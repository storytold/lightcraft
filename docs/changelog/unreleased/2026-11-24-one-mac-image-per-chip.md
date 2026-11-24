## Packaging

- **macOS:** 26.48 is the last release with the universal `-macos-universal.dmg`; from 26.49.0 there are only `-macos-arm64.dmg` and `-macos-x64.dmg` (#1269). Installs of 26.47.x or earlier update to 26.48 through the universal image as before, and from there the updater takes the image for its Mac.
- Desktop packages (Linux, macOS, Windows, FreeBSD) build with the `dist` cargo profile (thin LTO, one codegen unit).
- The release's package-size check now also covers the Flatpak, fails on a download with no budget entry, and checks pre-releases against the release's entries.
