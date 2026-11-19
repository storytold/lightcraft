# Olympus compressed ORF: support withdrawn

Compressed ORF sensor decoding introduced in #3775 has been removed while its provenance remains unresolved. The earlier clean-room and derivation claims are withdrawn; this note makes no legal determination and is not a format specification.

Compressed ORF files are unsupported for sensor decoding. They use an embedded JPEG preview when one is available; a file without a usable preview cannot be rendered. Preview-only editing and export operate on that rendered preview, not the raw sensor data.

Uncompressed 16-bit, packed 12-bit, two-field 12-bit and 12.8 bpp ORF support remains in `crates/raw/src/vendor/orf.rs`. This includes supported uncompressed high-resolution modes; it does not imply compressed-mode support on the same camera.

Existing ORF records are rechecked in the background when a library opens, including files previously marked as sensor-decodable. Supported uncompressed ORFs remain supported; compressed files become preview-only. Missing or offline files are left unchanged and retried on the next open. **Reload from Disk** (`photo.reload`) can also refresh this status manually. ORF render-cache identities invalidate older sensor-derived thumbnails and loupe renders. Existing edits may look different when applied to the camera JPEG instead of sensor data.

Coverage records no longer count the removed decoder as support or verification. See [raw-parity.md](raw-parity.md) and [raw/coverage.tsv](raw/coverage.tsv). Any future compressed ORF decoder requires independently established provenance and validation before support is claimed again. Git history is preserved.
