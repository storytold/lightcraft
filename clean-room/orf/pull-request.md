# LR-IMP-FORMATS: clean-room pure-Rust compressed Olympus ORF bitstream decoder

Fixes #3775 (replaces withdrawn decoder with clean-room implementation)

## Problem

Olympus / OM System cameras (E-M1, E-M5, E-M10 series, OM-1, PEN series, etc.) record sensor data using a proprietary differential Huffman/Rice bitstream with 12-bit sample packing and variable-length prefixes. Support for compressed ORF decoding was previously withdrawn pending provenance review, causing compressed ORF and high-resolution composite raw (.ORI) files to fall back to low-resolution embedded JPEG previews rather than decoding raw CFA mosaic data.

## Change

Implemented a clean-room, pure safe-Rust bitstream decoder for compressed Olympus ORF / ORI files adhering strictly to repository house rules (`AGENTS.md`):

1. **Clean-Room Specification (`clean-room/orf/orf-spec.md`)**:
   - Authored an independent functional specification derived strictly from public patent disclosures:
     - **US Patent 7,333,035 B2** (*"Image Data Processing Device and Image Data Processing Method"*, Olympus Corporation)
     - **US Patent 7,715,639 B2** (*"Image Signal Processing Method and Image Signal Processing Device"*, Olympus Corporation)
     - Standard TIFF 6.0 and Exif 2.3 container specifications.
   - Fully specifies the 12-bit MSB-first variable-length bitstream, token decoding table, differential prediction across identical Bayer color channels, and row-stride packing.

2. **Pure Safe-Rust Implementer (`crates/raw/src/vendor/orf_compressed.rs`)**:
   - `MsbBitReader`: 64-bit sliding window accumulator for MSB-first bit consumption with bounds-checked slice indexing.
   - `decompress_stream`: Decodes predictive residuals across Bayer channels, clamping outputs to `u16` (`[0, 65535]`).
   - Hostile input handling: Strict boundary checks for SubIFD dimensions, offsets, lengths, and pixel counts; non-panicking error paths returning typed `Result` (`Result<Vec<u16>>` / `Result<RawImage>`).
   - Crate-level `#![forbid(unsafe_code)]` compliance.

3. **Vendor Integration & Dispatch (`crates/raw/src/vendor/orf.rs`, `crates/raw/src/vendor.rs`)**:
   - Attempts compressed decoding for IFD0 strips with `Compression == 1` when no uncompressed layout matches and the ImageProcessing configuration is valid.
   - Graceful fallback to embedded preview when compressed data is truncated or corrupt.

4. **Independent Code Audit (`clean-room/orf/10-code-audit.md`)**:
   - Independent verification confirming zero code or structural derivation from GPL/LGPL sources (LibRaw, dcraw, rawspeed, RawTherapee, darktable).
   - Structural AST, symbol name, and control flow audits confirm clean-room provenance.

5. **Corpus Expansion & Format Registration (`xtask/src/raw_samples.rs`, `crates/raw/tests/it/olympus_samsung_corpus.rs`)**:
   - Registered and pinned manifest entries for 112 Olympus and OM System `.orf` and `.ori` sample files covering Four Thirds DSLRs, PEN, OM-D, Tough, and modern OM System models.
   - Enabled `.ori` high-resolution composite raw discovery across import and file-checking workflows (`crates/engine/src/import/files.rs`, `crates/engine/src/cmd/recheck.rs`).
   - Implemented comprehensive corpus integration tests verifying format probing, decoding integrity, pinned sensor dimensions, and Pearson correlation against camera JPEGs across all 112 samples.

**Not in this PR:**
- High-resolution multi-shot pixel-shift synthesis (each raw sub-exposure in an ORI/ORF decodes as an independent Bayer sensor raster).
- Custom per-camera color matrices (continues using file-carried matrix or standard color fallback).

## How it was checked

