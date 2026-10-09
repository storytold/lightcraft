# Sony ARW6 (Compressed RAW 2)

LightCraft decodes the raw sensor data of Sony's *Compressed* and *Compressed (HQ)* ARW files (TIFF Compression 32766, SonyRawFileType 6, "ARW 6.0") written by the A7R VI (ILCE-7RM6) in pure Rust. The shared RAW decoder serves import, desktop previews, editing, CLI rendering, web and export. Before this decoder these files failed the full decode and silently rendered from their embedded JPEG. The ILCE-7M5 (A7 V) writes the same codec; no sample of it was available, so it is unverified.

Decode timings: see the end of this document (measured in the performance pass).

## Format and implementation

The codec is Sony's use of SMPTE RDD 34 (LLVC, Low Latency Video Codec). `crates/raw/src/llvc.rs` is the codec core (bit reader, variable-length decoder, dequantiser, inverse 5/3 wavelet); `crates/raw/src/vendor/arw6.rs` is the Sony container, geometry and colour reconstruction; `crates/raw/src/vendor/arw6_curve.rs` is the companding table; `arw.rs` dispatches Compression 32766 to it. A test-only encoder (`arw6_testenc.rs`) builds synthetic tiles for round-trip tests.

### Container

- Raw SubIFD: Compression 32766, 14 bits per sample, BlackLevel 512 x4 (`0x7310`), WB `0x7313`, crop `0x74c7/0x74c8` (full frame: origin 12,8, size 9984x6656; APS-C: origin 16,10, size 6528x4352), CFA RGGB. The Sony tag-based white level (15360) is not read; the DNG `WhiteLevel` is (16383).
- The single strip starts with a tile table: `u32` count, `u32` 0, then per tile a `u64` offset (relative to the strip) and `u32` x, y, width, height. Seen: full frame 4 tiles of 5008x3336 (HQ) or 5024 / 4992 wide (Compressed); APS-C (6592x4372) one tile. A 480-byte block follows the table (not interpreted, not needed).
- A tile is a sequence of 16-byte words. Word 0 is RDD 34 `Picture_info` (HS = tile width, VS = tile height / 2, BBD = 16, NC = 3, NW = 3). Word 1 holds five `u24` group byte-sums, word 2 is zero, words 3-7 describe the groups (a count byte, then `u24` stream sizes in words). Groups: g0 = LL3 (3 streams, one per component), g1 / g2 / g3 = level-3 / 2 / 1 bands (3 streams each), g4 = green residual (1 stream). Streams follow in that order from word 8.
- A stream is a header word, an index table and data, each padded to a word. The index has one entry per transform unit (TU): `A(16) X B(16) X`, where A and B are the byte lengths of the TU's two halves and X are quantiser-index (QI) nibbles (one for g0 and g4, three for g1-g3: HL, LH, HH). Halves per tile = ceil((VS - 2 - s) / 8) + 1 with s the first-half shift (below), TUs = ceil(halves / 2): 105 and 137 for the sample sizes. Halves are numbered n = 2 * TU + h; each is byte-aligned and consecutive halves are contiguous.
- QI values seen, identical in Compressed and HQ files: level 1 component 0 (1,1,2), components 1-2 (2,2,3); level 2 (0,0,1); level 3 (0,0,0); LL3 0; residual 2. Compressed and HQ differ only in the coefficient data.

### Entropy coding and dequantisation

- Each line of a band is an RDD 34 section 6.3 coefficient-set stream: ceil(width / 4) sets of four coefficients driven by the DPT / ABS / SGN state machine, with zero runs and the "0^lim" end code. Values past the width are discarded. Lines of a half follow one another bit-contiguously; the state resets per line.
- Two deviations from the RDD 34 text were established by black-box comparison: in state DPTs = 0 with DPT = 0, a "0" code is a zero set that also enters DPTs = 1; and a run code of value V means V - 1 further zero sets before the post-run "0^n 1" depth code.
- Dequantisation is `recon(q) = sign(q) * (((2|q| + 1) << (QI - 1)) - (|q| & 1))`, `recon(0) = 0`, and QI = 0 leaves q unchanged. This replaces RDD 34's plain shift.
- LL3 lines are DPCM: a running sum along the line. The 2048 base (codes are unsigned 12-bit) applies to component 0 (the green mean) only; the chroma planes start at 0.
- Check: every half's lines end inside its last byte. The Phase 0 measurement found budget - 8 < consumed <= budget on every half of all 8 files; the decoder rejects a half that does not.

### Frame geometry

- Each tile holds three W/2 x H/2 planes (W, H = tile size): component 0 = M, the green mean; components 1-2 = chroma c1, c2; plus a W/2 x H/2 residual plane from g4. A three-level 5/3 wavelet is inverted (vertical, then horizontal) with whole-sample symmetric extension at every edge and integer arithmetic. Tiles are independent.
- The TU grid and the lifting phases are anchored to the full sensor, not to the tile. Let s = (lines in g4's first half) - 2, read from the stream: 0 on full frame, 3 on the APS-C crop (the crop's sensor-row offset 573 is 5 mod 8). Half n then covers plane rows 8n - 6 + s .. 8n + 1 + s, clipped to the frame. The lifting phases follow from s: full frame (0,1,1), APS-C (1,0,0). Each half holds, in clipped natural order, its rows of HL1, LH1, HH1, HL2, LH2, HH2, LL3, HL3, LH3, HH3 and, in g4, 8 residual rows; edge halves are shorter. With the right s every line of every file is used.

