# Sony ARW6 ("Compressed RAW 2") decoder: design

- **Date:** 2026-10-09 (Phase 0 results added the same evening)
- **Status:** Phase 0 passed. The bitstream model below is bit-exact against the black-box oracle on every tile of
  the 8 RawDB Compressed / HQ files, and its geometry matches the camera's embedded JPEG. Phase 0 and the
  implementation are on branch `claude/arw6-decoder`; the durable record is `docs/arw6-compression.md`.
- **Tracker rows:** LR-IMP-FORMATS, LR-IMP-CAMERA-COVERAGE (`docs/parity.md`)
- **Sub-project 1 of 3.** Follow-ups with their own specs: the ILCE-7RM6 colour profile (needs this decoder), and
  Sony embedded lens corrections (independent of it).

## Goal

Decode the raw sensor data of Sony's "Compressed" and "Compressed (HQ)" ARWs (TIFF Compression 32766,
SonyRawFileType 6, "ARW 6.0") written by the A7R VI (ILCE-7RM6) and, by the same codec, the A7 V (ILCE-7M5). Today
these files fail the full decode and silently render from their embedded JPEG.

### Success criteria

1. **Bit-exact:** the decoded CFA equals the oracle's raw sensor array (see *Sources*) on every tile of all 17 ARW6
   files available locally: 9 private full-frame HQ shots and 8 RawDB samples (FF and APS-C, Compressed and HQ,
   landscape and portrait). "Equals" means every pixel for FF files (bit-exact over the full frame); APS-C files are
   interior-exact: the oracle's own output is shifted up by 6 sensor rows (a LibRaw bug, see *Background*), so our row
   y+6 is compared with its row y for 12 ≤ y < h − 24 (its rows 0–11 and the last 18 rows of the overlap differ). A file the oracle cannot unpack (LibRaw issue #828
   reports two such A7R VI files) is instead checked against its embedded full-resolution JPEG, and the check that
   was used is recorded.
2. **Geometry:** the decoded frame lines up with the embedded JPEG at the file's DefaultCropOrigin (verified in
   Phase 0 for FF and APS-C; the oracle fails this on APS-C).
3. **Speed:** a 10016×6672 decode takes ≤ 1 s on the development machine (parallel per tile).
4. **Never crash:** truncated, hostile or unknown streams return `RawError::Corrupt` / `Unsupported`, never a
   panic; the photo then imports as preview-only with that reason.
5. Black level, white level, white balance, crop and orientation come out as the existing ARW path reads them,
   scaled to the decoder's output unit (2 × 14-bit DN, see *Output range*).

## Background: the verified bitstream model (Phase 0, 2026-10-09)

Everything below was established from the file structure plus black-box comparison with the oracle, never from
third-party decoder source. The python reference model, the companding table and the oracle binary are kept
locally in `plan/arw6/` (local, gitignored; see its README).

### Container (Sony)

- Raw SubIFD: Compression 32766, BitsPerSample 14, BlackLevel 512 ×4 (`0x7310`), WhiteLevel 15360, WB `0x7313`,
  crop `0x74c7/0x74c8` (FF: origin 12,8 size 9984×6656; APS-C: origin 16,10 size 6528×4352), CFA RGGB (R at 0,0).
  IFD2 `JpgFromRaw` is a full-resolution camera JPEG of the cropped size. `SonyToneCurve` (`0x7010`) is all zeros.
- The single strip starts with a tile table: `u32` count, `u32` 0, then per tile `u64` offset (relative to the
  strip), `u32` x, y, width, height. Layouts seen: FF HQ 4 tiles of 5008×3336; FF Compressed 4 tiles, 5024 and 4992
  wide; APS-C (6592×4372) 1 tile. After the table, a 480-byte block that holds ~120 non-zero bytes (unknown,
  signature-like; not needed for decoding).
- Tile = 16-byte words. Word 0 is RDD 34 `Picture_info` (64 reserved bits carry ASCII `0000` + a big-endian index;
  HS = tile width, VS = tile height / 2, BBD = 16, NC = 3, NW = 3, NP = `1000000000b`). Word 1: five `u24` group
  byte-sums at offsets 0, 3, 6, 9, 12. Word 2: zero. Words 3–7: one per group, a count byte then `u24` stream sizes in
  words. Groups: g0 = LL3 (3 streams, one per component), g1 = level-3 bands, g2 = level-2, g3 = level-1 (3 each),
  g4 = green residual (1 stream). Streams follow in that order from word 8.
