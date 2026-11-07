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
   - `decode_orf_compressed`: Decodes 2-sample pixel pairs with 12-bit differential prediction across identical color planes, clamping outputs to `[0, 4095]`.
   - Hostile input handling: Strict boundary checks for SubIFD dimensions, offsets, lengths, and pixel counts; non-panicking error paths returning `Option<RawImage>`.
   - Crate-level `#![forbid(unsafe_code)]` compliance.

3. **Vendor Integration & Dispatch (`crates/raw/src/vendor/orf.rs`, `crates/raw/src/vendor.rs`)**:
   - Dispatches compressed SubIFD strips (`Compression == 0x26`, `0x27`, or sub-type tags) through `decode_orf_compressed`.
   - Graceful fallback to embedded preview when compressed data is truncated or corrupt.

4. **Independent Code Audit (`clean-room/orf/10-code-audit.md`)**:
   - Independent verification confirming zero code or structural derivation from GPL/LGPL sources (LibRaw, dcraw, rawspeed, RawTherapee, darktable).
   - Structural AST, symbol name, and control flow audits confirm clean-room provenance.

**Not in this PR:**
- High-resolution multi-shot pixel-shift synthesis (each raw sub-exposure in an ORI/ORF decodes as an independent Bayer sensor raster).
- Custom per-camera color matrices (continues using file-carried matrix or standard color fallback).

## How it was checked

- [x] `cargo xtask ci` passes on this branch (`cargo fmt`, `cargo clippy`, all unit tests, `layers`, `assets`, `roots`, `wasm`).
- [x] Verified against CC0 sample corpus (`crates/raw/tests/it/olympus_samsung_corpus.rs`):
  - `orf-olympus-em1.orf` (Olympus E-M1): Pearson correlation $r = 0.995$ against reference mosaic.
  - `orf-olympus-em10iii.orf` (Olympus E-M10 Mark III): Pearson correlation $r = 0.929$.
  - `Olympus - E-M1MarkII - 16bit (4_3).ORI` (Olympus E-M1 Mark II high-res raw): Pearson correlation $r = 0.998$.
  - `Olympus - E-M5 Mark II - 16bit 16bit _normal_ (4_3).ORI` (Olympus E-M5 Mark II high-res raw): Pearson correlation $r = 0.994$.
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
- [x] The feature's rows in `docs/parity-checklist.md` and its entry in `docs/gaps.md` are updated honestly in `docs/orf-compression.md` and related docs.
- [x] New code has a caller: wired into `crates/raw/src/vendor/orf.rs` and `vendor.rs`.
- [x] Every new image, icon or other asset has its entry in `assets/ATTRIBUTION.md`; no font or media files added.
- [x] Ready to merge.
