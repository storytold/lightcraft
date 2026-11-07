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
| band budget | free device memory where it can be asked without `unsafe` (nvidia-smi; amdgpu sysfs) + own pooled buffers − 384 MB, else 2 GB; integrated GPUs at most 2 GB, less where the system has less available (`MemAvailable` on Linux, − 384 MB; an AMD APU's sysfs counts only its carve-out, see the third round) |

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

### Third round (Ryzen 5 7545U with its Radeon 740M iGPU, Linux; 24 MP Z f NEF at ISO 6400, raw default Color 25)

| Change | Result |
|---|---|
| CPU: each scale's four diffusion steps in one sweep (`diffuse_scale`): strips of rows, worked in tiles of 512 columns whose rows pass through two rings of `2·delta + 1` rows that stay in the core's cache; the halo (2·delta each side) is computed twice | diffusion 713 → ~210 ms over the six scales; bit for bit (`diffuse_scale_matches_four_passes`). Unlike the row bands tried before, the column tiles keep the rings small enough for the cache at delta 32 |
| CPU: horizontal step without branches in its interior columns | part of the above (66–139 → 25–53 ms per scale) |
| CPU: the pilot's blur from its own running sums (`PilotBlur`: σ 0.8 is one 3-px box per axis at any image size), no blurred copy of the image | pilot 305 → 70 ms; bit for bit with `gaussian` (`pilot_matches_the_gaussian`) |
| CPU: speck brake reads the rest of a pixel's border only once the ring makes it a candidate | brake 81 → 70 ms (synthetic); bit for bit (`despeck_matches_the_whole_border_version`) |
| CPU: next-scale weights without branches; skipping `exp` where the result is certainly 0 | no measurable gain: these passes are memory-bound (~36 bytes per pixel), not `exp`-bound; the second dropped |
| GPU: `nrcj_pilot` sums the 3-px box itself instead of reading a two-pass `gaussian` in a new buffer | blur + pilot 70 → 22 ms |
| GPU: `nrcj_dvh`, the middle vertical + horizontal step of each scale in one dispatch (a row segment and its reach in workgroup memory) | diffusion 251 → 203 ms; same result as the two kernels (`fused_diffusion_steps_match_the_separate_ones`) |
| GPU: integrated GPUs plan with up to the fixed 2 GB, less where the system has less available (`vram::free_bytes` answers with `MemAvailable` for them on Linux, not sysfs) | the APU's sysfs reported its 2 GB carve-out less what the render held: 468 MB, so 16 bands of 267 kept rows in 603 (2.3× the work, a `sync` each). Now one piece: WB + NR stage 931 → 560 ms on its own. Overdrawing shared memory can bring the kernel's out-of-memory killer rather than an error to fall back from, so the plan does not assume the 2 GB is there |

Totals (median of 3, JPEG export of the whole photo; main = the colour NR of processes V1 to V5, measured on main of 2 November):

| | main | before this round | after |
|---|---|---|---|
| CPU, colour NR (`nr joint`) | 330 ms (V1) | 1276 ms | ~780 ms |
| CPU, WB + NR stage | 574 ms | 1581 ms | 1073 ms |
| iGPU, WB + NR stage | 159 ms | 931 ms | 491 ms |
| CPU, whole export | 2.35 s | 3.34 s | 2.89 s |
| iGPU, whole export | 1.76 s | 2.49 s | 2.10 s |

An X-T2 RAF at ISO 6400: CPU 2.25 / 3.21 / 2.74 s, iGPU 1.68 / 2.33 / 1.98 s. CPU output is bit for bit the same as
before this round; GPU output within 1/255 of it on all 74 renders of the comparison set (mean difference 0.00).

What is left on the CPU is mostly memory traffic: six scales of next-scale weights (~50 ms each: paths, links and the
guide's three planes through memory), the diffusion sweeps, and the noise model of the frame's tiles (55 ms).
Computing each scale's weights inside its diffusion tile (the paths are sliding minima of the direct neighbours'
weights, so they could be made there) would save some of that traffic at the cost of recomputing the halo.

### Fourth round (Apple M2 Max: 12-core CPU, 30-core GPU through Metal, 32 GB; same Z f NEF and X-T2 RAF)

On this machine the third round's gains were smaller (colour NR −21 % on the CPU, GPU stage −14 %, GPU export
0.88 → 0.87 s); the integrated-GPU budget change does nothing here (one band under the fixed 2 GB already). Two
things cost more here than on the Ryzen: the first write to newly allocated memory, and mispredicted branches
(the CPU is not memory-bound: `links` scales almost linearly with cores, ~16 ns a pixel on one).