- Stream = header word (`u16` index words, `u24` data words, 11 further bytes: `0x40 0x40`/`0xC0`, `0x0112` = 2 ×
  TUs, `0x10`, …) + index table + data, each part padded to a word. Index entry per TU: `A(16) X B(16) X`, where
  A / B are the byte lengths of half 0 / half 1 of that TU and X is the QI nibble(s): 1 nibble for g0 and g4, 3 for
  g1–g3 (one per band, HL/LH/HH). TU k's data = A bytes then B bytes, each half byte-aligned, consecutive TUs
  contiguous. Halves per tile = ⌈(VS − 2 − s)/8⌉ + 1 with s the first-half shift (0 on FF, 3 on the APS-C crop),
  TUs = ⌈halves/2⌉ (105 and 137 for the samples; ⌈VS/16⌉ only coincides with that for the two sample sizes);
  halves are numbered n = 2·TU + h.
- QI values seen, identical in Compressed and HQ files: level 1 component 0 (1,1,2), components 1–2 (2,2,3);
  level 2 (0,0,1); level 3 (0,0,0); LL3 0; residual 2. Compressed differs from HQ only in the coefficient data.

### Entropy coding (RDD 34 §6.3 with two Sony deviations) and dequantisation

- Each line of a band is coded as RDD 34's CoefSet stream of L = ⌈width / 4⌉ sets of 4 coefficients (DPT / ABS /
  SGN state machine, ZR runs, "0^lim" end code with lim = ⌈log2(Cnt + 2)⌉); values past the width are discarded.
  Lines of a half follow one another bit-contiguously; the state resets per line.
- Deviation 1 (oracle-verified): in state DPTs = 0 with DPT = 0, a "0" code is a zero set that also enters state
  DPTs = 1 (RDD 34 keeps DPTs = 0).