### Colour reconstruction

With plane row j, column x (x +- 1 clamped at the edges), in 12-bit code units:

- `G1[j][x] = M[j][x] - ((res[j-1][x] + res[j-1][x+1] + res[j][x] + res[j][x+1] + 4) >> 3)`, with `res[-1] := res[0]`.
- `G2[j][x] = ((G1[j][x-1] + G1[j][x] + G1[j+1][x-1] + G1[j+1][x]) >> 2) + res[j][x]` (unclipped G1; the last row uses G1[j] for G1[j+1]).
- `R = 2*c1 + ((min(G1, 4095) + min(G2, 4095)) >> 1)`, `B = 2*c2 + (same)`.
- Every code is clipped to 0..4095. Per 2x2 cell: R (0,0), G1 (0,1), G2 (1,0), B (1,1).

### Output range (companding table)

- Codes map through one monotone table shared by all files: identity up to code 1427, then a smooth curve reaching 39002 at code 4095 (steps grow from 1 to 58). No closed form fits within +-22, so `arw6_curve.rs` embeds the table (2668 `u16` values from code 1428). It is not stored in the file.
- Provenance: the table was read off the black-box oracle's (LibRaw, below) output over the 8 Compressed / HQ files; every code from 1027 to 4095 occurred and no two files disagreed. It is therefore a measurement of that program's output, not an independent derivation.
- Unit: 2 x 14-bit DN. The decoder reports black = 2 x 512 = 1024 and white = 2 x the file's DNG `WhiteLevel` (16383), i.e. 32766; saturated highlights sit at code 4095 = 39002, above white, and clip as usual. If a file carries no usable white level, a data-derived fallback already in output units is used.
- The table has one knot: at code 3977 it runs 32679 -> 32766 -> 32777 (steps +87 then +11), the same in every file. 32766 = 2 x 16383 is exactly the reported white level, so the knot is the white point of the measured curve (inferred from the arithmetic, not from any documentation): codes 3978..4095 sit above white and clip.

## Verification

Method: LibRaw master (commit `d1f0dd95`, 2026-10-08, run as `unprocessed_raw`, 16-bit output before any processing) was used only as a black-box oracle: it was built and run, and its source was never read. A Python reference model of the format (the Phase 0 reference model, local, not in the repo) was written from RDD 34 and from the oracle's behaviour on real and synthetic input, then reproduced the oracle exactly on every tile of the 8 Compressed / HQ files below. A test-only encoder, validated byte for byte against real tiles, probed the oracle with synthetic coefficients (dequantiser, phases, edge mapping). The Rust decoder reproduces the Phase 0 reference sums on all 8 files.

Geometry was checked against each file's embedded full-resolution JPEG at the DefaultCropOrigin: our output aligns at offset 0 on full frame and APS-C.

### LibRaw's APS-C bug

LibRaw assumes the full-frame grid (s = 0) for every tile. On the APS-C crop that drops the first three plane rows, leaves the last three unfed, and shifts its whole output up by 6 sensor rows; its first rows and last rows are also wrong. Our decoder differs from LibRaw there on purpose. To compare against LibRaw on APS-C files, shift its output by 6 rows and skip its first 16 and last 20 rows; on full frame compare directly.

### Corpus

