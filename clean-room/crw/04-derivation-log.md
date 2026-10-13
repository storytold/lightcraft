# CRW entropy-coding derivation log (P2)

Chronological record of experiments and oracle probes, arw6-style. The oracle is LibRaw
0.22.2, official pre-built Windows binaries installed by the maintainer at
`C:\Users\Robert\LibRaw-0.22.2\bin` (2026-10-12); **only `bin\unprocessed_raw.exe` (plus
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
