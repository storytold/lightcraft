# Olympus and OM System ORF decoding

> **Last updated:** 2026-10-13 · **Change:** minor (invalidate smart previews on Reload, Relink and history transitions)

LightCraft reads ORF sensor data from older uncompressed cameras and from the lossless compressed
files written by Olympus PEN, OM-D and Tough cameras and OM System bodies. The original mosaic,
as-shot white balance, black levels, active crop and Exif colour-filter pattern feed the same raw
development pipeline as the other camera formats. A supported file no longer edits its embedded
JPEG or inherits the JPEG's monochrome picture style.

## Independent implementation

`crates/raw/src/vendor/orfc.rs` is original pure Rust. The general prediction and adaptive coding
methods are described in Olympus's [US7333035B2](https://patents.google.com/patent/US7333035B2/en)
and [US7715639B2](https://patents.google.com/patent/US7715639B2/en), by Takashi Ishikawa. Neither
specifies the ORF bit layout. We established the constants and packing from public-domain
[raw.pixls.us](https://raw.pixls.us/) files and the sensor pixels produced by the installed
`unprocessed_raw` executable, LibRaw 0.22.2-Release, run as a black box. No external decoder source,
camera matrices, Adobe profiles or app-bundle contents were read or copied. Exif and maker-note
fields follow the [ExifTool Olympus tag documentation](https://exiftool.org/TagNames/Olympus.html)
(tag-name tables only).

The measured stream layout:

- One strip, TIFF compression tag 1 and a seven-byte header `00 00 00 00 01 00 00`. The header
  selects the codec before byte-count-based packing guesses; noisy data may resemble packed data.
- The remaining bits are MSB first, with no byte stuffing or alignment between rows. Even and odd
  columns have independent state, reset at each row: previous unsigned magnitude `m`, correction
  `c`, and run of magnitudes at most 16. All start at zero.
- If the run is shorter than three, `e = 2`, otherwise `e = 0`. The Rice remainder width is
  `k = max(2 + e, bit_length(m) - e)`, with the subtraction saturating at zero.
- Read one sign bit and two raw low bits. The quotient is the number of zeros before a one, at
  most eleven; twelve zeros instead introduce a `16 - k` bit field, shifted right once to obtain
  the quotient. Then read the ordinary `k` remainder bits to obtain the new unsigned magnitude.
- A negative sign means `-m - 1`. Add the previous correction to get `d`, update the correction
  to `floor((3*d + c)/32)`, and reconstruct the residual as `4*d + low_bits`.
- Neighbours are two sensor positions away. The first two rows predict from the left, starting
  at zero; the first two columns of later rows predict from above. Else, if the upper-left value
  lies strictly between the left and upper values, use their mean when both gradients are at most
  32, or `left + up - upper_left` for larger gradients. Otherwise use the neighbour farther from
  upper-left. Equal distances select the upper neighbour.
- Reconstructed samples must be in `0..4095`. Truncated codes and invalid values report a
  structured error. The minimum six bits per sample and checked dimensions bound allocation before
  decoding; allocation failure is also an error.

Import probes validate the stream envelope without decompressing it when Exif states the Bayer
pattern. Full reads validate every code and sample. The other layouts (16-bit words, 12-bit packed
words, two-field packed rows, and ten samples in 16-byte blocks) remain supported.

## Verification

`crates/raw/tests/orf_corpus.rs` pins SHA-256 identities for the input files and the entire decoded
sensor mosaic (row-major little-endian `u16`, including borders). All 23 files match the independent
reference exactly. The files are downloaded by `cargo xtask corpus --download` into gitignored
`corpus/raw`; no media is committed.

Verified bodies: E-3, E-410, E-620, E-M1, E-M1 Mark II/III, E-M5 Mark II/III, E-M10 Mark III/IV,
PEN E-P1/E-P3/E-P7/**E-PL7**, PEN-F, SH-2, TG-6, and OM System OM-1, OM-1 Mark II, OM-3, OM-5,
OM-5 Mark II and TG-7. The E-M5 III sample is an 80 MP compressed high-resolution capture; the PEN-F
sample is an 80 MP 12.8-bpp uncompressed capture. The E-M5 II reference is the compressed ORF-format
sensor stream in its `.ORI` companion file, downloaded under the fixture's `.orf` name; directory
import of the `.ORI` extension is not included in this change.

Run:

```sh
cargo test -p lightcraft-raw --test orf_corpus -- --nocapture
cargo test -p lightcraft-raw --lib vendor::orf
cargo test -p lightcraft-raw --test vendor_robust
```

Synthetic tests also cover codec selection, header/full-read agreement, row-state reset, escapes,
negative residuals, the gradient threshold, hostile dimensions, truncated streams, arbitrary
payloads and overflowing maker-note crop coordinates.

The E-PL7 engine regression also checks import as raw, guarded camera-colour fitting, finite render
pixels, and Reload from Disk upgrading an old preview-only catalog entry without losing its edits.
Reload invalidates its JPEG-derived smart preview even when the original's bytes are unchanged;
use **Build Smart Previews** before taking the original offline to rebuild it from sensor data.
Relink, Undo and Redo also discard existing proxies when they change the preview-only state. If
discarding a proxy fails, the catalog change is refused; fix the storage problem and retry.
Render cache version 62 invalidates previously cached JPEG-fallback renders. Existing imports can
be upgraded with **Photo → Reload from Disk** after rebuilding and restarting LightCraft.

## Colour and limits

Colour uses the existing guarded fit to the file's own camera JPEG when it contains colour; a
monochrome preview contributes tone only, keeping the sensor's colour channels editable. See
[camera-preview-colour.md](camera-preview-colour.md). This is estimated camera colour, not measured
chart calibration or Lightroom Camera Matching profiles. Per-model calibration and automatic lens
corrections remain separate gaps. The decoder accepts the measured stream header; unknown headers
and damaged files retain the app's existing preview/error fallback. Unchecked camera models are
listed as unverified in [raw/coverage.tsv](raw/coverage.tsv).

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-13 | minor | Apply smart-preview invalidation to Relink and Undo/Redo, preserving history when deletion fails |
| 2026-10-13 | minor | Reload invalidates old JPEG-derived smart previews before switching to sensor decoding; synthetic and E-PL7 regressions |
| 2026-10-12 | minor | Lossless compressed ORF: independent Rust decoder, 23 exact sensor references including PEN E-PL7, fast probes and malformed-input regressions |
