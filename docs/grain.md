# Film grain

The Effects panel's Grain (`ctl:grain.*`, `crates/pipeline/src/grain.rs`, GPU port in
`crates/gpu/src/wgsl/finish.wgsl`) renders grain particles on the spot, per output pixel, rather
than overlaying a noise texture.

This is the grain of **process V3** and later ([process-versions.md](process-versions.md)). Photos
on V1 or V2 keep the value-noise grain they were edited with (`finish::ValueGrain`), so opening an
older catalog never changes a photo's grain; Update to Current Process (`develop.updateProcess`)
moves a photo to the particle grain, as one undo step. Size Variation, Clumping, Color and Response
only act on V3, so the Effects panel disables them on a V1 or V2 photo; Size, Roughness (Shape)
and Seed act on both models.

## Model

Implemented from the published Boolean model of Newson, Delon & Galerne, *Realistic Film Grain
Rendering* (IPOL 2017), extended to clumps of irregular grains. No code was taken from any
implementation.

- **Particles.** A *clump* is one to eleven grains around a centre. Clump centres form a Poisson
  process on a fixed grid of cells (2.5 mean grain radii), so no slider and no render size
  reshuffles them. Each grain has its own:
  - **size:** radius `exp(spread·z)`, z ≈ unit normal bounded to ±3 (Size Variation);
  - **shape:** an ellipse of random orientation and elongation up to 4:1 (area preserved), with a
    lobed outline: the radius varies with angle by `L·m·(0.6 cos(2θ − φ₂) + 0.4 cos(3θ − φ₃))`
    (Shape);
  - **position in the clump:** within ±0.8 to ±2 radii of its centre (Clumping);
  - **optical density:** 0.25–1, so overlapping grains give continuous tones.

  Every value comes from hashes of the seed and the grain's cell: a seed is a pattern.
- **Density follows tone.** A pixel of display-encoded luminance `l` has expected grain coverage
  `p = 0.2 + 0.6·(1 − l)^γ`, where darker means denser. The clump density is set so that
  `λ·E[∫a_clump] = −ln(1 − p)`.
- **Each pixel shows the coverage averaged over its footprint.** Every grain is softened by an edge
  width `s` that combines a constant film softness (0.4 radii) with one output pixel. The union is
  evaluated exactly as `1 − Π(1 − aᵢ)`.
- **Development halo.** Fine or smooth grain (below Roughness 50, or finer than Size 25) gets a
  ring of lower density around each clump, from the clump's size `c` (`1 + 0.7·spread` grain
  radii) out to `H = 3.2` times it, whose integral is `η` times its grains' (as if the developer
  were exhausted around a clump). It makes the grain regular: its autocorrelation dips below zero
  one to three pixels out, as measured in Lightroom Classic. The ring is
  `k·W·(P(d²/R_out²) − P(d²/R_in²))` with `W = Σ oᵢ·rᵢ²` over the clump's grains and
  `P(u) = (1 − u)²` below 1: a polynomial in the squared distance, evaluated once per clump
  rather than per grain (`clump_halo`). The halo is additive: the field is
  `1 − T − (1 − p)·(Hs − E[Hs])`, where `T = Π(1 − aᵢ)` and `Hs` sums the clumps' rings.
- **Exact statistics.** The Poisson generating functional gives the mean (`p` at any scale) and
  the variance `(1 − p)²·(exp(λ·E[∫a²]) − 1 + λ·(E[∫h²] − 2·E[∫a·h]))`. The clump integrals are
  measured for each setting by rasterising 512 sample clumps drawn by the renderer's own generator,
  in parallel and summed in a fixed order (`clump_moments`; cached, so a drag of Amount, Color or
  Response measures nothing).
- **Particles fade in with tone.** Within a cell, clumps arrive in mark order and fade in over the
  last clump's worth of the mark range. The field therefore changes continuously as the tone
  changes: no particle pops in or out between neighbouring pixels or slider steps.
