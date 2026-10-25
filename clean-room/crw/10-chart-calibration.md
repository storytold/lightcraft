# EOS D60 colour matrices from a ColorChecker (2026-10-25)

The Canon EOS D60 has no published spectral sensitivities, so it is not in `spectral_table.rs`.
These are its DNG-style matrices (`ColorMatrix1/2`, `ForwardMatrix1/2`), fitted to our own
photographs of a ColorChecker Classic under two lights. They are in
`crates/raw/src/chart_table.rs`. Measurements come from the maintainer's own captures (names,
sizes and SHA-256 in `logs/chart-captures.tsv`; no image content recorded) and the chart's
published reference colours (`sources.md`). Scripts and measurements are local-only, in
`plan/crw/chart/`.

## Captures

| Set | Frames used | Light | Notes |
|---|---:|---|---|
| `SunlightColorCard/` | 20 of 22 | Direct afternoon sun, ISO 100 | Calibrite ColorChecker Classic (50th-anniversary edition, post-2014 formulation) leaning on a wall, filling most of the frame. Auto and Daylight white balance, a shutter bracket. Two frames dropped: a white-patch sample ≥ 3900 (clip ≈ 4000 at ISO 100, `08-color.md` §2). |
| `TungstonColorChart/` 2696–2706 | 11 | One household tungsten bulb, ISO 100 | Chart flat on a wall, a shutter bracket. Brightest white patch 2657 of ~4000. |
| `TungstonColorChart/` 2688–2691 | not used | Same bulb, 8 minutes earlier | Steady within the set, but its neutral reads ~3,040 K against ~2,900 K for the later set: the light changed (window light or bulb warm-up). The later, warmer and better-exposed set is used. |

All 37 frames decode sample-exact against the black-box reference (`crw_oracle.rs`).

An earlier set on a ColorChecker-layout clone (Pixel Perfect "24 Standard Color Calibration
Chart", 2026-10-15) was not used. The maker publishes no reference values, and its grey patches
are not neutral: against an 18 % grey board they read R/G 0.88–0.94 in shade and B/G up to
1.29 under warm LED light, the darker patches drifting most. That is the sign of greys printed
from coloured inks.

## Measurement

- Patches are located on the embedded JPEG (four corner patch centres, bilinear grid), tracked
  per frame by phase correlation, and checked visually on drawn boxes. The tungsten-set chart
  was upside down in the first four frames; patch order is mapped accordingly.
- Each patch is measured in a central box of about 40 % of its width in the linear raw (black
  from the far masked border per CFA site; R, mean of both G, B per 2×2 site). Uniformity
  across the box (3×3 sub-blocks): ≤ 4 % in sun, ≤ 7 % under the bulb (light falloff across
  the closer chart).
- Frames are normalised by the four middle neutrals (patches 20–23), averaged, and white
  balanced on those neutrals. White balance (R, B multipliers, G = 1): sun 2.362, 1.298; bulb
  1.353, 2.213. The neutrals agree on it to ±2 %.

## Fit

- The reference is X-Rite's published L\*a\*b\* (D50) for charts made after November 2014.
- The model is a 3×3 matrix from white-balanced camera RGB to XYZ (D50), with neutral (1, 1, 1)
  held exactly at the engine's D50 white (`lightcraft_color::D50`): the third column is D50
  minus the first two. That is a DNG `ForwardMatrix`.
- **Chromaticity only.** Each prediction is scaled to the patch's reference luminance before
  the CIEDE2000 residual. In the sun the chart warped as it heated: the grey row's brightness
  slope swung from +4 % to −7 % across the 70 s of shooting, in time order, and the per-patch
  brightness needed was not smooth across the chart. A fit including lightness would have
  absorbed that into the matrix. Chromaticity still determines the whole 3×3 up to scale,
  which the white constraint fixes. The cost is that luminance of saturated blues and purples
  rests on the model, not the measurement.
- `ColorMatrix` per light = diag(1 / multipliers) · ForwardMatrix⁻¹ · Bradford(light → D50),
  scaled so the light's white maps to camera values with maximum 1. By construction it maps
  the filed illuminant's white to the measured camera neutral.

| Light | CIEDE2000 on the 24 patches | Leaving each patch out | Worst |
|---|---|---|---|
| Sun (filed as D50) | mean 1.70, median 1.46 | mean 1.97 | white 4.5 |
| Bulb (filed as A) | mean 2.42, median 1.79 | mean 2.74 | bluish green 6.7 |

- One matrix does not serve both lights: the sun's matrix on the bulb's chart gives mean 3.6
  (cyan 9.0), the bulb's on the sun's 3.4 (red 7.7). Hence the dual-illuminant entry.
- The neutral row reads a\* within ±0.4 for the middle greys; white and the darkest patches
  are off by up to 2 (white patch sheen, flare).

## Which illuminants

The two lights' colour temperatures come from the camera's own white-balance presets
(ColorBalance slots, `08-color.md` §3) at the temperatures Canon's manuals give them (Daylight
5200 K, Tungsten 3200 K, Fluorescent 4000 K), interpolated in mired on ln(R/B): sun ~4,850 K,
bulb ~2,900 K. They are filed under the nearest DNG illuminants the engine knows, **D50
(5003 K) and Standard Illuminant A (2856 K)**, so `Camera` now carries its own illuminant
pair (spectral rows: A and D65).

## Checks in the app

- `spectral::tests::the_chart_measured_eos_d60_reads_its_presets_in_kelvin`: the camera's
  presets, which the fit never saw, read through the matrices as Tungsten 3,264 K, Daylight
  5,603 K, Cloudy 6,613 K, Flash 7,085 K, near the locus. Canon's nominal values are 3200, 5200,
  6000 and 6000 K. Order and spacing hold; the daylight presets read 8–15 % high, as Sony's
  dial does against its spectral matrices (#730).
- Real files through the headless app (As Shot): WBModes Fluorescent 4,103 K, tint +26 (green
  compensation); LED-lit room, Auto 3,761 K; open shade, Auto 6,596 K; sunlit chart, Auto
  4,762 K.
- `files::tests::crw_probes_name_their_camera_from_the_ciff_records`: CRW imports now take make,
  model and capture time from the decoder's CIFF records (no Exif in a CRW; the catalog had an
  empty camera, so a D60 stayed on the relative white-balance scale).

## Limits

- One body, one bulb, one afternoon. The chart is 24 patches; a 3×3 matrix cannot follow the
  sensor's departures from the CIE observer (the bulb fit's worst patches are 5–7 ΔE).
- The lights are filed under standard illuminants they only approximate (~50 K for the bulb,
  ~150 K for the sun).
- The D60's look is still the per-file JPEG fit and the pooled profile; these matrices give the
  white-balance scale and the fallback colour only.
