# The colour mixer, measured (process V9)

> **Changes:** [revision history](#revision-history)

Process V9 renders the colour mixer (Hue, Saturation and Luminance of the eight bands) the way Lightroom does, from
measurements of Camera Raw's output. V1 to V8 are unchanged (`docs/process-versions.md`). The code is
`crates/pipeline/src/mixer_stage.rs`, its measured table `crates/pipeline/src/mixer_stage.bin`, and the GPU kernel's
`mixer_stage` in `crates/gpu/src/wgsl/finish.wgsl`.

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
2. Look up the response of every mixer slider that is set in Camera Raw's measured table: for the pixel's hue (72
   bins of 5°), chroma and lightness, the hue shift, the relative chroma change and the lightness change. The table
   holds each slider at ±25, ±50, ±75 and ±100. Amounts in between are interpolated linearly, towards 0 below 25.
   Hue is interpolated linearly. Chroma and lightness are interpolated bilinearly between the three measured levels
   of each, and clamped outside them. Below the lowest measured chroma the lightness change fades out, so greys stay
   grey.
3. All sliders act on the same input colour. Their hue and lightness changes add and their chroma changes multiply
   (charts with several sliders at once: closer than adding).
4. Convert back to linear Rec.2020.

V8's mixer in `ColorOps` is off under V9 (`ColorOps::mixer`), so the mixer applies once. With the mixer neutral the
stage does not run. Saturation, Vibrance, Point Color and B&W keep their V8 operators, now after
the curve stage (V1 to V8 run them before the output curves). Calibration stays scene linear, before the tone map. Colour Grading also runs after the curves, through its own measured V9 stage
([grading-stage.md](grading-stage.md)).

## How it was measured

`tools/lr_mixer_charts.py charts OUT_DIR` writes linear DNG charts of OkLCh patches (72 hues × 3 chroma levels × 3
lightness levels, patches outside sRGB left out), with the settings in the DNG's XMP. It writes one neutral chart and
each band's Hue, Saturation and Luminance at each measured amount (193 charts). The charts are original test data.
Every chart was rendered through Camera Raw to a 16-bit Adobe RGB (1998) TIFF. Only that output is measured, and
nothing of Adobe's is copied or shipped. `tools/lr_mixer_charts.py table RENDER_DIR OUT_BIN` reads each render's
patches as OkLab, takes each setting's response against Camera Raw's own neutral chart, and writes
`mixer_stage.bin` (format in the tool's docstring: per row a scale and 72 signed bytes, 394 KB). It needs numpy and
tifffile (`tools/requirements-lr-tone-charts.txt`). Running the tool again reproduces the committed charts and the
committed table byte for byte.

Camera Raw's neutral render desaturates bright colours: patches of one nominal chroma come out about three times
less chromatic at the top lightness level than at the bottom. The table therefore keeps chroma keys per lightness
level. Predicting Camera Raw's edited charts from its neutral chart with the stage's rules leaves 0.04 (Hue), 0.04
(Saturation) and 0.10 (Luminance) OkLab × 100, against average effects of 0.57, 0.51 and 1.27. That covers ±10 too,
which the table does not hold. With one set of chroma keys for all lightness levels it leaves 0.07, 0.08 and 0.28.

The order comes from separate check charts rendered while investigating: a strong curve together with mixer
settings. Predicting them from the single-setting renders leaves 1.17 (OkLab × 100) with the mixer before the curves
and 1.66 with it after. Those check charts are not part of the tool.

## Limits

- Camera Raw's process version 2012 controls (`crs:ProcessVersion` 11.0), one chart layout, sRGB-gamut patches. More
  saturated colours than the top measured chroma level take its response.
- Calibration is not measured in. Its measured response matched the charts better than V8's matrix, but made the
  held-out photos worse (OkLab colour error 1.316 to 1.331), so V9 keeps V8's Calibration.
- Masks' Hue and Saturation keep the earlier operators.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-11 | patch | Colour Grading under V9 has its own measured stage, not V8's operator |
| 2026-11-10 | minor | Process V9: the colour mixer measured against Lightroom, before the tone curves |
