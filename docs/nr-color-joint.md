# Colour NR: joint guide

From process V3 (`docs/process-versions.md`) colour noise reduction uses this method on the CPU
(`crates/pipeline/src/nrc_joint.rs`) and the GPU (`crates/gpu/src/wgsl/nrcj.wgsl`, in bands of rows
whose halo covers the filter's reach). Edits on V1 and V2 keep the blurred chromaticity. The history
of the method, including the square-root and per-channel variants it replaced, is in
`docs/nr-color-v2-history.md`.

## Method

- Filter signed, linear `R-Y` and `B-Y`, with original Rec.2020 luminance restored at each
  pixel. No channel clipping, root transform or Jensen correction is used in this filter.
- Estimate Y/chroma noise from median squared high-pass residuals at spacings 1, 2 and 4.
  Larger spacings account for positive correlations from demosaicing. Fit equal-count
  luminance bands with nonnegative coefficients, reject texture-dominated outliers and
  stop extrapolating beyond the bands supported by usable noise measurements.
- Speck brake (before the guide): a pixel whose chroma stands out from the median of the 8
  pixels at distance 2 by more than a threshold (in noise variances), whose luminance step is
  small against that colour step, and whose colour shift does not reach the 16-pixel border of
  its 5 × 5 square at two places (a line or an edge through it always does) gets its chroma moved
  towards that median.
  Threshold 64 and at most half at Color 25, threshold 25 and fully at 100. Luminance is never
  changed, so a bright point (a star) keeps its brightness and, by the luminance test, its colour.
- Make a fixed, lightly denoised guide. At a clear directional edge, average along the
  smoother axis; this keeps thin isoluminant lines visible to the guide.
- Apply conservative, symmetric colour exchanges at increasing spatial scales. The two
  chroma components have identical neighbour weights. Subtract the expected noise from
  guide distances; luminance texture cannot simply increase the raw-pixel retention factor.
  Long taps also check the guide edges between their endpoints.
- Symmetric exchanges preserve channel sums on the full-resolution grid, including its
  borders. This does not promise identical means inside arbitrary cropped regions.
- Colour amount controls the radius and ramps the filtered contribution continuously from
  zero to the normal default of 25. Colour Detail changes edge tolerance; Smoothness changes
  radius (`local::nrc_params`: the coarse reach is three times `(2 + 10 · Color) · (1 + Smoothness)`
  px at full size, scaled with the output).
- Windowed 8-bit renders request extra surrounding pixels for the diffusion's spatial
  dependencies. The existing window/full-frame comparison retains its original tolerance.
- The noise model belongs to the whole frame, not to the pixels a render holds
  (`lightcraft_pipeline::colour_noise`). A frame larger than 12 × 12 tiles of 128 px is measured
  on the tiles the luminance NR's statistics use (`nrl::tiles`, made once for both), resampled,
  tone-mapped, white-balanced and, before process V4, luminance-denoised like the frame (retouching
  and defringe are left out); a smaller frame rendered whole is measured on itself, a window of it on
  the whole frame. So a loupe window is filtered like the same pixels of the full render or the export
  (`joint_colour_nr_measures_the_noise_of_the_whole_frame`). The model is kept in the stage cache:
  moving a colour slider does not measure again.
- The stage cache keys the denoised image (and the image before the luminance NR) by the filters in
  use, so a change of process (Update to Current Process, undo) never reuses another filter's result
  (`a_process_change_does_not_reuse_the_other_noise_filters`).

## Local checks

```sh
cargo test --release -p lightcraft-pipeline --lib nrc_joint
cargo test --release -p lightcraft-pipeline --lib probe_nrc -- --ignored --nocapture
cargo test --release -p lightcraft-gpu --test equivalence
LIGHTCRAFT_GPU=0 cargo run --release -p lightcraft-cli -- render INPUT.ARW -o OUTPUT.png --set detail.nrColor=100
```

`LIGHTCRAFT_PROFILE=1` prints the filter's steps and times and the fitted Y/R-Y/B-Y model.

The regression tests cover signed dark means, luminance, chroma-noise reduction, leaf/black and
isoluminant edges, a two-pixel isoluminant line, structured highlights, spatially correlated noise,
the speck brake (specks removed, stars kept) and the routing by process (`colour_nr_follows_the_process`).
`nrc_joint_reference.rs` keeps an earlier version of the filter for bit-for-bit checks of later speedups.
