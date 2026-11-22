# Vendored crates

Third-party crates the app builds with local changes, wired in through `[patch.crates-io]` in
the workspace `Cargo.toml`. They are not workspace members (`exclude = ["vendor"]`), keep their
own licences, and change as little as possible so that an upgrade is a re-copy plus the patch.

## egui-wgpu 0.36.2 (MIT OR Apache-2.0, `vendor/egui-wgpu`)

Source: crates.io `egui-wgpu` 0.36.2 (<https://github.com/emilk/egui>), copyright Emil
Ernerfeldt and the egui contributors; licence texts in `LICENSE-MIT` / `LICENSE-APACHE`.

Patch: present mode (issue #633). `WgpuConfiguration::choose_present_mode` (copied to
`RenderState`) lets the app pick the window surface's present mode from the adapter, the surface's
colour space and the offered modes at every configure; `configure_surface` falls back to
`AutoVsync` (FIFO) when the chosen mode isn't offered. Marked `Patch (present mode)`.

Patch: no panic on a lost device (issue #1029). `Renderer::update_buffers` panicked when
`Queue::write_buffer_with` gave no staging buffer (the device out of memory or lost, seen on a
Qualcomm Adreno under Vulkan), and `Renderer::render` panicked on missing buffer slices. Now the
failed upload is logged (1st, 2nd, 4th… time) and leaves its slices empty, and `render` stops drawing
when slices are missing: the frame is skipped instead of the app crashing. Marked
`Patch (no panic on a lost device)`.

Patch: HDR display output (LR-VIEW-HDR-DISPLAY). Upstream egui-wgpu always picks an
8-bit surface in the default colour space and never hands the surface to the app. The changes,
each marked `Patch (HDR display output)` in the source:

- `WgpuConfiguration::hdr_output` (default `false`): configure the window surfaces as
  `Rgba16Float` in `SurfaceColorSpace::ExtendedSrgbLinear` (scRGB on Windows, EDR on macOS,
  extended linear sRGB on Vulkan / Wayland) when the surface offers it (`hdr_surface_format`); else
  `Rgba16Float` in `SurfaceColorSpace::Bt2100Pq` (GNOME's Mutter, where Mesa offers PQ only): the
  `output_pq` uniform makes the shader convert to BT.2020 and PQ-encode, `Renderer::set_pq_output`,
  and `capture.rs` decodes it.
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

## naga 30.0.1 (MIT OR Apache-2.0, `vendor/naga`)

Source: crates.io `naga` 30.0.1 (<https://github.com/gfx-rs/wgpu>), copyright the gfx-rs
developers; licence texts in `LICENSE.MIT` / `LICENSE.APACHE`.

Patch: no ArrayStride outside buffers (plan item 0.2). naga's SPIR-V writer decorates every
array type with `ArrayStride`, including arrays only used by Function, Private and Workgroup
variables (`hist.wgsl` `local_bins`, the `blur.wgsl` median window, `finish.wgsl`
`band_weights`, the `clip.wgsl` / `faces*.wgsl` / `bm3d*.wgsl` scratch arrays). The Vulkan
validation layer rejects that (VUID-StandaloneSpirv-None-10684: "the Workgroup storage class
has a explicit layout from the ArrayStride decoration"), and the full `lcc-ui-egui` test run on
Vulkan logged those errors before crashing. `src/back/spv/writer.rs` now collects the types
reachable from uniform / storage / immediate / task-payload globals (`explicit_layout_types`)
and only decorates those arrays. An array type shared by a buffer and a local would still carry
the stride; none of our shaders has one. Marked `Patch (no ArrayStride outside buffers)`.

To upgrade wgpu: copy the matching naga release over `vendor/naga` (drop `.cargo-ok`,
`.cargo_vcs_info.json`, `Cargo.lock`, `Cargo.toml.orig`), re-apply the marked change, and run
`WGPU_BACKEND=vulkan cargo test -p lcc-gpu --lib` with the Vulkan SDK's validation layer
installed: no `VUID-StandaloneSpirv-None-10684`. Drop the patch once naga stops emitting it.

## candle-core 0.9.2 (MIT OR Apache-2.0, `vendor/candle-core`)

Source: crates.io `candle-core` 0.9.2 (<https://github.com/huggingface/candle>), copyright the
candle authors; licence text in `LICENSE` (Apache-2.0; the crate declares MIT OR Apache-2.0).

Patch (zip 9): upstream pins zip 7.x, whose extract-to-disk advisories (RUSTSEC-2026-0367/68/69,
all fixed in zip >= 9.0) have no fixed 7.x release; candle 0.10+ would drag in `tokenizers`
(C++ via esaxx-rs), so the dependency stays at 0.9.2 and only the zip version moves. Changes,
each marked `Patch (zip 9)`:

- `Cargo.toml`: `[dependencies.zip]` version 7.2.0 -> 9.2.0 (`default-features = false`, as before).
- `src/npy.rs`: `ZipFile::name()` now validates the entry's encoding and returns `Result<Cow<str>>` — two call sites use `?` and borrow the name.
- `src/pickle.rs`: `ZipArchive::file_names()` items are now `Result<Cow<str>>` — `read_pth_tensor_info` maps each item into the crate's error type before collecting.

Everything else is upstream 0.9.2 unchanged; lcc-ml only touches safetensors loading, so the
patched paths (npz/pth) are not on the app's critical path. To upgrade candle: re-copy the new
release, re-apply the marked changes (or drop the patch once upstream bumps zip to 9 and stays
free of C dependencies).