- Deviation 2 (oracle-verified): a run code of value V means V − 1 further zero sets, then the post-run "0^n 1" DPT
  code (RDD 34's worked example implies V zero sets).
- Dequantisation is not RDD 34's `X = XQ << QI`: `recon(q) = sign(q) · (((2|q| + 1) << (QI − 1)) − (|q| & 1))`,
  `recon(0) = 0`, and QI = 0 leaves q unchanged (oracle-verified on every band).
- LL3 lines are DPCM: the coefficient is the running sum along the line, starting from 0. The 2048 base (the codes
  are unsigned 12-bit, 0..4095) applies to component 0 (the green mean) only; the chroma planes' sums start at 0.

### Frame geometry (sensor-anchored)

- Each tile is three W/2 × H/2 planes (W, H = tile size): component 0 = M, the green mean; components 1–2 = chroma
  c1, c2; plus the W/2 × H/2 residual plane `res` from g4. Three-level 5/3 wavelet, inverse order V IWT then H IWT,
  whole-sample symmetric extension at every edge, integer arithmetic.
- **The TU grid and the lifting phases are anchored to the full sensor, not to the tile.** Let s be the number of
  frame rows in the first half minus 2, read from the stream itself: s = (lines in g4's half 0) − 2 (FF: 0; the
  APS-C crop: 3; it equals the crop's sensor-row offset mod 8, 573 ≡ 5 → s = 3). Then half n covers plane rows
  8n−6+s .. 8n+1+s (clipped to the frame). Level-1 low-pass rows sit at plane rows ≡ s (mod 2); level-2 low-pass at
  LL1 rows ≡ p2; level-3 at LL2 rows ≡ p3, with p2 = ((2 + s − p1) / 2) mod 2 and p3 from the level-3 anchor
  (6 + s) mod 8 (FF phases (0,1,1), APS-C (1,0,0); `plan/arw6/scratch/assemble.py::geometry`).
- Each half holds, in natural clipped order, its rows of: HL1, LH1, HH1 (the half's 4 low / 4 high level-1 rows
  each), HL2 (2), LH2 (2), HH2 (2), LL3 (1), HL3, LH3, HH3 (1 each), and in g4 its 8 residual rows. Rows outside
  the frame are absent, so edge halves are shorter. With the right s every line of every file is used and none is
  missing (verified on all 8 files, both crops).
- Horizontal: band width = plane width / 2^level, phase 0, symmetric extension. Tiles are independent.

### Colour reconstruction (oracle-verified, exact)

With plane row j and column x (clamp x±1 at the edges), all in 12-bit code units:

- `G1[j][x] = M[j][x] − ((res[j−1][x] + res[j−1][x+1] + res[j][x] + res[j][x+1] + 4) >> 3)`, with `res[−1] := res[0]`.
- `G2[j][x] = ((G1[j][x−1] + G1[j][x] + G1[j+1][x−1] + G1[j+1][x]) >> 2) + res[j][x]` (unclipped G1; the last row
  uses G1[j] for G1[j+1]).
- `R = 2·c1 + ((clamp(G1, 0, 4095) + clamp(G2, 0, 4095)) >> 1)`, `B = 2·c2 + (same)`.
- Every output code is clipped to 0..4095. CFA positions: R (0,0), G1 (0,1), G2 (1,0), B (1,1) of each 2×2 cell.

### Output range (companding)

- The oracle maps codes through one monotone table shared by all files: identity for code ≤ 1427, then a smooth
  curve up to 39002 at code 4095 (steps grow from 1 to 58; no closed form fits better than ±22, so the product
  embeds the table: 2668 `u16` values, `plan/arw6/arw6_curve.txt`). It is not stored in the file (searched for).
  Its provenance is LibRaw's black-box output over 8 files (every code 1027..4095 seen, zero disagreements).
- The table's unit is 2 × 14-bit DN: the darkest pixels of all eight ISO 100 files sit at 1037..1059, just above
  2 × 512, and the lossless sibling's shadow distribution matches at ×2 (×2.38, i.e. 39002/16383, would put them
  76 DN below black; ×1 would leave no shadows). So the decoder reports black = 2 × tag black (1024) and white =
  2 × the file's DNG WhiteLevel tag (16383 → 32766; the Sony 15360 tag is not read), falling back to 32766
  when the tag is absent; 32766 is also the table's one knot (code 3977, steps +87 then +11, in every file), so codes 3978..4095 sit above white and clip as usual; saturated highlights sit at code 4095 = 39002. A
  tripod-matched lossless/compressed pair would confirm the factor; see *Risks*.

### LibRaw's APS-C bug (why the oracle is not the last word)

LibRaw assumes the FF grid (s = 0) for every tile. On the APS-C crop that drops the first three plane rows, leaves
the last three unfed (garbage), and shifts its whole output up by 6 sensor rows; its rows 0–11 and the last 18 rows
of the shifted overlap are also wrong (our row y+6 equals its row y for 12 ≤ y < h − 24). Phase 0 verified against the embedded JPEG that our model aligns at offset 0 and LibRaw's at −6 rows.
Our decoder therefore differs from LibRaw there on purpose, and tests compare with the shift and skip its edges.

### Current bug in LightCraft

`arw.rs` sends 32766 to the generic `read_image_in`. Header mode accepts it, so import does not mark the photo
preview-only. The full decode then fails and the render quietly falls back to the embedded JPEG.

## Sources and clean room

- **Specification:** SMPTE RDD 34:2015 "LLVC (Low Latency Video Codec) for Network Transfer", free PDF at
  <https://pub.smpte.org/latest/rdd34/rdd34-2015.pdf> (accessed 2026-10-09): Picture/TU/CU structure (§5), VLD
  (§6.3), inverse quantisation (§6.4), the 5/3 lifting IWT with symmetric extension (§6.5), post-IWT mapping
  (§6.6–6.7). Its figures are images; render the PDF pages to read them.
- **Black-box oracle:** LibRaw master (commit `d1f0dd95`, 2026-10-08; ARW6 decoder since `c05370da`), built in the
  scratchpad and *run only* (`unprocessed_raw`, 16-bit output before any processing). The same precedent as
  `docs/raf-compression.md` (rawpy 0.27.1). Not a product, test or build dependency. Its source is never read.
- **Visual reference:** each file's embedded full-resolution JPEG (geometry check).
- **Never read** (licence checked 2026-10-09): LibRaw source (LGPL/CDDL), rawspeed PRs #972 and #983 (LGPL-2.1),
  dnglab PR #825 (LGPL-2.1), NarrativeApp/LibRaw PR #2, Jiangtherapee "LLVC3" decoder (AGPL-3.0; only a one-paragraph
  summary was seen), abbradar/arw6_decode (no licence), RawTherapee PR #7768 (GPL-3.0), JixelLight (no licence).
- Every rule in `llvc.rs` cites an RDD 34 section or names the oracle experiment that established it. Every rule in
  `arw6.rs` says how it was established. `docs/arw6-compression.md` records the method, as
  `docs/raf-compression.md` does.

## Architecture and data flow

