# Luminance NR: wavelet on linear Y

From process V4 (`docs/process-versions.md`) luminance noise reduction is `crates/pipeline/src/nrl.rs` on the
CPU and `crates/gpu/src/wgsl/nrl.wgsl` on the GPU (in bands of rows when the device memory is short). Edits on
V1 to V3 keep the guided filter on log luminance. Issue #1129 has the measurements and figures.

## Method

- **Linear Y**, five à-trous (B3) levels with holes of 1 to 16 px. On each level the detail `d` is shrunk:
  `keep = max(0, 1 − t² · gain_j · var(Y) / energy)`, the removed part summed into the change of Y.
- **Energy.** On the two fine levels: along each of eight directions (the axes, the diagonals and the four angles
  between, the latter with ±1 px across) the smaller `|d|` of the two neighbours `2^(j+1)` px away, the largest of
  the eight, squared and calibrated to white noise (`DIRECTIONAL_MEAN_SQUARE`). The pixel itself never counts, so a
  line or edge is supported along its own direction and an isolated speck is not. On the coarse levels a Gaussian
  mean of `d²`.
- **Noise model** (`nrl::stats`), measured on fixed tiles of the whole frame (`nrl::tiles`, the same for every
  window; the joint colour NR measures its noise model on the same tiles, made once per render): each level's energy in blocks, the low tail of the block energies (texture and edges only raise a block),
  `var(Y) = α·Y + β` fitted at level 0, each level's gain relative to it. Relative to white noise, a level may not
  carry more than all coarser ones (a texture bump) nor more than twice the next finer one (content at the coarse
  scales of a small preview).
- **Sliders.** Amount sets the threshold (`t² = 2a(1 − a/4)`), Detail scales it on the fine levels
  (1.25 → 0.75), Contrast on the coarse ones (1 → 0.2).
- **Applying.** The change of Y is added to R, G and B alike (`add_luminance`): chroma stays, black and negative
  pixels move, shadows are not pulled down. Where a channel would go below `min(v, 0)`, the part it cannot take
  goes to the other channels.
- **Order and cache.** It runs after the colour NR, which hands it the luminance of its result. The stage cache
  keeps the image before it and its statistics (`Plan::pre_key`), so its sliders do not redo the colour NR.

## Previews

Noise lives at the original's pixels (issue #960), so a preview at `scale` output pixels per original pixel
(`Plan::detail_scale`) filters its level `j` with the threshold of the original's level `j + log2(1 / scale)`,
interpolated between whole levels; a level coarser than the original's coarsest is left alone, as the full-size
render leaves it (`nrl::threshold`). Below a fifth of the original's size the thresholds fade out towards a
20th: the noise is mostly averaged away there, and what the flattest blocks measure is largely texture. A
preview is then as close to the full-size render scaled down as with the guided filter, relative to how much
each removes (`tests_nr_scale::wavelet_luminance_nr_preview_matches_full_size_scaled_down`: 0.31 of the
difference without noise reduction at 0.5, 0.38 at 0.33, never above 1).

## Local checks

```sh
cargo test --release -p lightcraft-pipeline --lib nrl::
cargo test --release -p lightcraft-pipeline --lib nrl::tests::quality_probe -- --ignored --nocapture
cargo test --release -p lightcraft-gpu --test equivalence
```

The quality probe prints the noise left on flat areas, lines at several angles, a ring, short segments, texture,
specks, dark and saturated colours and the slider curves.
