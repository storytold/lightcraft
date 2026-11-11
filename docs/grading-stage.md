# Colour Grading, measured (process V9)

> **Changes:** [revision history](#revision-history)

Process V9 renders Colour Grading (the shadows, midtones, highlights and global wheels with Blending and Balance) the
way Lightroom does, from measurements of Camera Raw's output. V1 to V8 are unchanged (`docs/process-versions.md`).
The code is `crates/pipeline/src/grading_stage.rs`, its measured tables `crates/pipeline/src/grading_stage_tables.rs`,
and the GPU kernel's `measured_grade` in `crates/gpu/src/wgsl/finish.wgsl`.

## What was wrong

Up to V8 each wheel adds a fixed OkLab tint, 0.09 at saturation 100, in the direction of the pure sRGB colour of the
wheel's hue, weighted by smooth steps in lightness. Measured against Camera Raw on charts of grey steps and colour
patches:

- **The tints are the wrong size and direction.** Lightroom's tint at saturation 100 is 0.08 to 0.16 depending on
  the hue. Wheel hue 0 points to OkLab hue 11 to 15 in Lightroom, 29 in V8.
- **The wheels reach different tones.** At Blending 50 Lightroom's shadows wheel peaks at lightness 0.33 and gives
  at least half its tint from 0.12 to 0.52, so it barely touches the deepest shadows. V8's is strongest at black and
  gone by 0.6. Lightroom's global wheel peaks in the midtones (0.62, half or more from 0.23 to 0.89). V8's is the
  same at every lightness. Midtones peak at 0.59 and highlights at 0.84.
- **Colours that already lean the tint's way get less of it.** A saturated colour in the tint's direction gets 62%
  of the shadows tint and about 90% of the highlights and global tints. V8 adds the full tint to every colour.

## What V9 does

At the end of the colour tools, where V8 grades (`ColorOps::grade`), in OkLab:

1. Each wheel with a saturation above 0 adds its tint times its share at the pixel's lightness, times the damping.
   - The tint comes from the measured table: 12 hues of 30°, saturation 25, 50 and 100, linear in between, and
     from none at saturation 0.
   - The share comes from the profile measured at the pixel's Blending, plus the change that Balance makes (not for
     the global wheel).
   - The damping depends on the pixel's chroma (levels 0.04, 0.08 and 0.12, from none at 0) and on the angle between
     its hue and the tint's (bins of 30°).
2. Each wheel's Luminance adds a lightness change by lightness, measured at ±50 and ±100, linear in between.
3. The wheels act on the same input colour and their changes add.

V8's grading is off under V9 (`ColorOps::grading` is `None`). With Colour Grading neutral the stage does not run.

## How it was measured

`tools/lr_grading_charts.py charts OUT_DIR` writes linear DNG charts with the grey steps and colour patches of
`tools/lr_tone_charts.py`, with the settings in the DNG's XMP (197 charts). It writes:

- one neutral chart;
- each wheel at 12 hues × 3 saturations;
- each wheel's Luminance at ±50 and ±100;
- Blending 0, 25, 75 and 100, and Balance ±50 and ±100;
- two combinations used only as checks.

The charts are original test data. Every chart was rendered through Camera Raw to a 16-bit Adobe RGB (1998) TIFF.
Only that output is measured, and nothing of Adobe's is copied or shipped.

`tools/lr_grading_charts.py tables RENDER_DIR OUT_RS` reads each render's patches as OkLab and writes
`grading_stage_tables.rs`:

- The tints and profiles are fitted on the grey steps: the profile's shape is the mean tint size over the hues,
  and each tint is the least-squares scale of its grey-step change against that shape.
- The damping comes from the colour patches.
- The Luminance curves are the grey steps' lightness change.

It needs numpy and tifffile (`tools/requirements-lr-tone-charts.txt`). Running the tool again reproduces the
committed charts and the committed tables byte for byte.

Predicting Camera Raw's edited charts from its neutral chart with the stage's rules leaves 1.26 (OkLab × 100) on
average, against an average effect of 3.12. By kind: wheel hue and saturation 1.18 against 3.05, Luminance 0.79
against 2.84, Blending 1.89 against 3.82, Balance 1.60 against 3.27, the two combinations 1.43 against 2.67.

On 5 held-out Panasonic DC-S9 photos with 19 presets, rendered at 1,500 px and compared with Camera Raw's renders
after aligning the two engines' geometry, the mean OkLab a/b error falls from 1.32 to 1.19 (OkLab × 100) over all
edit groups, and on the Teal & Orange preset from 2.66 to 1.34.

## Limits

- Camera Raw's tint also changes grey steps' lightness a little. Leaving that out costs on the charts (1.26 against
  0.93 with it), but including it made photos with several strong wheels worse, so the tint leaves lightness alone.
- The damping's chroma levels are the charts' nominal chroma, and the measured hues are 30° apart.
- Camera Raw's process version 2012 controls (`crs:ProcessVersion` 11.0), one chart layout, sRGB-gamut patches.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-10 | minor | Process V9: Colour Grading measured against Lightroom |
