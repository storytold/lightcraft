# Film grain

The Effects panel's Grain (`ctl:grain.amount`, `ctl:grain.size`, `ctl:grain.roughness`; XMP
`crs:GrainAmount`, `crs:GrainSize`, `crs:GrainFrequency`) draws grain from a simulated emulsion of
crystal layers: `crates/pipeline/src/crystal.rs` on the CPU, the same tables read by the GPU
kernel (`crates/gpu/src/wgsl/finish.wgsl`).

This is the grain of **process V7** and later ([process-versions.md](process-versions.md)). Photos on
V1 to V6 keep the two-octave value noise they were edited with (`finish::ValueGrain`), so opening an
older catalog never changes a photo's grain; Update to Current Process moves a photo to the crystal
grain. The sliders, their ranges and their XMP names are unchanged.

## Model

After Aurélien Pierre, [*Stochastic photographic grain synthesis from crystallographic structure
simulation*](https://eng.aurelienpierre.com/2023/07/stochastic-photographic-grain-synthesis-from-crystallographic-structure-simulation/)
(2023). Implemented from the article's description of the method; no code was taken from it or from
any other implementation.

- **Emulsion.** A stack of layers. Each layer has one crystal shape: a regular polygon of
  `round(4.5 + z)` vertices (3–8), a random rotation, and a width (circumscribed diameter) drawn from a
  log-normal distribution. The layers' widths are the quantiles of that distribution, dealt out in an
  order the seed shuffles, so every seed has the same mix of sizes (the same grain strength and
  width) in a different arrangement.
- **Crystals.** Each layer is a Boolean model: Poisson seeds grown into crystals, at the density
  `λ = −ln(1 − F)/A` (crystal area `A`) that leaves a fraction `F = ½` of the layer covered.
- **Capture.** A point's grain is the number of layers covering it less the mean: many independent
  layers of different crystal sizes add up to dense, soft, cloudy grain with no hard particle edges.
- **Print.** The light a crystal captures is proportional to the light falling on it, and a print
  shows less grain towards paper white: the grain is `Y·(1 − Y)·g` in linear light (`Y` the pixel's
  luminance), zero at black and at white. It is applied in the display-encoded domain, to first
  order: each channel `v` moves by `k·lift(v)·(1 − Y)·g`, where `lift(v) = Y·dv/dY` of the sRGB curve.
  In that domain the grain is symmetric, so it neither darkens shadows (the article's model in
  linear light does, through the concave encoding) nor greys blown highlights, and it keeps hue.
- **Colour:** monochrome, as in Lightroom Classic.

## Resolution independence

The emulsion is drawn in frame coordinates at twice the resolution of a 6000 px export (a tile pixel
is half a reference pixel), on two periodic tiles (tori) of 640 and 567 reference pixels; layers
alternate between them. The tiles are stored as summed-area tables, so each output pixel takes the
**exact mean of the emulsion under its footprint** (16 table reads): a fit-to-window preview shows
the export's grain box-downscaled to its size, and an export larger than 6000 px (a 45 MP camera)
still shows crystals rather than enlarged pixels. The tori share no small factor: the two patterns
only line up again after 362,880 reference pixels, so the grain has no visible period.

In a rotated frame (a straightened or rotated crop) the footprint is approximated: each pixel takes
the mean over an axis-aligned square of its size centred on it, not over the rotated pixel (summed
area tables only give axis-aligned boxes). The grain strength is the same; the exact match with a
box-downscaled export holds for unrotated frames in any orientation.

Measured on the comparison renders (four photos, six regions each, 1500 px preview against the
6000 px export box-downscaled to 1500 px): preview / export grain strength **1.00–1.04** at the
default, finest and coarsest settings. `crystal::tests::small_render_is_the_downscaled_reference`
checks it exactly at 1/2, 1/4 and 1/8 size; `tests_grain::small_render_matches_the_downscaled_large_render`
through the whole pipeline.

## Mapping and calibration

Amount, Size and Roughness keep Lightroom Classic's meaning: strength and width at mid grey match its
exports (the measurements of open PR #1060, `docs/grain.md` on that branch).

| Quantity | Value |
|---|---|
| strength (levels of 255 at mid grey, 6000 px) | `Amount/100 · exp(2.447 + 1.386·exp(−Size/19.88)) · (1 − 0.084d + 1.6384d²)`, `d = Roughness/100 − ½`: 8.55 at 50 / 25 / 50 |
| median crystal width (reference px) | by Size: 0.55, 1.39, 2.95, 5.17, 8.78, 12.38 at 0, 10, 25, 50, 75, 100; × 0.734 / 1 / 1.422 at Roughness 0 / 50 / 100 (`fit_widths`) |
| width spread (log std) | `hypot(0.1 + 0.6·Roughness/100, 0.45)` |
| layers | by Size: 16, 31, 54, 70 at 0, 25, 50, 100 |

- **Amount only scales the strength**: the amplitude is measured from the drawn emulsion itself (its
  variance at the reference resolution), so every setting and seed has exactly its target strength,
  and dragging Amount draws nothing new. In the article the layer count sets the strength; here it
  sets only how dense the texture is (coarser grain averages more layers, so it stays a soft clump
  texture).
- **Texture.** The width spread was chosen so the anti-aliased crystals have the texture of the
  first, whole-pixel prototype at every scale, not just its width: autocorrelation at 1–5 px and the
  strength kept by box downscales of 2–16 match it within a few hundredths
  (`texture_report`).
- **Size 0** is as fine as the model gets: crystals narrower than a pixel, uncorrelated pixels
  (width 0.51 px; Lightroom Classic's 0.43 needs anti-correlated grain).

Measured (chart, 6000 px, 8-bit, against the same build without grain):

| Amount / Size / Roughness | Strength at grey 20 · 128 · 245 | Width | Lightroom Classic at 128 |
|---|---|---|---|
| 25 / 25 / 50 | 1.66 · 4.29 · 1.67 | 1.11 | 4.29, 1.13 |
| 50 / 25 / 50 | 3.27 · 8.57 · 3.30 | 1.12 | 8.55, 1.13 |
| 100 / 25 / 50 | 6.53 · 17.14 · 6.36 | 1.12 | 17.01, 1.14 |
| 50 / 0 / 50 | 8.80 · 23.10 · 8.55 | 0.53 | 23.08, 0.43 |
| 50 / 50 / 50 | 2.46 · 6.42 · 2.43 | 1.70 | — |
| 50 / 100 / 50 | 2.24 · 5.83 · 2.19 | 3.39 | 5.83, 3.38 |
| 50 / 25 / 0 | 4.73 · 12.44 · 4.67 | 0.85 | 12.42, 0.86 |
| 50 / 25 / 100 | 4.46 · 11.75 · 4.37 | 1.53 | 11.68, 1.54 |

Unlike Lightroom Classic's, the grain fades towards black and white (the print model). Mean
brightness change on 24 photo regions: within ±0.1 level on every dark and mid-tone region at every
setting (#1060: up to +3.8 on dark fabric at 50 / 0 / 50); up to −0.55 on sunlit skin at the
strongest settings, where grain clips at white; blown highlights stay clean (−0.01 on a blown
curtain; #1060: −9.3). Fine detail under the grain is kept (detail gain
0.96–1.00).

## Performance

- **Drawing the emulsion** (per Size, Roughness and seed; cached, so Amount drags and re-renders
  draw nothing): every crystal is stamped from a per-layer table of its rasterisation at 16 × 16
  sub-pixel phases, as one 64-bit mask per row (a fixed number of rows per layer, those outside a
  band landing in scratch rows around it, so a crystal costs no branch per row), layers are counted
  in bit-sliced counters, and the tiles are drawn in parallel bands. 7–15 ms on an M2 Max (12
  threads), 15–33 ms on a Ryzen 5 7545U (6 cores, 12 threads; 10–20 % less than before the row
  masks) at any setting (`tile_timing_report`).
- **Drags.** A draft render (a slider drag) doesn't wait for a tile it hasn't got if one for the
  photo's seed is cached: the last one drawn stands in while the new one is drawn in the background,
  on a pool of half the cores so the drag's renders don't queue behind it, and the next render takes
  it. With none cached for the seed there is nothing to stand in, so the draft waits for a draw
  already running or draws the tile itself. The full-quality render when the drag ends waits for the
  exact tile. A Size drag on the Ryzen with its
  Radeon 740M (GPU, 2.5 MP preview): 13–14 ms a frame instead of 62–66 ms. (On wasm, without
  threads, drafts wait.)
- **Drag benchmark** (`FRAMES=100 cargo run --release -p lightcraft-gpu --example grain_drag_bench`: GPU
  draft renders of the 1936 × 1290 demo photo, median ms per frame of 100; the example has no
  dependency on this model, so it can be copied unchanged into another branch). Ryzen 5 7545U /
  Radeon 740M:

  | Build | No grain (Exposure drag) | Amount drag | Size drag |
  |---|---|---|---|
  | main (value noise) | 8–12 | 10–13 | 10–13 |
  | #1060 (particles) | 8 | 11–14 | 151–179 (17–312) |
  | this model | 8–11 | 14–15 | 13 |

  M2 Max (Metal), same benchmark, the medians of three runs:

  | Build | No grain (Exposure drag) | Amount drag | Size drag |
  |---|---|---|---|
  | main (value noise) | 3.2–3.4 | 2.9–3.0 | 2.9 |
  | #1060 (particles) | 3.3 | 3.0–3.1 | 35.6–36.0 |
  | this model | 3.2–3.3 | 3.4–3.5 | 5.9–6.0 |

- **Per pixel:** two box sums, on the CPU and on the GPU. The tables (12 MB) stay on the GPU
  between renders, for the last tile used (`Gpu::grain_tables`), and are copied after the finish
  kernel's other tables in its `aux` buffer on the GPU: an Amount drag uploads nothing. On the
  Radeon 740M an Amount drag frame (2.5 MP) went from 16.9 to 11–15 ms (no grain: 8 ms).
- **24 MP NEF → PNG** (`lightcraft-cli render`, M2 Max, median of three): no grain 1.06 s, grain
  50 / 25 / 50 1.05 s on the GPU; 1.28 s and 1.43 s CPU-only (the finish stage 0.26 → 0.42 s).
  Grain costs main ≈ 0 on the GPU and +0.08 s CPU-only, #1060 +0.29 s and +1.71 s.
- **24 MP NEF → PNG on the Ryzen 5 7545U / Radeon 740M** (median of three): no grain 2.07 s, grain
  50 / 25 / 50 2.10 s on the GPU; 2.72 s and 2.88 s CPU-only. Grain costs main +0.03 s on the GPU
  and +0.10 s CPU-only, #1060 +1.81 s and +3.54 s.
