# Colour noise reduction V2: what was tried, measured and dropped

Prototype branch `nr-color-v2` for storytold/lightcraft#815 (dark-scene colour noise also in #820). This file
records every approach that was tried, including the ones that failed on quality or speed, so later work does
not repeat them. Method details of the current version: [`nr-color-joint.md`](nr-color-joint.md). All numbers
are our own measurements (October 2026); test files: the two ILCE-7M4 raws from #820 ("candle", ISO 3200;
"cat", ISO 25600) and own Sony α77 II / α99 II raws.

## Summary

| Version | Idea | Outcome |
|---|---|---|
| main (V1) | chroma as `rgb / Y`, plain Gaussian, mixed in by `t = amount·(1 − detail/2)` | weak at 25, colour fringes at 100, blows up near black |
| V2 "root" | `√(c+off) − √(Y+off)`, self-guided filter, ε from a global 20 % quantile | strong at 25, but **shifts dark-area colour** (clipping), loses edge chroma at ISO 25600, preview ≠ export |
| V2 "linear" | `c − Y`, additive, noise model `σ² = α·Y + β` | mean colour fixed, but **desaturated textured colour** (−48 % leaf chroma at 100) |
| **V2 "joint" (current)** | linear R−Y / B−Y, shared edge-aware weights, multi-scale diffusion with path maxima, measured noise | colour kept, no fringes; on GPU 0.2–0.4 s on a current card; known limits below |

## 1. V2 "root"

