# lightcraft-codecs (L1)

Standard image formats for LightCraft: format sniffing, decoding to **linear light** with colour
management, fast thumbnails, and encoders with metadata embedding. Pure Rust, no C dependencies,
builds for `wasm32-unknown-unknown`.

## API at a glance

```rust
let fmt: Option<Format> = sniff(&bytes);                 // JPEG/PNG/TIFF/WebP/AVIF/HEIF/JXL/GIF/BMP/PSD/raw
let d: Decoded = decode(&bytes, DecodeOptions::default())?;
// d.image: Rgb32f, linear light in the source primaries (d.space: SourceSpace)
// d.alpha, d.bit_depth, d.float, d.orientation (EXIF value, NOT applied), d.exif/xmp/icc (raw blobs)
let working: Rgb32f = to_working(&d);                    // linear Rec.2020 D65 (Bradford)
let fast = decode(&bytes, DecodeOptions::fit(2048, 2048))?; // JPEG: DCT-domain 1/2..1/8 scaling
let t: Thumbnail = decode_thumbnail(&bytes, 256)?;       // embedded EXIF/MPF preview or scaled decode
let h: Header = read_header(&bytes)?;                    // stored width/height + orientation, no pixels decoded

let icc = icc::write_named(NamedSpace::DisplayP3);       // or icc::write_matrix_trc(&space, &Trc::…)
let meta = EncodeMeta { icc: Some(&icc), exif: Some(&exif), xmp: Some(&xmp) };
encode_jpeg(&EncodeImage::rgba8(&img), 90, ChromaSubsampling::S444, &meta)?;
encode_png(&EncodeImage::new(w, h, 4, Samples::U16(&px)), &meta)?;
encode_tiff(&EncodeImage::new(w, h, 3, Samples::F32(&px)), TiffCompression::Deflate, &meta)?;
encode_webp_lossless(&img, &meta)?;
encode_avif(&img, 70, 6, &meta)?;                        // native + feature `avif`

// HDR gain map JPEG (ISO 21496-1): SDR + HDR renditions → base + gain map, and back
let (map, gm) = gainmap::compute(&sdr_linear, &hdr_linear, w, h, luma, &GainMapOptions::default())?;
gainmap::encode_jpeg(&EncodeImage::new(w, h, 3, Samples::U8(&base8)), &map, &gm, 90, ChromaSubsampling::S444, &meta)?;
let found = gainmap::read_jpeg(&bytes);                  // ours, cameras', Lightroom's
gainmap::apply(&mut base_linear, w, h, &found.gain, &found.meta, 1.0)?;
```

Thumbnail fallback decoding limits the source to 64 million pixels by default.
Non-JPEG decoders allocate at source resolution before resizing, even for a tiny
thumbnail. Use `decode_thumbnail_with` and `ThumbnailOptions::max_pixels` to set
a different source budget. Oversized sources return `Error::TooLarge`; embedded
JPEG previews can still be used without decoding the full source image.

`Format::RawTiffLike` (DNG, CR2, NEF, ARW, PEF, ORF, RW2, SRW, …) and `Format::RawOther` (CR3, RAF,
CRW, MRW, X3F) are detected so the engine can route them to `lightcraft-raw`; `decode` returns
`Error::Unsupported` for them.

## Formats

| Format | Decode | Encode | Notes |
|---|---|---|---|
| JPEG | yes (zune-jpeg; jpeg-decoder for DCT scaling, CMYK/YCCK, 12-bit, non-interleaved scans) | yes (jpeg-encoder, baseline, 4:4:4/4:2:2/4:2:0) | EXIF (APP1), XMP (APP1, ≤ 64 KiB), ICC (APP2, multi-chunk), Adobe APP14, MPF previews |
| PNG | yes, 1–16-bit, palette, tRNS, interlaced | 8/16-bit, gray/GA/RGB/RGBA | iCCP, eXIf, iTXt XMP; sRGB/cICP/cHRM/gAMA honoured when no ICC |
| TIFF | 8/16-bit int (32/64-bit int kept to 16 bits), 16/32/64-bit float, gray/RGB/CMYK/palette, alpha (assoc./unassoc.), planar, LZW/Deflate/PackBits/JPEG | 8/16-bit, 32-bit float; none/LZW/Deflate/PackBits | ICC (34675), XMP (700), Orientation; first IFD only; EXIF not written (use `lightcraft-tiff`) |
| WebP | lossy + lossless, alpha, first frame of animations | lossless only | ICCP/EXIF/XMP chunks |
| GIF / BMP | yes (first GIF frame) | — | via `image` |
| PSD / PSB | merged composite: 8/16/32-bit gray, RGB, CMYK, indexed, duotone (as gray); raw/RLE | — | ICC (1039), EXIF (1058), XMP (1060); ZIP-compressed composite, Lab and 1-bit unsupported |
| JPEG XL | yes (jxl-oxide, feature `jxl`, default on) | — | enum colour → rendered straight to linear Rec.2020; ICC → our ICC path; orientation applied by the decoder (reported as 1) |
| AVIF | **no** | yes (ravif/rav1e, native only, feature `avif`) | 8/10-bit sRGB, EXIF; no ICC in the muxer. HDR: `encode_avif_pq` writes 10-bit BT.2020 PQ (SDR white at 203 cd/m², CICP `nclx` + `clli`) |
| HEIC/HEIF | yes with feature `heif` (lightcraft-heif: heic-rs + libheif-matching colour; 4:2:0/mono, 8–10-bit, grids, irot/imir/clap, ICC/nclx/VUI, EXIF/XMP, thumbnail item) | — | 4:2:2/4:4:4 (Canon/Sony HIF) unsupported |
| Gain map JPEG (HDR) | gain map found and decoded (`gainmap::read_jpeg`): ISO 21496-1 metadata, else Adobe `hdrgm` XMP | yes (`gainmap::encode_jpeg`) | CIPA DC-007 MPF index (gain map typed `0x050000`), ISO 21496-1 APP2 on both images, `hdrgm` + Container XMP for Android/Chrome, Apple `HDRGainMap`/`HDRToneMap` XMP; gain maps are never used as thumbnails |