| Change | Result |
|---|---|
| GPU: buffers handed on instead of dropped and allocated again (the transformed band before the speck brake takes the guide, the transformed band the paths), and one band is written over the image (`nrcj_recon_in`; no whole-image output, and strength 0 returns the image without a copy). A kernel's first write to a new 290 MB buffer took 12 ms, the same kernel on a used one 2 ms (an explicit clear of a new buffer: 10 ms) | WB + NR stage 191 → 155 ms; device memory peak 1860 → 1674 MB. Same words (`one_band_matches_several` checks one band against several) |
| CPU: `path_weight` without its branch (exp(−0) is exactly 1 and paths are at most 1, so min(exp(−d), path) is the path at d = 0) | links at delta 2–32: 24 → 18 ms each; bit for bit |
| CPU: `weight` (direct neighbours) without its branch | slower (links 1: 41 → 46 ms): there a distance of 0 is common and predictable; kept |
| CPU: diffusion tiles in parallel as well as strips (16 strips at delta 32 for 12 cores), and taller strips | 3–4 ms over the six scales; dropped |
| CPU: new images (14 values a pixel, ~1.4 GB on 24 MP) cost their page faults: touching `paths` and `links` before `links 1` took 27 ms and `links 1` fell from 41 to 21 ms | not changed: the only buffer that could be recycled (the transformed image, as the paths) saves ~11 ms and needs casts between pixel types |

Totals (median of 5, interleaved; WB + NR stage and colour NR median of 3):

| | before this round | after |
|---|---|---|
| CPU, colour NR (`nr joint`), NEF / RAF | 337 / 335 ms | 311 / 310 ms |
| GPU, WB + NR stage, NEF / RAF | 191 / 190 ms | 155 / 156 ms |
| CPU, whole export, NEF / RAF | 1.22 / 1.17 s | 1.20 / 1.13 s |
| GPU, whole export, NEF / RAF | 0.88 / 0.81 s | 0.84 / 0.78 s |

On the Ryzen 5 7545U / Radeon 740M (third round's machine, builds merged onto main of 2 November) the same two changes
give less but no loss on any of the three photos tried (ISO 6400 NEF and RAF, ISO 200 NEF): colour NR 793 → 757 ms,
iGPU WB + NR stage 492 → 468 ms, whole export 2.92 → 2.90 s (CPU) and 2.07 → 2.02 s (iGPU) for the NEF.

CPU and GPU JPEGs byte-identical to before this round, on both machines. In the app the GPU's first-write cost is mostly
paid once (the device buffer pool keeps buffers between renders for reuse); the CPU's images are
allocated anew for every render.

### Both rounds on main at `d2bf0810` (Ryzen 5 7545U / Radeon 740M, Linux, 6 November)

Before and after both rounds, process V6, raw default; median of 7 interleaved runs. Stage times come from runs with
`LIGHTCRAFT_PROFILE=1`, which itself adds over a second to an export, so exports were timed in separate runs without it.

| | | Z f NEF, ISO 6400 | X-T2 RAF, ISO 6400 | Z f NEF, ISO 200 |
|---|---|---:|---:|---:|
| CPU, colour NR (`nr joint`), ms | before | 1299 | 1270 | 1358 |
| | after | 764 | 748 | 826 |
| CPU, whole export, s | before | 3.43 | 3.39 | 4.21 |
| | after | 2.90 | 2.86 | 3.69 |
| iGPU, WB + NR stage, ms | before | 859 | 821 | 706 |
| | after | 469 | 470 | 465 |
| iGPU, whole export, s | before | 2.45 | 2.42 | 3.03 |
| | after | 2.11 | 2.13 | 2.85 |

On this iGPU peak device memory rises from about 1270 to 1670 MB: the stage now runs in one piece (see the third
round's budget row), where it ran in 16 bands.

Over 74 full-size renders (seven photos, ten settings, NR off to Color 100): CPU PNGs byte-identical; iGPU PNGs at most
1/255 apart, in at most 0.025 % of a photo's pixels.

## 5. Not part of this work

- White dots near black in #820 came from the camera look's hue/saturation table, fixed by #858.
- A raw-level hot pixel filter was prototyped on a separate branch; the #820 dots were not hot pixels (their
  positions don't repeat between frames), so it is not part of this change.
