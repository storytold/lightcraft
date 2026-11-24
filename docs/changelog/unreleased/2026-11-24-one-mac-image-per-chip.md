## Packaging

- **macOS:** releases ship `-macos-arm64.dmg` and `-macos-x64.dmg` only; the universal image is no longer published (#1269). The updater since 26.48.0 takes the image for its Mac; installs of 26.47.x or earlier download one by hand once.
- Desktop packages (Linux, macOS, Windows, FreeBSD) build with the `dist` cargo profile (thin LTO, one codegen unit).
- The release's package-size check now also covers the Flatpak, fails on a download with no budget entry, and checks pre-releases against the release's entries.
