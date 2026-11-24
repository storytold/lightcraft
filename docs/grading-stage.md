# Colour Grading, measured (process V9)

> **Changes:** [revision history](#revision-history)

Process V9 renders Colour Grading (the shadows, midtones, highlights and global wheels with Blending and Balance) the
way Lightroom does, with LightCraft's own model fitted to measurements of Camera Raw's output. V1 to V8 are unchanged
(`docs/process-versions.md`). The code is `crates/pipeline/src/grading_stage.rs`, the model's fitted constants
`crates/pipeline/src/grading_stage_model.rs`, and the GPU kernel's `measured_grade` in
`crates/gpu/src/wgsl/finish.wgsl`.

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
   - The tint comes from the model (below): its direction and size around the hue circle, the wheel's size and its
     saturation curve, from none at saturation 0.
   - The share comes from the model's profile at the pixel's lightness, for the Blending and Balance set (the global
     wheel ignores Balance).
   - The damping depends on the pixel's chroma (levels 0.04, 0.08 and 0.12, from none at 0) and on the angle between
     its hue and the tint's (the model sampled at the centres of bins of 30°, linear between them).
2. Each wheel's Luminance adds a lightness change by lightness, from the model.
3. The wheels act on the same input colour and their changes add.

V8's grading is off under V9 (`ColorOps::grading` is `None`). With Colour Grading neutral the stage does not run.

## The model

262 constants (`grading_stage_model.rs`):

- **Tint.** All four wheels share one shape around the hue circle: a Fourier series of the wheel hue (4 harmonics) for
  the tint's direction as an offset from the wheel hue, and one for its log size. Each wheel adds its own size and a
  saturation curve `s (1 + g1 (s − 1) + g2 (s − 1)²)`, `s` = saturation / 100. 30 constants.
- **Share by lightness.** Per wheel, two bumps of the kind the curve stage uses ([curve-stage.md](curve-stage.md)),
  whose gain and exponents are quadratic in Blending and in Balance. 30 constants per wheel.
- **Luminance.** Per wheel and direction, two bumps whose gain and exponents change with the amount. 24 constants
  per wheel.
- **Damping.** Per wheel, `1 + (C / 0.12)^e (d0 + d1 cos a + d2 cos 2a)` for chroma `C` and angle `a` between the
  colour's hue and the tint's. 4 constants per wheel.

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

`tools/lr_grading_charts.py fit RENDER_DIR OUT_RS` reads each render's patches as OkLab, measures the following in
memory, fits the model to it and writes only the model's constants, `grading_stage_model.rs`. No measurement is
committed.

- The tints and profiles are fitted on the grey steps: the profile's shape is the mean tint size over the hues,
  and each tint is the least-squares scale of its grey-step change against that shape.
- The damping comes from the colour patches.
- The Luminance curves are the grey steps' lightness change.

It needs numpy, scipy and tifffile (`tools/requirements-lr-tone-charts.txt`). Running the tool again reproduces the
committed charts byte for byte, and the constants with the pinned packages.

How close the model is to those measurements: the tints within 0.57 RMS (OkLab × 100) of an average tint of 6.94;
the share by lightness within 0.7 to 2.6 RMS (× 100, the shadows wheel worst); Luminance within 0.01 to 0.27 RMS
(OkLab L × 100); the damping within 1.0 to 4.7 RMS (× 100). The damping's measurements do not change steadily with the
angle (the midtones wheel's at chroma 0.12 go 1.02, 0.99, 0.90, 0.92, 0.96, 1.02), so the smooth model leaves part of
that out.

On 5 held-out Panasonic DC-S9 photos (24 edits each plus Neutral, 1,500 px, aligned against Camera Raw's renders),
the whole V9 series with every stage's fitted model (curves, mixer, this stage, the default base curve, Highlights and
Calibration) is 2.424 in tone and 0.557 in colour (OkLab × 100, lower is better), against 2.418 and 0.556 with the
measured tables it replaces, and 6.98 and 1.52 before the series (V7).

## Limits

- Camera Raw's tint also changes grey steps' lightness a little. Leaving that out costs on the charts (1.26 against
  0.93 with it), but including it made photos with several strong wheels worse, so the tint leaves lightness alone.
- The damping's chroma levels are the charts' nominal chroma, and the measured hues are 30° apart.
- Camera Raw's process version 2012 controls (`crs:ProcessVersion` 11.0), one chart layout, sRGB-gamut patches.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-10 | minor | Process V9: Colour Grading measured against Lightroom |
| 2026-11-24 | minor | Colour Grading is a fitted model of 262 constants instead of measured tables |
