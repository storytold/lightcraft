# Film grain

The Effects panel's Grain (`ctl:grain.*`, `crates/pipeline/src/grain.rs`, GPU port in
`crates/gpu/src/wgsl/finish.wgsl`) renders grain particles on the spot, per output pixel, rather
than overlaying a noise texture.

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
  evaluated exactly as `1 − Π(1 − aᵢ)`. The Poisson generating functional then gives the mean (`p`
  at any scale) and the variance `(1 − p)²·(exp(λ·E[∫a_clump²]) − 1)`. The two clump integrals
  are measured for each setting by rasterising 128 sample clumps drawn by the renderer's own
  generator (`clump_moments`, cached, so a drag of Amount, Color or Response measures nothing).
- **Particles fade in with tone.** Within a cell, clumps arrive in mark order and fade in over the
  last clump's worth of the mark range. The field therefore changes continuously as the tone
  changes: no particle pops in or out between neighbouring pixels or slider steps.
- **Output:** each channel gets `amp · w(l) · (p − coverage)` in display-encoded space, before the
  profile LUT. `w(l)` is 1 in the midtones and 0.35 at black and white, and the Response slider
  moves its peak.
- **Colour:** with Color `c`, each channel gets `√(1 − c)·shared + √c·dye`, where `shared` is the
  one grain layer and `dye` the channel's own independent layer. Colour 0 is monochrome grain,
  100 independent dye clouds, and the strength stays the same in between.

## Resolution independence

Grain geometry is defined in normalized frame coordinates (fractions of the uncropped frame's long
edge, like grain on a negative). Only the softening (one output pixel) depends on the render size, so
a small render shows what the export shows downscaled:

- **Explicit particles** are used while the softening is up to 0.6 clump radii.
- **Per-pixel Gaussian noise** with the exact variance above is used from 1.1 clump radii, i.e.
  clumps much smaller than a pixel (grid thumbnails, fit previews of fine grain).
- **Between the two**, the explicit and Gaussian parts cross-fade with variance-preserving weights.
- **Calibration:** the pixel softness, 0.44 px without clumps rising to 0.5 px at Clumping 100, gives
  a small render the grain strength of the export box-downscaled to its size.
- **Tests:** `small_render_matches_a_downscaled_full_size_render` (grain level) and
  `small_pipeline_render_matches_downscaled_full_render` (whole pipeline).
- **Measured:** a quarter-size render is within 4 % of the downscaled export's grain strength
  across sizes and clumping.

Amount is calibrated at a 6000 px long edge (a 24 MP export). Amount 100 gives a standard deviation
of about 0.06 (15 levels of 255) in mid-grey there, a little more for coarse grain.

## Controls

| Control | Range | Meaning |
|---|---|---|
| `grain.amount` | 0–100 | strength (XMP `crs:GrainAmount`) |
| `grain.size` | 0–100 | mean grain radius on a 6000 px export: 0.7 px (0), 0.8 (25), 1.2 (50), 1.9 (75), 3 px (100); 4.3–18 µm on a 35 mm frame (XMP `crs:GrainSize`) |
| `grain.sizeVariation` | 0–100 | spread of grain sizes: 0 = all the mean size; 100 = from 0.35× to 2.9× |
| `grain.roughness` (Shape) | 0–100 | grain shape: 0 = round, even beads; 100 = elongated (up to 4:1), lobed, irregular grains (XMP `crs:GrainFrequency`) |
| `grain.clumping` | 0–100 | 0 = single grains; 100 = clusters of six grains on average (up to eleven) |
| `grain.color` | 0–100 | monochrome grain (0) to independent dye clouds per channel (100) |
| `grain.response` | −100–100 | where grain is strongest: shadows (−) … midtones (0) … highlights (+) |
| `grain.seed` | 0–99,999 | the particle pattern: every value is a different one (drag to try patterns, or type one) |

Settings saved before Size Variation and Clumping existed load with their defaults (40 and 30);
those without Colour and Response load with both at 0. LightCraft keeps all four and the seed in its
own settings; Camera Raw has no equivalents.

## Performance

- **GPU:** the per-pixel kernel evaluates the clumps within reach of each pixel. On the 24 MP test
  export the finish pass takes ≈ 200 ms with default grain (≈ 80–270 ms without; ≈ 650 ms with
  Color: four layers). A 1500 px preview takes ≈ 15 ms.
- **CPU:** works on bands of 8 output rows. Each clump is generated once per band and its grains
  are splatted onto the pixels they reach (`band_matches_per_pixel` checks the result equals the
  per-pixel evaluation). On 32 threads a 24 MP export's finish stage goes from ≈ 110 ms to ≈ 0.9 s
  at the defaults (≈ 4 s with Color); a 1500 px render takes ≈ 30 ms.

CPU-only exports with grain are over the 1 s budget: real particles cost more than a noise
texture. Interactive renders and exports use the GPU path when an adapter exists.
