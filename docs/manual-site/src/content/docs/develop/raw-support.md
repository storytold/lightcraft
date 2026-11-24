---
title: Raw support
description: Which cameras and formats open, and where support is partial.
sidebar:
  order: 16
---

- **Reference**: 1,446 camera models listed by Adobe; about 645 decode and are checked on a real file, about 679 should decode but are not checked (mostly DNG phones), 87 show the embedded preview only, 35 are unsupported. An estimated 98% of photographers' cameras decode.
- **Image formats**: JPEG, PNG, TIFF, WebP, JXL, PSD, GIF, BMP, DNG (including lossy and JPEG XL).
- **Raw decoders**, in pure Rust: Sony ARW (including cRAW, lossless and Compressed RAW 2 from the A7R VI, in both its 14-bit and its 12-bit continuous-drive mode), Canon CR2, CR3 (CRX lossless and C-RAW, including partial subbands and tiled version 0x200 C-RAW; sRAW, mRAW and other variants fall back to the preview) and CRW, Nikon NEF (including High Efficiency for the Z50 II, and NEFX, the merged pixel-shift images NX Studio writes: checked on 16-shot merges from the Z 8), Fujifilm RAF (uncompressed, lossless and lossy), Panasonic and Leica RW2, Olympus ORF, Sony SRF (DSC-F828, DSC-V3), Fujifilm FinePix SuperCCD and S200EXR RAF, Pentax PEF and DNG, Samsung SRW, Epson ERF, Mamiya MEF, Apple ProRAW and other DNG.
- **Preview only**: Hasselblad 3FR and FFF, Phase One IIQ, MOS.
- **Not imported**: some Kodak and Sigma X3F files.
- **Approximate colour**: the Sony DSC-F828 (SRF) has a four-colour sensor; its emerald photosites are read as green.
- **Colour**: bundled camera profiles, per-file fits and measured matrices; Adobe's camera profiles are not used. This is the biggest open gap: see [Colour, grading and profiles](../colour/).
- Details per format are in the repository's `docs/raw-parity.md`, `docs/cr3.md`, `docs/nikon-he.md`, `docs/raf-compression.md` and `docs/arw6-compression.md`.
