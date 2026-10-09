# Olympus ORF: independently observed container and packed 12-bit layout

This document covers container metadata and the implemented packed layout.
The separate [measured compressed 12-bit profile](orf12-compressed-measured.md)
describes the implemented E01/V02 token/state/predictor rules and complete raster
comparisons on the examined originals. A separate [specification review](orf12-review.md)
preceded product implementation. Other compressed fingerprints and the generalized
14-bit variant remain unsupported.
See [provenance and acceptance gates](orf12-provenance.md).

## Scope and evidence

The initial local sample set has eight original ORFs from an Olympus E-M5 II and
E-M5 III, and two companion JPEGs. The creator has released the originals as CC0 in a
[separate corpus repository](https://github.com/voshart/Rust-Olympus-RAW-decoder/tree/4d29fe4886d893883a78f6099f02d8dd17d47192/corpus/voshart-olympus).
Media and generated sensor arrays stay outside LightCraft's Git history. The repeatable tools are in [the public research tools](https://github.com/voshart/Rust-Olympus-RAW-decoder/tree/bb4f216a13a4701321f202fee0336a56ddae533d/tools).
File hashes and experiment results live in the gitignored `plan/orf-research/`.
The specimen identifiers below are resolved to hashes in that local manifest.
This is verified sample coverage, not a guarantee for every camera or mode.

| Specimen group | Count | Full sensor | Maker-note active crop | Exif CFA | Stored bits per sensor sample |
|---|---:|---|---|---|---:|
| E-M5 II ordinary shots | 5 | 4640 Ã— 3472 | (8, 8), 4608 Ã— 3456 | RGGB | 6.041â€“7.272 |
| E-M5 III ordinary shot | 1 | 5240 Ã— 3912 | (12, 12), 5184 Ã— 3888 | RGGB | 8.155 |
| E-M5 III high-resolution shot | 1 | 10400 Ã— 7792 | (8, 8), 10368 Ã— 7776 | RGGB | 6.647 |
| E-M5 II high-resolution shot | 1 | 9280 Ã— 6932 | (10, 10), 9216 Ã— 6912 | GRBG | exactly 12.8 |

All eight files declare `BitsPerSample=16`, `Compression=1`, one strip covering
the full sensor, and Olympus ImageProcessing `ValidBits=[12, 0]`. Those standard
TIFF tags therefore do **not** distinguish packed samples from the vendor's
compressed samples. The seven shorter strips were the initial compressed
candidates; their coding rules are now covered by the separate measured profile.
A camera-model lookup cannot
replace the per-file CFA tag: the E-M5 II's ordinary and high-resolution files
declare different layouts.

## Container and metadata

Observed files start with `IIRO` and use classic little-endian TIFF IFD framing.
The existing reader also supports the established `IIRS` and `MMOR` magics.
Exif tag `0xa302` is an UNDEFINED blob: two 16-bit repeat dimensions in the TIFF
byte order, followed by row-major one-byte colour values. For Bayer, require
exactly eight bytes, dimensions 2 Ã— 2 and one of these four layouts:

| Bytes after dimensions | Layout |
|---|---|
| 0, 1, 1, 2 | RGGB |
| 2, 1, 1, 0 | BGGR |
| 1, 0, 2, 1 | GRBG |
| 1, 2, 0, 1 | GBRG |

The layout is anchored at the full sensor origin. Crop offsets are applied later
by the existing normalization path. Missing, malformed or non-Bayer Exif layouts
fall back to the existing data-based phase estimate on packed/word routes. The
measured compressed route requires a valid Exif layout. The fallback only identifies
the green diagonal and does not establish which remaining site is red or blue.
It compares same-parity block means to reduce sensitivity to point texture.

Olympus maker-note sub-IFDs supply the crop and black/WB tags through the existing
parser. New-style notes in these files start with `OLYMPUS\0II`, with offsets
relative to the maker note. Crop coordinates are checked before use. This work
does not introduce a colour matrix, a camera look or lens calibration.

Metadata sources: [CIPA Exif 2.32](https://www.cipa.jp/std/documents/e/DC-X008-Translation-2019-E.pdf),
[ExifTool EXIF tag names](https://exiftool.org/TagNames/EXIF.html) and
[Olympus tag names](https://exiftool.org/TagNames/Olympus.html).
Only format/tag descriptions were used, not decoder or metadata-library source.

## Established packed layout: E-M5 II high-resolution specimen

Rule P1: each row has `width / 10 Ã— 16` bytes; width is divisible by ten.
The observed 9280 Ã— 6932 specimen has 14,848 bytes per row and a 102,926,336-byte
strip. That is exactly ten samples per 16 bytes, without an extra strip header.

Rule P2: each group contains five three-byte pairs, followed by one zero byte.
All **6,432,896** observed padding bytes are zero. For a pair `(a, b, c)`:

```text
sample[0] = a | ((b & 0x0f) << 8)
sample[1] = (b >> 4) | (c << 4)
```

Rules P1/P2 were proposed from byte measurements before comparing reference
pixels. The opposite nibble layout was a competing hypothesis. On 1,792 samples
from the left eight columns, the selected layout had mean 350.713 and standard
deviation 32.339; the opposite layout had mean 903.503 and standard deviation
723.595. Border plausibility alone was not treated as proof.

Rule P3: concatenate groups left to right and rows top to bottom, preserving the
full sensor including borders. A subsequent black-box comparison against a
pinned binary instrument matched **all 64,328,960** samples, including borders,
with zero difference. Both independently unpacked and reference arrays have
SHA-256 `aea032c10a27ff82918a465b4cc0bdb701f65ce119a3d134ad8ebd60ff5d61d3`
when serialized as row-major little-endian u16.
The final Rust reader independently produced that same full-sensor hash.
A held-out CC0 E-M5 II high-resolution file from PIXLS.US record 2856 also matches
all 64,328,960 samples in the Python hypothesis, final Rust reader and binary
instrument. Its input SHA-256 is
`5c42fa75d6b549514b722e2c50726c03e34fac4909ec3640e147ea7fa825fc8d`;
its full-sensor little-endian u16 SHA-256 is
`79aa165b0c74e88ff6c3e3e027e86aad30a6957d9b7d3d8990668aafbb393ef4`.
See [the held-out results and limits](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/4d29fe4886d893883a78f6099f02d8dd17d47192/research/public-samples.md#held-out-e-m5-ii-high-resolution-packing).
This verifies two originals in the same camera mode.

The product recognizes this packing ahead of the existing LE32/MSB-first packed
layout for a little-endian, single-full-height-strip, unsigned one-sample layout,
validates zero padding, and reads Exif CFA. Unverified variants stay unsupported. A tagged packed header probe
checks the strip but does not allocate/unpack sensor samples. Word16 files still
need samples to establish effective depth; packed files without a valid Exif CFA
still need samples for the fallback phase estimate. `probe_info` and successful
full decode must describe the same image.

## Initial compressed-stream observations

The paragraphs below preserve the initial limited observations. Subsequent
derivation and full-frame checks are in the
[measured profile](orf12-compressed-measured.md).

The seven compressed candidates share a seven-byte strip prefix; a prefix is an
observation, not a documented codec identifier. The isolated perturbation tool
changes one strip byte at a time and records reference success/failure and the
affected sensor bounds. An error from the reference is not a product-format rule.
On one E-M5 II ordinary specimen, XOR 1 at each of strip bytes 0â€“6 caused the
reference to reject the input. Bytes 7 and 8 changed roughly the entire sensor;
byte 9 changed 4,027,172 samples, byte 128 changed 2,849,404, and byte 4096 changed
16,800 within columns 4599â€“4639 and rows 0â€“1598. This only records the behaviour
of LibRaw 0.22.1 on those exact mutations; it does not define a valid header,
predictor, adaptive rule or reset interval.
Long-range changes can be caused by variable-length coding, predictor propagation,
adaptive state or several mechanisms together. These experiments do not select
one explanation, establish bit order, or justify a decoder implementation.

The subsequent experiments distinguish competing bit-order, code-length,
predictor and reset hypotheses. The resulting evidence account, separate review
and product implementation are linked above; these initial observations alone
were not used to justify the compressed reader.
The rejected compressed encoder/decoder
from PR #240 is not an input to this document or implementation.