| File | Source path (RawDB, ILCE-7RM6) | Size (bytes) | SHA-256 |
|---|---|---|---|
| `arw6-sony-ilce7rm6-apsc-land-hq.arw` | `arw6/ILCE-7RM6_APSC_land_RAW-C-HD.ARW` | 29 720 576 | `b60a7e752d2a8102ccbe6eebcfdbf533c6b0f2545739090873e9e8eb32cc0637` |
| `arw6-sony-ilce7rm6-apsc-land-c.arw` | `arw6/ILCE-7RM6_APSC_land_RAW-C.ARW` | 30 900 224 | `1c803304e63ec508b2b5f9d59d4a7e30ed279315b35fa4c5d75f959bed62198b` |
| `arw6-sony-ilce7rm6-apsc-port-hq.arw` | `arw6/ILCE-7RM6_APSC_port_RAW-C-HD.ARW` | 28 758 016 | `0b6d782571e86868670f613de9fa1f1364623b24fbfd21b704b2c516d7a1c1cb` |
| `arw6-sony-ilce7rm6-apsc-port-c.arw` | `arw6/ILCE-7RM6_APSC_port_RAW-C.ARW` | 29 044 736 | `c41674a485ffcf6d873e20e545ae62936192d94e95be3d53548e93bc543ecfd4` |
| `arw6-sony-ilce7rm6-ff-land-hq.arw` | `arw6/ILCE-7RM6_FF_land_RAW-C-HD.ARW` | 63 451 136 | `559fa26501d7813e56afecde2aea3d9965a99228162b194752d986193abb172c` |
| `arw6-sony-ilce7rm6-ff-land-c.arw` | `arw6/ILCE-7RM6_FF_land_RAW-C.ARW` | 63 844 352 | `d900d6cbe838be864269ad54de4a33fcac7eb3a3d4f91009739f770b8f85a23b` |
| `arw6-sony-ilce7rm6-ff-port-hq.arw` | `arw6/ILCE-7RM6_FF_port_RAW-C-HD.ARW` | 62 640 128 | `81c709633cb8f6340e612c3f53bb6ec6b0cccfe5b713c67efb1e977ece9e266d` |
| `arw6-sony-ilce7rm6-ff-port-c.arw` | `arw6/ILCE-7RM6_FF_port_RAW-C.ARW` | 63 168 512 | `f4448485629c3f82ff1955fd046404350e9a1f09b346038574b89c074d7c50fd` |
| `arw-sony-ilce7rm6-apsc-land-lossless.arw` | `raw_modes/ILCE-7RM6_APSC_land_RAW.ARW` | 44 167 168 | `7aaf0b3941509d7364a6f03844783155ebc912d246d1c456e5cdb4693ec09fbc` |
| `arw-sony-ilce7rm6-apsc-port-lossless.arw` | `raw_modes/ILCE-7RM6_APSC_port_RAW.ARW` | 43 692 032 | `0f011933ee96525a39710daa50b59d39702b4ec5934fa0206742e2128c55f366` |
| `arw-sony-ilce7rm6-ff-land-lossless.arw` | `raw_modes/ILCE-7RM6_FF_land_RAW.ARW` | 100 851 712 | `5cb3c870bc961cf6b079d56c860d0cdf11acd1afd04e5fb8dbe041bcfd2d6201` |
| `arw-sony-ilce7rm6-ff-port-lossless.arw` | `raw_modes/ILCE-7RM6_FF_port_RAW.ARW` | 100 884 480 | `b1a4026aa6adbaa27973b8b3426d8b4aa0c1cae1845cf5102359327d3b069691` |

These 12 files are CC0 1.0 per the RawDB record of the set (uploaded by abbradar; <https://rawdb.dnglab.org/sets/Sony/ILCE-7RM6>); the server answers 303 to a signed object URL, so fetch with `curl -L` from `https://rawdb.dnglab.org/api/download/Sony/ILCE-7RM6/<source path>`. They live in the gitignored corpus; `cargo xtask corpus --download` fetches them and [arw6-corpus.sha256](arw6-corpus.sha256) pins their identities. `cargo test -p lightcraft-raw corpus_sony -- --nocapture` verifies each available file's checksum, then the decoder's sensor sum and position-weighted sum. The first 8 rows (ARW6) are checked against the Phase 0 reference sums; the 4 lossless siblings go through the existing LJ92 path and their sums are regression values from LightCraft's own decoder. The test skips absent files.

Nine further full-frame HQ photos from the maintainer's A7R VI were compared with the oracle locally; they are private and never committed or published.

Unconditional synthetic tests (encode -> decode round trips for both geometries, the RDD 34 worked example, the dequantiser, hostile and truncated input) run without the corpus.

## Coverage

| Coverage | Status |
|---|---|
| ILCE-7RM6 Compressed and HQ, full frame, landscape and portrait | 4 files, exact against the reference sums |
| ILCE-7RM6 Compressed and HQ, APS-C crop, landscape and portrait | 4 files, exact (with LibRaw only after the 6-row shift) |
| ILCE-7RM6 lossless (existing LJ92 path) | 4 files, regression values |
| Private A7R VI HQ photos | 9 files, compared locally |

## Not covered

- The 12-bit Compressed mode (electronic shutter with continuous drive): no sample exists, so it returns `Unsupported` unless its headers prove identical.
- The ILCE-7M5: same codec, no sample; unverified.
- The x2 scale is an assumption backed by shadow-floor evidence (the darkest pixels of all eight ISO 100 files sit at 1037..1059, just above 2 x 512, and the lossless sibling's shadows match at x2; x2.38 would put them 76 DN below black). A tripod-matched lossless / compressed pair would confirm it. If wrong, only the reported black and white levels change.
- Tiles whose plane width is not a multiple of 8.
- Colour: the ILCE-7RM6 bundled colour profile and Sony's embedded lens corrections are separate work.

## Sources

- SMPTE RDD 34:2015 "LLVC (Low Latency Video Codec) for Network Transfer", <https://pub.smpte.org/latest/rdd34/rdd34-2015.pdf> (accessed 2026-10-09): picture / TU structure, VLD, inverse quantisation, 5/3 lifting with symmetric extension.
- LibRaw master `d1f0dd95`, run only as a black-box oracle (not a product, test or build dependency).
- The embedded JPEG of each file, for the geometry check.
- Not read, by licence: LibRaw, rawspeed, dnglab, RawTherapee and other ARW6 decoder sources. Exact byte layouts, deviations, geometry and the table were established through sample analysis and black-box comparison.

## Timings

pending