- **Output:** each channel gets `amp · w(l) · (p − field)` in display-encoded space, before the
  profile LUT. `w(l)` divides by the field's standard deviation at the reference resolution, so
  the grain is **equally strong at every tone**, as in Lightroom Classic (measured flat within 1 %
  from 63 to 179 of 255). Response tilts it: `w` is multiplied by `exp(t·(l − ½))` with
  `t = ln 4 · Response/100`, so at ±100 the grain is 4× stronger at one end than the other (and
  the particle density shifts with tone, as before).
- **Colour:** with Color `c`, each channel gets `√(1 − c)·shared + √c·dye`, where `shared` is the
  one grain layer and `dye` the channel's own independent layer. Colour 0 is monochrome grain,
  100 independent dye clouds, and the strength stays the same in between.

## Resolution independence

Grain geometry is defined in normalized frame coordinates (fractions of the uncropped frame's long
edge, like grain on a negative). Only the softening (one output pixel) depends on the render size, so
a small render shows what the export shows downscaled:

- **Explicit particles** are used while one output pixel's softening (the softness less the
  film's) is up to 0.35 clump radii.
- **Per-pixel Gaussian noise** with the exact variance above is used from 0.8 clump radii, i.e.
  clumps smaller than a pixel: grid thumbnails, fit previews, and fine grain even on a 24 MP
  export (Size below about 10, whose particles are a third of a pixel wide). The noise is
  correlated with its four neighbours (`noise_e`) so that the next pixel's correlation is the one
  the reference render has once box-averaged to this render's pixels (`block_correlation`): Size 0
  keeps its negative correlation (−0.2 at one pixel) as noise.
- **Between the two**, the explicit and Gaussian parts cross-fade with variance-preserving weights.
- **Calibration:** the pixel softness, 0.44 px without clumps rising to 0.5 px at Clumping 100, gives
  a small render the grain strength of the export box-downscaled to its size for grain without a
  halo. With a halo (band-pass grain, whose strength a box removes very differently from a soft
  footprint), below 6000 px the amplitude is set from the reference render's own covariance:
  `ref_covariance` measures it at lags up to 16 (pitch ≥ 1 reference pixel) from 128 sample clumps,
  and its box-averaged variance gives the strength a render 6000/k px wide must have.
- **Tests:** `small_render_matches_a_downscaled_full_size_render` (grain level) and
  `small_pipeline_render_matches_downscaled_full_render` (whole pipeline).
- **Measured:** renders at 1/2 to 1/8 size are within about 10 % of the downscaled export's grain
  strength across sizes, roughness and clumping (`downscale_report`, ignored: run it with
  `--ignored --nocapture`).

Amount is calibrated at a 6000 px long edge (a 24 MP export); grain strength is linear in Amount.
Amount 100 gives 17 levels of 255 at the defaults there (Size 25, Roughness 50), 46 at Size 0 and
12 at Size 100: finer grain is stronger, as in Lightroom Classic.

## Mapping (`GrainLook::new`)

Size `S` and Roughness `ρ = Roughness/100` set the look. The curves were fitted to the seven
Lightroom Classic exports below; between those points they are smooth guesses.

| Quantity | Formula | Size 0 / 25 / 100 (Roughness 50) |
|---|---|---|
| mean radius (px at 6000) | `(0.3 + 2.7·(S/100)^1.17) · (0.96 − 0.33ρ + 0.82ρ²)` | 0.30 / 0.83 / 3.0 |
| shape (elongation, lobes) | `ρ` | |
| halo integral `η` | `(1 − 2ρ)⁺ + 1.1·((1 − S/25)⁺)²` | 1.1 / 0 / 0 |
| halo radius `H` (clump sizes) | `3.2` | 3.2 |
| levels at Amount 100 | `exp(2.447 + 1.386·exp(−S/19.88)) · (1 − 0.084d + 1.6384d²)`, `d = ρ − ½` | 46.2 / 17.1 / 11.7 |

Roughness 0 is finer (0.96× the radius), regular (halo 1) and 1.45× stronger than Roughness 50;
Roughness 100 is coarser (1.45× the radius), irregular and 1.37× stronger.

## Calibration against Lightroom Classic

