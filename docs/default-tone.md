# Lightroom's default base curve, measured (process V8)

> **Changes:** [revision history](#revision-history)

Process V8 gives a raw rendered through a camera profile without a tone curve of its own the base tone Lightroom
gives it, from measurements of Camera Raw's output. V1 to V7 are unchanged (`docs/process-versions.md`). The code is
`crates/pipeline/src/default_tone.rs`, its measured table `crates/pipeline/src/default_tone_table.rs`, and the GPU
kernel's `rgb_tone` in `crates/gpu/src/wgsl/finish.wgsl`.

## What was wrong

A DNG camera profile (`.dcp`) may carry a tone curve (`ProfileToneCurve`). Adobe Standard profiles do not: they leave
the base tone to the raw converter. Lightroom then applies its default base curve. Up to V7 LightCraft keeps the
photo's own tone instead: the file's DNG tone curve, or else the curve fitted to the camera JPEG
(`files::load`, `kept_tone`). Measured against Camera Raw:

- **The base curve is a different one.** Lightroom's default curve lifts the midtones more and rolls off into white
  later than a typical camera JPEG's. On 5 held-out Panasonic Lumix DC-S9 raws with the Adobe Standard profile, the
  unedited V7 render is off by 5.03 in lightness (RMS, OkLab × 100), and brighter by 2.41 on average.
- **It is applied to luminance, not to the channels.** V7 scales all three channels by the curve's change in
  luminance. Lightroom applies the curve to the largest and the smallest channel in linear ProPhoto RGB, and the
  middle channel keeps its relative position between them. On the 327 colour patches of the neutral chart this rule
  predicts Camera Raw's output within 0.26 (OkLab × 100). Scaling by luminance misses by 5.31, and the curve on each
  channel by 0.86.

## What V8 does

For a raw whose profile has no tone curve (`SourceInfo::default_tone`, set by `files::load` when a camera profile
without `ProfileToneCurve` is chosen):

1. The base tone map's camera curve is the measured default curve (`default_tone::curve`): 32 knots evenly spaced in
   stops over the chart's grey steps, the scene shifted by the camera's exposure offset. Past the last knot it rolls
   off into white in stops, as V5 extends every camera curve (#1632).
2. The tone map applies hue preserving on linear ProPhoto channels (`default_tone::rgb_tone`), not by luminance.
3. Contrast and Blacks keep V4's calibrated operators, and Whites, the curves, the colour mixer and Colour Grading
   follow as before ([curve-stage.md](curve-stage.md), [mixer-stage.md](mixer-stage.md),
   [grading-stage.md](grading-stage.md)).

A raw without a camera profile, a profile with a tone curve of its own, and a rendered file are unchanged.

**The exposure offset.** The chart is a DNG with a baseline exposure of 0. For a raw of another format (RW2, ARW, NEF
and so on) Camera Raw uses an exposure offset of its own per camera, which the file does not carry and LightCraft
does not know. `default_tone::camera_ev` holds the offsets measured on photos:

| Camera | Offset (stops) | Measured on |
|---|---:|---|
| Panasonic Lumix DC-S9 (RW2) | −0.10 | 5 photos, the shift that leaves Lightroom's unedited renders no brighter or darker on average |

Other cameras get 0. Without the offset the S9 renders 1.1 lighter in lightness than Lightroom (OkLab × 100), so a
camera not measured can be off by about that much.

## How it was measured

`tools/lr_tone_charts.py base RENDER_DIR OUT_RS` reads Camera Raw's render of the neutral chart of
`tools/lr_tone_charts.py charts` (48 grey steps, geometric from 0.002 to 1, linear DNG, settings neutral) and writes
the grey steps (chart value in, linear ProPhoto value out) to `default_tone_table.rs`. The chart is original test
data. It was rendered through Camera Raw to a 16-bit Adobe RGB (1998) TIFF, and only that output is measured. Nothing
of Adobe's is copied or shipped, and no profile's contents are read: the only fact taken from a profile is whether it
has a tone curve. Running the tool again writes the committed table byte for byte.

The offset was fitted on 5 training photos of the DC-S9 and checked on 5 others held out from fitting.

## Results

Held-out Panasonic Lumix DC-S9 photos (RW2, Adobe Standard profile, 1,500 px, aligned against Camera Raw's renders),
lightness RMS and mean a/b error (OkLab × 100), lower is better:

| Edits | Tone: before → this | Colour: before → this |
|---|---|---|
| Unedited | 5.03 → 0.69 | 1.00 → 0.27 |
| Highlights, Shadows | 6.20 → 3.26 | 1.02 → 0.38 |
| Highlights and Shadows together | 8.66 → 6.59 | 1.01 → 0.62 |
| 19 presets (7 groups) | 6.38 → 2.61 | 1.26 → 0.73 |

"Before" is process V8 without this change.

Unedited, the five photos are off by 0.44 to 1.10 in lightness and on average 0.09 darker than Lightroom (2.41
lighter before). No preset gets worse in tone or in colour.

## Limits

- One camera's offset is measured. Others render with the chart's curve as it is.
- The curve is measured for Camera Raw's process version 2012 (`crs:ProcessVersion` 11.0), one chart, grey steps up to
  the DNG's white. Above white it rolls off as V5's camera curves do, not as measured.
- Smart previews are built without the imported camera profile, so a photo whose original is offline renders
  without the profile, and so without this curve, until the original is back (as before).

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-11 | minor | Process V8: Lightroom's default base curve for a raw whose camera profile has none |
