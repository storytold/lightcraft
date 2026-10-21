# HDR display

How LightCraft shows an HDR edit in HDR on an HDR monitor (tracker row LR-VIEW-HDR-DISPLAY).

## The window

The window asks egui-wgpu for an HDR surface (`SurfaceConfig::prefer_hdr`). When the surface
offers `Rgba16Float` in the extended linear sRGB color space, that becomes the target format.
That color space is scRGB: `1.0` is SDR white, and larger values are brighter.

wgpu 30 does the per-platform setup, so LightCraft has no platform code and no `unsafe` for this:

| Platform | What wgpu does |
|---|---|
| Linux / Vulkan | `VK_COLOR_SPACE_EXTENDED_SRGB_LINEAR_EXT` |
| macOS / Metal | extended linear sRGB or Display P3, and the EDR layer setup |
| Windows / DX12 | `DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709` through `SetColorSpace1` |

egui draws its UI at SDR white on that surface, so the UI looks the same as on an 8-bit window.

An scRGB surface has defined primaries (sRGB), and the system converts it for the panel. So while
the window is HDR, previews and thumbnails are made in sRGB and the monitor profile (Settings ▸
Display, or the screen's own profile) is set aside: applying it as well would convert twice, and
SDR photos would no longer match their HDR rendition. The setting is kept and applies again on an
SDR window; Settings ▸ Display says so.

`prefer_hdr` is emilk/egui#8703. Until an egui release has it, the root `Cargo.toml` takes every
egui crate from one git branch: egui 0.36.2 plus that commit. Whether to keep that patch or wait for
the release is a maintainer decision.

On Linux only native Wayland windows get an HDR surface (X11 windows, XWayland included, don't). So
while HDR display is on, LightCraft keeps native Wayland, and file drag-and-drop from the file
manager isn't available (winit has none on Wayland). With HDR display off it runs through XWayland
for drag-and-drop. `LIGHTCRAFT_WAYLAND=1` / `0` forces one or the other.

On KDE Plasma (Wayland), the surface is offered even with the system's HDR mode off: values above
`1.0` are then clipped to SDR white. Switching the system's HDR mode while LightCraft runs takes
effect live, with nothing to recreate.

## The loupe

`RenderJob::with_hdr_display(true)` is set by the loupe when the window is HDR. For an HDR edit
(not for overlays, soft proofs, windowed renders or high-bit-depth requests), the job renders its
HDR rendition as well:

- the extra render is `OutputDepth::F32Hdr` in sRGB primaries: display-linear, `1.0` = SDR white;
- it goes into `Rendered::deep`;
- the usual 8-bit SDR rendition stays the image that overlays, previews, thumbnails and the
  histogram use.

ui-egui keeps the HDR values on the loupe texture (`hdr_view::HdrPixels`) and paints them through
the host's `hdr_view::HdrPresenter`. ui-egui stays renderer-agnostic, since the web build uses glow.

The desktop app's presenter is `lightcraft-hdr-display::HdrDisplay`:

- it uploads the values as an `Rgba16Float` texture, only when the render's key changes;
- it draws them with an egui-wgpu paint callback over the image's rectangle. egui-wgpu clamps the
  callback's viewport to the window, so the shader takes texture coordinates from each fragment's
  position in the image's whole rectangle: a zoomed photo that reaches past the window keeps its
  pan;
- when a display headroom is set (`HdrDisplay::set_headroom`, the highest value the display shows),
  it eases highlights above 80 % of it in towards it, keeping hue, instead of letting the panel clip.

HDR edits on an HDR window render the whole frame (no zoomed 8-bit windows). The HDR render runs
on the CPU, so a drag's drafts skip it: while a slider moves, the photo shows its SDR rendition
(the highlights at SDR white), and the full render at the end brings the HDR rendition back. On
the CPU it would make each draft 3–5× slower than the GPU's SDR draft (measured 30 → 104 ms at
667×1000, 49 → 264 ms at 1600×2400).

## Settings and checks

- **Settings ▸ Performance ▸ Show HDR photos in HDR on HDR displays** (applies after a restart,
  since the surface is chosen when the window is created). Off by default on Linux and the BSDs,
  where it means native Wayland (no file drag-and-drop) and the window can't tell beforehand
  whether the monitor is HDR: KDE offers the surface with the system's HDR mode off too, and the
  monitor profile would then be set aside for nothing. On by default on macOS and Windows.
- **`LIGHTCRAFT_HDR_DISPLAY=0` / `1`** overrides the setting.
- **`ui.inspect` → `hdrDisplay`:** `active` (the window is HDR) and `loupeHdr` (the loupe paints an
  HDR rendition).
- Screenshots of an HDR window work: egui-wgpu reads the frame back as sRGB, with HDR clipped.
  Headless screenshots (`ui.screenshot {"headless": true}`, and the fallback when no frame comes)
  rasterize on the CPU, which has no paint callbacks, so they draw the loupe's SDR rendition.

## Status

- **Linux:** confirmed on an HDR monitor (KDE Plasma 6.6 Wayland, Intel Xe, Mesa 26).
- **macOS:** planned on top of this. Choose extended linear sRGB or Display P3, check that the
  swapchain gets EDR, and feed wgpu's `DisplayHdrInfo::tone_map_headroom` to
  `HdrDisplay::set_headroom`, re-queried when the display, the window's screen or the brightness
  changes. The surface isn't reachable through eframe today, so this needs a way to query it.
- **Windows:** untested.
- **Not yet:** zoomed windows, compare, the second window, a GPU HDR render, and a "Preview for SDR
  display" toggle.
