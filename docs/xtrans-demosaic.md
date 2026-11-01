# X-Trans demosaic

> **Last updated:** 2026-10-24 · **Code:** `crates/raw/src/demosaic/xtrans_ri.rs`, filters in `xtrans_taps.rs` ·
> **Tools:** `crates/raw/examples/xtrans_truth.rs`, `crates/raw/examples/xtrans_bench/`

Fujifilm's X-Trans sensors use a 6 × 6 colour pattern instead of Bayer's 2 × 2. Bayer methods (AHD, PPG) don't apply,
and the obvious generalisations lose to plain bilinear interpolation. This page describes what LightCraft does, how it
was measured, and how to reproduce or improve it.

## Method

`Method::Ahd` (the quality default) on an X-Trans mosaic, in any phase and at least 24 × 24 pixels:

1. **White balance first.** The mosaic is multiplied by the as-shot multipliers (`RawImage::develop` passes them as
   `demosaic::Options::balance`) and divided back at the end. Balanced, a colour edge is mostly a luminance edge, and
   every sample is an estimate of it; in camera RGB, R − G and B − G change with brightness across every edge, which
   any interpolation turns into fringes.
2. **First-pass green** at red and blue sites: four directional candidates (horizontal, vertical, both diagonals),
   each the linear interpolation between the nearest green on either side, weighted by `1 / (ε + activity)²`; the
   activity is the mean absolute difference of same-colour pairs one step along that direction in the 5 × 5 window.
3. **Class** of each red/blue pixel from the structure tensor of that green (5 × 5 Gaussian, σ = 1.2): gradient angle in
   8 bins, relative strength (`√λ₁ / local mean`) in 3, coherence in 3 — 72 classes. The strength and coherence
   thresholds are training quantiles (50 / 80 %, 33 / 66 %).
4. **Green** at red/blue sites: a 7 × 7 linear filter over the balanced mosaic, one per pattern position and class
   (8 positions: the pattern maps onto itself under a (3, 3) shift, so 16 red/blue phases share 8 filter sets).
   Least-squares (LMMSE) fits, constrained so flat colour comes out exactly (taps on green sum to 1, on red and on
   blue to 0).
5. **Red and blue** by residual interpolation (Kiku, Monno, Tanaka & Okutomi, *Residual interpolation for color image
   demosaicking*, ICIP 2013): per colour, a guided filter fits `C ≈ a·G + b` over the colour's samples in the 7 × 7
   window (slope pulled toward 1 by a ridge λ = 10⁻⁵ and kept ≥ 0), averages `a`, `b` over the window, and the
   residual `C − (a·G + b)` at the samples is interpolated with weights `1 / (d² (ε + |G(q) − G(p)|))`.

The image is processed in 160 px tiles with a 16 px halo, in parallel, each worker reusing one set of buffers. Beyond
the image, samples repeat from the nearest pixel at the same pattern position, so the pattern stays intact to the
border. Every pixel's result depends only on its neighbourhood, never on where tiles fall (a test checks it bit for
bit). Native samples are passed through unchanged.

`Method::Ppg` keeps the previous algorithm (edge-weighted directional colour difference, unbalanced): about 3× faster,
much lower quality. Other 6 × 6 patterns and images under 24 × 24 use it too.

## Ground truth and training

Every pattern-aligned 3 × 3 block of an X-Trans mosaic (green centre and corners, a red pair and a blue pair on opposite
sides) holds all three colours with one common centroid. Binning each block into one pixel gives a full-colour image at
a third of the resolution with no interpolation at all: real sensor, real lens, real noise floor.
`xtrans_truth` does this for every X-Trans raw in a folder (as-shot white balance, exposure normalised, blocks with a
clipped sample marked invalid). The raws are the CC0 Fujifilm files of the pinned corpus (`cargo xtask corpus
--download`, or only the `Fujifilm` entries of `RAW_SAMPLES` in `xtask/src/raw_samples.rs`): 38 usable scenes from about 25
bodies, X-Trans I to V.

`xtrans_bench` re-mosaics each scene under three illuminants (camera RGB = balanced ÷ gains) and three noise levels,
demosaics it with every variant and scores CPSNR and chroma RMSE in sRGB. 13 real scenes are held out for testing
(`TEST_REAL` in `main.rs`; near-duplicate pairs stay on one side); the other 25 train the filters. Scenes from
`scikit-image` (natural photos, test only) and four synthetic stress patterns are scored too.

Regenerate the filters (about a minute). Only variants that use the `prod` set train it, so `XB_ONLY` names
`proto` (which writes the filters) and `lib` (which scores the shipped path):

```
cargo run --release -p lightcraft-raw --example xtrans_truth -- corpus/raw <truth dir>
XB_AUG=8 XB_WRITE=crates/raw/src/demosaic/xtrans_taps.rs XB_ONLY=proto,lib \
  cargo run --release -p lightcraft-raw --example xtrans_bench -- <scene dir> <truth dir>
```

## Results

CPSNR dB ↑ / chroma RMSE ↓ (sRGB 8-bit levels), mean over three illuminants:

| | real clean | real ISO 6400 | photo clean | synthetic clean |
|---|---|---|---|---|
| bilinear | 33.93 / 6.57 | 29.43 / 11.21 | 33.33 / 7.95 | 25.49 / 40.1 |
| previous (directional) | 34.15 / 6.58 | 27.54 / 11.30 | 32.55 / 8.39 | 23.94 / 38.7 |
| **current** | **38.94 / 3.99** | **29.59 / 6.84** | **38.13 / 4.40** | **28.55 / 22.9** |

Time for a 40 MP RAF (7728 × 5152), 20 threads: bilinear 45 ms, directional 190 ms, current 550 ms (2.6 s on one
thread). Measured with `xtrans_raf_bench`, which also writes full-size JPEGs of each variant for visual comparison.

## What was tried and dropped

Measured on the same bench, each against the method above at the time:

- A 3 × 3 chroma median after demosaicing: +0.1–0.3 dB on its own, nothing on top of residual interpolation.
- Iterative green refinement from colour differences: helps synthetic scenes, costs 1–2 dB on photos.
- A second learned stage for red and blue (mosaic + green window): no better than residual interpolation, which is
  nonlinear in a way a per-class linear filter isn't.
- 16 angle bins, 9 × 9 filters: no gain. A cheaper first pass (3 × 3 green mean) for the classifier: −0.14 dB real,
  −1.1 dB synthetic.
- Training with noise mixed in: no measurable change.
- A 5 × 5 residual window: 10 % faster, −0.08 dB and more colour noise at high ISO.

Open: measuring against Lightroom's own X-Trans rendering; a lighter path for 1:1 previews if the extra 0.35 s shows.
