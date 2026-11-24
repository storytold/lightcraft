# The colour mixer, measured (process V9)

> **Changes:** [revision history](#revision-history)

Process V9 renders the colour mixer (Hue, Saturation and Luminance of the eight bands) the way Lightroom does, with
LightCraft's own model fitted to measurements of Camera Raw's output. V1 to V8 are unchanged
(`docs/process-versions.md`). The code is `crates/pipeline/src/mixer_stage.rs`, the model's fitted constants
`crates/pipeline/src/mixer_stage_model.rs`, and the GPU kernel's `mixer_stage` in `crates/gpu/src/wgsl/finish.wgsl`.

## What was wrong

Up to V8 the mixer is a model: eight bands with smooth weights around fixed OkLCh hues, a hue rotation, a chroma
scale and a lightness change, linear in the slider, applied after the tone map (so after Whites) and before the output curves. Measured against Camera Raw on
fine colour charts:

- **The bands sit in the wrong place.** V8 centres each band on the OkLab hue of the pure sRGB colour (Red 29,
  Orange 82, Yellow 110, Green 142, Aqua 195, Blue 254, Purple 312, Magenta 335). Lightroom's Saturation +50 peaks
  at 10, 50, 95, 140, 180, 255, 320 and 350 (5° bins). A red-orange such as skin or autumn leaves gets Lightroom's
  Orange adjustment but V8's Red one.
- **The widths differ.** At half height Lightroom's Blue band is about 85° wide against V8's 59°, its Red and Magenta
  bands about 20° against 54° and 39°.
- **The response is not linear.** Saturation +100 does 2.6 to 2.9 times what +50 does, Luminance +100 about 1.7
  times. At ±100 Lightroom's Hue and Saturation sliders also move lightness, by up to 3 to 17 (OkLab L × 100)
  depending on the band. Its hue shift barely depends on the input's chroma (0.8 to 1.15 times between the lowest and
  highest measured level), while V8 scales the shift by chroma up to 0.12, so a colour of chroma 0.04 gets a third.
- **The order differs.** Lightroom applies the mixer before Whites; V8 applies Whites in the tone map, before the mixer (below).

## What V9 does

After the tone map and V4's calibrated Contrast and Blacks, before the curve stage ([curve-stage.md](curve-stage.md)):

1. Convert linear Rec.2020 to OkLCh.
2. Look up the response of every mixer slider that is set, from the model (below), sampled once per render into a
   table: for the pixel's hue (72 bins of 5°), chroma and lightness, the hue shift, the relative chroma change and
   the lightness change. Hue is interpolated linearly. Chroma and lightness are interpolated bilinearly between three
   levels of each (lightness 0.55, 0.78 and 0.92, chroma levels lower for brighter colours), and clamped outside
   them. Below the lowest chroma level the lightness change fades out, so greys stay grey.
3. All sliders act on the same input colour. Their hue and lightness changes add and their chroma changes multiply
   (charts with several sliders at once: closer than adding).
4. Convert back to linear Rec.2020.

V8's mixer in `ColorOps` is off under V9 (`ColorOps::mixer`), so the mixer applies once. With the mixer neutral the
stage does not run. Saturation, Vibrance, Point Color, B&W and Colour Grading keep their V8 operators,
now after the curve stage (V1 to V8 run them before the output curves). Calibration stays scene linear, before the
tone map.

## The model

168 constants (`mixer_stage_model.rs`). For slider kind `k` (Hue, Saturation, Luminance) of band `b` and output `o`
(hue shift in degrees, relative chroma change, lightness change), with `t` = amount / 100, `dc` = (C − 0.09) / 0.05
and `dl` = (L − 0.75) / 0.15 for the colour's OkLCh chroma and lightness:

    response = amp[b][k][o] · gain[k][o](t) · band[b](h) · exp(ac dc + al dl)
    gain(t)  = g1 |t| + g2 t² + g3 |t|³            (g4, g5, g6 for positive amounts)
    band(h)  = exp(−z² / 2),                       z = wrap(h − centre) / e^(left or right of the centre)

- **Per band** (24 constants): its centre on the OkLab hue circle and its width on each side, shared by the band's
  Hue, Saturation and Luminance sliders.
- **Per kind and output** (72 constants): the gain over the amount, which grows faster than linearly, and the
  dependence on the colour's chroma and lightness, shared by all eight bands.
- **Per band, kind and output** (72 constants): an amplitude.

## How it was measured

`tools/lr_mixer_charts.py charts OUT_DIR` writes linear DNG charts of OkLCh patches (72 hues × 3 chroma levels × 3
lightness levels, patches outside sRGB left out), with the settings in the DNG's XMP. It writes one neutral chart and
each band's Hue, Saturation and Luminance at each measured amount (193 charts). The charts are original test data.
Every chart was rendered through Camera Raw to a 16-bit Adobe RGB (1998) TIFF. Only that output is measured, and
nothing of Adobe's is copied or shipped. `tools/lr_mixer_charts.py fit RENDER_DIR OUT_RS` reads each render's
patches as OkLab, takes each setting's response against Camera Raw's own neutral chart, fits the model to it and
writes only the model's constants, `mixer_stage_model.rs`. No measurement is committed. It needs numpy, scipy and
tifffile (`tools/requirements-lr-tone-charts.txt`). Running the tool again reproduces the committed charts byte for
byte, and the constants with the pinned packages.

Camera Raw's neutral render desaturates bright colours: patches of one nominal chroma come out about three times
less chromatic at the top lightness level than at the bottom. The model therefore takes the colour's chroma and
lightness as they are, and the table it is sampled into keeps lower chroma levels for brighter colours.

How close the model is: predicting Camera Raw's edited charts (each slider at ±25, ±50, ±75 and ±100) from its
neutral chart with the stage's rules leaves 0.35 (Hue), 0.24 (Saturation) and 0.55 (Luminance) OkLab × 100, against
average effects of 0.68, 0.62 and 1.54. The measured responses themselves, used the same way, would leave 0.04,
0.05 and 0.12. A model with a band shape per slider and output (1,584 constants) leaves 0.15, 0.16 and 0.37, but
does no better on photos than this one. On photos: see
[Results](#results).

The order comes from separate check charts rendered while investigating: a strong curve together with mixer
settings. Predicting them from the single-setting renders leaves 1.17 (OkLab × 100) with the mixer before the curves
and 1.66 with it after. Those check charts are not part of the tool.

## Results

Held-out Panasonic Lumix DC-S9 photos (5 raws, 24 edits each plus Neutral, 1,500 px, aligned against Camera Raw's
renders), with the curve stage of [curve-stage.md](curve-stage.md). Tone is RMS lightness error, colour the mean a/b
error (OkLab × 100), all edit groups weighted equally. Lower is better.

| Mixer | Tone | Colour |
|---|---|---|
| Measured table (before) | 7.008 | 1.314 |
| This model | 7.015 | 1.288 |

## Limits

- Camera Raw's process version 2012 controls (`crs:ProcessVersion` 11.0), one chart layout, sRGB-gamut patches. More
  saturated colours than the top measured chroma level take its response.
- Calibration is not measured in. Its measured response matched the charts better than V8's matrix, but made the
  held-out photos worse (OkLab colour error 1.316 to 1.331), so V9 keeps V8's Calibration.
- Masks' Hue and Saturation keep the earlier operators.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-10 | minor | Process V9: the colour mixer measured against Lightroom, before the tone curves |
| 2026-11-24 | minor | The mixer is a fitted model of 168 constants instead of a measured table |
