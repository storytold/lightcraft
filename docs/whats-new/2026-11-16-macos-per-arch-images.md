### Smaller macOS downloads
- macOS releases come as one disk image per Mac: `-macos-arm64.dmg` for Apple silicon and `-macos-x64.dmg` for Intel, each about half the size of the universal image, which is still published for now.
- Check for Updates downloads the image for your Mac, and the universal one when a release has none for it. An Intel build running under Rosetta on Apple silicon updates to the Apple silicon build (issue #1228).
