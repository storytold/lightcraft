# Vendored crates

Third-party crates the app builds with local changes, wired in through `[patch.crates-io]` in
the workspace `Cargo.toml`. They are not workspace members (`exclude = ["vendor"]`), keep their
own licences, and change as little as possible so that an upgrade is a re-copy plus the patch.

## egui-wgpu 0.36.2 (MIT OR Apache-2.0, `vendor/egui-wgpu`)

Source: crates.io `egui-wgpu` 0.36.2 (<https://github.com/emilk/egui>), copyright Emil
Ernerfeldt and the egui contributors; licence texts in `LICENSE-MIT` / `LICENSE-APACHE`.

Patch: HDR display output (LR-VIEW-HDR-DISPLAY). Upstream egui-wgpu always picks an
8-bit surface in the default colour space and never hands the surface to the app. The changes,
each marked `Patch (HDR display output)` in the source:

- `WgpuConfiguration::hdr_output` (default `false`): configure the window surfaces as
  `Rgba16Float` in `SurfaceColorSpace::ExtendedSrgbLinear` (scRGB on Windows, EDR on macOS,
  extended linear sRGB on Vulkan / Wayland) when the surface offers it (`hdr_surface_format`).
- `RenderState::{target_color_space, surface_formats, display_hdr_info}`: the colour space
  chosen, what the first surface offered, and the display's `wgpu::DisplayHdrInfo`, re-queried
  on every surface configure (resize, moved window, OS HDR switch).
- `Renderer::set_output_scale`: egui's own drawing on a float target goes through a new
  fragment entry point, `fs_main_extended_linear_framebuffer` (sRGB-decode, then × the scale),
  so the UI stays at SDR white ("paper white") while textures holding values above 1 (the
  loupe's HDR texture) go above it.

- `WgpuConfiguration::{open_device, on_surface_configured}` (`RenderState::on_surface_configured`):
  hooks to create the device (the app adds `VK_EXT_hdr_metadata` through `crates/hdrmeta`) and to
  run after every surface configure (the app sets HDR metadata on the new swapchain).

To upgrade egui: copy the new egui-wgpu release over `vendor/egui-wgpu` (drop `.cargo-ok`,
`.cargo_vcs_info.json`, `Cargo.lock`, `Cargo.toml.orig` and the `include` list), re-apply the
marked changes, and run the app's HDR tests (`hdr_display`, `crates/hdrmeta`). Drop the patch once egui exposes the
surface format, colour space and display HDR info itself.
