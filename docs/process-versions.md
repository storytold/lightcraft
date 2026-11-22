# Process versions

Every photo's develop settings record the **rendering process** they are interpreted by
(`DevelopSettings::process`, a `ProcessVersion` in `crates/develop/src/settings.rs`). Lightroom calls
this the process version. It lets the look of the app improve (a new default tone curve, issue #146;
stronger Light sliders, issue #196) without changing a single photo someone has already edited.

| Process | What it changes |
|---|---|
| **V1** | the app's rendering from before process versions existed. Frozen. |
| **V2** | Highlights and Shadows keep fine detail (issue #632): their base layer (`local::tone_base`) is the edge-aware filter of a separable median of log luminance (radius 0.15 % of the long edge, at most 24 px, `local::BASE_DETAIL`) instead of log luminance itself, so twigs, lines and texture take their surroundings' shift and keep their contrast; region edges stay sharp. CPU (`local::prepare`) and GPU prepare branch on `local::tone_base_detail` (0 = no median under V1); the base plane is cached per process (`local::BaseKey`). Renders without Highlights or Shadows (global or local) are the same as V1's. Tone map unchanged. |
| **V3** | Dehaze (issue #1237): the haze plane is the Gaussian of a patch minimum (`local::min_hv`, radius 0.5 % of the long edge, at most 64 px, `local::dehaze_patch_radius`) of each pixel's darkest channel instead of the Gaussian of that channel itself, so textured, haze-free regions are no longer read as haze; positive amounts ease in (`k·(0.4 + 0.6·k)`), the transmission floor is 0.3, and the veil removed never exceeds 90 % of the pixel's darkest channel (`finish::dehaze_v3_pixel`), so shadows next to bright sky no longer clip to black. Negative dehaze is V1's. The dark plane's cache key carries the patch radius (`local::dark_key`). Renders without Dehaze are the same as V2's. |
| **V4** | Sharpening (upstream #757 / #762): an unsharp mask of log luminance against its own exact Gaussian (`local::sharpen_sigma`, σ = Radius in source pixels × output/source scale, 0.3–4 px, fading out below 0.5 px), soft-clipped by Detail (`finish::sharpen_limit`, `finish::sharpen_term`), gated by Masking on that term; local Sharpness uses the same band. Before V4 sharpening boosts the Texture band and ignores Radius and Detail (`finish::sharpen_v4` picks; GPU `SHARPEN_V4`). Renders without sharpening are the same as V3's. |
| **V6** | AI Denoise's Amount eases in: the share of the denoised picture mixed in is `1 − (1 − a)³` of the slider fraction `a` (`settings::denoise_weight`, applied in `DevelopSettings::denoise_amount`) instead of `a` itself, so 50 leaves about 0.17 of the noise σ on a flat patch where the straight mix left 0.51 (`tests_denoise::denoise_response_v6_is_mostly_clean_at_half_and_monotone`), close to Lightroom, whose Denoise is nearly clean by 30–50. The mix stays linear in scene-linear light (`lcc_denoise_core::product::blend`). Renders without AI Denoise, or at 0 or 100, are the same as V5's. |
| **V7** | Film grain is drawn as clumps of irregular particles (a filtered Boolean model after Newson, Delon & Galerne, IPOL 2017, resolution independent), with Size Variation, Clumping, Color and Response controls, its strength calibrated against Lightroom Classic exports of a grey chart (aggregate numbers only; [grain.md](grain.md); OneSlips). V1–V6 keep their value-noise grain, whatever the new grain fields say (`tests/it/grain_process.rs`). Renders without grain are the same as V6's. |
| **V8** (the latest) | 8-bit output is dithered (adapted from upstream #3509): before it is truncated to 8 bits, each channel gets an offset from an 8×8 ordered (Bayer) matrix (`finish::bayer_dither_offset`, every threshold once, mean a half) instead of a plain half-level rounding, so a smooth gradient no longer breaks into flat bands: on a slow ramp the worst 8×8 tile mean is 0.03 levels from the exact tone where rounding leaves 0.43 (`tests/it/dither_process.rs`). Black, white (anything within a tenth of a level of it) and every exact 8-bit level stay exact. The pattern belongs to the whole output (its origin is the window's place in it, floored), so zoomed tiles line up, and the GPU computes the same thresholds (`bayer_dither` in `finish.wgsl`, GPU `DITHER`; `dithered_gradients_match_in_phase`). 16-bit and float output are not dithered. A raw's tone curve also rolls off into white in stops (adapted from upstream #3783), so raising Exposure keeps highlights the sensor recorded apart instead of turning them white: a camera curve past its last fitted knot (`CameraTone::apply_rolloff`, `ToneMap::camera_rolloff_hdr`), where V1–V7 close on white exponentially in linear light, and the generic raw curve from half a stop below its white point (`ToneMap::rolloff_hdr`, `GENERIC_KNEE_STOPS`), where V1–V7 clip. Both use one shoulder (`tone::rolloff_past`): same value and slope at the join, then the gap to white halving every stop (`ROLLOFF_STOPS`). On the CC0 corpus raws (long edge 1000, import defaults) the share of pixels at 8-bit white at Exposure +2 drops from 18–39 % to 7–18 % where the highlights weren't clipped in the raw (Sony A7 III, A7 IV, A6400A, Nikon Z 6II, Zf), and at +1 the A7CR's from 14–15 % to 2–3 %; at Exposure 0 the green channel moves by at most 1 level on average. JPEGs and other display-referred sources are unchanged; an HDR edit's SDR rendition rolls the V8 curve off into SDR white as it does the V7 one (`ToneMap::sdr_rolloff`). The CPU and GPU share the tone table (`base_tone`). Colour noise reduction filters the linear colour differences R−Y and B−Y jointly with the method in `docs/nr-color-joint.md` (adapted from upstream #1348): edge-aware weights from a denoised guide shared by both differences, diffusion over increasing scales and a brake for isolated colour specks; Y is kept exactly, and the noise model is measured on the whole frame, so a loupe window is filtered like the export. V1–V7 keep the blurred chromaticity and the speckle median (`local::denoise_with`); the CPU (`local::denoise_with`) and GPU (`render::denoise`) branch on `local::joint_colour_nr`. |
| **V5** | Over-range colours keep their hue (upstream #2262): after the tone map and in the output's gamut mapping, a colour whose brightest channel is above white has that channel limited to white with the darkest kept and the middle one in its place between them (`finish::hue_clip`), instead of moving toward its luminance (bright saturated reds went pink). It fades back to the V4 mapping as its brightest channel goes over white (fully at 0.5 EV over, `HUE_FADE_EV`) or as the colour's luminance nears white (from 35 %, `HUE_FADE_FROM`), so a blown sky still goes white and exposure ramps have no step; negative channels are lifted toward luminance after the limit (`finish::lift_negatives`). GPU `HUE_CLIP`. Renders with no colour above white are the same as V4's. Tuned in place on 2026-10-30 (review fix 5): the fade keyed mainly on luminance (brightest-channel fade over 2 EV), so dark-luminance hues (blue, violet, magenta) stayed purple up to two stops over while warm hues of the same brightest channel went white; the brightest-channel key now does most of the fading, over 0.5 EV, so every hue fades alike. It changes only photos on V5: the process is kept per photo, so photos on V4 keep V4's look until someone runs Update to Current Process. |

## The rule

**A change that alters how existing settings render ships as a new process. V1 is frozen.**

- A deliberate change of look (tone model, slider response, default curve, how a stage interprets a
  value) goes into a new process. Photos stay on the process they have until someone runs Update to
  Current Process (or Reset).
- Bug fixes that make output correct where it was broken (a crash, NaN or black output, a decoder
  error, CPU and GPU disagreeing) are not looks and can land in place, as before.
- Changes to how a file is decoded or which camera colour it gets change the source, not how settings
  are interpreted; the process doesn't reach that stage today.

**HDR edits' SDR rendition (fork #69)** landed in place rather than as a process: the rendition
used to clip the highlights the HDR edit keeps (thumbnails and SDR exports lost them), which is
output broken where it should be correct. It changes nothing for SDR edits (HDR off), on any process.

**Neutral display tone map** (JPEG, PNG and other display-referred sources with Contrast, Whites and
Blacks at 0) is the identity up to white: the SDR shoulder no longer pulls 1.0 down to 0.9875 (8-bit
255 to 254, issue #1945). It landed in place as a bug fix, since the tone map promised identity at
neutral settings; edited curves keep the shoulder and their V1 look.

**A mask whose first component is Intersect** starts from that component, on the CPU and the GPU
(upstream #3854). It used to start from nothing, so alone it selected nothing, with a later Add it gave
only what was added, and with the mask's Invert it gave the whole photo. Such masks now include the
first component, on every process. It landed in place as a bug fix: intersecting with nothing is not
a look anyone chose.

**AI mask edges** (Object and Describe selections stored as a model's grid) follow the photo's edges
when they are enlarged to the output (upstream #3298): the plain enlargement spilled 1–2 px onto the background,
which a raised Exposure turned into a bright outline around the subject in previews and exports.
Existing AI masks become 1–2 px tighter at a subject's edges, on every process; their inside, their
outside and edges without contrast in the photo are unchanged. It landed in place as a bug fix: the
outline is output that is neither in the photo nor in the edit, not a look anyone chose.

## Adding a process

Before a retune, regenerate the render-fidelity references on `main` (`cargo xtask fidelity --regen`,
[testing-sil](testing-sil.md#render-fidelity-reference-suite)) and run `cargo xtask fidelity` on the change: the
existing processes must stay within tolerance.


1. Add a variant to `Process` (`V2`), append it to `Process::ALL` (oldest first), give it its number in
   `Process::version` and make it `Process::LATEST`.
2. The compiler then points at every `match` on `Process`. The base tone map is chosen in one place,
   `lcc_pipeline::finish::base_tone` (both the CPU and the GPU renderer take it from there): give
   V2 its own arm and leave V1's alone. A process that changes another stage branches there the same
   way, on `s.process.process()`; if that stage is cached (`Plan::lin_key`, the planes in `local.rs`),
   add the process to its key.
3. Keep `settings_saved_before_process_versions_load_as_v1` (`crates/develop/src/tests_process.rs`)
   passing: settings saved without the field must stay V1, not become the new latest. That is the
   field's `#[serde(default = "ProcessVersion::legacy")]`; the struct-level `Default` gives new
   settings the latest instead.
4. Render a few procedural scenes on both processes and compare, and update `docs/parity-checklist.md`.

## What gets which process

| | Process |
|---|---|
| A newly imported, merged or Lightroom-imported photo | the latest |
| Settings saved without the field (catalogs and their snapshots, versions and history, `lc:settings` in XMP sidecars, browser libraries: everything from before process versions), or with a damaged value | V1 |
| Reset (`develop.reset`) | the latest |
| Reset of a section or a slider | unchanged |
| Update to Current Process (`develop.updateProcess`) | the latest, for photos on an older one; one undo step. The menu item follows the selection; a call with `ids` / `id` acts on those photos whatever is selected |
| Virtual copy, duplicate | the source's |
| Copy / paste / sync / Auto Sync / presets | unchanged (see below) |
| `crs:` edits (XMP from other raw developers, Lightroom catalogs, imported presets) | unchanged; a new photo has the latest |
| Restoring a version or history step | the one it had |

**Copy, paste, sync and presets** never carry the process: it is in no settings group, so the photos
they change keep their own and interpret the pasted values with it. (Lightroom offers the process
version as a separate checkbox when syncing; the app's groups are coarser and keep it out.) Partial
settings JSON that names `process` itself (`develop.merge`, a hand-written preset) sets it.

**Lightroom edits** are re-interpreted by the app's mapper (`crates/engine/src/crs.rs`), which is
written against the current rendering. `crs:ProcessVersion` numbers Adobe's renderer, not ours, and is
ignored; a photo imported with Lightroom edits is on the app's latest process, and an existing
photo that receives them keeps its own.

**A number this build doesn't know** (a library or sidecar written by a newer version of the app) loads, is
kept when saved, and renders with the newest process this build has. Update to Current Process never
moves a photo to an older process; Reset does move it to this build's latest.

**A damaged value** (anything but a whole number from 0 to 2^32 - 1; `2.0` counts as 2) reads as V1,
like a missing field, and costs nothing else: the rest of the settings load from the catalog, a
snapshot, the op log or an `lc:settings` sidecar as usual. The damaged value isn't kept: the photo is
saved as V1.

## Storage

The process is saved as `"process": <number>` in the settings JSON, except V1, which is left out.
Settings made before process versions existed are therefore saved exactly as before, and their
preview-cache keys (`DevelopSettings::hash64`) don't change. `develop.get` (MCP `get_develop`) always
states the process.