## Colour

Decoders never guess silently; `SourceSpace::origin` says where the interpretation came from:
`IccMatrixTrc` (profile applied exactly: TRC linearization + colorants → XYZ D50), `IccCms` (LUT-based
or CMYK profile converted by the CMS straight to linear Rec.2020), `Container` (PNG chunks, JXL enum,
EXIF `R03` Adobe RGB hint), `Untagged` (assumed sRGB; untagged float data assumed linear),
`IccUnsupported` (profile present but unusable → **sRGB fallback**), `Naive` (CMYK without profile).

`decode_jpeg_with_fallback(bytes, opts, space)` lets an enclosing RAW container supply the colour
space of a JPEG with no ICC or EXIF metadata. It applies the transfer curve before linear-light
resizing; a JPEG carrying its own metadata retains its existing interpretation.

ICC profiles are parsed with `moxcms` (v2/v4, `curv`/`para` TRCs, `chad`); sRGB, Display P3,
Adobe RGB (1998), ProPhoto (ROMM) and Rec.2020 are recognised by colorants. `icc::write_matrix_trc`
emits v4 display profiles (D50 PCS, Bradford `chad`) for export embedding.

## Dependencies and licences

All pure Rust, verified with `cargo info` / `cargo tree`:

| Crate | Licence | Use |
|---|---|---|
| zune-jpeg, zune-core | MIT OR Apache-2.0 OR Zlib | JPEG decode |
| jpeg-decoder | MIT OR Apache-2.0 | scaled / CMYK JPEG decode |
| jpeg-encoder | (MIT OR Apache-2.0) AND IJG | JPEG encode (IJG: permissive, attribution in docs) |
| png | MIT OR Apache-2.0 | PNG |
| tiff | MIT | TIFF |
| image-webp | MIT OR Apache-2.0 | WebP |
| image (gif, bmp only) | MIT OR Apache-2.0 | GIF/BMP |
| moxcms | BSD-3-Clause OR Apache-2.0 | ICC parse/write, CMS |
| jxl-oxide | MIT OR Apache-2.0 | JPEG XL decode |
| ravif (+ rav1e, avif-serialize) | BSD-3-Clause / BSD-2-Clause | AVIF encode (native only; heavy to compile) |

Deliberately **not** used: anything AGPL from imazen (heic, zenjpeg, jxl-encoder, rav1d-safe),
jpegxl-rs / imagequant (GPL / C), dav1d / libheif (C).

## Performance (Apple M-series, release, synthetic 24 MP JPEG q90 4:2:0)

Run `cargo test --release -p lightcraft-codecs --test bench -- --ignored --nocapture`.

| Operation | Time |
|---|---|
| full decode → linear `Rgb32f` | ~92 ms (≈ 260 MP/s) |
| decode fit 2048 (DCT 1/2) | ~81 ms |
| thumbnail 256, scaled decode (DCT 1/8) | ~54 ms (entropy decoding bound) |
| thumbnail 256 from embedded EXIF/MPF preview | ~0.6 ms |
| `to_working` matrix | ~23 ms |
| PNG / TIFF-deflate encode (fast levels) | ~430 ms / ~900 ms |

## Gaps / limitations

- **AVIF decode**: no permissively licensed, pure-Rust AV1 decoder exists today (dav1d is C;
  rav1d-safe is AGPL). Files are sniffed and rejected with `Error::Unsupported`.
- **HEIC** needs the `heif` feature (off by default: HEVC patents are the distributor's call);
  without it, `Error::Unsupported` says the build has no HEIC support. 4:2:2 and 4:4:4 HEIF
  (Canon/Sony HIF) is refused: heic-rs 0.1.1 loses sync on those bitstreams.
- JPEG: arithmetic coding unsupported (as in both decoders); extended XMP (> 64 KiB) neither read
  nor written; EXIF > 64 KiB not written. The encoder uses standard Huffman tables (interleaved
  baseline) for maximum compatibility.
- Thumbnails under 15 ms need an embedded preview ≥ the requested edge (EXIF IFD1 or MPF). Most
  cameras store a 160 px EXIF thumbnail: use `decode_thumbnail_with` with `min_embedded_edge: 160`
  for an instant placeholder, otherwise the DCT-scaled path takes ~50 ms for 24 MP.
- TIFF: first image only; EXIF is not re-emitted on encode; Lab/uncompressed-YCbCr unsupported.
- PSD: only the merged composite (no layers); ZIP-compressed composites unsupported.
- Panics inside third-party decoders are caught (`catch_unwind`) on unwinding targets; on wasm32
  (`panic = abort`) we rely on the property tests of our own code plus the decoders' own hardening.
