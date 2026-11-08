# Olympus compressed ORF: Clean-Room Implementation

LightCraft decodes uncompressed 16-bit, packed 12-bit, two-field 12-bit, 12.8 bpp blocks and compressed ORF/ORI sensor data in pure Rust.

The compressed ORF sensor decoding engine is implemented clean-room in `crates/raw/src/vendor/orf_compressed.rs`, adhering strictly to the clean-room requirements in `AGENTS.md` and `CLAUDE.md`.

## Provenance & Clean-Room Documentation

The implementation is accompanied by full clean-room derivation records:
- **Specification:** [`clean-room/orf/orf-spec.md`](../clean-room/orf/orf-spec.md), derived purely from US Patent US7333035B2 and US Patent US7715639B2 (Takashi Ishikawa, Olympus Corporation), TIFF 6.0, Exif 2.3 specifications, and black-box analysis of public CC0 samples from raw.pixls.us.
- **Audit Record:** [`clean-room/orf/10-code-audit.md`](../clean-room/orf/10-code-audit.md), certifying independent formulation, zero GPL/LGPL derivation, and absence of external code translation.
- **Implementation:** [`crates/raw/src/vendor/orf_compressed.rs`](../crates/raw/src/vendor/orf_compressed.rs).

## Verification on Real CC0 Corpus

Verified against real CC0 files from `raw.pixls.us`:

| File | Camera Model | Sensor Size | Green Photosite $r$ vs Embedded Preview |
| :--- | :--- | :---: | :---: |
| `orf-olympus-em1.orf` | Olympus OM-D E-M1 | 4640 × 3472 | 0.995 |
| `orf-olympus-em10iii.orf` | Olympus OM-D E-M10 Mark III | 4640 × 3472 | 0.929 |
| `Olympus - E-M1MarkII - 16bit (4_3).ORI` | Olympus OM-D E-M1 Mark II (ORI) | 5240 × 3912 | 0.998 |
| `Olympus - E-M5 Mark II - 16bit 16bit _normal_ (4_3).ORI` | Olympus OM-D E-M5 Mark II (ORI) | 4640 × 3472 | 0.994 |

The test suite `compressed_olympus_bodies_decode_like_camera_jpeg` in `crates/raw/tests/it/olympus_samsung_corpus.rs` checks that decoded green mosaic sites follow the camera's embedded JPEG with $r > 0.6$. Note that this test skips missing files when the local corpus is unavailable rather than enforcing a mandatory four-file regression check, and asserts the $r > 0.6$ correlation threshold rather than the empirical values reported above.