```
arw.rs::decode ── Compression 32766 ──► vendor/arw6.rs        (Sony container + colour; black-box findings)
                                          ├ tile table, 480-byte block skipped, per tile (rayon):
                                          │   tile header → 13 streams → index tables → halves
                                          │   s from g4 half 0 → geometry (row ranges, phases)
                                          │   llvc.rs: VLD per line → dequant → band arrays
                                          │   llvc.rs: 3-level inverse 5/3 (V then H) per component
                                          │   LL3 DPCM, greens from M + res, R/B from c1/c2, clip 0..4095
                                          │   companding table → u16 (2 × 14-bit DN)
                                          └ tiles → one CFA; black/white × 2 → arw.rs
arw.rs (unchanged) ── WB / crop / orientation / maker note ──► RawImage
```

### `crates/raw/src/llvc.rs`: the codec core

- Bit reader (MSB first, bounded, errors at end of data), VLD line decoder (RDD 34 §6.3 state machine with the two
  Sony deviations, documented at the exact lines), dequantiser (parity rule), 1-D 5/3 inverse lifting with phase
  and whole-sample symmetric extension (RDD 34 §6.5), 2-D inverse (V then H), 3-level reconstruction given band
  arrays and phases. Integer arithmetic only.
- API sketch: `vld_decode_line(reader, width) -> Result<Vec<i32>>`, `dequant(q, qi) -> i32`,
  `inverse_53(ll, lh, hl, hh, vphase) -> Plane`, `reconstruct(bands: &Bands3, phases) -> Plane`. No camera
  knowledge; limits (max DPT, line width, plane size) come in as parameters.

### `crates/raw/src/vendor/arw6.rs`: the Sony container and colour

- Tile-table parse and validation: count ≥ 1, offsets inside the strip, tiles disjoint and covering the image, even
  dimensions, sizes capped by `crate::MAX_SAMPLES`.
