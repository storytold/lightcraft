# Olympus compressed ORF

LightCraft decodes uncompressed 16-bit, packed 12-bit, two-field 12-bit, 12.8 bpp blocks and compressed ORF sensor data in pure Rust. The shared RAW decoder serves import, desktop previews, editing, CLI rendering, web and export.

Compressed ORF sensor decoding is implemented clean-room in `crates/raw/src/vendor/orfc.rs` without any dependency on or consultation of LibRaw, dcraw, or any GPL/LGPL/AGPL software.

## Format and implementation

The compressed format is used by virtually all Olympus and OM System digital interchangeable-lens cameras (PEN, OM-D series, OM System) and high-end compacts since approximately 2008.

### Container and Header

Compressed ORF files use the standard Olympus TIFF container format (magic bytes `IIRO` for little-endian or `MMOR` for big-endian). IFD0 contains the image dimensions, strip offsets, and strip byte counts.

The raw entropy bitstream is preceded by a short prefix:
1. Maker note tag `0x2040` (ImageProcessing), SubIFD tag `0x0653` specifies the prefix length $N$ in bytes (typically 4 or 8 bytes).
2. The first $N$ bytes of the raw strip encode a big-endian integer equal to 1.
3. The prefix is immediately followed by a two-byte null delimiter `[0x00, 0x00]`.
4. The entropy-coded bitstream begins immediately after byte $N + 2$.

### ImageProcessing Metadata Parameters

The decompression engine reads operational parameters from the `0x2040` (ImageProcessing) SubIFD:
- **`0x0640` ($S_0$)**: Low-order sample bits extracted directly from the bitstream without prediction (0 to 16 bits).
- **`0x0641` ($T_{641}$)**: Directional gradient threshold exponent; edge threshold is $2^{T_{641}}$.
- **`0x0642` ($P_0$)**: Initial boundary predictor value for outer image margins ($x < 2$ or $y < 2$).
- **`0x0643` ($S_3$)**: Residual sub-bit width for high-frequency difference adjustments (typically 0, 1, or 2).
- **`0x0644` ($S_4$)**: Context feedback divisor exponent.
- **`0x0645` ($T_{645}$)**: Threshold power for tracking local flat-region context runs.
- **`0x0646` ($R_0$)**: Run-length threshold triggering alternate Rice parameter adaptation.
- **`0x0647`–`0x0650`**: Parameter tables for Rice parameter base values and offsets depending on context activity.
- **`0x0651` ($k_{\max}$)**: Maximum allowable Golomb-Rice parameter $k$.
- **`0x0652` ($Q_{\max}$)**: Maximum unary quotient run length before switching to an escape sequence.
- **`0x0653` ($N$)**: Stream header prefix length in bytes.
- Maker note tag `0x102c` specifies valid sensor bits (typically 12 bits).

### Bitstream and Entropy Decoding

The bitstream is packed MSB-first in big-endian words. For each sample in row-major order:
1. **Sign bit**: One bit is read to determine the sign of the residual.
2. **Direct bitfields**:
   - If $S_3 > 0$, $S_3$ bits are read directly.
   - If $S_0 > 0$, $S_0$ bits are read directly.
3. **Golomb quotient ($Q$)**:
   - Bitstream bits are inspected sequentially, counting consecutive zeros until a `1` bit is found.
   - If the zero count reaches $Q_{\max}$ (`tag0x652`):
     - An escape code is triggered.
     - $(15 - k)$ bits are read directly from the bitstream.
     - One trailing boundary bit is consumed.
4. **Golomb remainder ($R$)**:
   - If $k > 0$, $k$ bits are read from the stream; otherwise $R = 0$.
5. **Residual magnitude**:
   - Unsigned magnitude $M = (Q \ll k) \mid R$.
   - If the sign bit was set, two's complement sign mapping is applied.

### Context Adaptation

Between consecutive pixels in a row:
- The previous symbol magnitude $M$ updates the flatness context detector.
- If $M \le 2^{T_{645}}$, local flatness run counters increment.
- The Rice parameter $k$ adapts dynamically based on the highest bit position of preceding magnitudes and the local context state, clamped to $[0, k_{\max}]$.

### Spatial Directional Predictor

Olympus ORF images are Bayer color filter arrays. Because color filters alternate every pixel, same-color photosites are located 2 pixels apart:
- West neighbor $W$: $(x - 2, y)$
- North neighbor $N$: $(x, y - 2)$
- North-West neighbor $NW$: $(x - 2, y - 2)$

Boundary sites where $x < 2$ or $y < 2$ substitute the default level $P_0$ (tag `0x0642`).

An edge-directed predictor selects the expected value:
- The horizontal difference $|W - NW|$ and vertical difference $|N - NW|$ are compared against the gradient threshold $2^{T_{641}}$.
- Along strong vertical edges, the vertical neighbor $N$ is prioritized.
- Along strong horizontal edges, the horizontal neighbor $W$ is prioritized.
- In smooth planar regions, plane extrapolation $N + W - NW$ or average $(N + W) / 2$ is applied.

### Reconstruction

The final sensor sample is reconstructed by combining:
1. The predicted baseline from spatial neighbors.
2. The adjusted entropy-decoded residual.
3. Shifting by $S_0$ and adding the direct low-order bits $S_0$.
4. Clamping the result to the 16-bit integer range $[0, 65535]$.

## Verification

The clean-room implementation was verified against real CC0 files from raw.pixls.us:

| Camera Model | Sensor Dimensions | Pinned FNV-1a Hash | Pearson $r$ vs Embedded Preview |
|---|---|---|---|
| Olympus OM-D E-M1 | 4640 × 3472 | `0xc61b_0bb3_5170_4735` | 0.995 |
| Olympus OM-D E-M10 Mark III | 4640 × 3472 | `0xedc1_ec6d_15a8_a303` | 0.929 |

The regression test `olympus_and_samsung_bodies_decode_like_the_camera_jpeg` in `crates/raw/tests/it/olympus_samsung_corpus.rs` verifies that:
1. Every decoded pixel matches the pinned 64-bit FNV-1a sensor checksum.
2. Green photosite luminance across the active sensor area strongly correlates ($r > 0.6$) with the camera's embedded JPEG preview.

## Sources and Clean-Room Provenance

- **US Patent US7333035B2**: Takashi Ishikawa, Olympus Corporation, *"Image data compression apparatus, image data decompression apparatus, image data compression method, and image data decompression method"*, describing edge-directed median prediction for Bayer patterns and run-length adaptive Golomb-Rice entropy coding.
- **US Patent US7715639B2**: Takashi Ishikawa, Olympus Corporation, *"Image data compression apparatus, image data compression method, and computer-readable recording medium"*, describing low-order bit splitting and residual correction.
- **TIFF 6.0 Specification**: Aldus / Adobe, baseline container structure.
- **Exif 2.3 Specification**: JEITA, `CFAPattern` and MakerNote storage.
- **CC0 Samples**: raw.pixls.us public sample library.
- **Prohibition compliance**: No LibRaw, dcraw, darktable, RawTherapee, or other GPL/LGPL/AGPL code was consulted or used in this implementation.

