# Whites and the tone curves, measured (process V9)

> **Changes:** [revision history](#revision-history)

Process V9 renders Whites, the parametric curve and the point curves the way Lightroom does, from measurements of
Camera Raw's output. V1 to V8 are unchanged (`docs/process-versions.md`). The code is
`crates/pipeline/src/curve_stage.rs`, its measured tables `crates/pipeline/src/curve_stage_tables.rs`, and the GPU
kernel's `curve_stage` in `crates/gpu/src/wgsl/finish.wgsl`.

## What was wrong

Up to V8 Whites is part of the tone map, which acts on luminance, and the curves act on each channel of the
display-encoded output. Measured against Camera Raw, this goes wrong in three ways:

- **Whites reaches too little.** Lightroom's Whites moves the upper midtones too: at Whites −50 an sRGB-encoded 0.6
  drops by about 10/255. V8's Whites barely touches them.
- **The parametric regions are too narrow.** Lightroom's Highlights, Lights, Darks and Shadows regions are broad and
  overlap, and they add: the sum of the single regions reproduces three at once within 1/255.
- **Per-channel curves shift hues.** A contrast curve applied to R, G and B separately pushes orange towards red and
  saturates. Lightroom's master curve keeps the hue. It maps the largest and the smallest channel through the curve,
  and the middle channel keeps its relative position between them in linear light.

## What V9 does

After the tone map and V4's calibrated Contrast and Blacks, before the colour stage:

1. Convert linear Rec.2020 to linear ProPhoto RGB (D50, Bradford) and encode with the sRGB curve, as the V4
   operators do (`light_v3::prophoto_matrices`).
2. Build the base table: Whites, then the parametric curve, then the master point curve. Whites and the four regions
   are Camera Raw's measured output minus input (257 inputs each). Whites is measured at ±25, ±50 and ±100, the
   regions at ±25, ±50, ±75 and ±100, and amounts in between are interpolated linearly, towards 0 below 25. The
   tables hold for Lightroom's default splits (25, 50, 75). With other splits the parametric curve is the analytic
   model of the earlier processes. The table is kept non-decreasing.
3. Apply the base table to the largest and smallest channel. The middle channel keeps its relative position between
   them in linear light.
4. Apply the red, green and blue point curves to each channel.
5. Refine Saturation (below 100) moves saturation back towards the input's, as in V8, and then rescales to the
   curves' linear luminance, so the tone change stays.
6. Values above SDR white (an HDR render) continue with slope 1.

The tone map no longer applies Whites under V9 (`finish::base_tone`), and the V8 output-curve tables are not built
(`FinishParams::curves` is `None`). With Whites, the parametric curve and the point curves all neutral, the curve
stage does not run.

## How it was measured

`tools/lr_tone_charts.py charts OUT_DIR` writes linear DNG charts: 48 grey steps from 0.002 to 1 and OkLCh colour
patches, with the settings in the DNG's XMP. It writes one neutral chart, Whites at each measured amount, and each
parametric region at each measured amount. The charts are original test data. Every chart was rendered through
Camera Raw to a 16-bit Adobe RGB (1998) TIFF. Only that output is measured, and nothing of Adobe's is copied or
shipped. `tools/lr_tone_charts.py tables RENDER_DIR OUT_RS` reads the grey steps of each render as sRGB-encoded
linear ProPhoto values and writes `curve_stage_tables.rs`. It needs numpy and tifffile
(`tools/requirements-lr-tone-charts.txt`). Running the tool again reproduces the committed charts and the committed
tables byte for byte.

The grey-step tables leave out how colour is treated. To check that, the colour patches of the same renders were
predicted from the grey-step curves under the hue-preserving rule in step 3. The prediction is within 0.03 to 0.16
(OkLab × 100) of Camera Raw's patches, against 0.3 to 3.4 for V8's per-channel curves.

## Limits

- The tables are measured with Lightroom's default splits. Moved splits use the earlier analytic model.
- The Edit panel's curve graph still draws the analytic parametric curve and drags against it. Under V9 at the
  default splits the photo renders the measured regions, so the graph's shape can differ from the render's.
- The point curves' shape between the points is LightCraft's spline, not measured. What is measured for them is how
  they apply: hue preserving, then the red, green and blue curves per channel.
- Local (mask) Whites keeps the earlier processes' operator.
- The measurements use Camera Raw's process version 2012 controls (`crs:ProcessVersion` 11.0).

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-10 | minor | Process V9: measured Whites and parametric curve, hue-preserving master curve |