Seven settings exported from Lightroom Classic 15.1.1 (process version 15.4) as full-size JPEGs of
a 6000 × 4000 chart of flat grey steps (39, 63, 118, 179, 231 of 255), measured on the luminance of
each step (900 × 3400 px): the standard deviation in 8-bit levels, and the lag (px) at which the
autocorrelation (mean of horizontal and vertical) falls to ½. "Ours" is `lightcraft-cli render` of
the same chart (CPU), measured the same way; "before" is the grain before this calibration.

| Amount / Size / Roughness | Lightroom Classic: sd at 39 · 63 · 118 · 179 · 231; width | Ours | Before |
|---|---|---|---|
| 25 / 25 / 50 | 4.29 · 4.29 · 4.29 · 4.29 · 4.29; 1.13 | 4.24 · 4.22 · 4.24 · 4.14 · 4.23; 1.24 | 1.78 · 2.24 · 2.83 · 2.52 · 1.43; 1.14 |
| 50 / 25 / 50 | 8.55 · 8.56 · 8.55 · 8.55 · 8.52; 1.13 | 8.65 · 8.65 · 8.60 · 8.59 · 8.58; 1.17 | 3.43 · 4.39 · 5.61 · 4.97 · 3.05; 1.16 |
| 100 / 25 / 50 | 16.81 · 17.03 · 17.01 · 17.01 · 15.71; 1.14 | 17.39 · 17.37 · 17.31 · 17.28 · 17.25; 1.13 | 6.79 · 8.81 · 11.19 · 10.06 · 6.02; 1.16 |
| 50 / 0 / 50 | 22.12 · 23.01 · 23.08 · 23.06 · 20.00; 0.43 | 22.41 · 23.29 · 23.30 · 23.32 · 20.21; 0.41 | 3.01 · 3.91 · 4.98 · 4.50 · 2.74; 1.02 |
| 50 / 100 / 50 | 5.82 · 5.84 · 5.83 · 5.85 · 5.83; 3.38 | 5.87 · 5.81 · 5.80 · 5.89 · 5.84; 3.45 | 4.61 · 5.91 · 7.52 · 6.73 · 3.82; 3.47 |
| 50 / 25 / 0 | 12.41 · 12.42 · 12.42 · 12.41 · 12.05; 0.86 | 12.60 · 12.58 · 12.53 · 12.51 · 12.44; 0.87 | 3.42 · 4.39 · 5.62 · 4.96 · 3.04; 1.20 |
| 50 / 25 / 100 | 11.68 · 11.68 · 11.68 · 11.68 · 11.42; 1.54 | 11.87 · 11.87 · 11.84 · 11.75 · 11.66; 1.48 | 3.43 · 4.40 · 5.60 · 4.97 · 3.04; 1.09 |

Autocorrelation at lags 1, 2, 3 px in mid grey: Size 0 is −0.17, 0.02, 0.00 in Lightroom Classic
(ours −0.22, 0.01, 0.00); Roughness 0 is 0.42, −0.15, −0.12 (ours 0.43, −0.12, −0.12).

Box-downscaled by 2, 4, 8 and 16, the mid-grey step keeps (levels):

| Amount / Size / Roughness | Lightroom Classic | Ours |
|---|---|---|
| 50 / 25 / 50 | 6.65 · 3.94 · 2.14 · 1.13 | 6.78 · 4.36 · 2.42 · 1.28 |
| 50 / 0 / 50 | 9.00 · 3.90 · 1.80 · 0.87 | 8.97 · 3.84 · 1.75 · 0.83 |
| 50 / 100 / 50 | 5.67 · 5.09 · 3.74 · 2.27 | 5.60 · 5.02 · 3.85 · 2.39 |
| 50 / 25 / 0 | 8.63 · 3.39 · 1.15 · 0.41 | 8.82 · 3.79 · 1.60 · 0.79 |
| 50 / 25 / 100 | 10.12 · 7.10 · 4.10 · 2.21 | 9.97 · 7.25 · 4.35 · 2.37 |

The grain's large-scale content is within about 15 %, except at Roughness 0: its regular grain
keeps 1.1–1.9× as much as Lightroom Classic's at 4–16 px. Lightroom Classic's strong grain (Amount
100, or Size 0) is weaker at 231 of 255; ours stays flat there. Not measured: Size between 25 and
100, Size and Roughness together away from these points, export sizes other than 6000 px, and
anything about Lightroom Classic's own grain model (Size Variation, Clumping, Color and Response
have no Lightroom Classic counterpart to compare).

