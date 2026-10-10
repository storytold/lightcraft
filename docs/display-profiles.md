# Display (monitor) profiles

LightCraft can show photos through the monitor's ICC profile, so colours are right on wide-gamut and calibrated
displays. Without one, previews are sRGB, which a wide-gamut panel shows oversaturated.

## Using it

Settings ▸ Display ▸ **Choose Profile…** picks an `.icc` / `.icm` file (the dialog opens where the system keeps
them: `~/.local/share/icc` on Linux, ColorSync's folders on macOS, `…\spool\drivers\color` on Windows). **Use sRGB**
goes back to no profile. The choice is an app setting (`ui.json`, `displayProfile`) and applies at once; a file that
can't be used is reported there and previews stay sRGB.

Agents and scripts: `app.displayProfile {"path": "/path/to/monitor.icc"}` (`""` or `null`: none; no `path`: report
only) returns `{path, description, kind: "matrix" | "lut", primaries: {red, green, blue: [x, y]}}`.

## How it works

- **The loupe renders into the display's gamut.** Views (Detail, Before, Compare / Reference, preset and profile
  hover, the second window) ask the pipeline for a render in the display's own primaries
  (`lightcraft_pipeline::DisplaySpace`, `RenderRequest::display`): the per-pixel stage converts from the working space
  to the display's linear RGB, gamut maps into *its* gamut and encodes with the sRGB curve, on the CPU and the GPU
  alike (only `FinishParams::to_out` / `out_luma` change). Colours an sRGB render would clip stay as saturated as the
  display can show. For a display that is exactly a standard space the result equals an 8-bit render into that space.
- **Then to the display's device values** (`lightcraft_codecs::display::DisplayProfile::correct`): for a matrix/TRC
  profile, three 256-entry tables from the sRGB curve to the profile's tone curves (nothing at all when they are the
  sRGB curve, as for Display P3); for a LUT-based profile, a `moxcms` transform through its B2A tables. A LUT-only
  profile (no colorant tags) gets its primaries measured through its A2B tables.
- **Everything made for sRGB is converted** (`lightcraft_engine::display::present`, on the render workers):
  thumbnails, cached view previews, embedded camera JPEGs and merge previews.
- **What stays sRGB:** the histogram (of the same sampled pixels, converted back to sRGB), the rendered-thumbnail and
  view-preview caches (a profile change doesn't invalidate them), exports, and the UI itself.
- **Soft proofing:** the blue "display" gamut warning shows what the monitor can't show, from its profile.
- Conversions are relative colorimetric: the display's white is the image's white.

## Cost

Measured on a 2-core cloud VM, 2560 × 1707 preview: the tone-curve tables take about 5 ms (single-threaded); converting the sampled
histogram pixels a few ms; the sRGB copy for the view-preview cache (full-quality renders only) about 30 ms; a
LUT-based profile's correction about 65 ms. Drafts during slider drags skip the full sRGB copy.

## Not yet

- Picking up the profile the operating system assigns (X11 `_ICC_PROFILE`, colord, ColorSync, WCS) and following
  the window across monitors with different profiles.
