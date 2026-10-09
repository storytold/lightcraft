# Vendored crates

Upstream crates carried here with a small patch, until the change is available upstream.
They are not workspace members (`exclude = ["vendor"]`): `cargo xtask ci` does not lint or
format them, and they keep their own licences. Cargo uses them through `[patch.crates-io]` in
the root `Cargo.toml`.

## egui-wgpu 0.36.2

Source: <https://crates.io/crates/egui-wgpu/0.36.2> (repository
<https://github.com/emilk/egui>), MIT OR Apache-2.0, © the egui authors (see `LICENSE-MIT`,
`LICENSE-APACHE`). Its `unsafe` (surface creation from a window handle) is upstream's.

Patch (every change is marked `LightCraft patch`), for HDR display (LR-VIEW-HDR-DISPLAY):

- `SurfaceConfig::prefer_hdr`: when the window surface offers `Rgba16Float` in the extended
  linear sRGB colour space (scRGB: 1.0 = SDR white, above it brighter), use it as the target
  format and configure the surface in that colour space.
- egui's fragment shader takes its linear-output path for `Rgba16Float` targets, so the UI looks
  the same as on an 8-bit surface.

To update: copy the new upstream release here, re-apply the marked changes, bump the version
in this file.
