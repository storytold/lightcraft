# Display (monitor) profiles

The app can show photos through the monitor's ICC profile, so colours are right on wide-gamut and calibrated
displays. Without one, previews are sRGB, which a wide-gamut panel shows oversaturated.

**macOS uses the screen's own profile** (the one System Settings ▸ Displays assigns) while none is chosen. macOS doesn't
colour-manage the app's window (wgpu leaves its Metal layer without a colour space, which Core Animation shows
unmatched), so on a P3 screen previews used to look more saturated than the exported files in Preview or Photos (issue
#787). The profile is read again about once a second, so moving the window to another screen is followed.

**Linux does the same** (`apps/app/src/screen_profile_linux.rs`, no `unsafe`): on X11 and XWayland it reads the
`_ICC_PROFILE_n` root-window property (ICC Profiles in X) of the RandR monitor the window overlaps most, the one that
colour-management daemons and calibration tools set; without it, the default profile colord assigns to that monitor
(matched by `XRANDR_name`) over D-Bus. On pure Wayland a client can't learn which monitor it is on, so the first colord
display device is used.

**Windows** (`apps/app/src/screen_profile_windows.rs`, no `unsafe` in our code: the `winreg` crate's safe API) reads the
Windows Color System association from the registry: the device's per-user `ICMProfile` list when "use my settings for
this device" is on (`HKCU\…\ICM\ProfileAssociations\Display\{4d36e96e-…}\NNNN`), else the system-wide one
(`HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e96e-…}\NNNN`); the default is the list's last entry, a file in
`%SystemRoot%\System32\spool\drivers\color` (or an absolute path), checked like on Linux. Finding the window's monitor
needs FFI (`MonitorFromWindow`), so the first display device with a profile is used.

## Using it

Settings ▸ Display ▸ **Choose Profile…** picks an `.icc` / `.icm` file (the dialog opens where the system keeps
them: `~/.local/share/icc` on Linux, ColorSync's folders on macOS, `…\spool\drivers\color` on Windows). **Use sRGB**
(on macOS and Linux **Use Screen Profile**) goes back to no profile. The choice is an app setting (`ui.json`, `displayProfile`)
and applies at once; a file that can't be used is reported there and previews stay sRGB.

Agents and scripts: `app.displayProfile {"path": "/path/to/monitor.icc"}` (`""` or `null`: none; no `path`: report
only) returns `{path, description, kind: "matrix" | "lut", primaries: {red, green, blue: [x, y]}}`; the screen's
profile (macOS and Linux, none chosen) reports `path: null, screen: true`. Hosts supply it as `Services::screen_profile`
(`apps/app/src/screen_profile.rs`); headless sessions have none and stay sRGB.

## How it works

- **The loupe renders into the display's gamut.** Views (Detail, Before, Compare / Reference, preset and profile
  hover, the second window) ask the pipeline for a render in the display's own primaries
  (`lcc_pipeline::DisplaySpace`, `RenderRequest::display`): the per-pixel stage converts from the working space
  to the display's linear RGB, gamut maps into *its* gamut and encodes with the sRGB curve, on the CPU and the GPU
  alike (only `FinishParams::to_out` / `out_luma` change). Colours an sRGB render would clip stay as saturated as the
  display can show. For a display that is exactly a standard space the result equals an 8-bit render into that space.
- **Then to the display's device values** (`lcc_codecs::display::DisplayProfile::correct`): for a matrix/TRC
  profile, three 256-entry tables from the sRGB curve to the profile's tone curves (nothing at all when they are the
  sRGB curve, as for Display P3); for a LUT-based profile, a `moxcms` transform through its B2A tables. A LUT-only
  profile (no colorant tags) gets its primaries measured through its A2B tables.
- **Everything made for sRGB is converted** (`lcc_engine::display::present`, on the render workers):
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

- Following the window across monitors on Windows (the first display device's profile is used).
- Per-monitor profiles on pure Wayland (until a colour-management protocol tells clients their output).
