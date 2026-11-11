# Highlights relative to the frame's tonal range (process V9)

> **Changes:** [revision history](#revision-history)

Process V9 makes Highlights act relative to the photo's own tonal range, as Lightroom's does. V1 to V8 are unchanged
(`docs/process-versions.md`), and Shadows keeps V8's rule. The code is `crates/pipeline/src/hs_range.rs`, the range
`local::frame_range`, and the GPU kernel's `hs_range_highlights` in `crates/gpu/src/wgsl/finish.wgsl`.

## What was wrong

Up to V8 Highlights moves the base layer's log luminance by a fixed weight of its absolute level: `1.7 × amount` at
the top, fading out below −1 stop from middle grey. Lightroom's Highlights depends on the photo. Measured on Camera
Raw renders of photos at Highlights −50 and −100, Lightroom's Highlights works relative to the frame's own tonal
range, anchored on its darkest tones:

- **On a photo with a wide tonal range it works harder.** Its pull at the top grows with the span from the frame's
  darkest to its brightest tones.
- **It reaches further down.** On such a photo it also darkens the upper midtones, which V8 leaves alone.

Region size barely matters: the response follows the frame's range, not how much of the frame is bright.

## What V9 does

1. **The frame's range.** The base layer's 2nd and 99th percentile in log₂ luminance (`FrameRange`), plus exposure.
   It comes from a render of the whole output frame at a fixed 384 px on the long edge, whatever the output's size,
   built like the proxy Dehaze uses for its airlight (`local::frame_range`, `Plan::hs_range`). So a zoomed window, a
   thumbnail, a preview and an export of one photo share it.
2. **Highlights** adds `amount × a × smooth(low, 2.8, base)` to the base layer's log luminance (`FrameRange::highlights`):
   - `span` is the range's width in stops, at least 0.1.
   - The strength is `a = 0.7 × (span / 5)^2.4`, clamped to 0.2…8.
   - It reaches down to `low = −1 − 0.8 × max(span − 5, 0)` stops.
3. **Shadows**, local Whites, Blacks, Contrast, Clarity and Texture are V8's.

It applies to global Highlights and to Highlights in masks. With Highlights at 0 (globally and in every mask) the range
is not measured and Highlights adds nothing.

## How it was fitted

The form comes from an earlier analysis of Camera Raw renders of photos: the response scales with the frame's range
and is anchored on its darkest tones. The five constants were fitted by coordinate descent against Camera Raw renders
of 5 training photos (Panasonic Lumix DC-S9, RW2, Adobe Standard), on Highlights −50 and −100, the Highlights and
Shadows combination and one preset of each group. They were checked on 5 other photos held out from fitting. These
renders are the contributor's own photos and are not in the repository. Unlike the curve, mixer and grading stages,
no synthetic chart is involved: a flat chart has no tonal range for the rule to act on.

A version that also made Shadows relative to the range was measured too. It moved Shadows +100 further from Lightroom
(4.35 to 4.53) and the Highlights and Shadows combination less than Highlights alone did, so Shadows keeps V8's rule.

## Results

Held-out Panasonic Lumix DC-S9 photos (RW2, Adobe Standard profile, 1,500 px, aligned against Camera Raw's renders),
lightness RMS and mean a/b error (OkLab × 100), lower is better. "Before" is process V9 without this change.

| Edits | Tone: before → this | Colour: before → this |
|---|---|---|
| Highlights −50, −100 | 3.30 → 2.17 | 0.37 → 0.27 |
| Highlights −100 with Shadows +100 | 6.59 → 4.03 | 0.62 → 0.42 |
| Shadows +50, +100 | 3.23 → 3.23 | 0.39 → 0.39 |
| 19 presets (7 groups) | 2.61 → 2.14 | 0.73 → 0.69 |
| Unedited | 0.69 → 0.69 | 0.27 → 0.27 |

Three presets get worse: Muted (tone 4.92 → 5.35, colour 0.72 → 0.85), Sunset (tone 2.24 → 2.43) and Cool (colour
1.53 → 1.58, tone better).

## Limits

- Fitted on one camera's photos (10 in all) with Lightroom's renders as reference; other cameras and scenes are
  unchecked.
- The percentiles come from a 384 px proxy, so a frame whose range is set by a few tiny highlights can read a
  slightly different range than the full image would.
- The proxy is white-balanced but not passed through a DNG's own local tone map (`ProfileGainTableMap`, applied
  before the base layer when the photo's Profile option asks for it). For such a photo the range describes the
  tones before that map.
- The formula is a fit, not a measurement of Lightroom's operator: its shape matches on these photos, its constants
  may not hold for very wide or very narrow ranges (the strength is clamped).

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-11 | minor | Process V9: Highlights relative to the frame's tonal range |