- Reviewer measurements (#815): blue −25…−45 % in dark patches already at 25, green/cyan cast at ISO 25600,
  up to −57 % chroma at colour edges at ISO 25600, the blue shift flips sign between 1600 px and full size.
- **Jensen correction** (add the removed variance back when squaring): only ~3 points better
  (−37.6 → −34.4 %). Not the main cause.
- **Float test without 8-bit clipping** (`probe_nrc_mean_drift`): a channel near 0 gains **+90 %**
  (0.00080 → 0.00152), almost exactly what rectifying negative noise gives (0.00165). Cause: `v.max(0.0)` in
  `nrc_chroma` and `.max(0.0)` in `nrc_recombine`.
- **Measurement pitfall:** comparisons against an 8-bit Color 0 render are biased, because that reference is
  itself clipped (a channel near 0 is lifted by rectified noise). Re-rendering at +2.5 EV does not remove the
  bias (noise and mean scale together). Check means in float.

## 2. V2 "linear" (`c − Y`)

Mean kept within 0.05 % in float (3 seeds × 2 patches × 2 strengths). Noise model fitted per channel on
equal-count luminance bands with a truly non-negative fit (`fit_nonneg_line`). Synthetic checks
(`probe_nrc_real_colour`): an isoluminant red/green edge, a 2 px red line on grey, saturated blue near black,
a flat grey patch, later a bright leaf next to black.

| Tried | Result |
|---|---|
| per channel, K 16 | dark blue 99 % kept (root: 69 %), but the thin line only 27 % (root: 78 %); compare only at equal residual noise |
| **shared weights** (one `a` for all channels) | **worse** than per channel: edge error 12.7 % vs 9.6 %, line 52 % vs 59 % at equal noise |
| luminance guide in the shared mode | no measurable effect (the test line is isoluminant on purpose) |
| coarse pass with the same K | destroys thin lines (94 % without coarse pass, but too much residual noise) |
| coarse pass with K × 0.1 | synthetic fine, **real ISO 25600 file far too weak** (spread 13.3 vs 6.0): real noise is blotchy after demosaicing |
| coarse pass smoothing only the coarse part | line survives (75–86 %); the cat needs K 128 to match root V2, then the line drops to 37 % |
| **per channel, K 64, coarse-only (shipped in a test package)** | **failed:** lit leaves at Color 100 keep only **52 %** of their chroma (V1 96 %, root 98 %); the synthetic leaf test (95 %) did **not** catch it |
| luminance guide per channel | leaves 92 %, but flat dark areas less smoothed (cat 10.6 vs 6.0): luminance noise counted as structure |

Pitfalls of this phase: flat-patch selection used interleaved columns instead of spatial quadrants (fixed);
environment knobs read per pixel made the linear mode 11× slower (fixed: read once); a CLI not rebuilt after
changing defaults measured the old mode; renders without the #858 fix showed white dots in every variant;
background load from other jobs made single timings useless (compare in one process, interleaved).

## 3. V2 "joint" (current)

- Results at Color 25 / 100 (cat: spread / blotch in flat dark patches; candle: chroma of the lit leaves):

  | | Cat 25 | Cat 100 | Leaves 25 / 100 |
  |---|---|---|---|
  | V1 | 14.8 / 5.5 | – | 100 % / 96 % |
  | root V2 | 5.1 / 1.8 | 3.4 / 0.6 | 99 % / 97 % |
  | **joint** | 6.5 / 2.8 | 6.1 / 2.4 | 97 % / 97 % |

- Dark-area mean colour against the Color 0 render (8-bit, see the pitfall above): cat R −14 % at 25 and 100
  (V1 −13 % at 25, −51 % at 100; root V2 −43 %).
- `tests_window::noise_reduction_does_not_depend_on_the_window` passes (root and linear V2 failed it).

### Known limits

- Specks that the speck brake leaves (section 3a): ones with their own luminance step (they stay as
  grey or bright dots, as in V1) and, at Color 25, all but the strongest (on purpose, see below).
- Flat dark areas are smoothed somewhat less than by root V2 (cat spread 6.5 vs 5.1 at 25).
- The Color slider changes little above 25 (the blend is complete at 25; only the reach grows). To us 25 already
  removes the visible colour noise.
- Prototype switches (`NRV2_*`, `LIGHTCRAFT_NR_V2`) instead of a process version; debug output; a test-only copy
  of an earlier version (`nrc_joint_reference.rs`) for bit-exactness checks.

## 3a. Speck brake

Isolated, strongly coloured pixels in dark areas (a red, a blue speck) were treated as real detail by the
edge-aware weights and stayed visible at Color 100, while V1's blind blur largely removed them. Measured on
dumps of the transformed image (`NRV2_DUMP`) of the two #820 files:

- The specks are not single pixels at ISO 25600: demosaicing spreads them over 2 × 2 to 3 × 3 pixels. A first
  test "no direct neighbour shares the colour shift" therefore protected them; the brake now asks whether the
  shift reaches the 16-pixel border of the 5 × 5 square (any thin line or edge through the pixel does).
- In the centre of the remaining cat specks the chroma excess is 250–1500 noise variances, the luminance step
  0.02–0.17 of it, the border share ≤ 0.4: all three tests pass clearly. What stays visible afterwards is a grey
  or bright dot (luminance noise, as in V1).
- Real noise has far heavier tails than a Gaussian: 570 000 pixels of the candle exceed 36 (a Gaussian: 0.5).
  A threshold of 12 at Color 100 also moved the mean of a synthetic flat patch (2.2e-6, the test allows 2e-6);
  now 64 at 25 and 25 at 100, at most half the colour replaced at 25.

- First compactness rule "the largest border share stays below 0.6": one noisy border pixel leaning the same
  way protected a speck (the three dots below a lit leaf tip in the candle file: border shares 0.44 and 0.71
  kept two of them). Now the **second** largest share decides (0.5 → full brake, 0.8 → none): a line or an
  edge through the pixel always crosses the border at least twice.

| Full-size, 8-bit, against the same build without the brake | Joint | with brake |
|---|---|---|
| Leaves (candle), C* at 25 / 100 | 97 % / 97 % | 97 % / 98 % |
| Cat dark mean R / G / B at 100 | −14.1 / +1.2 / +6.7 % | −11.9 / +2.0 / +2.9 % |
| Candle dark mean B at 25 / 100 | −26.1 / −26.0 % | −26.4 / −29.8 % |
| Cat flat dark spread / blotch at 25 | 6.46 / 2.78 | 5.81 / 2.57 |
| Cat flat dark spread / blotch at 100 | 6.11 / 2.37 | 4.17 / 1.47 |
| Candle flat dark spread / blotch at 100 | 4.67 / 3.13 | 4.10 / 2.97 |

Crops of the 10 most visible specks per file: blue specks on black become grey at 100, a magenta speck in
orange almost disappears, the three dots below the candle's leaf tip go except for faint dark traces
(luminance); at 25 they are only weakened. The largest changes in bright areas are specks too
(pink dots in the cat's blue, yellow dots in cream); no edge or line changed visibly. Synthetic test
`speck_brake_removes_compact_colour_specks_but_not_stars`: an isoluminant 2 × 2 speck loses most of its colour
at 100, less at 25; a coloured 2 × 2 star keeps its colour within 2 %.

Cost (machine under heavy load, so rough): CPU 24 MP synthetic 0.54 s against 4.1 s for the rest of the filter
(a sort per median: 1.8 s; now a sorting network and the luminance median only past the threshold). GPU
(Intel HD 630, 33 MP, white balance + NR stage): median 6.19 s with, 5.61 s without. GPU and CPU agree to 1/255
on the cat; on the candle 26 pixels differ by up to 30/255 with and without the brake (not caused by it).

## 4. Speed

CPU, all bit-identical to the first joint version (`8b89526d`):

| Change | Result |
|---|---|
| no duplicate distance pass, paths + next weights in one pass, none after the last scale, two-channel buffers, parallel reconstruction | 6.2 → 5.0 s (33 MP), buffers 2.36 → 1.44 GB |
| separate H/V slice loops, zero-difference shortcut (no division, weight 1 without `exp`) | 24 MP synthetic 4.5 → 2.7 s |
| **cache-blocked H→V→H→V per band** (bands 4–256 rows, deltas up to 2/4/8) | **no gain** (2.77–3.22 s), slower at delta 8; dropped |
| remaining cost | next-scale weights (`exp` + distance), hard to reduce without changing results |

GPU (`crates/gpu/src/wgsl/nrcj.wgsl`, kernels mirror the CPU steps, the noise model is fitted on the CPU from
GPU samples; at most 1/255 from the CPU):

| Tried | Result |
|---|---|
| a new path/weight buffer per scale, only `flush` | out of device memory, whole render fell back to the CPU |
| two ping-pong buffers + `flush` | still out of memory: **3950 MiB real use** (nvidia-smi) while lightcraft counted 1.9 GB – the driver frees dropped buffers only when the GPU is done with them |
| **`sync` after large drops** | fixes it (3.27 GB one piece, 2.4 GB in bands) |
| bands with halo 4·Σδ + 8 | correct but slow at small budgets; now 2·Σδ + δmax + 8 (bands vs one piece: ≤ 1/255 in a few dozen pixels, from the box blur's running sums) |
| band budget | free device memory where it can be asked without `unsafe` (nvidia-smi; amdgpu sysfs) + own pooled buffers − 384 MB, else 2 GB |

Measured (white balance + noise reduction stage on the GPU, median of 3; CPU: colour NR only):

| | 24 MP | 33 MP | 43 MP | preview 2560 px |
|---|---|---|---|---|
| Radeon RX 9070 XT (Windows) | 0.19 s | 0.28 s | 0.34 s | 55 ms |
| Quadro M2200 4 GB (Linux) | 1.07 s | 1.54 s | 2.01 s | 0.2 s |
| Intel HD 630 | 3.0 s | 4.3 s | 5.6 s | |
| CPU Ryzen 7 5800X3D | 1.5 s | 2.3 s | 2.9 s | |
| CPU i7-7820HQ | 2.6 s | 3.7 s | 4.8 s | |

V1 on the same Radeon: 0.08–0.12 s. A Windows test build made with MinGW was ~3× slower than Linux in the
camera-colour step (not in the noise reduction); the official MSVC release still has to be compared.

### Second round (after the speck brake; 33 MP candle at Color 100, median of 3–6, interleaved)

| Change | Result |
|---|---|
| guide as three planes, distances one row at a time without branches (the compiler vectorises the three divisions) | CPU next-scale weights ~360 → ~190 ms per scale; bit for bit (test `distance_run_matches_distance`) |
| paths stored as exp(−path): longer paths take minima, a new distance of 0 needs no `exp` (exp is monotonic, so bit for bit) | alone no measurable gain on the CPU (distance, not `exp`, was the cost); kept, it halves the GPU path buffers |
| pilot straight into the planes; speck brake in place (it changes < 1 % of pixels) with a min/max pre-test before the medians | brake 492 → 252 ms; pilot + planes 713 → 568 ms |
| vertical diffusion in one loop, horizontal interior split from the borders | **no gain** (3.59 vs 3.54 s): the diffusion is memory-bound (~24 bytes per pixel and pass); dropped |
| skipping the `exp` also where the new distance is below the path's | counted: only 2–24 % of pairs beyond the 40–67 % with distance 0, ~15 ms per scale for one more buffer; not done |
| GPU: separate path and weight pairs (2 + 2 instead of two 4-value quads), next paths into the free scratch buffer | 60 → 48 bytes per pixel planned; 33 MP fits in one piece in 2 GB (before: two bands with 328-row halos) |
| GPU: medians and pilot without private arrays indexed in loops (those go to slow local memory) | speck brake on the Quadro 0.33 → 0.05 s; pilot ~2 % |
| GPU: dropping the `sync` calls | Quadro: out of device memory (4032 of 4096 MiB), CPU fallback; kept |
| noise model: fit cached for identical samples (moving a slider); compact key sort | fit 13–37 ms (noisy); helps repeated previews only |

Totals, white balance + NR stage (GPU) / colour NR (CPU), before → after this round:

| | full 33 MP | preview 2560 px |
|---|---|---|
| Quadro M2200 | 2.15 → 1.47 s | 323 → 248 ms |
| Intel HD 630 | 5.66 → 4.26 s | 818 → 776 ms |
| CPU i7-7820HQ | 5.17 → 3.54 s | |

CPU output is bit for bit the same as before; GPU output agrees with the previous GPU build to 1/255.

## 5. Not part of this work

- White dots near black in #820 came from the camera look's hue/saturation table, fixed by #858.
- A raw-level hot pixel filter was prototyped on a separate branch; the #820 dots were not hot pixels (their
  positions don't repeat between frames), so it is not part of this change.
