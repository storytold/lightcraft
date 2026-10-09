# dac-heif

The optional HEIF / HEIC decoder of LightCraft: iPhone and Mac photos. A thin, panic-guarded wrapper
around [`heic-rs`](https://github.com/tbraun96/heic-rs), a pure-Rust HEVC still-picture decoder
(no `unsafe`, MIT OR Apache-2.0): single pictures and grid-tiled photos, 8- and 10-bit (10-bit
decodes to 16-bit), alpha auxiliary images, the container's rotation/mirror/crop, ICC, EXIF and XMP.
Read-only: writing would need an HEVC encoder.

```rust
let info = dac_heif::probe(&bytes)?;           // size, depth, alpha: check limits first
let img = dac_heif::decode(&bytes, &dac_heif::Options::default())?;
// img.width, img.height, img.has_alpha, img.sixteen_bit, img.data (RGB/RGBA), img.icc, img.exif, img.xmp
```

Errors are `Error::Unsupported` (image sequences, overlays, …), `Error::Limit` or
`Error::Malformed`. It never panics: every call into heic-rs runs under `catch_unwind`, and a panic
inside it (PhotoCraft's fuzzing found two in 0.1.1) becomes `Error::Malformed`. The crate is a port
of PhotoCraft's `photocraft-heif` — the sibling apps deliberately share one decoder, one pin and
one set of regression fixtures.

## Why it is a separate, optional crate

- **A young decoder.** heic-rs is new and has a single maintainer. It is pinned exactly
  (`=0.1.1`); moving the pin is a reviewed change (re-run the crate tests and
  `cargo test -p dac-codecs --features heif`).
- **HEVC patents are a distributor's call.** HEVC is patent-encumbered in some jurisdictions, so
  whether a build includes an HEVC decoder is a build-time choice. `dac-codecs` uses this
  crate only behind its non-default `heif` feature; the apps forward it
  (`cargo build -p dac-app --features heif`). Without it, HEIC files are still recognised and
  opening one is a clear "HEIC/HEIF support isn't included in this build" error — the same policy
  as PhotoCraft.

The crate depends on no other LightCraft crate; `codecs` → `heif` is the only edge between them.
