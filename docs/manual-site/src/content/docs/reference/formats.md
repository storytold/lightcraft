---
title: Formats and cameras
description: What opens, what exports, and how well cameras are covered.
sidebar:
  order: 2
---

- Numbers are from the repository's measured parity documents (`docs/file-format-parity.md`, `docs/raw-parity.md`); they change as support grows.

## Pictures

| Format | Open | Export |
|---|---|---|
| JPEG, PNG, TIFF (8/16-bit, float on export) | yes | yes |
| WebP | yes | yes |
| JPEG XL | yes | yes (lossy or lossless, native builds) |
| AVIF | yes (native builds) | yes (8/10-bit, HDR) |
| HEIC / HEIF | yes in release builds, and on Android | no |
| PSD / PSB | merged composite | PSD; also Edit In round trips (TIFF) |
| GIF, BMP | yes | no |
| DNG | yes (all kinds) | yes (incl. lossy, rendered DNG) |
| HDR gain-map JPEG, PQ AVIF / JXL, float TIFF | yes | yes (no HLG) |
| Original plus XMP sidecar | n/a | yes |
| Print and contact-sheet PDF | n/a | yes |

- **Video**: MP4, MOV, MKV/WebM, MPEG-TS, MXF with H.264, HEVC, VP9, AV1, ProRes, DNxHR, MPEG-2 open, play, trim and grade; H.264 and ProRes export. Sound output is partial.
- **Colour spaces** on export: sRGB, Display P3, Adobe RGB-compatible, ProPhoto, Rec.2020. Custom ICC output is not built.

## Raw cameras

- **1,446** camera models are on the reference list; about **41%** are verified on a real file, about **50%** are expected to work but unchecked, about **6%** open from the embedded preview only, about **3%** are not supported.
- **Formats decoded from sensor data**: DNG, CR2, CR3 (lossless and C-RAW), CRW, ARW (incl. the newest compressed), NEF (lossless, lossy, High Efficiency on verified bodies), RAF (Bayer and X-Trans, compressed), RW2, ORF (incl. compressed), PEF, SRW, MRW, ERF, MEF, 3FR / FFF, DCS.
- **Preview only** (embedded JPEG): Phase One IIQ, Sigma X3F, some CR3 variants, NEF lossy-after-split, a few older bodies. The grid marks them; reload re-checks them after an update.
- **Colour**: each raw starts from a fit to its own camera JPEG, a bundled profile (11 so far), spectral matrices or the public dcraw matrices (about 550 older models). Measured camera calibration is still partial; see [What is not built yet](../not-built/).
- **Lenses**: corrections from the file (DNG opcodes, Panasonic and Leica, several Sony and Nikon) and a user-supplied lensfun-format database. No bundled lens database yet.
- **Your own profiles**: import `.dcp` camera profiles you own; `<cli> camdb show MAKE MODEL` prints the entry a camera uses.
