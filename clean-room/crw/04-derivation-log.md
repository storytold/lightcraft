# CRW entropy-coding derivation log (P2)

Chronological record of experiments and oracle probes, arw6-style. The oracle is LibRaw
0.22.2, official pre-built Windows binaries installed by the maintainer in
a local `LibRaw-0.22.2\bin` folder (2026-10-12); **only `bin\unprocessed_raw.exe` (plus
`libraw.dll`) is executed, as a black box. The LibRaw source tree that ships alongside those
binaries is never opened, read or searched** — all structure below comes from feeding inputs
to the binary and observing outputs. The Python reference model lives in the gitignored
`plan/crw/model/` (maintainer decision #4); findings and probe campaigns are recorded here.

## 2026-10-12 — oracle baseline runs

`unprocessed_raw.exe -T <file>` over 7 public CC0 samples (D30, D60, 10D-a, 300D-a, G1, G5,
G6), each copied to a scratch directory first. Output: 16-bit TIFF of the full raw frame.
Oracle frame sizes equal SensorInfo raw sizes on all 7. Oracle "margins" (active area) differ
from SensorInfo's crop rectangle (e.g. D60: LibRaw top=12/left=64 vs SensorInfo (72,16)) —
LibRaw's own choice, noted, not adopted.

## 2026-10-12 — low-bits region: CONFIRMED (B6 settled)

`p2_oracle_lsb.py` (committed) compares the oracle's pixel LSBs against the payload's
candidate low-bits regions, both packing orders:

| File | Region [0:Q) LSB-first packing | Notes |
|---|---|---|
| D60, 10D-a, 300D-a, D30 | **100.000000 %** (1 629 584/1 629 584 … 809 536/809 536 bytes) | EOS prefix confirmed |
| G6 | **100.000000 %** (1 851 760/1 851 760) | at [0:Q), **not** [514:514+Q) — P1's reading of the G6 layout corrected: G6 low bits precede the 514 block, same as EOS |
| G5 | 47.34 % at [0:Q), 0.39 % at [514:514+Q) | anomaly, parked (scope decision #5: D60-class first) |
| G1 | no region (0.38 % everywhere) | 10-bit body, no low bits ✓ |

**Packing: LSB-first** — byte b holds pixels 4i..4i+3 with pixel 4i in bits 1:0 and pixel
4i+3 in bits 7:6, row-major over the full raw frame. MSB-first packing scores ~6 %.

Oracle code ranges: D60 max 4000, 10D max 4003, 300D max 4008, D30 max 3206, G6 max 4095
(12-bit class); G1 max 936 (10-bit). G5/G6 min ≈ 101/102 (no black subtraction in the oracle).

Corrections this forces on `03-payload-layout.md`: G6 (and presumably the G5 pattern, once
understood) is `[low bits Q][514-byte block][stuffed entropy][ff d9]`; the G6 "514 zeros at
offset 0" measured in P1 were the low bits of its first 2056 pixels (top masked row), and the
block after the low bits is *not* all-zero on G6. The EOS picture is unchanged.

## 2026-10-12 — entropy stream: structure findings from known-plaintext analysis

Using the D60 public file and its oracle decode (scripts `plan/crw/model/align.py`,
`sweep.py`, local):

- Budget check: unstuffed entropy = 5 049 856 bytes = 6.198 bits/pixel. Under the
  two-predictor diff model (B7) E[category] = 3.23 and H(category) ≈ 2.9 bits — model
  "code(k) + k magnitude bits" fits the budget almost exactly. Single-predictor E[k] = 5.9
  does not fit. **Two predictors by column parity supported.**
- **Bit order is LSB-first within each byte** (after ff00 unstuffing). Hand-verified at two
  anchors: stream start decodes as code `100` (stream order) + 10 magnitude bits = 511 =
  JPEG-style magnitude of −512 (first pixel 0 against predictor 512); the first token of row 7
  (byte-aligned at bit 20776) decodes as a 2-bit code + 6 magnitude bits = +55 against
  predictor 0 carried from row 6. B8's MSB-first recollection is **refuted**; so is its
  "no byte-stuffing" (every 0xff in the entropy data is followed by 0x00, which the decoder
  must strip — `03-payload-layout.md`).
- Predictors persist across rows (row 7's first diff is +55 against the row-6 value 0), i.e.
  no per-row reset; image start uses 512/512 (10-bit "hi" domain: pixel = hi·4 + lowbits).
- Zero diffs are **run-coded** (a plain per-value zero code is inconsistent with the bits in
  the 7 all-zero top rows, which compress to a strictly periodic 7-bit token).
- JPEG-style magnitude convention (negative d stored as d + 2^k − 1) consistent at both anchors.

## 2026-10-12 — black-box probe campaign 1 (D60 body, patch-in-place)

Harness `plan/crw/model/probe.py` (local): takes the public D60 file, zeroes the low-bits
region (so TIFF pixel = hi·4), overwrites the entropy region with chosen bits (LSB-first
packed, ff restuffed), optionally patches the DecoderTable selector, runs the oracle, reads
decoded rows. First probes (filler = repeated bit):

| Entropy bits | Row 0 decoded (hi) | Reading |
|---|---|---|
| all 0 | 497 505 490 498 483 491 476 484 … (parities fall by 7) | repeated all-zero token = k=3 code + mag `000` = −7 per parity; first token longer (−15, k=4) |
| all 1 | 512 512 512 … | all-ones token(s) = zero diffs |
| `100` + 511 + zeros | 16144 505 16137 498 … | no clamping in the oracle; convention probe |

Then `plan/crw/model/treewalk.py`: adaptive prefix expansion. Three aborted attempts taught
the method its final shape: (1) a race — the first walker's process survived its kill and
fought a second one over a shared probe file (fixed with per-PID work files; both result
sets discarded as corrupt); (2) enumerating magnitude bits explodes 2^k-fold; (3) leaf
detection via an assumed magnitude convention fails because the convention is *not* plain
JPEG (single-bit probes behind a known codeword give non-binary-weight responses).

## 2026-10-12/13 — truncation probing of the real stream (campaign 2)

`plan/crw/model/boundary.py` / `boundary2.py`: feed the oracle the real stream truncated at
bit B plus a constant filler. `boundary2`'s agreement scan (decode under 0-filler AND
1-filler; count leading pixels where both agree and match the real decode) yields **exact
token boundaries**: agreement can only grow when a token completes inside [0,B).

Zero-region result (D60, table 0, bits 0–120 ≈ pixels 0–321, all-zero rows; data in
`bounds2_zero.json`, local):

| token bits (stream order, LSB-first in bytes) | len | emits pixels |
|---|---|---|
| `1001111111111111` | 16 | 1 (pix 0, d = −512) |
| `011111111111100111111111` | 24 | 2 (zeros) |
| `11111011` | 8 | 61 → ends exactly at pixel 64 (EOB) |
| `0000101111100000` | 16 | 1 (pix 64, first of block 1) |
| `1110000` | 7 | 1 |
| `1` | 1 | 62 → ends exactly at pixel 128 (**1-bit EOB**) |
| `11111011` | 8 | 2 |
| `11111101` | 8 | 63 (ends at pixel 193) |
| `0111111` | 7 | 1 |
| `010111111` | 9 | 63 |
| `11011111` | 8 | 63 |
| `11101111` | 8 | 1 |

Established: **64-pixel block structure** (B4's "blocks of 64" holds); an EOB-style token
ends a block; the same bit string decodes differently in different blocks (`11111011` = EOB
in block 0 but a 2-pixel token later) → at least two tables/contexts in play (B5's two-table
recollection pointing the right way). First-of-block tokens are long (16 bits here, carrying
the −512 initial diffs). The value-token magnitude convention is still open — it is *not*
plain JPEG — and is being extracted from a second agreement scan over row 7's textured data
(`bounds2_row7.json`).

## 2026-10-13 — flip probing (campaign 3): code/magnitude split read directly

`plan/crw/model/flipscan.py`: flip one bit of the real stream at a time (real
continuation), diff the full decode against the real decode. A flipped magnitude bit
changes exactly one pixel by a power of two; a flipped code bit derails the parse.

First result, the pixel-0 token `1001111111111111` (16 bits): bits 0–7 are the code
(`10011111`), bits 8–15 are an 8-bit magnitude, weights 1,2,4,…,128 in stream order
(LSB-first), and the decoded value responds **linearly** across the would-be JPEG sign
boundary (pixel0 = m − 255 over the whole probed range m = 127…255): the magnitude
convention is offset-style, not JPEG's two-branch rule, at least for this token. A full
flip scan over a textured region (row 11) is mapping code and magnitude bits for ~25
consecutive tokens.

## 2026-10-13 — bit order corrected: MSB-first; token structure pinned

A systematic contradiction (magnitude fields appearing split across bytes, "token
boundaries" clustering at 8 bits) resolved when the flip map was re-plotted in MSB-first
bit coordinates: an early mis-step had locked the analysis into LSB-first reading, which
mirrors every byte. **The stream is MSB-first after ff00 unstuffing.** In MSB coordinates
every magnitude field is contiguous and the JPEG rule is confirmed by exact branch-flip
deltas (e.g. flipping the top magnitude bit of a k=5, m=2 token: −29 → +18, Δ+47 observed).

Flip-verified token chain (D60 table 0, pixels 34994–34999, bits 20784–20819):

| bits | table | meaning | pixels |
|---|---|---|---|
| `01` + `00010` | A | k=5, m=2 → −29 | 34994 |
| `11111100` + `11110` | B | run 1 + k=5, m=30 → +30 | 34995 (zero), 34996 |
| `1100` + `1` | A | k=1 → +1 | 34997 |
| `00` + `010` | B | k=3, m=2 → −5 | 34998 |
| `00` + `010` | A | k=3 → −5 | 34999 |

Established:
- **Two Huffman tables, strictly alternating per token (A, B, A, B, …)** — forced by
  prefix-freeness: the verified codes cannot coexist in one table or in parity-assigned
  tables, and alternation renders both tables prefix-free. (This refines B5.)
- Tokens carry a **(zero-run r, size s)** pair: r zero-diff pixels plus one value of
  size s; s = 0 codes are pure runs/EOB. Predictors: two, by pixel parity, init 512,
  carried across rows (flip cascades propagate stride-2 to the image end).
- The 10-bit "hi" plane is nearly constant in dark/flat areas — the reason the low-bits
  plane exists: the sensor noise lives in the 2 raw LSBs, keeping the entropy-coded
  plane highly compressible.

Open: exact placement of the bundled zeros (before vs after the value) in corner cases,
EOB/block semantics, the first-token special case at pixel 0, and the complete code
tables for DecoderTableNumber 0, 1, 2 (being extracted by continued flip scans plus the
known-plaintext aligner `plan/crw/model/aligner4.py`).

## 2026-10-13 — complete token grammar established (D60, table 0)

The MSB-faithful agreement scan (`bounds3_row11_msb.json`, 187 exact micro-boundaries over
bits 20776–21500) plus a merge analysis settled the remaining structure. Sub-boundary rises
occur inside tokens (fillers can reproduce zero pixels), and merging every zero segment into
the following value yields a token stream that is fully consistent — zero semantic conflicts
and zero prefix violations — under exactly one model:

- **64-pixel blocks** (image order). The token that starts at a block's first pixel comes
  from a **"first" Huffman table**; every other token from a **"second" table**. (The
  apparent two-alternating-tables stage was a phantom: fitted as alternating tables, the two
  halves come out *identical*, collapsing to one "second" table. B5's recollection — "one
  table for the first element of a block, another for the rest" — was exactly right.)
- **Token = code → (run r, size s)**: r zero-diff pixels, then one value of size s coded as
  s JPEG-rule magnitude bits (MSB-first). s = 0 codes are EOB (zeros to end of block).
- Two DPCM predictors by pixel parity, init 512, never reset.

Second-table codes verified so far (D60, DecoderTableNumber 0): `00`=(0,3), `100`=(0,2),
`101`=(0,5), `1100`=(0,1), `111011`=(1,2), `111100`=(1,3), `111101`=(1,1), `11111100`=(1,5),
`1111111000`=(2,1), `1111111100`=(2,3), `11111110111100`=(16,1), and 16-bit codes
(3,1) (4,1) (4,2) (5,1) (5,4) (5,5) (6,3) (6,4) (8,1). First-table: `101`=(0,2),
`111110`=(0,10) at pixel 0. The code set has a canonical Huffman shape.

A seeded known-plaintext aligner (`aligner6.py`) is now walking the whole 6.5-Mpixel stream
to learn the remaining codes of both tables and verify every pixel; the same harness then
runs on files with DecoderTableNumbers 1 and 2 and on the other bodies.

## 2026-10-13 — the carry chain, and the first/second table split confirmed

Synthetic sentinel probes plus the zero-region MSB scan established two more rules:

- **Carry on block DC**: each 64-pixel block's first diff is coded relative to the
  previous block's *combined* first diff (`diff0 += carry; carry = diff0`). This single
  rule reconciles pixel 0 (coded −512), pixel 64 (coded +512 → effective 0) and pixel
  128 (coded 0 → effective 0) with plain parity chains init 512 — the earlier
  "predictor anomalies" were all carry.
- **The first-of-block token really does use a separate Huffman table** (`101` = (0,2)
  there vs (0,5) in the main table, verified by decode-iteration at pixel 35008), i.e.
  the classic first/second table structure of B5.
- The S-EOB of the main table is 9 bits (`111111011`); agreement boundaries inside
  all-zero blocks proved unreliable (fillers self-heal on zeros), so zero-region token
  readings are being re-derived by decode-iteration rather than scans.

Current state: the main table has ~20 verified codes; a decode-and-learn loop
(`plan/crw/model/aligner8.py`) walks the real stream, decoding deterministically with
known codes and learning first-table/main-table rows at each stall, each verified
against the oracle pixels.

## 2026-10-13 (cont.) — row reset; full model assembled

Two refinements completed the structural model, each validated against the stream:

- **Chains reset to 512 at every row start** (not row padding — a padded-grid variant was
  tried and refuted by block alignment at pixel 35008). With the reset, every all-zero row
  needs one k=10 "−512" value token at its first pixel, which is exactly what the stream
  shows: a 15-bit second-table code (15,10) at the first row crossing, mag 511 → −512.
- The verified second table gains `01` = (0,5) (flip-verified at pixel 34994); both `01`
  and `101` map to (0,5) — duplicate leaves are legal and present.

Model now complete: flat 64-sample blocks; first/second Huffman tables (first = the token
starting each block); (run ≤ 15, size) leaves with JPEG MSB magnitudes; (0,0) mid-block =
EOB; per-block DC carry; parity chains init 512, reset at row starts; low-bits recombined
as the 2 LSBs. Remaining: complete code tables per DecoderTableNumber, extracted by the
known-plaintext walker (`aligner11.py`), then full-corpus verification.

## 2026-10-13 — SELECTOR-0 TABLES COMPLETE; four bodies decode end-to-end

The greedy known-plaintext extractor (`plan/crw/model/aligner12.py`: deterministic decode
with deep trial-validation per unknown code, flip-scans resolving the few ambiguous sites)
completed the DecoderTableNumber-0 Huffman tables. Results:

- **D60 (public): the full 6 518 336-sample frame decodes from bit 0 against the oracle,
  pixel-exact,** in seconds, with only the final 3 samples open (stream tail handling).
- **10D, 300D and Digital Rebel (selector 0) decode end-to-end with the same table** —
  per-selector tables are shared across bodies, as the plan hoped. Each file exercised a
  few additional rare high-k codes (merged).
- Final selector-0 tables: 11 first-table codes, 131 second-table codes (incl. the 16-bit
  `11111111xxxxxxxx` run/size family); saved as `logs/table_sel0_d60class.json`.
- The decisive last fix: a hand-guessed 15-bit seed blocked the true 11-bit (1,7) code by
  prefix; a flip-scan at the stall site read the real code and magnitude (m = 83) directly.

Selector 1 (from the D30) and selector 2 (from the second 10D file) extractions running
with the same tooling. Remaining after that: G1/G5/G6 PowerShot handling (10-bit G1; the
G5 low-bits anomaly), the stream-tail rule, then the frozen format document and the
Python reference decoder for the P4 verification pass.

## 2026-10-13 — selectors 1 and 2; G1, G6, private files; G5 isolated

- Anchor hunting (truncation-probe agreement at byte boundaries + bit-level pinning)
  plus structural bootstrap (reading each file's opening k=10 tokens and the steady
  zero-block unit directly off the bits) made extraction routine for new table sets.
- **Selector 1** (extracted from private CRW_0476, topped up from the public D30):
  D30 **EXACT**, PowerShot **G1 EXACT** (10-bit, no low-bits plane — detected by the
  frozen unstuffed-ff scan), CRW_0476 **EXACT**. Cross-body sharing confirmed again.
- **G6 EXACT** with the selector-0 table once the low-bits rule was frozen (presence
  by the ff-scan; entropy always at low-bits-size + 514).
- **G5 remains anomalous**: its low-bits plane matches the oracle's LSBs ~97.5 % in the
  top rows (not 100 %), degrading to 0 % in the lower half, and no constant bit-shift
  explains it; its entropy stream also diverges from the selector-1 parse within the
  first rows. G5 is documented as unresolved and will be refused (or preview-only) by
  the decoder pending further work.
- Selector 2 (from the public 10D-b) extraction completing.

## 2026-10-13 — P2 complete; tables frozen; 28 of 29 files verify EXACT

A final systematic fix (first-table magnitudes reach s = 11+: the carry-coded block DC
spans ±2046) unlocked selector 2 (extracted from the 300D-b after the saturated 10D-b
proved hostile learning ground). Table top-ups over every private file completed the
three table sets (12 first + 154/161/162 second codes). The final sweep: **28 of 29
corpus files decode with zero mismatching samples** against LibRaw 0.22.2 — every EOS
body and selector, the 10-bit G1, the G6, and all 18 private D60s. The PowerShot G5
remains the open anomaly (stream decodes, values diverge from sample 2056; low-bits
plane fits no tested layout) and is excluded pending follow-up.

Frozen at this commit: `06-crw-format.md` (the format), `logs/table_sel{0,1,2}.json`
(the tables), `07-verification.md` (the evidence). P3 (black/WB/colour records) and P5
(the Rust decoder) proceed only after maintainer review, per the plan.

## 2026-10-13 — P5: Rust decoder lands; P3 colour/geometry derived

The maintainer approved the freeze. `crates/raw/src/vendor/crw.rs` implements the frozen
model (tables generated from `logs/table_sel{0,1,2}.json` into `crw_tables.rs`); the Rust
decoder matched the oracle **sample-exact on all 28 decodable corpus files on its first
run** (`crates/raw/tests/crw_oracle.rs`, ~65 ms per 8 MP file). The G5 is refused by model
string. Colour/geometry work (CFA phases by chroma correlation + green-diagonal tests,
border black levels, 0x10a9/0x10aa white balance, rotation and timestamps) is documented in
`08-color.md`; ExifTool prose pages and a raw.pixls.us repository-listing fetch (to recover
the 11 corpus files' stable URLs for `cargo xtask corpus`) are logged in `sources.md`.

## 2026-10-13 — Freeze reopened (revision 2): 1,516 D60 files, two corrections

The maintainer put all his D60 files in `../RawImages` (1,516). The revision-1 decoder
failed on 232 of them, which the 29-file corpus could not have shown:

- **223 "undecodable entropy code".** The second tables were learned from exercised codes
  only: selector 0 had 154 of 162 symbols, selector 1 had 161. Laying out the 16-bit code
  range showed exactly eight unassigned slots inside selector 0's range and one inside
  selector 1's, against exactly eight and one missing symbols: (7..14, 10) and (14, 10).
  For selector 1 the assignment follows by elimination; the learner (`aligner12.py`, same
  known-plaintext method, LibRaw as the black box) confirmed it and assigned selector 0's
  eight from the failing files.
- **9 files decoded as 10-bit garbage.** Their low-bits plane holds no 0xff byte, because
  the lowest bit of nearly every sample is 0, so the revision-1 presence rule said "no
  plane". My first reading (an all-zero plane) was wrong: I had counted 0xff bytes, not
  nonzero bytes; the plane has 1.5 M nonzero bytes. The rule now also accepts the zero
  header found right after the plane.

Result: 1,516 of 1,516 EXACT, and the first corpus unchanged (`07-verification.md`).
A lesson for the remaining unverified bodies: a table learned from a few files is complete
only when its symbol count is; selector 2 was already at 162.
