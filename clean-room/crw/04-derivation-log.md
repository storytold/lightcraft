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

Then `plan/crw/model/treewalk.py`: adaptive prefix expansion — a prefix whose first decoded
pixel is identical under 0-filler and 1-filler has its first token complete; leaves give
(token bits, first diff, run behaviour). Campaign running for DecoderTableNumber 0, then 1, 2.
