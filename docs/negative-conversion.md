# Film negative conversion: development plan

> **Status:** v1 built (see [What v1 has](#what-v1-has)) · **Owner decision needed:** see [Open questions](#open-questions) ·
> **Changes:** [revision history](#revision-history)

Lots of people scan film by photographing it with a digital camera, or use a film scanner, and then invert the
negative in Lightroom with the Negative Lab Pro (NLP) plug-in. LightCraft has no plug-in support (gap 19 in
[gaps.md](gaps.md)), and running NLP itself isn't realistic: it would need a Lua 5.1 VM in pure Rust plus a copy of
Lightroom's SDK API. So this plan builds negative conversion **into the develop pipeline**: good automatic defaults,
and conversion presets you can configure, save and share, in the style of NLP's. It does better than a plug-in in
one important way: after conversion, every normal slider works the right way round.

## What v1 has

Built against this plan (milestones N0–N3, N5 and N6 in full or in part; N4 and N7 partly):

- **Settings and stage (N1):** the `negative` group (film type, tone, colour model, stored analysis, 13 `negative.*`
  sliders), converted first in the scene-linear stage. Working space is the source's linear Rec.2020 (open
  question 1 stays open: camera-native RGB isn't built). GPU renders run the film stage on the CPU (N4: no WGSL
  kernel yet); the result is the same on both.
- **Analysis (N2):** film base found on the film's unexposed edge anywhere around the picture, also on uncropped
  camera scans in a holder (tiles that are uniform, clearer than the picture in every channel, one colour along the
  edge and shaped like an edge; light clipped at the scan's own white level is rejected); else estimated, and the
  panel says so. Range measured inside the crop (or the frame the found edge outlines); per-channel balance at nine
  tone levels; auto exposure anchors the frame's median at mid grey.
- **Roll sync (N2):** `negative.syncRoll` (Photo ▸ Sync Roll…, or Sync Roll in the panel with two frames or more
  selected) reads every frame on a worker with a row in the activity stack, pools them into one analysis (a sampled
  base if any frame has one, else the median of the bases found on film edges, else the clearest estimate; range and
  balance from every frame's pixels together) and writes it to every frame, locked, as one undo step. Re-analyze and
  Convert leave a locked frame's numbers alone until `negative.unlock`; picking a base on a frame unlocks it. Frames
  whose film bases differ by more than 15 % look scanned differently, and the sync says so; a frame edited while the
  roll is read keeps its edit and is left out. An analysis records its film type and is measured again for another.
- **Copy, paste and presets:** the Negative Conversion group is not in the default copy set (a preset or copy made from
  an ordinary photo would otherwise switch a scan back to its negative); ticked, it carries the conversion whole,
  "off" and "unlocked" included.
- **Commands (N3):** `negative.convert`, `negative.conflicts`, `negative.analyze`, `negative.sampleBase`,
  `negative.set`, `negative.syncRoll`, `negative.unlock`, `negative.reset`; CLI and MCP get them as usual.
- **UI (N5):** the Negative section (at the top for converted photos, last otherwise), Film Base Selector
  eyedropper (`tool.filmBase`), Re-analyze hint when the crop changed, and a dialog before Convert resets edits made
  on the negative (`dialog.convertNegative`, Photo ▸ Convert Negative…). No analysis-area overlay; no batch convert.
- **Presets (N6):** nine Film Conversion presets and eleven Film Stocks looks (portrait, landscape, everyday, pro,
  cine tungsten, B&W 100 / 400 / 3200, slide), our own slider settings with a matching grain, named by kind and
  character rather than by trademark (no maker's data). All carry options only, never a frame's base or range, and
  set the treatment to match the film. Apply on import is not built.
- **After conversion** every slider, Auto, Auto White Balance and the white balance selector work on the positive;
  white balance is relative (as converted = neutral); converted photos use the scene tone map (raw and rendered
  scans of one negative convert alike) and skip the camera's own local tone map.
- **Defaults** were fitted on one maintainer-supplied 6×6 colour negative (a Fuji RAF camera scan and two JPEG scans)
  against a positive the maintainer made from the same frame with Negative Lab Pro, used only as a visual target:
  mean L* within 0.5 of it on all three scans, greys neutral. No NLP data, curves or code are used.
- **Tests:** synthetic negatives (orange mask, rebate, holder, bare light, raw white-balance gains) in
  `crates/pipeline/src/negative.rs` and `tests_negative.rs`; command tests in `crates/engine/src/tests/negative.rs`;
  UI tests in `crates/ui-egui/src/tests/negative.rs`; and an opt-in test of real scans named by a manifest
  (`LC_NEGATIVE_SCANS`, `crates/engine/tests/it/negative_scans.rs`), since real scans can't be committed.

## Goals

- **One click converts a negative** with good defaults. Film type is detected from the frame where possible and can
  be chosen otherwise: colour negative (C-41), black-and-white negative, or slide (E-6, no inversion, only
  base/range normalisation).
- **The positive is scene-referred and linear.** Exposure, Contrast, Highlights, Temp/Tint, curves, masks, presets
  and AI masks then act on the positive the same way they do on a digital photo, and none of them work backwards.
- **Conversion presets:** a named bundle of conversion parameters (tone profile, colour model, base and range
  handling, saturation, shadow/highlight balance) and, optionally, ordinary develop settings. Built-in presets give
  sensible starting points; users save, import, export and share their own (`.lcpreset`).
- **Roll workflow:** analyse one frame or a whole roll together, lock the film base and range, and sync them across
  the roll. Batch conversion runs in the background with progress in the activity stack.
- **Deterministic and resolution independent:** automatic analysis resolves to numbers stored in the settings, so
  the preview, the export, the CLI, MCP and the web build all render the same picture.

## Non-goals (for v1)

- Running NLP or any Lightroom plug-in (gap 19 stays open).
- Reading NLP's own conversion metadata. Positives exported by NLP (TIFF) import as ordinary photos; Lightroom
  tone-curve settings in `crs:` sidecars are already read.
- Dust and scratch removal from infrared channels (ICE): a separate feature. Existing spot removal and dust
  visualisation still apply.
- Profiles for individual film stocks. See [Clean-room](#clean-room-and-assets).

## How it works

### Where it sits in the pipeline

Stage order (see `crates/pipeline/src/lib.rs`) becomes:

1. geometry (unchanged)
2. **film conversion (new)** → white balance, exposure, dehaze, local tone, masks …
3. tone map, 4. colour, 5. display (unchanged)

Conversion has to run on **linear** data, **before white balance**: the orange mask is a per-channel multiplicative
transmittance, and inverting after the tone curve (today's workaround with an inverted point curve) gives muddy
colour and backwards sliders.

**v1 (built):** the film stage works on the source as the renderer receives it: linear Rec.2020, developed for its
as-shot white, raw and rendered scans alike. It doesn't undo the as-shot white balance or go through the camera's
matrix: dividing by the film base (measured in the same space) removes the orange mask and the light's colour
together, and each channel's range is then matched.

**Planned (not built):** two working spaces behind one function so tests can compare them:

- **Raw camera scans** with a colour model (`SourceInfo::camera_color`): convert to **camera-native RGB** (inverse
  of the camera→Rec.2020 matrix) for the density maths, then back. Dye transmittance doesn't behave like scene
  colour, and the camera's own channels separate the dyes better than a matrix fitted for daylight scenes.
- **Rendered scans** (TIFF and JPEG from film scanners) and raws without a matrix: linear Rec.2020 with the as-shot
  white undone.

Which space should be the default is an [open question](#open-questions).

### The conversion maths (per pixel, per channel c)

1. **Film base:** `v_c / base_c`, where `base_c` is the transmittance of unexposed film (the rebate between
   frames). This removes the orange mask: the base becomes 1.0 in every channel.
2. **Density:** `D_c = −log10(max(v_c / base_c, ε))`. At the base, D is 0; denser film (bright in the scene) gives
   larger D.
3. **Range:** `n_c = (D_c − lo_c) / (hi_c − lo_c)`, with `lo` and `hi` from the analysis (robust percentiles,
   below). Matching each channel's range is what neutralises the colour cast; the user's black/white clip sliders
   move `lo` and `hi`.
4. **Balance (crossover):** separate per-channel offsets near `n = 0` (shadows) and `n = 1` (highlights),
   blended smoothly. They correct the colour drift between shadows and highlights that film and scanning light
   cause (NLP calls this shadow/highlight toning). They are set as temperature/tint pairs, not raw RGB.
5. **Back to scene-linear:** `L_c = mid · 2^((n_c − n_mid) · stops)`. The tone profile sets `stops` (how many
   stops of scene range the film's density range covers) and an optional gentle shoulder and toe. "Linear" gives a
   flat positive for grading; "Standard" and "Soft"/"Hard" act like paper grades.
6. **Colour model:** an optional 3×3 matrix plus saturation, applied in linear space. It undoes dye cross-talk
   (cyan dye absorbs some green, and so on) and gives the neutral / warm / cool "lab scanner" looks. Each model is a
   small matrix we fit ourselves (see [Clean-room](#clean-room-and-assets)).

After step 6 the image is an ordinary positive. From there, white balance works on a **relative scale around
neutral** (Bradford, like rendered files) instead of the lightbox's Kelvin, which no longer means anything.
`SourceInfo` reports `relative_wb` for converted photos.

Black-and-white negatives use one channel (the luminance of `v / base`) and skip steps 4 and 6. Slide film skips
steps 1, 2 and 5 (no inversion), keeping range normalisation and balance.

For speed, steps 2–5 per channel become a 1D LUT in the log domain (4096 entries, built once per settings change),
so the per-pixel cost is three LUT lookups plus two 3×3 matrices.

### Automatic analysis (sensible defaults)

Analysis runs **once**, on a fixed grid of samples (at most 512 along the measured region's long edge) of the
source's Preview level, and `negative.convert`, `negative.analyze`, `negative.set`, `negative.sampleBase` and
`negative.syncRoll` write its results into the settings, so preview and export convert with the same numbers. A
converted photo with no stored analysis (or one measured for another film type), such as settings merged in by
hand or by an agent, is measured inside every render instead: deterministic for a given source, but a cost on each
render and a little different between the preview and full-size sources. The panel then asks for Re-analyze.

- **Film border detection:** find the unexposed rebate or the holder/sprocket area along the frame edges: bright,
  low-variance and roughly uniform in hue. That gives the **film base** (median of the rebate's pixels) and an
  **analysis rectangle** that excludes the border, holder and light leaks, plus a user-set buffer (default 5 % of
  the edge). If a crop is set, the crop is the analysis region.
- **No rebate visible:** estimate the base from the brightest robust percentile of each channel (99.8 %) inside
  the analysis region, and flag the estimate as such (the UI says "base estimated; sample the film edge for a better
  result").
- **Range:** per-channel density percentiles inside the analysis region (default `lo` 0.1 %, `hi` 99.9 %), from
  histograms that ignore clipped pixels and pixels flagged as dust by the existing dust visualiser.
- **Film type guess:** a strong orange base (R ≫ G > B transmittance) means colour negative; a neutral, dense base
  means B&W negative; a near-clear base with the image's highlights near the base means slide. The guess is only a
  default; the user picks the type in the panel.
- **Roll analysis:** pool the histograms of every selected frame. The base comes from the frame with the clearest
  rebate, the range from the pooled percentiles. The result is written to all frames with `locked: true`, so
  re-analysing one frame doesn't drift it away from its roll.

## Settings and data model

A new group in `crates/develop/src/settings.rs`:

```rust
#[serde(skip_serializing_if = "Negative::is_default")]  // existing photos keep their JSON and hash64
pub negative: Negative,

pub struct Negative {
    pub film: FilmType,              // Off (default) | ColorNegative | BwNegative | Slide
    pub base: [f64; 3],              // transmittance of unexposed film, linear, > 0
    pub base_source: BaseSource,     // Auto | Estimated | Sampled { rect } | Preset
    pub lo: [f64; 3], pub hi: [f64; 3], // density range per channel
    pub analysis: Option<Rect>,      // normalized image coords
    pub locked: bool,                // roll-synced: re-analysis leaves base and range alone
    pub tone: ToneProfile,           // Linear | Standard | Soft | Hard
    pub color_model: ColorModel,     // Neutral | Warm | Cool | None
    pub stops: f64, pub black_clip: f64, pub white_clip: f64,
    pub saturation: f64,
    pub shadow_temp: f64, pub shadow_tint: f64,
    pub highlight_temp: f64, pub highlight_tint: f64,
}
```

- Every field is clamped when read (`ControlSpec::clamp` for sliders; base and range must be finite, positive and
  ordered, else the photo renders unconverted and reports the problem). Unknown enum values fall back to their
  default (`#[serde(other)]`).
- It is stored in XMP as part of `lc:settings`; nothing new is needed in `crates/meta`. No `crs:` mapping: Lightroom
  has no equivalent.
- A new `SettingsGroup::Negative` in `crates/develop/src/presets.rs`, so copy/paste, sync and presets can carry it.
  It is not in the default "copy all" set for photos that aren't film scans.
- A section toggle (eye) `negative` in `DevelopSettings::effective`: off shows the scan as a negative again.
- `before_settings` keeps the conversion on, so the Before/After view compares edits, not negative against positive.
  Holding the section's eye shows the negative.

## Presets (configurable, NLP-style)

Conversion presets are **ordinary LightCraft presets** whose JSON includes the `negative` group. They use the
existing preset machinery (`crates/engine/src/presets.rs`, import, export, groups, amount) rather than a second
system. What they add:

- A preset group **Film Conversion** with built-ins (our own work, described below):
  - *Color Negative · Standard* (Neutral model, Standard tone, auto base/range)
  - *Color Negative · Soft* (lower `stops`, gentle shoulder: portraits, overexposed film)
  - *Color Negative · Vivid* (Warm model, +saturation, Hard tone)
  - *Color Negative · Flat* (Linear tone, no colour model: for grading by hand)
  - *B&W Negative · Standard*, *B&W Negative · High Contrast*
  - *Slide · Standard*
- **Partial presets:** a preset may hold only some `negative` fields (for example a tone profile and colour model
  but no base), so applying it never overwrites a frame's measured base and range unless the preset says so.
  Presets saved from a photo leave base, range and analysis rectangle out by default, with a "Include film base and
  range" checkbox for presets made for one stock or one scanning setup.
- **Combined presets:** a conversion preset can also carry other groups (Basic, Curve, Colour Grading, Grain) for a
  full look in one click.
- **Apply on import:** the import dialog's existing develop preset choice can pick a conversion preset, so a roll
  imports already converted (analysis queued as a background task).
- Shared as `.lcpreset` files and zips, like other presets.

## Commands, controls, CLI and MCP

Everything is a command (`crates/engine`). New module `crates/engine/src/cmd/negative.rs`:

| Command | Params | Does |
|---|---|---|
| `negative.convert` | `ids?`, `film?`, `preset?` | analyse and turn conversion on; many ids → background task |
| `negative.analyze` | `ids?`, `roll?: bool` | re-run analysis (base unless locked or sampled, range, rectangle) |
| `negative.sampleBase` | `rect` (normalized) | set the film base from a region (eyedropper) |
| `negative.syncRoll` | `ids`, `reference?` | pool or copy base and range across frames, set `locked` |
| `negative.unlock` / `negative.reset` | `ids?` | allow re-analysis / turn conversion off |

Controls (`crates/develop/src/controls.rs`, section `Negative`; each slider is a `ControlSpec`):
`negative.contrast`, `negative.brightness`, `negative.whitePoint`, `negative.blackPoint`, `negative.temp`,
`negative.tint`, `negative.saturation`, `negative.balance`, `negative.shadowTemp`, `negative.shadowTint`,
`negative.highlightTemp`, `negative.highlightTint`, `negative.border`. Film type, tone profile and colour model are
parameters of `negative.set` (and `negative.convert`), not controls.

All of these reach the CLI and MCP (one tool per command) with no extra code: `lightcraft-cli run --import scan.dng
negative.convert film=colorNegative develop.set control=negative.contrast value=10 app.export path=out.jpg`, or
`lightcraft-cli render scan.dng -o out.jpg --settings conversion.json` with a partial such as
`{"negative": {"film": "colorNegative"}}` (`render --set` takes numbers only).

## UI

- A **Negative** panel at the top of Develop's right column, above Basic. It is collapsed (only "Convert
  Negative…" showing) for photos that aren't converted, and opens fully once one is.
- Contents: film type, a preset dropdown (Film Conversion group), **Convert / Re-analyse**, a base eyedropper (drag a
  rectangle on the film edge; it shows the sampled colour), an "analysis area" overlay toggle, the sliders above,
  and a lock icon when roll-synced.
- Filmstrip/grid: selecting many and choosing Convert runs a batch with one `TaskGuard`
  (`session.activity.start`, `Cancel::Flag`) and per-photo progress, as in
  [background-tasks.md](background-tasks.md). No separate progress window.
- Photo ▸ Develop Settings menu: Convert Negative, Re-analyse, Sync Roll, Reset Conversion (menu paths and
  shortcuts registered with the commands).
- Every label goes through the localisation catalogue.

## Never crash

- A base of zero, NaN or ∞, or `hi ≤ lo`: no division and no log of zero (`ε` clamp). An invalid conversion is
  skipped (the photo renders unconverted with a notice), never a panic.
- Analysis on an image smaller than the proxy, all one colour, all clipped, or with no pixels left in the analysis
  rectangle: returns an "unable to analyse" error the UI shows, and nothing is written.
- Sample rectangles from users and agents: clamped to the image, refused when empty or not finite.
- Analysis allocations are capped by the proxy size, never the input size.
- Each fix of this kind lands with a synthetic regression test.

## GPU

The film stage goes into the GPU per-pixel path (`crates/gpu/src/wgsl/`, beside white balance) using the same LUT
and matrices uploaded as uniforms and a texture. CPU/GPU parity test on synthetic negatives (existing tolerance),
plus a GPU-off fallback test. Budget: ≤ 1 ms added at the draft size (~2.5 MP) on the CPU path, effectively free on
the GPU.

## Testing

- **Synthetic negatives** (`crates/scenes`): take a procedural positive and a simple film model (characteristic
  curve per layer, orange mask, dye cross-talk, scanning-light colour, a rebate border and sprocket holes) to make a
  negative. Converting it must give back the positive within a stated ΔE₀₀ (for example mean < 3, 95th percentile
  < 6 with the Neutral model), for each film type and with and without a visible rebate.
- Unit tests: density maths, LUT vs direct formula, border detection on synthetic frames (rebate on 0–4 sides,
  holder, light leak), base sampling, percentile range, crossover balance, roll pooling, preset partial merges
  (base kept unless included), settings JSON and hash unchanged for unconverted photos.
- Property tests: no NaN or ∞ out for any finite input and any clamped settings.
- Resolution independence: preview vs export of the same settings match.
- End-to-end: CLI render with `--set negative.*`, and an MCP `negative.convert` + render in
  `crates/mcp/tests/e2e.rs`.
- Real scans: CC0 film scans (or scans contributed under an open licence) fetched into the gitignored `corpus/` with
  their source recorded; never committed. Before/after screenshots checked in the running app.

## Clean-room and assets

- NLP is a reference for **workflow and behaviour only**: no copying of its presets, curves, LUTs, colour models or
  code, and none of its look files.
- Film-stock names are trademarks, and stock data from manufacturers' datasheets isn't ours to ship as profiles. The
  built-in colour models are generic (Neutral / Warm / Cool), fitted by us on synthetic negatives and on our own
  or CC0 scans of a colour chart. Users can make stock-specific presets themselves.
- No new image assets are needed; if built-in previews of the presets are added later they are rendered
  procedurally.

## Milestones

| # | Work | Estimate |
|---|---|---|
| N0 | Synthetic negative generator in `scenes`, ΔE harness | 3–5 h |
| N1 | `Negative` settings, film stage on the CPU (colour negative), camera-native path, tests | 6–9 h |
| N2 | Automatic analysis: border and base, range, film type guess, roll pooling | 5–7 h |
| N3 | Commands, controls, section toggle, CLI and MCP; e2e tests | 3–5 h |
| N4 | GPU kernel, parity tests, benchmarks | 3–5 h |
| N5 | Negative panel, eyedropper, analysis overlay, menus, batch in activity stack | 6–9 h |
| N6 | Presets: Film Conversion group, built-ins, partial-merge rules, apply on import | 3–5 h |
| N7 | B&W and slide film, crossover balance tuning, docs, parity rows, What's New | 3–5 h |
| | **Total** | **≈ 32–50 h** |

Each milestone ships green (`cargo xtask ci --quick`) and on its own commit with its task id (for example
`N1: film stage for colour negatives`). N0–N3 are useful by themselves: conversion through the CLI and MCP before any
UI exists.

When it lands: add a row to [parity-checklist.md](parity-checklist.md) (Lightroom has no built-in equivalent, so it
goes under beyond-target features), note in gap 19 of [gaps.md](gaps.md) that the most-asked plug-in workflow is
covered natively, and add an entry to [whats-new.md](whats-new.md).

## Open questions

1. **Default working space for raw scans:** camera-native RGB (better dye separation; depends on the camera
   matrix) or as-shot-white-undone Rec.2020 (simpler, same for every source)? Plan: build both in N1, decide from
   the ΔE harness.
2. **Panel placement:** a separate Negative panel (planned) or a third Treatment next to Color / B&W?
3. **Colour models:** are three generic models enough for v1, or should we fit more from contributed scans of
   colour charts?
4. **Gap tracking:** should this get its own row in [gaps.md](gaps.md), or stay part of gap 19?

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-02 | minor | v1 built; plan updated to match |
