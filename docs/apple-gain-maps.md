# Legacy Apple gain-map reconstruction

`lightcraft_codecs::gainmap::apple` is a bounded reconstruction helper for an already decoded
SDR base and Apple's legacy, grayscale 8-bit auxiliary gain map. It is not connected to import:
JPEG / HEIF auxiliary discovery, metadata parsing, version checking, orientation and colour
management are the caller's responsibility. Existing `gainmap::apply`, `read_jpeg` and ISO /
Ultra HDR export retain their current semantics.

## Mathematical contract

For normalized map code `m` and linear headroom ratio `H`, the full HDR rendition is
`base * (1 + (H - 1) * m^2.2)`, independently for each linear RGB channel, without offsets.
Base and map must cover the same image rectangle, in the same orientation. Map samples are
bilinearly interpolated at aligned pixel centres, clamped at the edges, **before** the 2.2 curve.
The base stays in its source primaries. The helper does not perform display-headroom adaptation.

This ratio interpolation is different from ISO 21496-1 / Ultra HDR's interpolation in log2 gain.
The Apple auxiliary URN is `urn:com:apple:photo:2020:aux:hdrgainmap`; its presence alone does not
justify ignoring an ISO description of the same file. Container readers must distinguish these
families and validate their versions before choosing a reconstruction path.

[Apple's public description](https://developer.apple.com/documentation/appkit/applying-apple-hdr-effect-to-your-photos)
provides the maker-note conversion to stops and the final ratio multiplication, but describes
inverse Rec.709 for the map. The 2.2 exponent here is an **empirical Core Image compatibility
choice**, not a claim that the documentation specifies that exponent.

A complete maker-note tag 33 / 48 pair wins over `HDRGainMapHeadroom`. Its piecewise conversion
is Apple's published one, clamping negative stops to zero and converting stops to a linear ratio.
If both maker-note tags are absent, the headroom tag is treated as a linear ratio. **Tag-only
headroom is covered by arithmetic tests but has not been independently measured against Core
Image.** Partial pairs, non-finite metadata, missing headroom and ratios outside 1..=65536 are
refused. The upper bound is this helper's conservative numerical budget, not a format limit.
Malformed buffers or samples are rejected before any base pixel changes. Negative, finite RGB
channels are preserved for wide-gamut colour; no clipping or gamut conversion is introduced.

## Reproduce a comparison on macOS

The optional Swift probe uses ImageIO to extract an Apple auxiliary map and metadata, and Core
Image to render the source twice, with `expandToHDR` false and true. It renders extended linear
Display P3 float32 RGB, retaining stored orientation. The Rust example applies the helper to
**Core Image's SDR pixels**, then compares with **Core Image's HDR pixels**. This isolates map
reconstruction from differences between LightCraft's and Apple's base decoders. It is not an
end-to-end LightCraft import test.

Run from the repository root, with macOS 15 or later and Xcode command-line tools:

```sh
swiftc tools/apple-gainmap-reference.swift -o /tmp/apple-gainmap-reference
/tmp/apple-gainmap-reference /absolute/path/to/your-image.heic /tmp/apple-reference
cargo run -p lightcraft-codecs --example apple_gainmap_compare -- /tmp/apple-reference
```

The output directory must not already exist. Keep it outside the checkout: it contains decoded
image data. The probe accepts grayscale 8-bit Apple maps and sources up to 32 MP, refuses ISO
maps, strips row padding, and writes `reference.json`, `base.rgbf32le`, `hdr.rgbf32le`, and
`gain.gray8`. The JSON records OS/ImageIO versions and both stated and Core Image headroom.
The comparison prints mean and maximum absolute channel error in units of SDR white, plus
99th-percentile relative channel error with a 0.01 floor on the reference magnitude. It reports
measurements without a universal pass/fail threshold. An agent sandbox may need permission for
ImageIO's system plugins; failed auxiliary extraction is not evidence that the input lacks a map.

For redistributable inputs, the public
[libultrahdr test directory](https://github.com/google/libultrahdr/tree/66821e0a261aa3a06c0e7c889f52eced52850be1/tests/data)
contains `apple_gainmap_new.jpg` and `apple_gainmap_old.jpg`; its separate test-data license is
CC-BY-4.0. Download locally if desired; this contribution includes no media or binary references.
Their SHA-256 digests at that revision are:

| Input | SHA-256 |
|---|---|
| `apple_gainmap_new.jpg` | `492a94bd0636bcf15b3f560c68142a7783936c1597e78b0d0461d8be7fddc078` |
| `apple_gainmap_old.jpg` | `2e4310a0dd37a98678e057e25d936bbc4f936bb6fe17380c0ce99e58ecaa603b` |

Local measurements on macOS 26.7 (25G229), ImageIO 3.3.0: both JPEGs resolve to maker-note
headroom 8, matching Core Image. The newer JPEG's headroom tag says 4.532783, demonstrating
precedence. Maximum absolute channel error is below 0.000002 for both. A further local 12 MP
Apple HEIC measured below 0.000002 too, with matching resolved headroom about 3.0197604;
that input is not distributed. These are limited observations, not evidence for every Apple
version, family, orientation or map size. The committed procedural tests check the chosen
arithmetic, metadata rejection and positioning; **they are not independent Core Image oracles**.
