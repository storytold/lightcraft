# Whites and the tone curves, measured (process V9)

> **Changes:** [revision history](#revision-history)

Process V9 renders Whites, the parametric curve and the point curves the way Lightroom does, with LightCraft's own
model fitted to measurements of Camera Raw's output. V1 to V8 are unchanged (`docs/process-versions.md`). The code is
`crates/pipeline/src/curve_stage.rs`, the model's fitted constants `crates/pipeline/src/curve_stage_model.rs`, and the
GPU kernel's `curve_stage` in `crates/gpu/src/wgsl/finish.wgsl`.

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
2. Build the base table: Whites, then the parametric curve, then the master point curve. Whites and each of the four
   regions add a fitted response to the diagonal (below). The regions are fitted for Lightroom's default splits
   (25, 50, 75). With other splits the parametric curve is the analytic model of the earlier processes. The table is
   kept non-decreasing.
3. Apply the base table to the largest and smallest channel. The middle channel keeps its relative position between
   them in linear light.
4. Apply the red, green and blue point curves to each channel.
5. Refine Saturation (below 100) moves saturation back towards the input's, as in V8, and then rescales to the
   curves' linear luminance, so the tone change stays.
6. Values above SDR white (an HDR render) continue with slope 1.

The tone map no longer applies Whites under V9 (`finish::base_tone`), and the V8 output-curve tables are not built
(`FinishParams::curves` is `None`). With Whites, the parametric curve and the point curves all neutral, the curve
stage does not run.

## The model

Each slider direction (negative and positive) of Whites and of each region has its own response, output minus input
on sRGB-encoded values: the sum of two smooth bumps. With `t` = |amount| / 100,

    response(x) = Σ (a1 t + a2 t² + a3 t³) · bump(x, e^(p0 + p1 t), e^(q0 + q1 t))
    bump(x, p, q) = (x / m)^p · ((1 − x) / (1 − m))^q,   m = p / (p + q)

A bump is 0 at black and white and 1 at its peak `m`. The gain grows faster than linearly with the amount, and the
bump's position and width move with the amount, as Lightroom's regions widen and move when pushed harder. That is 7
constants per bump, 14 per slider direction and 140 in all. The fit keeps every response growing with the amount at
every input and never acting against the slider, so dragging the graph moves the slider steadily.

## The Edit panel's graph

The parametric graph draws the curve the photo renders, from `curve_stage::ParametricGraph`: the fitted regions
under V9 at the default splits, else the analytic model. Dragging the line moves the region under the pointer by
the amount that puts the drawn curve where the pointer is. The fitted regions are not linear in the amount, so the
amount is searched for, on the side of the starting value the drag moves to, on the curve as drawn (kept monotone, so
where opposite regions fold it back the line is held flat by the peak to its left). A level out of the region's reach
saturates at ±100. A region that cannot move the line by half an 8-bit level in that direction keeps its value.
Whites is not part of the graph, as in Lightroom.

## How it was measured

`tools/lr_tone_charts.py charts OUT_DIR` writes linear DNG charts: 48 grey steps from 0.002 to 1 and OkLCh colour
patches, with the settings in the DNG's XMP. It writes one neutral chart, Whites at each measured amount, and each
parametric region at each measured amount. The charts are original test data. Every chart was rendered through
Camera Raw to a 16-bit Adobe RGB (1998) TIFF. Only that output is measured, and nothing of Adobe's is copied or
shipped, and no measurement is committed. `tools/lr_tone_charts.py fit RENDER_DIR OUT_RS` reads the grey steps of
each render as sRGB-encoded linear ProPhoto values, fits the model to them and writes only its constants,
`curve_stage_model.rs`. It needs numpy, scipy and tifffile (`tools/requirements-lr-tone-charts.txt`). Running the
tool again reproduces the committed charts byte for byte, and the constants with the pinned packages.

How close the model is to the measured response, as the stage renders it (both clamped to 0..1), over 257 inputs and
every measured amount, in 8-bit levels of the sRGB-encoded output:

| Setting | negative: RMS | max | positive: RMS | max |
|---|---|---|---|---|
| Whites | 0.15 | 0.88 | 0.27 | 1.62 |
| Shadows | 0.39 | 2.18 | 0.34 | 1.37 |
| Darks | 0.33 | 1.73 | 0.19 | 1.32 |
| Lights | 0.05 | 0.17 | 0.37 | 1.04 |
| Highlights | 0.15 | 0.64 | 0.52 | 2.29 |

Every largest error but one (Darks +50, at black) is at ±75 or ±100.

The grey steps leave out how colour is treated. To check that, the colour patches of the same renders were
predicted from the grey-step curves under the hue-preserving rule in step 3. The prediction is within 0.03 to 0.16
(OkLab × 100) of Camera Raw's patches, against 0.3 to 3.4 for V8's per-channel curves.

## Limits

- The regions are fitted with Lightroom's default splits. Moved splits use the earlier analytic model.
- The point curves' shape between the points is LightCraft's spline, not measured. What is measured for them is how
  they apply: hue preserving, then the red, green and blue curves per channel.
- Local (mask) Whites keeps the earlier processes' operator.
- The measurements use Camera Raw's process version 2012 controls (`crs:ProcessVersion` 11.0).

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-10 | minor | Process V9: measured Whites and parametric curve, hue-preserving master curve |
| 2026-11-11 | minor | The Edit panel's parametric graph draws and drags the measured regions under V9 |
| 2026-11-24 | minor | Whites and the regions are a fitted model instead of measured tables |
