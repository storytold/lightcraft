# Sony ARW6 Decoder Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Decode Sony "Compressed RAW 2" ARWs (TIFF compression 32766, ILCE-7RM6 Compressed and Compressed HQ) to the sensor CFA, bit-exact against the Phase 0 reference model, in pure Rust.

**Architecture:** `crates/raw/src/llvc.rs` holds the camera-agnostic codec core (RDD 34 VLD with Sony's two deviations, parity dequantiser, 5/3 inverse lifting with phases). `crates/raw/src/vendor/arw6.rs` holds the Sony container (tile table, tile/stream headers, half layout on the sensor-anchored grid), the colour reconstruction and the companding table, and `arw.rs` gains one match arm. A test-only encoder (`arw6_testenc.rs`) produces synthetic files so every layer round-trips exactly without any committed media.

**Tech Stack:** Rust 2021 workspace crate `lightcraft-raw` (L1): `lightcraft-tiff` (IFD / strip access, test TIFF writer), `rayon` (tiles, streams), `proptest` + `sha2` (dev). No new dependencies.

**Spec:** `docs/superpowers/specs/2026-10-09-sony-arw6-decoder-design.md` (the bitstream model, with every value this plan quotes). Local artefacts: `plan/arw6/` (reference python model, `arw6_curve.txt`, oracle binary; gitignored).

## Global Constraints

- Pure Rust, `#![forbid(unsafe_code)]`; the crate denies `unwrap`, `expect`, `panic`, `unreachable`, `todo`, indexing in non-test code: use `get()`, `let … else`, checked / saturating maths for every file-derived number.
- Clean room: never open LibRaw, rawspeed, dnglab, RawTherapee, Jiangtherapee, arw6_decode or JixelLight source. Every rule carries a comment naming its RDD 34 section or the Phase 0 experiment (spec *Background*).
- Never commit media: corpus files come through `cargo xtask corpus --download` and are gitignored; synthetic fixtures are generated in tests, not checked in.
- Numbers from the spec: codes are 12-bit (0..=4095), LL3 DPCM base 2048, residual rows per half 8, TU = 16 plane rows, companding knee 1428 (identity below), curve length 2668, curve(4095) = 39002, output unit = 2 × 14-bit DN (black 1024 = 2 × tag, white = 2 × WhiteLevel), first-half shift `s` ∈ 0..=5 read from the stream (FF 0, APS-C 3).
- `cargo xtask ci` (fmt, clippy -D warnings, tests, layers, assets, wasm) green before every commit; one commit per task, message prefix `LR-IMP-FORMATS:`.
- Decode budget: ≤ 1 s for the 10016×6672 FF file (release build).

## Review Focus

- A half whose byte budget runs out before its lines do (truncated or hostile file): every line must end within the half or the tile is `Corrupt`; the bit reader yields zeros past its end, so the check is on consumed bits (Task 4 test `half_too_short_is_corrupt`).
- Tile tables that lie: zero tiles, offsets past the strip, overlapping tiles, tiles that do not cover the image, odd sizes, a plane width not divisible by 8 → error, never a panic or a half-filled image (Task 3 tests `tile_table_rejects_*`).
- A crop offset other than the two seen (s = 1, 2, 4, 5): the geometry formulas are generic; synthetic round trips for every s in 0..=5 pin them (Task 4 test `round_trip_every_shift`).
- Saturated highlights: codes above 4095 clip, the chroma sums use clipped greens while the green predictor uses unclipped ones, otherwise R/B near clipped areas are off by one or more codes (Task 5 test `colour_clips_like_the_oracle`).
- A file whose tile table and headers are valid but whose stream data is garbage: header-mode import must succeed (so the photo imports) and the full decode must fail with `Corrupt` (so it becomes preview-only with a reason), never produce an image (Task 5 test `garbage_streams_import_then_fail`).

---

### Task 1: VLD line decoder (`llvc.rs`)

**Files:**
- Create: `crates/raw/src/llvc.rs`
- Modify: `crates/raw/src/lib.rs` (add `pub(crate) mod llvc;` next to the other internal modules)
- Test: inline `#[cfg(test)] mod tests` in `llvc.rs`

**Interfaces:**
- Consumes: `crate::vendor::pef::Bits` (MSB-first reader, `get(k)`, `consumed_bits()`, yields zeros past the end), `crate::{Result, RawError}`.
- Produces:
  - `pub(crate) const MAX_DPT: u32 = 24;`
  - `pub(crate) fn vld_decode_line(bits: &mut Bits<'_>, width: usize) -> Result<Vec<i32>>` — decodes one RDD 34 coefficient line of `L = width.div_ceil(4)` sets and returns exactly `width` values (the padding values of the last set are dropped). Errors (`RawError::Corrupt`) when a DPT code would exceed `MAX_DPT` or when the reader has consumed more bits than the source holds (`bits.consumed_bits() > 8 * source_len`, checked by the caller through `Bits::overrun()` is too lax: the function takes `source_bits: usize` as a third parameter and checks after each DPT/ABS read).

Signature decision: `pub(crate) fn vld_decode_line(bits: &mut Bits<'_>, source_bits: usize, width: usize) -> Result<Vec<i32>>`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/raw/src/llvc.rs, mod tests
use super::*;
use crate::vendor::pef::Bits;

fn bits_of(s: &str) -> Vec<u8> {
    let mut out = vec![0u8; s.len().div_ceil(8)];
    for (i, c) in s.bytes().enumerate() { if c == b'1' { out[i / 8] |= 0x80 >> (i % 8); } }
    out
}
fn line(s: &str, width: usize) -> (Vec<i32>, usize) {
    let src = bits_of(s);
    let mut b = Bits::new(&src);
    let v = vld_decode_line(&mut b, src.len() * 8, width).unwrap();
    (v, b.consumed_bits())
}

#[test]
fn rdd34_figure_6_9_worked_example() {
    // RDD 34:2015 §6.3.6, Figure 6.9: 8 sets, 91 bits
    let s = "1000010110000010000100110000101000010100110100000000000011000010010001001010011100110000000";
    let (v, used) = line(s, 32);
    assert_eq!(v, vec![6, 0, -8, 4, 0, 0, 0, 0, -9, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(used, 91);
}
#[test]
fn sony_zero_at_dpt0_enters_run_state() {
    // Phase 0 vector "dev1": a "0" at DPT 0 is a zero set that moves to DPTs = 1; the next set uses the "1 0^n 1" code
    let (v, used) = line("01001101000011001010", 8);
    assert_eq!(v, vec![0, 0, 0, 0, 5, 0, -3, 1]); assert_eq!(used, 20);
}
#[test]
fn sony_run_value_means_minus_one_zero_sets() {
    // Phase 0 vector "dev2": run code value 3 = two more zero sets, then "0^n 1" post-run DPT
    let (v, used) = line("001101100000000", 16);
    assert_eq!(v, vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0]); assert_eq!(used, 15);
}
#[test]
fn end_of_line_zero_code() {
    let (v, used) = line("1001110100101100100", 12);
    assert_eq!(v, vec![3, -1, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0]); assert_eq!(used, 19);
}
#[test]
fn dpt_moves_up_down_and_to_zero() {
    let (v, used) = line("1011000100101111111110011101001011111100000", 20);
    assert_eq!(v, vec![1, 0, 0, 0, 7, 7, -7, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0]); assert_eq!(used, 43);
}
#[test]
fn all_zero_line_is_two_bits_and_width_truncates() {
    let (v, used) = line("00", 6);           // L = 2 sets, width 6 keeps 6 of 8 values
    assert_eq!(v, vec![0; 6]); assert_eq!(used, 2);
}
#[test]
fn hostile_input_errors_instead_of_panicking() {
    let ones = vec![0xffu8; 64];
    let mut b = Bits::new(&ones);
    assert!(matches!(vld_decode_line(&mut b, 512, 16), Err(RawError::Corrupt(_))));   // DPT climbs past MAX_DPT
    let short = bits_of("10");                                                           // needs more bits than it has
    let mut b = Bits::new(&short);
    assert!(matches!(vld_decode_line(&mut b, 2, 16), Err(RawError::Corrupt(_))));
}
proptest::proptest! {
    #[test]
    fn random_bits_never_panic(src in proptest::collection::vec(proptest::num::u8::ANY, 0..256), width in 1usize..300) {
        let mut b = Bits::new(&src);
        let _ = vld_decode_line(&mut b, src.len() * 8, width);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lightcraft-raw llvc::`
Expected: compile error, `vld_decode_line` not found.

- [ ] **Step 3: Implement `vld_decode_line` in `crates/raw/src/llvc.rs`**

Module doc: "RDD 34:2015 LLVC codec core as Sony's ARW6 uses it; see docs/arw6-compression.md". Port the state machine of RDD 34 §6.3 Figures 6.5–6.8 as the Phase 0 reference (`plan/arw6/scratch/vld.py::vld_line`) implements it; the parts the signature and tests do not determine:

```text
state: ABS[4]=0, DPT=0, Cnt=L, DPTs=0, ABSv=0, ABSe=0, DPTv=1, DPTe=0, ZR=0, out=[]
loop:
  if Cnt==0 {DPTv=0}; if Cnt==1 {DPTe=1}; if Cnt>0 {Cnt-=1}; if ZR>0 {ZR-=1}
  if DPTv==1:
    DPTs==0: bit b.  b==0: (Sony dev. 1) if DPT==0 {DPTs=1}            // RDD 34: no state change
                     b==1: bit c. c==0: n = count of 0 bits until a 1 (n>MAX_DPT → Corrupt); DPT += n+1
                                  c==1: n = zeros read while n < DPT-1 and bit==0;
                                        if n >= DPT-1 {DPT=0; DPTs=1} else {DPT -= n+1}
    DPTs==1: bit b. b==1: n zeros until a 1 (bounded); DPT=n+1; DPTs=0
                    b==0: lim = ceil(log2(Cnt+2)); m=1; while m<lim and bit==0 {m+=1}
                          if m==lim {ZR=Cnt+2; DPTs=2}                   // "0^lim": rest of line is zero
                          else {ZR=1; repeat m times {ZR = ZR<<1 | bit}; DPTs=2}
    DPTs==2: if ZR==1 { n zeros until a 1 (bounded); DPT=n+1; DPTs=0 }   // (Sony dev. 2: ZR==1, RDD: 0)
  if ABSv==1: for i in 0..4 { v=ABS[i]; if v!=0 && bit==1 {v=-v}; out.push(v) }   // signs of the previous set
  if ABSe==1: return out truncated to width
  ABSv=DPTv; ABSe=DPTe
  if DPTv==1: for i in 0..4 { ABS[i] = get(DPT) }
```

After every read check `bits.consumed_bits() <= source_bits` else `Corrupt("ARW6 VLD past the end of its half")`. `lim` is `(Cnt + 2).next_power_of_two().trailing_zeros()` adjusted so that lim = ⌈log2(Cnt+2)⌉ (Cnt+2 = 2 → 1, 3 → 2, 4 → 2, 5 → 3).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lightcraft-raw llvc::`
Expected: all 8 pass.

- [ ] **Step 5: Lints and commit**

Run: `cargo fmt && cargo clippy -p lightcraft-raw --all-targets -- -D warnings && cargo xtask ci`
Expected: green.

```bash
git add crates/raw/src/llvc.rs crates/raw/src/lib.rs
git commit -m "LR-IMP-FORMATS: RDD 34 VLD line decoder with Sony's ARW6 deviations"
```

---

### Task 2: Dequantiser and 5/3 inverse lifting (`llvc.rs`)

**Files:**
- Modify: `crates/raw/src/llvc.rs`
- Create: `crates/raw/src/vendor/arw6_testenc.rs` (test-only; this task adds the forward transform, Task 3 the rest)
- Modify: `crates/raw/src/vendor/mod.rs` (add `#[cfg(test)] pub(crate) mod arw6_testenc;`)

**Interfaces:**
- Produces (llvc.rs):
  - `pub(crate) fn dequant(q: i32, qi: u32) -> i32` — Phase 0 parity rule: 0 → 0; qi = 0 → q; else `sign(q) · (((2|q| + 1) << (qi − 1)) − (|q| & 1))`.
  - `#[derive(Clone, Debug, PartialEq, Eq)] pub(crate) struct Plane { pub width: usize, pub height: usize, pub data: Vec<i32> }` with `pub fn zeros(width: usize, height: usize) -> Plane` and `pub fn row(&self, y: usize) -> Option<&[i32]>`.
  - `pub(crate) fn band_rows(n: usize, phase: u8) -> (usize, usize)` — `(low, high)` sample counts of a signal of `n` samples whose low-pass samples sit at indices ≡ `phase` (mod 2): low = `(n + 1 − phase) / 2`, high = `n − low`.
  - `pub(crate) fn inverse_53_1d_rows(low: &Plane, high: &Plane, phase: u8) -> Result<Plane>` — vertical; `inverse_53_1d_cols(low, high, phase)` — horizontal.
  - `pub(crate) fn inverse_53_2d(ll: &Plane, lh: &Plane, hl: &Plane, hh: &Plane, vphase: u8) -> Result<Plane>` — vertical first on (ll, lh) and (hl, hh), then horizontal (phase 0) on the two results (RDD 34 §6.5 order: V IWT then H IWT). Height = ll.height + lh.height, width = ll.width + hl.width; mismatched band sizes → `Corrupt`.
  - `pub(crate) struct Bands3 { pub ll3: Plane, pub hl3: Plane, pub lh3: Plane, pub hh3: Plane, pub hl2: Plane, pub lh2: Plane, pub hh2: Plane, pub hl1: Plane, pub lh1: Plane, pub hh1: Plane }`
  - `pub(crate) fn reconstruct3(b: &Bands3, phases: [u8; 3]) -> Result<Plane>` — level 3 with `phases[2]`, level 2 with `phases[1]`, level 1 with `phases[0]`.
- Produces (arw6_testenc.rs, `#[cfg(test)]`): `pub(crate) fn forward_53_1d_rows(x: &Plane, phase: u8) -> (Plane, Plane)` (low, high), `forward_53_1d_cols`, `pub(crate) fn forward_53_2d(x: &Plane, vphase: u8) -> (Plane, Plane, Plane, Plane)` (ll, lh, hl, hh; horizontal first then vertical, the exact inverse of `inverse_53_2d`), `pub(crate) fn forward3(x: &Plane, phases: [u8; 3]) -> Bands3`, `pub(crate) fn quantize(v: i32, qi: u32) -> i32` = `sign(v) · (|v| >> qi)` (exact inverse of `dequant` on representable values).

Lifting (RDD 34 §6.5, whole-sample symmetric extension of the interleaved signal: X[−1] := X[1], X[n] := X[n−2]):
- phase 0 (low at even i): `X[2i] = L[i] − (H[i−1] + H[i] + 2) >> 2`, then `X[2i+1] = H[i] + (X[2i] + X[2i+2]) >> 1`; out-of-range H / X come from the extension (H[−1] := H[0]; for the last even sample without a right neighbour, H[nH] := H[nH−1] and X[n] := X[n−2]).
- phase 1 (low at odd i): `X[2i+1] = L[i] − (H[i] + H[i+1] + 2) >> 2`, then `X[2i] = H[i] + (X[2i−1] + X[2i+1]) >> 1`, same extension rule.
- Arithmetic shifts on `i32` (`>>` floors), `i64` for sums if an overflow test demands it.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn dequant_parity_rule() {
    for (qi, table) in [(0u32, [-3, -2, -1, 0, 1, 2, 3]), (1, [-6, -5, -2, 0, 2, 5, 6]), (2, [-13, -10, -5, 0, 5, 10, 13]), (3, [-27, -20, -11, 0, 11, 20, 27])] {
        for (q, v) in (-3..=3).zip(table) { assert_eq!(dequant(q, qi), v, "q={q} qi={qi}"); }
    }
}
#[test]
fn band_rows_counts() {
    assert_eq!(band_rows(1093, 0), (547, 546)); assert_eq!(band_rows(1093, 1), (546, 547));
    assert_eq!(band_rows(1668, 0), (834, 834)); assert_eq!(band_rows(5, 1), (2, 3)); assert_eq!(band_rows(1, 1), (0, 1));
}
fn col(v: &[i32]) -> Plane { Plane { width: 1, height: v.len(), data: v.to_vec() } }
#[test]
fn inverse_53_hand_vectors() {
    // computed by hand from RDD 34 §6.5 with X[-1] := X[1], X[n] := X[n-2]; checked against the Phase 0 model
    assert_eq!(inverse_53_1d_rows(&col(&[10, 20]), &col(&[3, -4]), 0).unwrap().data, vec![8, 17, 20, 16]);
    assert_eq!(inverse_53_1d_rows(&col(&[10, 20]), &col(&[3, -4]), 1).unwrap().data, vec![13, 10, 12, 22]);
    assert_eq!(inverse_53_1d_rows(&col(&[10, 20, 30]), &col(&[3, -4]), 0).unwrap().data, vec![8, 17, 20, 22, 32]);
    assert_eq!(inverse_53_1d_rows(&col(&[3, -4]), &col(&[10, 20, 30]), 1).unwrap().data, vec![5, -5, 9, -17, 13]);
}
#[test]
fn inverse_53_rejects_mismatched_bands() {
    assert!(inverse_53_1d_rows(&col(&[1, 2, 3]), &col(&[1]), 0).is_err());
}
proptest::proptest! {
    #[test]
    fn forward_then_inverse_is_identity(w in 1usize..24, h in 1usize..40, p1 in 0u8..2, p2 in 0u8..2, p3 in 0u8..2, seed in proptest::num::u64::ANY) {
        use crate::vendor::arw6_testenc::{forward3, forward_53_2d};
        let mut s = seed; let mut next = || { s ^= s << 13; s ^= s >> 7; s ^= s << 17; (s % 8191) as i32 - 4095 };
        let x = Plane { width: w, height: h, data: (0..w * h).map(|_| next()).collect() };
        let (ll, lh, hl, hh) = forward_53_2d(&x, p1);
        prop_assert_eq!(inverse_53_2d(&ll, &lh, &hl, &hh, p1).unwrap(), x.clone());
        if w >= 8 && h >= 8 { prop_assert_eq!(reconstruct3(&forward3(&x, [p1, p2, p3]), [p1, p2, p3]).unwrap(), x); }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lightcraft-raw llvc::`
Expected: compile errors for the new names.

- [ ] **Step 3: Implement `dequant`, `Plane`, `band_rows`, the 1-D inverses, `inverse_53_2d`, `Bands3`, `reconstruct3` in `llvc.rs`, and the forward transforms, `forward3`, `quantize` in `arw6_testenc.rs`**

The forward 1-D (test only) is the lifting run backwards with the same extension: `H[i] = X[h_i] − (X[h_i − 1] + X[h_i + 1]) >> 1`, then `L[i] = X[l_i] + (H[left] + H[right] + 2) >> 2`, X and H extended symmetrically (`plan/arw6/scratch/fwd.py::fwd1d`).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lightcraft-raw llvc::`
Expected: pass, including 256 proptest cases.

- [ ] **Step 5: Lints and commit**

Run: `cargo fmt && cargo xtask ci` — Expected: green.

```bash
git add crates/raw/src/llvc.rs crates/raw/src/vendor/arw6_testenc.rs crates/raw/src/vendor/mod.rs
git commit -m "LR-IMP-FORMATS: ARW6 parity dequantiser and 5/3 inverse lifting with phases"
```

---

### Task 3: Container parsing and the test encoder (`arw6.rs`, `arw6_testenc.rs`)

**Files:**
- Create: `crates/raw/src/vendor/arw6.rs`
- Modify: `crates/raw/src/vendor/arw6_testenc.rs`, `crates/raw/src/vendor/mod.rs` (add `pub mod arw6;`)
- Test: inline tests in `arw6.rs`

**Interfaces:**
- Consumes: `llvc::{vld_decode_line, dequant, Plane, band_rows}` (Task 1–2), `crate::vendor::pef::Bits`, `lightcraft_tiff::{IfdBuilder, TiffWriter, ImageData, Value, ByteOrder, tags}` (tests only).
- Produces (arw6.rs):
  - `pub(crate) struct TileEntry { pub offset: usize, pub x: usize, pub y: usize, pub width: usize, pub height: usize }`
  - `pub(crate) fn parse_tile_table(strip: &[u8], width: usize, height: usize) -> Result<Vec<TileEntry>>` — `u32` count (1..=16), `u32` zero, then 24-byte entries (`u64` offset relative to the strip, `u32` x, y, w, h, all little-endian). Rejects: count 0 or > 16, offset ≥ strip len, offsets not increasing, odd x/y/w/h, w/2 not a multiple of 8, tiles overlapping, union ≠ width × height, w·h > `crate::MAX_SAMPLES`. Tile i's bytes run from its offset to the next tile's offset (last: strip end).
  - `pub(crate) struct StreamRef { pub group: u8, pub comp: u8, pub word: usize, pub size_words: usize, pub index_words: usize, pub data_words: usize }`
  - `pub(crate) struct TileHeader { pub hs: usize, pub vs: usize, pub ntu: usize, pub streams: Vec<StreamRef> }` (13 streams in file order: g0 c0..2, g1 c0..2, g2 c0..2, g3 c0..2, g4 c0)
  - `pub(crate) fn parse_tile_header(tile: &[u8], tw: usize, th: usize) -> Result<TileHeader>` — word 0 bits (MSB first from bit 64): HS = bits 64..80, VS = 80..96, BBD = 102..108, NC = 112..115, NW = 115..118, NP = 118..128; require HS = tw, VS = th / 2, BBD = 16, NC = 3, NW = 3, NP = 0b1000000000 (else `Unsupported("ARW6 picture header variant …")`). Words 3..=7: count byte then `u24` big-endian stream sizes in words; counts must be 3, 3, 3, 3, 1. Stream word positions run from 8, cumulatively; each stream header word: `u16` index words, `u24` data words (big-endian); stream must fit in the tile and index + data + 1 ≤ size. `ntu = vs.div_ceil(16)`.
  - `pub(crate) struct Half { pub start: usize, pub len: usize, pub qi: [u32; 3] }` (byte offset within the tile)
  - `pub(crate) fn stream_halves(tile: &[u8], s: &StreamRef, ntu: usize) -> Result<Vec<Half>>` — 2·ntu halves. Index entries are 5 bytes for g0/g4 (`A:16 X:4 B:16 X:4`), 7 bytes for g1–g3 (`A:16 X:12 B:16 X:12`, three nibbles = QI of HL, LH, HH); the index table holds exactly ntu entries within `index_words` words. Half 0 of TU k = A bytes at the running data offset, half 1 = the following B bytes; all halves inside the stream's data area, else `Corrupt`.
- Produces (arw6_testenc.rs, `#[cfg(test)]`):
  - `pub(crate) struct BitWriter { bits: Vec<u8> }` with `put(&mut self, bit: bool)`, `put_n(&mut self, v: u32, n: u32)`, `finish(self) -> Vec<u8>` (zero-padded to a byte).
  - `pub(crate) fn vld_encode_line(values: &[i32], width: usize, out: &mut BitWriter)` — mirror of the decoder (`plan/arw6/scratch/tilewrite.py::vld_encode`): per set DPT = bit length of max |v|; emits the codes of Task 1 including the two Sony deviations; a run of zero sets of length r (after a zero set) emits `0^m 1 <m low bits of r+1>` with m = floor(log2(r + 1)) or `0^lim` when the zeros reach the line end.
  - `#[derive(Clone)] pub(crate) struct Qis { pub l1: [[u32; 3]; 3], pub l2: [[u32; 3]; 3], pub l3: [[u32; 3]; 3], pub res: u32 }` with `Qis::ZERO` and `Qis::REAL` (= spec: l1 = [[1,1,2],[2,2,3],[2,2,3]], l2 = [[0,0,1]; 3], l3 = [[0; 3]; 3], res = 2).
  - `pub(crate) struct TileSpec { pub s: u8, pub qi: Qis, pub m: Plane, pub c1: Plane, pub c2: Plane, pub res: Plane }` (planes W/2 × H/2 in 12-bit codes / signed residual)
  - `pub(crate) fn encode_tile(t: &TileSpec) -> Vec<u8>` — forward transforms with the phases of `s`, quantises every band with its QI (`quantize`), LL3 as DPCM lines (first value − 2048, then differences), packs each half in natural clipped order (spec *Frame geometry*), builds index tables, stream headers (extra 11 bytes `40 40 01 12 10 00 …`), group tables, word 1 sums, word 0 picture header; pads to 16-byte words and the tile to a multiple of 4096.
  - `pub(crate) fn arw6_file(tiles: &[(usize, usize, usize, usize, Vec<u8>)], width: usize, height: usize) -> Vec<u8>` — a little-endian TIFF: IFD0 (Make "SONY", Model "ILCE-7RM6", Orientation 1) with a SubIFD: ImageWidth/Length, BitsPerSample 14, Compression 32766, Photometric CFA, CFARepeatPatternDim 2 2, CFAPattern 0 1 1 2, BlackLevel (0x7310) 512 ×4, WhiteLevel (0x7312 as the existing path reads it: `t::WHITE_LEVEL`) 15360, strip = tile table (8 + 24·n bytes) + zero padding to 512 + the tiles at their offsets (x, y, w, h, bytes).

- [ ] **Step 1: Write the failing tests**

```rust
// arw6.rs tests
use super::*; use crate::vendor::arw6_testenc::*; use crate::llvc::Plane;

fn planes(w2: usize, h2: usize, seed: u64) -> (Plane, Plane, Plane, Plane) { /* M in 2048±200, c1/c2 in ±60, res in ±24, xorshift from seed */ }
fn one_tile(w: usize, h: usize, s: u8, qi: Qis) -> (Vec<u8>, TileSpec) { let (m, c1, c2, res) = planes(w / 2, h / 2, 7); let t = TileSpec { s, qi, m, c1, c2, res }; (encode_tile(&t), t) }

#[test]
fn tile_table_round_trips() {
    let (tile, _) = one_tile(64, 48, 0, Qis::ZERO);
    let file = arw6_file(&[(0, 0, 64, 48, tile.clone())], 64, 48);
    let strip = strip_of(&file);                       // helper: the SubIFD strip bytes via lightcraft_tiff
    let t = parse_tile_table(strip, 64, 48).unwrap();
    assert_eq!(t.len(), 1); assert_eq!((t[0].x, t[0].y, t[0].width, t[0].height, t[0].offset), (0, 0, 64, 48, 512));
}
#[test]
fn tile_table_rejects_bad_layouts() {
    let (tile, _) = one_tile(64, 48, 0, Qis::ZERO);
    let strip = strip_of(&arw6_file(&[(0, 0, 64, 48, tile.clone())], 64, 48)).to_vec();
    let mut zero = strip.clone(); zero[0] = 0;                     assert!(parse_tile_table(&zero, 64, 48).is_err());
    let mut far = strip.clone(); far[8..16].copy_from_slice(&u64::MAX.to_le_bytes()); assert!(parse_tile_table(&far, 64, 48).is_err());
    let mut odd = strip.clone(); odd[20..24].copy_from_slice(&63u32.to_le_bytes());  assert!(parse_tile_table(&odd, 64, 48).is_err());
    assert!(parse_tile_table(&strip, 64, 64).is_err());            // does not cover the image
    let two = arw6_file(&[(0, 0, 64, 48, tile.clone()), (0, 0, 64, 48, tile.clone())], 128, 48);
    assert!(parse_tile_table(strip_of(&two), 128, 48).is_err());   // overlapping
    assert!(parse_tile_table(&strip[..7], 64, 48).is_err());       // truncated table
}
#[test]
fn tile_header_and_streams() {
    let (tile, _) = one_tile(64, 48, 3, Qis::REAL);
    let h = parse_tile_header(&tile, 64, 48).unwrap();
    assert_eq!((h.hs, h.vs, h.ntu, h.streams.len()), (64, 24, 2, 13));
    assert_eq!(h.streams.iter().map(|s| (s.group, s.comp)).collect::<Vec<_>>(), [(0,0),(0,1),(0,2),(1,0),(1,1),(1,2),(2,0),(2,1),(2,2),(3,0),(3,1),(3,2),(4,0)]);
    let g3 = stream_halves(&tile, &h.streams[9], h.ntu).unwrap();
    assert_eq!(g3.len(), 4); assert_eq!(g3[0].qi, [1, 1, 2]);
    let g4 = stream_halves(&tile, &h.streams[12], h.ntu).unwrap();
    assert_eq!(g4[1].qi, [2, 2, 2]); assert!(g4[3].start + g4[3].len <= tile.len());
    let mut bad = tile.clone(); bad[13] = 0xff;                      // NC/NW field
    assert!(matches!(parse_tile_header(&bad, 64, 48), Err(RawError::Unsupported(_))));
    assert!(parse_tile_header(&tile, 64, 50).is_err());              // VS ≠ th / 2
}
#[test]
fn encoder_decodes_its_own_lines() {
    let vals: Vec<i32> = (0..100).map(|i| ((i * 37) % 23) as i32 - 11).collect();
    let mut w = BitWriter::default(); vld_encode_line(&vals, 100, &mut w); let bytes = w.finish();
    let mut b = Bits::new(&bytes);
    assert_eq!(crate::llvc::vld_decode_line(&mut b, bytes.len() * 8, 100).unwrap(), vals);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lightcraft-raw arw6::`
Expected: compile errors.

- [ ] **Step 3: Implement the parsers in `arw6.rs` and the encoder in `arw6_testenc.rs`**

`arw6.rs` module doc names the sources (spec *Sources*) and the Phase 0 method. Every offset through `get()`; sizes multiplied with `checked_mul`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lightcraft-raw arw6::` — Expected: pass.

- [ ] **Step 5: Lints and commit**

Run: `cargo fmt && cargo xtask ci` — Expected: green.

```bash
git add crates/raw/src/vendor/arw6.rs crates/raw/src/vendor/arw6_testenc.rs crates/raw/src/vendor/mod.rs
git commit -m "LR-IMP-FORMATS: ARW6 tile table, tile and stream headers; synthetic ARW6 encoder for tests"
```

---

### Task 4: Geometry, band assembly and tile reconstruction (`arw6.rs`)

**Files:**
- Modify: `crates/raw/src/vendor/arw6.rs`
- Test: inline tests

**Interfaces:**
- Consumes: Task 1–3 names; `rayon::prelude::*`.
- Produces:
  - `pub(crate) fn phases(s: u8) -> [u8; 3]` — spec *Frame geometry*: `p1 = s % 2`, `p2 = ((2 + s − p1) / 2) % 2`, `k3 = ((6 + s) % 8 − p1) / 2`, `p3 = ((k3 − p2) / 2) % 2`; FF (s = 0) → [0, 1, 1], APS-C (s = 3) → [1, 0, 0].
  - `pub(crate) fn half_rows(n: usize, s: u8, rows: usize) -> std::ops::Range<usize>` — plane rows `8n − 6 + s .. 8n + 2 + s` clipped to `0..rows` (empty when disjoint).
  - `pub(crate) fn first_half_shift(tile: &[u8], g4: &Half, width2: usize) -> Result<u8>` — decodes residual lines from half 0 while at least 8 bits remain and fewer than 8 lines were read; `s = lines − 2`; `Unsupported` unless 0 ≤ s ≤ 5.
  - `pub(crate) struct TilePlanes { pub m: Plane, pub c1: Plane, pub c2: Plane, pub res: Plane }`
  - `pub(crate) fn decode_tile_planes(tile: &[u8], tw: usize, th: usize) -> Result<TilePlanes>` — header, halves, `s`, then: the 13 streams' lines are VLD-decoded in parallel (`par_iter` over streams; each yields `Vec<Vec<Vec<i32>>>` = per half, per line), dequantised with the half's QI nibble for that band (LL3: nibble must be 0 else `Unsupported`; residual: its nibble), assigned to band rows in natural clipped order, and the three components reconstructed (`reconstruct3`, in parallel with `rayon::join`). A half that holds fewer lines than its rows (a line reads past the half: `vld_decode_line` returns `Corrupt`) fails the tile.
- Row assignment per half n (plane rows T = `half_rows(n, s, vs)`, phases `[p1, p2, p3]`): L1 = `{(t − p1) / 2 : t ∈ T, t % 2 == p1}`, H1 = `{(t − (1 − p1)) / 2 : t ∈ T, t % 2 != p1}`; L2 = `{(k − p2) / 2 : k ∈ L1, k % 2 == p2}`, H2 = the others of L1 → `(k − (1 − p2)) / 2`; L3/H3 likewise from L2 with p3. Stream g3 of a component: HL1 rows L1, LH1 rows H1, HH1 rows H1; g2: HL2 rows L2, LH2 rows H2, HH2 rows H2; g0: LL3 rows L3; g1: HL3 rows L3, LH3 rows H3, HH3 rows H3; g4: residual rows T. Line widths: level ℓ band = (tw / 2) / 2^ℓ; residual tw / 2. Band heights from `band_rows`: level 1 `band_rows(vs, p1)`, level 2 `band_rows(nL1, p2)`, level 3 `band_rows(nL2, p3)`.
- LL3 lines: running sum of the decoded differences from 0, plus 2048 (`i32`).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn phases_and_half_rows_match_the_spec() {
    assert_eq!(phases(0), [0, 1, 1]); assert_eq!(phases(3), [1, 0, 0]);
    assert_eq!(half_rows(0, 0, 1668), 0..2); assert_eq!(half_rows(209, 0, 1668), 1666..1668); assert_eq!(half_rows(1, 0, 1668), 2..10);
    assert_eq!(half_rows(0, 3, 2186), 0..5); assert_eq!(half_rows(273, 3, 2186), 2181..2186); assert_eq!(half_rows(300, 3, 2186), 2186..2186);
}
#[test]
fn first_half_shift_is_read_from_the_stream() {
    for s in [0u8, 3, 5] {
        let (tile, _) = one_tile(64, 48, s, Qis::REAL);
        let h = parse_tile_header(&tile, 64, 48).unwrap();
        let g4 = stream_halves(&tile, &h.streams[12], h.ntu).unwrap();
        assert_eq!(first_half_shift(&tile, &g4[0], 32).unwrap(), s);
    }
}
#[test]
fn round_trip_every_shift() {
    for s in 0u8..=5 {
        for (w, h) in [(64usize, 48usize), (80, 96), (64, 36)] {
            let (tile, spec) = one_tile(w, h, s, Qis::ZERO);   // QI 0: lossless, planes must come back exactly
            let p = decode_tile_planes(&tile, w, h).unwrap_or_else(|e| panic!("s={s} {w}x{h}: {e}"));
            assert_eq!(p.m, spec.m, "s={s} {w}x{h} m"); assert_eq!(p.c1, spec.c1); assert_eq!(p.c2, spec.c2); assert_eq!(p.res, spec.res);
        }
    }
}
#[test]
fn round_trip_with_real_quantisers() {
    // bands drawn as quantised integers q, planes = reconstruct3(dequant(q)); the encoder must reproduce them bit for bit
    let (tile, spec) = one_tile_quantised(96, 64, 3, Qis::REAL);   // helper in arw6_testenc: builds TileSpec from random q
    let p = decode_tile_planes(&tile, 96, 64).unwrap();
    assert_eq!(p.m, spec.m); assert_eq!(p.c1, spec.c1); assert_eq!(p.res, spec.res);
}
#[test]
fn half_too_short_is_corrupt() {
    let (mut tile, _) = one_tile(64, 48, 0, Qis::ZERO);
    let h = parse_tile_header(&tile, 64, 48).unwrap();
    let w = h.streams[9].word * 16 + 16;                 // first index entry of g3 c0: shrink A by 1 byte
    let a = u16::from_be_bytes([tile[w], tile[w + 1]]) - 1; tile[w..w + 2].copy_from_slice(&a.to_be_bytes());
    assert!(matches!(decode_tile_planes(&tile, 64, 48), Err(RawError::Corrupt(_))));
}
proptest::proptest! {
    #[test]
    fn mutated_tiles_never_panic(flips in proptest::collection::vec((proptest::num::usize::ANY, proptest::num::u8::ANY), 1..12), cut in proptest::num::usize::ANY) {
        let (mut tile, _) = one_tile(64, 48, 3, Qis::REAL);
        for (i, b) in flips { let i = i % tile.len(); tile[i] ^= b; }
        let n = cut % (tile.len() + 1);
        let _ = decode_tile_planes(&tile[..n], 64, 48);
        let _ = decode_tile_planes(&tile, 64, 48);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lightcraft-raw arw6::` — Expected: compile errors.

- [ ] **Step 3: Implement `phases`, `half_rows`, `first_half_shift`, `TilePlanes`, `decode_tile_planes` in `arw6.rs`, `one_tile_quantised` in `arw6_testenc.rs`**

`one_tile_quantised`: random q per band position in −7..=7 (level-1 bands wider range), `v = dequant(q, qi)`, planes via `reconstruct3`; LL3 random around 2048; the encoder quantises back with `quantize`, which is exact on these values.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lightcraft-raw arw6::` — Expected: pass.

- [ ] **Step 5: Lints and commit**

Run: `cargo fmt && cargo xtask ci` — Expected: green.

```bash
git add crates/raw/src/vendor/arw6.rs crates/raw/src/vendor/arw6_testenc.rs
git commit -m "LR-IMP-FORMATS: ARW6 sensor-anchored frame geometry and tile reconstruction"
```

---

### Task 5: Colour, companding table, `decode`, and the `arw.rs` arm

**Files:**
- Create: `crates/raw/src/vendor/arw6_curve.rs`
- Modify: `crates/raw/src/vendor/arw6.rs`, `crates/raw/src/vendor/mod.rs` (`mod arw6_curve;`), `crates/raw/src/vendor/arw.rs` (match arm around line 455, black/white after line 505), `crates/raw/tests/vendor_robust.rs` (new sample kind)
- Test: inline tests in `arw6.rs`; `vendor_robust.rs`

**Interfaces:**
- Produces (arw6_curve.rs): `pub(crate) const CURVE_KNEE: usize = 1428; pub(crate) const ARW6_CURVE: [u16; 2668];` — the values of `plan/arw6/arw6_curve.txt` lines 1429..=4096 (0-based index 1428..=4095), generated once with `sed -n '1429,4096p' plan/arw6/arw6_curve.txt | paste -sd, | fold -w 118` into the array literal. Doc comment: "code → 2 × 14-bit DN; identity below the knee; derived in Phase 0 from the black-box oracle's output over 8 files (0 disagreements), see docs/arw6-compression.md".
- Produces (arw6.rs):
  - `pub(crate) fn curve(code12: i32) -> u16` — clamps to 0..=4095; `< 1428` → the code; else `ARW6_CURVE[code − 1428]` (via `get`, falling back to 39002).
  - `pub(crate) fn colour(p: &TilePlanes) -> Result<Vec<u16>>` — spec *Colour reconstruction*: tile mosaic `(2·W2) × (2·H2)` of `curve(code)`, R at (0,0), G1 (0,1), G2 (1,0), B (1,1); `res[−1] := res[0]`; x ± 1 clamped; G2's predictor uses unclipped G1 and `G1[j+1] := G1[j]` on the last row; chroma sums use `min(G, 4095)`; `i64` sums.
  - `pub(crate) fn decode_tile(tile: &[u8], tw: usize, th: usize) -> Result<Vec<u16>>` = `colour(&decode_tile_planes(…)?)`.
  - `pub(crate) fn decode(strip: &[u8], width: usize, height: usize, mode: crate::Mode) -> Result<Vec<u16>>` — tile table; `Mode::Header`: `parse_tile_header` + `stream_halves` for every tile, returns `Vec::new()`; `Mode::Full`: tiles decoded with `par_iter`, each placed at (x, y) into a `width × height` buffer.
- arw.rs arm (inside the `match info.compression`, before `_ =>`):
  ```rust
  32766 if chunks.len() == 1 => {
      let src = chunk_bytes(bytes, &chunks[0]).ok_or_else(|| RawError::Corrupt("raw strip outside file".into()))?;
      (RawData::U16(arw6::decode(src, w, h, mode)?), 16)
  }
  32766 => return Err(RawError::Unsupported("ARW6 with more than one strip".into())),
  ```
  and after `black`/`white` are computed: `let unit = if info.compression == 32766 { 2.0 } else { 1.0 };` applied to every black value and to `white` (`white * unit`). Module doc of `arw.rs` gets a line: "ARW6 (Compressed RAW 2, compression 32766): `arw6.rs`, output in 2 × 14-bit units."

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn curve_anchors() {
    assert_eq!(ARW6_CURVE.len(), 2668);
    for (c, v) in [(0, 0), (1427, 1427), (1428, 1429), (1436, 1438), (2048, 2469), (3000, 8010), (4094, 38944), (4095, 39002)] { assert_eq!(curve(c), v, "code {c}"); }
    assert_eq!(curve(-5), 0); assert_eq!(curve(5000), 39002);
    assert!(ARW6_CURVE.windows(2).all(|w| w[0] < w[1]));
}
fn plane(w: usize, v: &[i32]) -> Plane { Plane { width: w, height: v.len() / w, data: v.to_vec() } }
#[test]
fn colour_matches_the_phase0_example() {
    let p = TilePlanes { m: plane(3, &[2048, 2052, 2060, 2050, 2049, 2070]), res: plane(3, &[4, -8, 2, 0, 2, -6]), c1: plane(3, &[10, -10, 0, 0, 3, -2]), c2: plane(3, &[-5, 7, 1, 2, 0, 4]) };
    let out = colour(&p).unwrap();                       // 6 wide × 4 rows, curve units
    let code = |r: usize, c: usize| out[r * 6 + c] as i32;
    assert_eq!([code(0, 1), code(0, 3), code(0, 5), code(2, 1), code(2, 3), code(2, 5)], [curve(2049) as i32, curve(2053) as i32, curve(2059) as i32, curve(2050) as i32, curve(2050) as i32, curve(2071) as i32]); // G1
    assert_eq!([code(1, 0), code(1, 2), code(1, 4), code(3, 0), code(3, 2), code(3, 4)].map(|v| v), [2053, 2042, 2060, 2050, 2052, 2054].map(|c| curve(c) as i32)); // G2
    assert_eq!([code(0, 0), code(0, 2), code(0, 4)], [2071, 2027, 2059].map(|c| curve(c) as i32));   // R
    assert_eq!([code(1, 1), code(1, 3), code(1, 5)], [2041, 2061, 2061].map(|c| curve(c) as i32));   // B
}
#[test]
fn colour_clips_like_the_oracle() {
    let mut p = /* same planes */; p.m.data[1] = 4100;
    let out = colour(&p).unwrap();
    assert_eq!(out[0 * 6 + 3], 39002);            // G1 = 4101 → clipped output
    assert_eq!(out[1 * 6 + 2], curve(2554));      // G2 predicted from the unclipped G1
    assert_eq!(out[0 * 6 + 2], curve(3304));      // R = 2·(−10) + ((4095 + 2554) >> 1): chroma uses the clipped greens
}
#[test]
fn decode_header_and_full_and_truncations() {
    let (tile, _) = one_tile(64, 48, 3, Qis::REAL);
    let file = arw6_file(&[(0, 0, 64, 48, tile)], 64, 48);
    let strip = strip_of(&file);
    assert!(decode(strip, 64, 48, crate::Mode::Header).unwrap().is_empty());
    assert_eq!(decode(strip, 64, 48, crate::Mode::Full).unwrap().len(), 64 * 48);
    for n in 0..strip.len() { let _ = decode(&strip[..n], 64, 48, crate::Mode::Full); let _ = decode(&strip[..n], 64, 48, crate::Mode::Header); }
}
#[test]
fn garbage_streams_import_then_fail() {
    let (mut tile, _) = one_tile(64, 48, 0, Qis::REAL);
    let h = parse_tile_header(&tile, 64, 48).unwrap();
    let data = h.streams[9].word * 16 + 16 + h.streams[9].index_words * 16;   // g3 c0 data area
    for (i, b) in tile[data..data + 64].iter_mut().enumerate() { *b = (i as u8).wrapping_mul(97) ^ 0xa5; }
    let strip = strip_of(&arw6_file(&[(0, 0, 64, 48, tile)], 64, 48)).to_vec();
    assert!(decode(&strip, 64, 48, crate::Mode::Header).is_ok());
    assert!(matches!(decode(&strip, 64, 48, crate::Mode::Full), Err(RawError::Corrupt(_))));
}
#[test]
fn four_tiles_place_correctly_and_arw_reports_doubled_levels() {
    let tiles: Vec<_> = [(0, 0), (64, 0), (0, 48), (64, 48)].iter().enumerate().map(|(i, &(x, y))| { let (t, _) = one_tile(64, 48, 0, Qis::ZERO); (x, y, 64, 48, t) }).collect();
    let file = arw6_file(&tiles, 128, 96);
    let img = crate::decode(&file).unwrap();
    assert_eq!((img.width, img.height, img.bits), (128, 96, 16));
    assert_eq!(img.black.values, vec![1024.0; 4]); assert_eq!(img.white, vec![30720.0]);
    assert_eq!(img.cfa.as_ref().map(|c| c.pattern.clone()), Some(vec![0, 1, 1, 2]));
    let crate::RawData::U16(d) = img.data else { panic!() };
    let single = decode(strip_of(&arw6_file(&tiles[..1], 64, 48)), 64, 48, crate::Mode::Full).unwrap();
    assert_eq!(&d[..64], &single[..64]);                                   // tile 0's first row lands at (0, 0)
    assert_eq!(crate::probe_info(&file).unwrap().width, 128);             // header mode: tile tables and headers only
}
```

`vendor_robust.rs`: add `cfa_tiff("SONY", 32766, 14, ByteOrder::Little)` to `samples()` and widen `kind in 0usize..17` to the new count.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lightcraft-raw arw6:: && cargo test -p lightcraft-raw --test vendor_robust` — Expected: compile errors / the new robust kind fails.

- [ ] **Step 3: Generate `arw6_curve.rs`, implement `curve`, `colour`, `decode_tile`, `decode`, the `arw.rs` arm and level scaling**

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lightcraft-raw` — Expected: all pass (unit, robust, corpus skips).

- [ ] **Step 5: Lints and commit**

Run: `cargo fmt && cargo xtask ci` — Expected: green.

```bash
git add crates/raw/src/vendor/arw6.rs crates/raw/src/vendor/arw6_curve.rs crates/raw/src/vendor/mod.rs crates/raw/src/vendor/arw.rs crates/raw/tests/vendor_robust.rs
git commit -m "LR-IMP-FORMATS: ARW6 colour reconstruction, companding table and ARW dispatch"
```

---

### Task 6: Corpus verification, docs and tracker

**Files:**
- Modify: `xtask/src/main.rs` (corpus list, next to the `raf-fuji-*` entries), `crates/raw/tests/corpus.rs`, `docs/parity.md` (rows LR-IMP-FORMATS, LR-IMP-CAMERA-COVERAGE), `ROADMAP.md` (*Where we stand*, RAW coverage row)
- Create: `docs/arw6-corpus.sha256`, `docs/arw6-compression.md`

**Interfaces:**
- Consumes: `lightcraft_raw::decode` (Task 5), `corpus_root()` and the `corpus_fujifilm_compressed_samples` pattern in `corpus.rs`.
- Corpus entries (CC0 1.0 per the RawDB API record of the set, uploaded by abbradar): names and URLs

| corpus name | URL (`https://rawdb.dnglab.org/api/download/Sony/ILCE-7RM6/` + path; the server answers 303 to a signed object URL, `curl -L` follows) | SHA-256 |
|---|---|---|
| arw6-sony-ilce7rm6-apsc-land-hq.arw | arw6/ILCE-7RM6_APSC_land_RAW-C-HD.ARW | b60a7e752d2a8102ccbe6eebcfdbf533c6b0f2545739090873e9e8eb32cc0637 |
| arw6-sony-ilce7rm6-apsc-land-c.arw | arw6/ILCE-7RM6_APSC_land_RAW-C.ARW | 1c803304e63ec508b2b5f9d59d4a7e30ed279315b35fa4c5d75f959bed62198b |
| arw6-sony-ilce7rm6-apsc-port-hq.arw | arw6/ILCE-7RM6_APSC_port_RAW-C-HD.ARW | 0b6d782571e86868670f613de9fa1f1364623b24fbfd21b704b2c516d7a1c1cb |
| arw6-sony-ilce7rm6-apsc-port-c.arw | arw6/ILCE-7RM6_APSC_port_RAW-C.ARW | c41674a485ffcf6d873e20e545ae62936192d94e95be3d53548e93bc543ecfd4 |
| arw6-sony-ilce7rm6-ff-land-hq.arw | arw6/ILCE-7RM6_FF_land_RAW-C-HD.ARW | 559fa26501d7813e56afecde2aea3d9965a99228162b194752d986193abb172c |
| arw6-sony-ilce7rm6-ff-land-c.arw | arw6/ILCE-7RM6_FF_land_RAW-C.ARW | d900d6cbe838be864269ad54de4a33fcac7eb3a3d4f91009739f770b8f85a23b |
| arw6-sony-ilce7rm6-ff-port-hq.arw | arw6/ILCE-7RM6_FF_port_RAW-C-HD.ARW | 81c709633cb8f6340e612c3f53bb6ec6b0cccfe5b713c67efb1e977ece9e266d |
| arw6-sony-ilce7rm6-ff-port-c.arw | arw6/ILCE-7RM6_FF_port_RAW-C.ARW | f4448485629c3f82ff1955fd046404350e9a1f09b346038574b89c074d7c50fd |
| arw-sony-ilce7rm6-apsc-land-lossless.arw | raw_modes/ILCE-7RM6_APSC_land_RAW.ARW | 7aaf0b3941509d7364a6f03844783155ebc912d246d1c456e5cdb4693ec09fbc |
| arw-sony-ilce7rm6-apsc-port-lossless.arw | raw_modes/ILCE-7RM6_APSC_port_RAW.ARW | 0f011933ee96525a39710daa50b59d39702b4ec5934fa0206742e2128c55f366 |
| arw-sony-ilce7rm6-ff-land-lossless.arw | raw_modes/ILCE-7RM6_FF_land_RAW.ARW | 5cb3c870bc961cf6b079d56c860d0cdf11acd1afd04e5fb8dbe041bcfd2d6201 |
| arw-sony-ilce7rm6-ff-port-lossless.arw | raw_modes/ILCE-7RM6_FF_port_RAW.ARW | b1a4026aa6adbaa27973b8b3426d8b4aa0c1cae1845cf5102359327d3b069691 |

- Expected sums of our decoder's full CFA (u16, curve units), from the Phase 0 reference model (`plan/arw6/scratch/sums.py`), the same wrapping `u64` sum and `(i + 1)`-weighted sum as the Fujifilm test:

```rust
("arw6-sony-ilce7rm6-apsc-land-hq.arw", 6592, 4372, 85834467900, 1323814215465139776),
("arw6-sony-ilce7rm6-apsc-land-c.arw", 6592, 4372, 100710478599, 1516298925086164077),
("arw6-sony-ilce7rm6-ff-land-hq.arw", 10016, 6672, 258417758289, 9667602918876623332),
("arw6-sony-ilce7rm6-ff-land-c.arw", 10016, 6672, 258558260019, 9665041696390559268),
("arw6-sony-ilce7rm6-apsc-port-hq.arw", 6592, 4372, 126965386051, 1647947621898118992),
("arw6-sony-ilce7rm6-apsc-port-c.arw", 6592, 4372, 122629012213, 1601616875702755341),
("arw6-sony-ilce7rm6-ff-port-hq.arw", 10016, 6672, 242772794178, 8043470668590476975),
("arw6-sony-ilce7rm6-ff-port-c.arw", 10016, 6672, 241868995482, 8014659803009871380),
```

- [ ] **Step 1: Write the failing corpus test** (in `crates/raw/tests/corpus.rs`, after the Fujifilm one)

```rust
/// Reference sums from the oracle-verified Phase 0 model (docs/arw6-compression.md); the files are CC0 from RawDB.
#[test]
fn corpus_sony_arw6_samples() {
    use sha2::{Digest, Sha256};
    let checksums = include_str!("../../../docs/arw6-corpus.sha256");
    let cases: &[(&str, usize, usize, u64, u64)] = &[ /* the 8 rows of the sums table */ ];
    // body identical to corpus_fujifilm_compressed_samples (checksum, decode, size, sums, skip absent), eprintln "verified {seen} Sony ARW6 sensor arrays"
}
/// The lossless siblings keep decoding through the LJ92 quad-tile path (sizes only; sums recorded on first run).
#[test]
fn corpus_sony_ilce7rm6_lossless_samples() { /* 4 rows: name, 10240/6656 × 7168/4608, sum, weighted — fill from the first run, note "from LightCraft's own LJ92 path, 2026-10-09" */ }
```

- [ ] **Step 2: Add the 12 entries to the `xtask` corpus list and the 12 lines (`<sha256>  corpus/raw/<name>`) to `docs/arw6-corpus.sha256`; run the download**

Run: `cargo xtask corpus --download` — Expected: the 12 files appear in `corpus/raw/` with matching checksums (the download takes a few minutes; the files are 29–101 MB each).

- [ ] **Step 3: Run the corpus tests**

Run: `cargo test -p lightcraft-raw corpus_sony -- --nocapture`
Expected: `verified 8 Sony ARW6 sensor arrays`; the lossless test prints its sums on first run, paste them in, rerun → pass. A sum mismatch means a decoder bug, not a table error: debug against `plan/arw6/scratch/verify.py` on the same file (compare tile by tile, then plane by plane).

- [ ] **Step 4: Write `docs/arw6-compression.md`** with the sections of `docs/raf-compression.md`: *Format and implementation* (container, VLD deviations, dequantiser, geometry with `s`, colour, companding table and its ×2 unit), *Verification* (method: LibRaw master `d1f0dd95` run as a black box, the Phase 0 probes, 8 files bit-exact on every tile, the APS-C 6-row LibRaw discrepancy and the embedded-JPEG geometry check, how to compare against LibRaw with the shift), the corpus table with SHA-256, the private files note, *Not covered* (12-bit mode, ILCE-7M5, the ×2 scale assumption), decode timings (filled in Task 7).

- [ ] **Step 5: Update the tracker and roadmap**

`docs/parity.md` LR-IMP-FORMATS ARW text: append "ARW6 Compressed / Compressed (HQ) of the ILCE-7RM6 (`crates/raw/src/vendor/arw6.rs`, `crates/raw/src/llvc.rs`, 8 CC0 files bit-exact against a black-box oracle, geometry checked against the camera JPEG, `docs/arw6-compression.md`; the ILCE-7M5 writes the same codec, unverified)". LR-IMP-CAMERA-COVERAGE: append "ILCE-7RM6 ARW6 sensor arrays verified on 8 CC0 files (FF and APS-C, both compression modes)". `ROADMAP.md` RAW coverage row: add "ARW6 (A7R VI Compressed / HQ)" to the supported list. Then `cargo xtask parity --write`.

Run: `cargo xtask parity` — Expected: no missing ids.

- [ ] **Step 6: Commit**

Run: `cargo fmt && cargo xtask ci` — Expected: green.

```bash
git add xtask/src/main.rs crates/raw/tests/corpus.rs docs/arw6-corpus.sha256 docs/arw6-compression.md docs/parity.md ROADMAP.md
git commit -m "LR-IMP-FORMATS: ARW6 corpus verification, docs and tracker"
```

---

### Task 7: Performance and end-to-end check

**Files:**
- Modify: `crates/raw/src/vendor/arw6.rs` (only if over budget), `docs/arw6-compression.md` (timings)

- [ ] **Step 1: Measure**

Run (release):
```bash
cargo build --release -p lightcraft-cli
for f in ff-land-hq apsc-land-hq ff-land-c; do LIGHTCRAFT_PROFILE=1 ./target/release/lightcraft-cli render corpus/raw/arw6-sony-ilce7rm6-$f.arw -o /tmp/arw6-$f.jpg 2>&1 | grep -i -E "decode|raw"; done
```
Expected: the raw decode stage ≤ 1.0 s for the FF files. Record all three figures.

- [ ] **Step 2: If over budget, parallelise further** — the per-stream VLD already runs on rayon (Task 4); add `rayon::join` for the three components' `reconstruct3` and split the residual/colour step into row bands with `par_chunks_mut`. Re-measure; stop at the first change that meets the budget.

- [ ] **Step 3: Look at the result**

Run: `nix run nixpkgs#exiftool -- -b -JpgFromRaw corpus/raw/arw6-sony-ilce7rm6-apsc-land-hq.arw > /tmp/apsc-ref.jpg` and open `/tmp/arw6-apsc-land-hq.jpg` next to it (and the FF pair). Expected: same framing (no vertical offset; the crop origin is 16,10 on APS-C and 12,8 on FF), plausible colour, no banding in shadows, highlights clip cleanly.

- [ ] **Step 4: Import check**

Run: `cargo run --release -p lightcraft -- --control 18762` then, through the control channel (`docs/showcase/run.py` with a one-line script that imports `corpus/raw/arw6-sony-ilce7rm6-ff-land-hq.arw`), `ui.inspect` the photo. Expected: `previewOnly` false, a rendered loupe from sensor data; a copy of the file truncated to 1 MB imports with `previewOnly` true and a reason mentioning ARW6 / corrupt.

- [ ] **Step 5: Record timings in `docs/arw6-compression.md`, run `cargo xtask bench` (must stay green), commit**

```bash
git add docs/arw6-compression.md crates/raw/src/vendor/arw6.rs
git commit -m "LR-IMP-FORMATS: ARW6 decode timings and parallel reconstruction"
```

---

## Self-review notes

- Spec coverage: container/VLD/dequant → Tasks 1–3; geometry/assembly → Task 4; colour, curve, output unit, `arw.rs`, header mode, never-crash → Task 5; corpus, docs, tracker → Task 6; performance and visual/import checks → Task 7. The spec's "oracle gaps" (files LibRaw cannot unpack) need no task: the corpus files all decode.
- Names used across tasks: `vld_decode_line`, `dequant`, `Plane`, `band_rows`, `inverse_53_2d`, `Bands3`, `reconstruct3`, `TileEntry`, `parse_tile_table`, `StreamRef`, `TileHeader`, `parse_tile_header`, `Half`, `stream_halves`, `phases`, `half_rows`, `first_half_shift`, `TilePlanes`, `decode_tile_planes`, `curve`, `colour`, `decode_tile`, `decode`, `ARW6_CURVE`, `CURVE_KNEE`; test helpers `BitWriter`, `vld_encode_line`, `Qis`, `TileSpec`, `encode_tile`, `arw6_file`, `one_tile`, `one_tile_quantised`, `strip_of`, `forward_53_2d`, `forward3`, `quantize`.
- Header-mode entry point: `lightcraft_raw::probe_info` (`decode_with(bytes, Mode::Header)`), used by import.