- Tile header parse (group counts and sizes), stream headers, index tables (A / B / QI), half byte ranges.
- Geometry from s (count the lines of g4's first half with the VLD before assigning rows; reject s outside 0..5),
  natural clipped line-to-row assignment, band arrays per component.
- Colour reconstruction and clipping as in *Background*; the companding table as `const ARW6_CURVE: [u16; 2668]`
  (identity below 1428 computed, not stored), with its provenance in the doc comment.
- `decode(bytes, info, mode) -> Result<Vec<u16>>`. In `Mode::Header`, parse only the tile table and each tile's
  header word, so import stays header-only. Black and white levels for this path are the tag values × 2.
- Unknown variants (NP / BBD / NC / NW / group counts never seen, e.g. the 12-bit Compressed mode that no sample
  covers) return `RawError::Unsupported` with a specific reason.

### `arw.rs` changes

- One match arm: `32766 => arw6::decode(...)` for both modes, before the generic fallback; black and white
  doubled for that arm.
- Everything else stays the same: WB, crop, orientation, maker note. Module docs list ARW6 and point to `arw6.rs`.

## Phase 0 result (gate passed)

Done 2026-10-09 in the session scratchpad, python only, nothing committed except this spec: oracle built;
RDD 34 figures rendered; spec-only VLD decoded the first TU and, with the two deviations above, every half of every
stream of every file (3562 halves per APS-C tile) within its byte budget; a VLD encoder + tile serializer
reproduced each tile byte-for-byte and served to probe the oracle with synthetic coefficients (dequantiser,
phases, line-to-row mapping at the frame edges); the full model then matched the oracle exactly on the interior of
all 8 files (APS-C with the 6-row shift) and the embedded JPEG at offset 0. The scratch code is kept in `plan/arw6/`
for the implementation; it is not product code.

## Errors (never crash)

- All offsets and lengths from the file go through `get()` and checked or saturating maths. Tile sizes are capped
  by `crate::MAX_SAMPLES`. The VLD bit reader returns an error at end of data instead of reading past it. DPT is
  bounded (a coefficient deeper than the 32-bit range is `Corrupt`). The ZR run is bounded by the line length. Band
  values are `i32`; sums in the colour step are `i64` or checked.
- A tile that fails to decode fails the whole image with `Corrupt`; there are no partial images. `files.rs`
  `preview_reason` already turns a failure of a recognised raw into preview-only with the reason.
- The crate keeps its lint rules: no unwrap, expect, panic or indexing in non-test code.

## Performance

- Tiles decode in parallel with rayon: 4 on FF, 1 on APS-C. If APS-C (1 tile, ~29 MP) misses the budget, decode
  the 13 streams' VLD in parallel (they are independent) and the three components' inverse transforms in parallel.
  TU-level parallelism of the transform is not possible, because the IWT spans TUs.
- Measure with `LIGHTCRAFT_PROFILE=1` and the CLI before and after, and record the figures in
  `docs/arw6-compression.md`. `cargo xtask bench` stays green.

## Testing and verification

- **Unit tests (synthetic, committed):** a test-only encoder (port of the Phase 0 `tilewrite.py`: VLD encoder with
  the Sony deviations, forward 5/3, tile serializer) so that encode → decode round-trips exactly on small pictures
  for both geometries (s = 0 and s = 3). Fixed vectors: the RDD 34 §6.3.6 worked example (Figure 6.9) as a VLD test,
  the three oracle-verified VLD cases (zero at DPT 0, run value, post-run code), the dequantiser table for QI 0..3,
  the lifting steps. Hostile inputs (truncated tile, offset past the strip, huge DPT, zero tiles, overlapping tiles,
  odd sizes, s out of range, a half shorter than its lines) return errors; each would panic or overflow without
  its guard.
- **Corpus (not committed):** add the 12 RawDB ILCE-7RM6 files (<https://rawdb.dnglab.org/sets/Sony/ILCE-7RM6>,
  uploaded by abbradar; licence as stated on that page, recorded in the corpus list) to `cargo xtask corpus`,
  pinned by SHA-256 (`rawdb/SHA256SUMS`). A corpus test checks each available file's checksum, then a sensor sum
  plus a position-weighted sum of our decode, with the expected values taken once from the Phase 0 reference
  model (itself oracle-verified), skipping absent files (the pattern of `corpus_fujifilm_compressed_samples`). The
  3 lossless samples join it as regression coverage of the existing path.
- **Private files:** the 9 user photos are used locally only, for the oracle comparison and timing. They are never
  committed or published.
- **End to end:** after the decoder lands, run `lightcraft-cli render` on FF and APS-C samples and view the output
  next to the embedded JPEG (geometry and colour). Import must no longer use the fallback, and a damaged ARW6 must
  import preview-only with a reason.
- `cargo xtask ci` green before every commit.

## Docs and tracker (same commits as the code)

- `docs/arw6-compression.md`: sources, method, the bitstream model above, the LibRaw discrepancy, the companding
  table's provenance and unit, the sample list with SHA-256, results (exact match counts), timings, and what is not
  covered.
- `docs/parity.md`: the LR-IMP-FORMATS ARW entry (ARW6 Compressed / HQ on ILCE-7RM6, by codec also ILCE-7M5) and
  the LR-IMP-CAMERA-COVERAGE counts; refresh with `cargo xtask parity --write`.
- `ROADMAP.md` *Where we stand*, where the Sony shooter row is affected.
- `assets/ATTRIBUTION.md`: nothing to add (no assets are committed).

## Out of scope

- ILCE-7RM6 bundled colour profile (sub-project 2) and Sony lens corrections from `0x7032/0x7035/0x7037`
  (sub-project 3).
- The 12-bit Compressed mode (electronic shutter + continuous drive): no sample exists, so it returns `Unsupported`
  unless its headers prove identical.
- An A7 V claim beyond "same codec": no ILCE-7M5 sample is available locally. Its docs/parity wording says
  unverified.
- Confirming the ×2 output scale with a tripod-matched pair (needs new photos). The pipeline only needs black and
  white to be consistent with the data, which they are.
- The other open Sony colour issues (#190, #244, #305, #378, #454).

## Risks

- **Output scale:** the ×2 unit rests on the shadow-floor evidence above. If it is wrong, only the reported black
  and white levels change (one constant), not the decoder.
- **Companding table provenance:** the table reproduces LibRaw's output; if LibRaw's curve were wrong, we inherit
  it. Mitigation: the table is one isolated const with its provenance; the identity region and smooth monotone
  shape make a transcription error unlikely; the JPEG comparison catches gross tone errors.
- **Edge rows:** our APS-C output differs from LibRaw's at the top and bottom by design (verified with the JPEG);
  anyone comparing against LibRaw must know this. Recorded in `docs/arw6-compression.md`.
- **Unknown fields:** the 480-byte block after the tile table and the 11 extra stream-header bytes are not
  interpreted; a future firmware could change them. The decoder validates what it uses and rejects the rest with
  `Unsupported`.
- **Oracle gaps:** LibRaw may fail on a few A7R VI files (issue #828). Mitigation: the embedded-JPEG check, recorded
  per file.
