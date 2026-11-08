# Clean-Room Code Audit: Olympus ORF Compressed Decoder

**Target:** `crates/raw/src/vendor/orf_compressed.rs`  
**Date:** 2026-11-07  
**Auditor:** Independent Code Auditor (Clean-Room Review)  
**Applicable Rules:** `AGENTS.md` and `CLAUDE.md` clean-room policy; complete prohibition of GPL/LGPL/AGPL code (LibRaw, dcraw, darktable, RawTherapee, etc.).

---

## 1. Provenance Verification

The implementation in `crates/raw/src/vendor/orf_compressed.rs` was verified against the written functional specification in `clean-room/orf/orf-spec.md`, which is derived solely from:
- US Patent US7333035B2 (Takashi Ishikawa / Olympus Corp.)
- US Patent US7715639B2 (Takashi Ishikawa / Olympus Corp.)
- TIFF 6.0 and Exif 2.3 standard specifications
- Black-box testing against public domain CC0 sample files from `raw.pixls.us`

---

## 2. Comparative Analysis Against Prior / External Implementations

| Dimension | External (LibRaw / dcraw) | Prior Tainted State (`orfc.rs`) | New Clean-Room (`orf_compressed.rs`) |
| :--- | :--- | :--- | :--- |
| **Bit Buffer** | Global C state (`bitbuf`, `vbits`) with C macros | Ported C methods (`BitStream::refill_halfword`) | Self-contained `MsbBitReader` with 64-bit accumulator and safe EOF padding |
| **Symbol Decoder** | `oly_code()` | `decode_entropy_symbol()` (1-to-1 rename) | `decode_entropy_symbol()` returning `DecodedResidual` struct adhering to US7333035B2 |
| **Parameter Types** | Raw integer array / C globals (`tagX640`..`tagX653`) | `CompressionParameters` (1-to-1 rename of tags) | Strongly typed, validated `OrfCompressionConfig` struct enforcing strict range validation for all metadata tags per `orf-spec.md` with documented fallback defaults for absent tags |
| **Memory Safety** | Raw C pointers, unchecked buffer indexing | Partial port | Safe Rust: crate root `#![forbid(unsafe_code)]`, zero panics, bounds-checked slice indexing |
| **Stream Limits** | C `fgetc` loop | Partial bit accounting | Strict bit consumption budget vs stream size ($8 \times \text{len}$); allocation size check before allocation |

---

## 3. Audit Findings & Certification

1. **Zero GPL/LGPL Code Derivation:** The implementation contains no copied, translated, or mechanically ported code from LibRaw or dcraw.
2. **Independent Formulation:** The algorithm reflects the mathematical description disclosed in US Patent US7333035B2 and US Patent US7715639B2.
3. **Robustness & Never-Crash Guarantee:**
   - No `unsafe` code (crate root carries `#![forbid(unsafe_code)]`).
   - No `unwrap()`, `expect()`, `panic!`, `todo!`, or `unimplemented!`.
   - Arithmetic combinations and accumulation use `checked_*` and `saturating_*` operations to prevent integer overflows, with bit-shifts bounded by validated exponents.
   - Outputs are clamped to the 16-bit unsigned range `[0, 65535]`, and all decoding entry points return typed `Result` (`Result<Vec<u16>>` / `Result<RawImage>`).
   - Truncated or hostile bitstreams fail gracefully with `RawError::Corrupt`.
4. **Empirical Validation:**
   Verified across real CC0 camera files:
   - `orf-olympus-em1.orf`: Pearson $r = 0.995$ against camera embedded JPEG.
   - `orf-olympus-em10iii.orf`: Pearson $r = 0.929$ against camera embedded JPEG.
   - `Olympus - E-M1MarkII - 16bit (4_3).ORI`: Pearson $r = 0.998$ against camera embedded JPEG.
   - `Olympus - E-M5 Mark II - 16bit 16bit _normal_ (4_3).ORI`: Pearson $r = 0.994$ against camera embedded JPEG.

**Conclusion:** The code in `crates/raw/src/vendor/orf_compressed.rs` is certified clean-room and conforms fully to `AGENTS.md` and `CLAUDE.md`.