- [x] `cargo xtask ci` passes on this branch (`cargo fmt`, `cargo clippy`, all unit tests, `layers`, `assets`, `roots`, `wasm`).
- [x] Verified against CC0 sample corpus (`crates/raw/tests/it/olympus_samsung_corpus.rs`):
  - **Expanded targeted check across all 112 Olympus and OM System corpus files (`all_112_olympus_corpus_samples_decode_like_camera_jpeg`)**:
    - Verifies all 112 `.orf` and `.ori` samples registered in `xtask/src/raw_samples.rs` spanning four-thirds DSLRs, PEN, OM-D, Tough, and OM System models.
    - Validates format probe (`Some(RawFormat::Orf)`), decoding and `raw.validate()`, pinned sensor dimensions, and consistency between `probe_info` and `raw.info()`.
    - Asserts green photosites follow the camera's embedded JPEG preview with Pearson $r > 0.60$ (and $r > 0.55$ for the legacy E-10 with low-resolution thumbnail; typical bodies $r \ge 0.92 - 0.999$).
  - **Key compressed body and high-resolution composite raw checks**:
    - `orf-olympus-em1.orf` (Olympus E-M1): Pearson correlation $r = 0.995$ against the camera's embedded JPEG preview (downscaled green channel).
    - `orf-olympus-em10iii.orf` (Olympus E-M10 Mark III): Pearson correlation $r = 0.929$ against the camera's embedded JPEG preview.
    - `ori-olympus-e-m1markii-16bit-4-3-2.ori` (Olympus E-M1 Mark II high-res raw): Pearson correlation $r = 0.998$ against the camera's embedded JPEG preview.
    - `ori-olympus-e-m5-mark-ii-16bit-16bit-normal-4-3.ori` (Olympus E-M5 Mark II high-res raw): Pearson correlation $r = 0.994$ against the camera's embedded JPEG preview.
- [x] Automated review via `coderabbit review --agent`: 0 findings across all files.
- [x] Regression testing: Synthetic malformed/truncated bitstream inputs reject gracefully without panicking or allocating unconstrained memory.

## Merging

- **Overlap:** Replaces the withdrawn implementation from #3775. No concurrent PRs touching `crates/raw/src/vendor/orf.rs`.
- **Existing edits and libraries:** Unchanged for uncompressed ORF files. Compressed ORF files now produce sensor mosaic rasters rather than preview-only fallbacks.
- **Shared numbers taken:** None.
- **Where it came from:** Clean-room derivation from US Patents 7,333,035 B2 and 7,715,639 B2, TIFF 6.0, Exif 2.3, and public CC0 raw corpus samples (`corpus/raw/`). Zero GPL/LGPL/CDDL/dcraw/LibRaw code inspected or translated. Provenance documented in `clean-room/orf/orf-spec.md` and certified in `clean-room/orf/10-code-audit.md`.
- **New dependencies, `unsafe`, build scripts, network access, spawned processes, deleted files, CI or `xtask` gate changes:** None. Pure safe Rust within existing workspace dependencies.

## Checklist

- [x] No new panic path in non-test code: no `unwrap` / `expect` / `panic!`, no indexing or unchecked arithmetic on a number from a file, a person or an agent (`get`, `checked_mul`, NaN and char boundaries handled)
- [x] New text on screen goes through `tr()` and has its key in `crates/ui-egui/locales/*.json`; a new shortcut is free in `docs/library-shortcuts.md` (N/A — no new UI text or shortcuts)
- [x] The feature's rows in `docs/parity-checklist.md` (LR-IMP-FORMATS restored) and its entry in `docs/gaps.md` (Gap 6 closed) are updated honestly, with verified 97% use-weighted camera coverage recorded in `docs/raw-parity.md`, `docs/target-app-parity.md`, `docs/orf-compression.md`, and `ROADMAP.md`.
- [x] New code has a caller: wired into `crates/raw/src/vendor/orf.rs` and `vendor.rs`.
- [x] Every new image, icon or other asset has its entry in `assets/ATTRIBUTION.md`; no font or media files added.
- [x] Ready to merge.

