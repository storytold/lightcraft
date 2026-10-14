# Display (monitor) profiles

LightCraft can show photos through the monitor's ICC profile, so colours are right on wide-gamut and calibrated
displays. Without one, previews are sRGB, which a wide-gamut panel shows oversaturated.

**macOS uses the screen's own profile** (the one System Settings ▸ Displays assigns) while none is chosen. macOS doesn't
colour-manage LightCraft's window (wgpu leaves its Metal layer without a colour space, which Core Animation shows
unmatched), so on a P3 screen previews used to look more saturated than the exported files in Preview or Photos (issue
#787). The profile is read again about once a second, so moving the window to another screen is followed.

**Linux uses the profile the desktop assigns in colord** (GNOME / KDE Settings ▸ Color, or a calibrator such as
DisplayCAL) while none is chosen. A background thread asks colord (`org.freedesktop.ColorManager` on the system bus)
every 3 s, so the UI never waits on D-Bus, and a profile change in the desktop's settings shows within that. eframe
doesn't say which monitor the window is on, so with several displays the profile is used only when all of them that
have one share it; otherwise, and without colord, previews stay sRGB (choose a profile by hand then). A compositor
that colour-manages windows itself with the same profile (KWin with an ICC profile set in Display Configuration)
would correct twice; KWin keeps that profile in its own settings, not colord, so this only happens when both are set.

## Using it

Settings ▸ Display ▸ **Choose Profile…** picks an `.icc` / `.icm` file (the dialog opens where the system keeps
them: `~/.local/share/icc` on Linux, ColorSync's folders on macOS, `…\spool\drivers\color` on Windows). **Use sRGB**
(on macOS and Linux **Use Screen Profile**) goes back to no profile. The choice is an app setting (`ui.json`, `displayProfile`)
and applies at once; a file that can't be used is reported there and previews stay sRGB.

Agents and scripts: `app.displayProfile {"path": "/path/to/monitor.icc"}` (`""` or `null`: none; no `path`: report
only) returns `{path, description, kind: "matrix" | "lut", primaries: {red, green, blue: [x, y]}}`; the screen's
profile (macOS or Linux, none chosen) reports `path: null, screen: true`. Hosts supply it as `Services::screen_profile`
(`apps/lightcraft/src/screen_profile.rs`, `apps/lightcraft/src/colord.rs`); headless sessions have none and stay sRGB.

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

- Picking up the profile Windows assigns (WCS: needs FFI, so an isolated `unsafe` helper crate) and X11's
  `_ICC_PROFILE`; following the window across monitors on Linux and Windows (macOS does both, above).