`grain_matches_lightroom_classic_exports` renders the seven settings on a procedural flat-grey
chart at 6000 px and asserts these numbers (strength and width within 10 %, flat from 63 to 179
within 6 %, the negative lobes of Size 0 and Roughness 0). `lrc_fit_report` (ignored) prints the
full table for a candidate mapping.

## Controls

| Control | Range | Meaning |
|---|---|---|
| `grain.amount` | 0–100 | strength (XMP `crs:GrainAmount`) |
| `grain.size` | 0–100 | mean grain radius on a 6000 px export at Roughness 50: 0.3 px (0), 0.83 (25), 1.5 (50), 2.2 (75), 3 px (100); 1.8–18 µm on a 35 mm frame. Finer grain is stronger (XMP `crs:GrainSize`) |
| `grain.sizeVariation` | 0–100 | spread of grain sizes: 0 = all the mean size; 100 = from 0.35× to 2.9× |
| `grain.roughness` (Shape) | 0–100 | 0 = fine, round, regular grain (with a development halo); 100 = coarser, elongated (up to 4:1), lobed, irregular grains; both ends are stronger than 50 (XMP `crs:GrainFrequency`) |
| `grain.clumping` | 0–100 | 0 = single grains; 100 = clusters of six grains on average (up to eleven) |
| `grain.color` | 0–100 | monochrome grain (0) to independent dye clouds per channel (100) |
| `grain.response` | −100–100 | where grain is strongest: equally strong at every tone (0); up to 4× stronger in the shadows (−100) or the highlights (+100) than at the other end |
| `grain.seed` | 0–99,999 | the particle pattern: every value is a different one (drag to try patterns, or type one) |

Settings saved before Size Variation and Clumping existed load with their defaults (40 and 30);
those without Colour and Response load with both at 0. LightCraft keeps all four and the seed in its
own settings; Camera Raw has no equivalents.

## Performance

- **GPU:** the per-pixel kernel evaluates the clumps within reach of each pixel. On the 24 MP test
  export the finish pass takes ≈ 200 ms with default grain (≈ 80–270 ms without; ≈ 650 ms with
  Color: four layers). A 1500 px preview takes ≈ 15 ms. These were measured before the Lightroom
  Classic calibration and not since (no GPU on the calibration machine).
- **CPU:** works on bands of 8 output rows. Each clump is generated once per band and its grains
  are splatted onto the pixels they reach (`band_matches_per_pixel` checks the result equals the
  per-pixel evaluation). The finish stage (`LIGHTCRAFT_PROFILE=1`) of a `lightcraft-cli render`
  of the 24 MP chart on 32 threads, fastest of three, the same build settings, before the
  Lightroom Classic calibration (`765142bac`) and after:

  | Amount / Size / Roughness | 0 (no grain) | 50 / 25 / 50 | 50 / 0 / 50 | 50 / 25 / 0 | 50 / 100 / 50 |
  |---|---|---|---|---|---|
  | before | 0.12 s | 1.09 s | 1.25 s | 0.78 s | 0.59 s |
  | calibrated, halo per grain (first version) | 0.13 s | 1.25 s | 7.72 s | 2.39 s | 0.70 s |
  | calibrated, halo per clump, fine grain as noise | 0.13 s | 1.15 s | 0.31 s | 1.49 s | 0.66 s |

  The machine was also running another build, so the times vary by about 10 %. Size 0 is now
  cheaper than before (noise instead of particles); Roughness 0 costs about twice as much: its ring
  around each clump reaches 4–5 px.
- **Parameters:** a new Size, Roughness, Size Variation or Clumping measures the clump moments
  (512 sample clumps) and, below 6000 px or for noise, the reference covariance: a few ms on 32
  threads, cached per setting.

CPU-only exports with grain are over the 1 s budget: real particles cost more than a noise
texture. Interactive renders and exports use the GPU path when an adapter exists.
