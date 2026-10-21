# Photo Merge (HDR, Panorama, HDR Panorama, Focus Stack)

`crates/merge` (layer L3, no UI dependencies) implements the algorithms; `crates/engine/src/merge.rs`
plans, runs and finishes merges in the library; the desktop UI shows a dialog with a live preview.
All algorithms are our own implementations from published papers (cited in the module docs).

## Using it

- **App:** select the photos, then *Photo Merge ▸ HDR…* (`Ctrl+H`), *Panorama…* (`Ctrl+M`),
  *HDR Panorama…* or *Focus Stack…* from the photo context menu (or the `dialog.merge*` commands). The dialog previews
  the merge at ≤ 1024 px and re-runs the preview when an option changes; *Merge* runs the full merge
  in the background (a row in the activity stack shows its progress, with ✕ to stop it). The result is written next to the first photo as
  `<name>-HDR.dng`, `-Pano.dng`, `-HDR-Pano.dng` or `-Focus.dng`, imported and selected. *Photo Merge ▸ HDR for Each Bracket Set*
  merges many brackets in one go with the last HDR options: one HDR per stack among the selected photos, then per
  bracket (photos taken less than a second apart, after the shutter time, at exposures at least ½ EV apart; a burst at
  one exposure is skipped), one after the other in the activity stack.
- **Commands** (CLI, MCP, control channel):
  - `merge.hdr {ids?, align=true, deghost=none|low|medium|high, autoSettings=true, stack=false, preview=false, showOverlay=false, previewPath?}`
  - `merge.panorama {ids?, projection=auto|spherical|cylindrical|perspective, boundaryWarp=0..100, autoCrop=false, fillEdges=false, autoSettings=true, maxMegapixels (default: the photos' own resolution), preview, previewPath?}`
  - `merge.hdrPanorama {ids?, bracket=0 (from EXIF), …both sets of options}`
  - `merge.hdrBatch {ids?, align, deghost, autoSettings, stack}` → `{merged: [{id, path, …}], failed: [{photos, error}]}`
  - `merge.focusStack {ids?, align=true, autoSettings=true, stack=false, preview=false, previewPath?}`
- **CLI:** `lightcraft-cli merge hdr|panorama|hdr-panorama|hdr-batch|focus [options] FILES…` (`hdr-batch` finds the
  brackets among the files, e.g. a whole folder);
  `lightcraft-cli synth-merge hdr|panorama|focus -o DIR` writes synthetic inputs (procedural scene).

## What it does

**HDR** (`hdr.rs`): frames ordered by EXIF exposure (or median luminance); the middle one is the
reference. Auto Align: features on exposure-normalised luminance at ≤ 1200 px, RANSAC translation / similarity /
homography (the simplest model that explains ≥ 97 % of the matches), then refined at full size (`refine.rs`): 48 px
patches at the most textured spot of each of ~24 cells along the long edge, matched by normalised cross-correlation
over the pixels usable in both frames (clipped ones aren't) with a sub-pixel peak, and the model fitted again at
0.75 px. A frame the small versions can't align at all (a bright frame mostly clipped) is recovered the same way
from no alignment, searching ±16 px. What one model leaves (parallax between near and far things when the camera
moved, lens distortion, rolling shutter) is corrected tile by tile (`flow.rs`): ~96 tiles along the long edge, each
matched within ±5 px, outliers against their neighbours' median dropped, gaps filled, the field smoothed; each
frame is resampled once through the homography plus the local shift. Where the reference is well exposed the
merge's finest detail (below a σ = 2 px blur, on luminance) is the reference's own, the one frame neither resampled
nor aligned to only a fraction of a pixel; it fades out towards the reference's clip and its shadows, where other
frames carry cleaner detail. Frames are resampled with Lanczos-3, held to
the range of their 4 × 4 neighbours so edges don't ring (`lightcraft_raster::sample`); bilinear lost a third of the
finest detail. Exposure ratios are measured
(median log ratio of well-exposed pixels between brightness neighbours), EXIF is the fallback.
Radiance is a weighted mean in linear light, weight `k · (1 − smoothstep(0.8·clip, 0.96·clip, max
channel within 1 px))`: a pixel next to clipped photosites was demosaiced from them and reads low in the clipped
channel, so it goes with them (on real ARW brackets the brightest frame's green was 3–5 % low there, and bright
windows took a magenta tinge). The frames are taken in brightness order (ties by content), so the result doesn't
depend on the order they come in. Where the reference is at or near its clip the scene is at least that bright, so a frame that reads far
darker there (an object moved: a star, the moon, a passing light) is left out and the clipped highlight stays (always
on: a moving star otherwise left a black hole). Deghost: a per-pixel consistency test on ≤ 2048 px versions against the
reference radiance (with a noise floor; the reference predicts down to 0.1 % of its clip, so a brighter frame's ghost in
a night sky is caught), dilated and feathered masks; the union is the deghost overlay. Output: 16-bit float DNG.
Lens corrections: frames are merged uncorrected, and the DNG's `OpcodeList3` carries the reference frame's
(`WarpRectilinear`, `WarpRectilinear2`, `FixVignetteRadial`: a DNG's own, or a camera's correction a raw reader
converts, e.g. Sony, Panasonic), re-expressed for its default crop (`Opcode::cropped`). So Optics ▸ Enable Lens
Corrections works on the result as on the sources; a vertical bracket keeps its pixels unturned and its
corrections.

**Panorama** (`pano/`): MOPS-style features, pairwise RANSAC homographies with Brown & Lowe's
verification, focal from homographies (or EXIF 35 mm), rotations chained along a maximum spanning
tree, Levenberg–Marquardt bundle adjustment of rotations + focal lengths, straightening.
Projections: spherical, cylindrical, perspective; *Auto* picks perspective for compact fields of
view < 110°, cylindrical when the vertical field is < 90°, else spherical. Exposure: EXIF, then
gain compensation from the overlaps. Seams: each pixel goes to the image with the highest
centre-weighted feather weight; multi-band blending (Laplacian pyramids) in log space. Boundary
Warp: a separable stretch of the smoothed valid-area edges to the canvas edges. Auto Crop: the
largest rectangle of image data (set as the develop crop, so it stays adjustable). Fill Edges:
push–pull diffusion. Output: 16-bit integer DNG (16-bit float for HDR panoramas).

**Focus Stack** (`focus.rs`, beyond Lightroom): the photos are put in shooting order (capture time,
then file name), since each is aligned to its neighbour. Full-size frames are never all held at
once: each is read twice. First small (≤ 1200 px; raws binned straight from the mosaic) versions,
in parallel: features, then each frame aligned to its neighbour with a similarity (focusing changes
the magnification; a homography fitted to features crowded into the sharp slice of the scene
extrapolates badly) and the steps chained. A frame whose step finds no fit is aligned to one of
the three frames before it instead; a frame that aligns to none of them is left out and reported
(`skipped`), as is any frame not chained to the largest group. The frame that sees the least of
the scene (the most magnified) becomes the reference, so every other frame covers it. Brightness:
each frame against the one it is chained to, as the ratio of their luminance thumbnails' sums over
the pixels both see away from clipping (sums, because blur keeps them while per-pixel ratios of
differently blurred frames lean one way); steps under 0.05 EV count as none, so measuring noise
can't wander over a long stack; EXIF only when the pixels can't tell. Measured, not read from
EXIF: on a Sony A7R VI, auto ISO's 160 → 125 left the raw data as bright. Then full size, one frame
at a time, outwards from the reference: the chained alignment refined at full size against the neighbour already
aligned (`refine.rs`, similarity only, ±8 px), brought to the reference's brightness, resampled (Lanczos-3) into its
geometry straight onto the fusion canvas, decomposed into a Laplacian pyramid in square-root space. Every band is the mean of
the frames' bands, weighted by their energy `e`: that of the band's luminance summed over a
Gaussian window (σ = 3 level pixels) wider than a defocused edge spreads (else the blurred frame
wins just beside every sharp edge: a halo), against the band's noise floor `ε` (the 10th
percentile of the first frame's energy). A frame weighs 1 while `e ≤ 2ε` and `(1 + e/ε − 2)¹⁶`
above (the sums kept relative to the largest weight so far, so the power can't overflow): where
frames carry detail the sharpest dominates (Burt & Kolczynski's max-activity rule, made steep and
soft); where none carries more than noise they average, so out-of-focus areas get quieter instead
of keeping every frame's noise peaks. The coarsest level is the frames' average. Output: 16-bit integer DNG with the reference frame's colour, metadata and lens
corrections (colour tags are fitted for the reference only).

**DNG output** (`output.rs`): LinearRaw, 3 samples, Deflate tiles; values scaled below 1.0 with the
scale in `BaselineExposure`; raw sources keep their DNG colour tags (camera RGB), other sources get
`ColorMatrix1`/`ForwardMatrix1` of their RGB space.

## Measured (synthetic tests, `cargo test -p lightcraft-merge`)

- HDR, 3 brackets ±2 EV with 14-bit noise and handheld shifts: alignment error < 0.3 px, exposure
  error < 0.02 EV, radiance median log error < 0.03 (noise-limited, bias < 0.01), recovered
  highlights < 0.05; moving object: ghost error 0.2–2.0 without deghosting, < 0.06 with High; a star that moves
  across a night sky keeps its clipped core and, with High, leaves no ghost of the brightest frame.
- HDR at full size (`tests/sharpness.rs`): three 3000 × 2000 frames of an analytic scene (texture up to 0.35 cycles
  per pixel), each turned ≤ 0.11° and moved by a fraction of a pixel, the brightest over half clipped. Alignment
  corner error 0.014 and 0.11 px, 98.8 % of the scene's detail kept; before the refinement and the Lanczos-3 warp
  0.43 and 6.2 px (the bright frame wasn't aligned) and 86 %. Measured with `metrics.rs` (`detail`, `detail_ratio`,
  `detail_gain`, `residual`).
- Real handheld bracket (Sony, 33 MP, gravel a few metres away, a building ~1 km off; `tests/real_bracket.rs`, run
  by hand): the global fit, dominated by the foreground, left the building 2.3 px off (median; 95th percentile
  3.0 px) and the merge doubled its inscription; with local alignment 0.20 px (0.43 px), the whole frame 0.13 px.
  With the reference's detail the merged building is as crisp as the single raw (by eye; it keeps less of the raw's
  grain). Synthetic, other frames softened σ = 0.7 px: 66 % of the scene's detail kept before, 97 % with it.
- Real brackets (Sony A7R VI, 67 MP, 2026-10-14): frames agree with the merged radiance within ~1 % from 3 to 90 % of
  their clip in all three channels; measured exposure steps match EXIF within 0.03 EV.
- Panorama, 5 views over 182° with a 1.3× exposure drift: relative rotations < 0.1°, focal < 1 %,
  BA RMS ≈ 0.35 px; median log error vs ground truth < 0.02; no seam steps.
- Focus stack, 6 frames 640 × 420 of a scene whose depth runs across the frame (blur up to σ = 10 px,
  0.6 % focus breathing per frame): corner alignment error 0.4–1.35 px in either order, < 2 px
  from half-size alignment versions; the worst tenth of the frame errs 0.057 (mean absolute,
  linear) against 0.103 for the best single frame; frames darkened ½ EV each are measured within
  0.04 EV, without EXIF or with wrong EXIF; a featureless frame is left out and bridged over;
  noise in equally blurred areas drops to 0.36× a single frame's (8 frames).
- Focus stack at full size (`tests/focus_sharpness.rs`): 9 frames 3200 × 2000 of an analytic scene (waves up to
  0.33 cycles per pixel and Gaussian blobs, whose defocus is exact; σ up to 6 px; 0.4 % breathing per frame; noise),
  aligned on versions 8× smaller as a 61 MP raw's are. Before: corner errors up to 1.9 px (the chain), texture
  56 / 72 / 73 % (high / mid / low contrast) of an ideal stack's (each pixel from its best-focused frame, noiseless);
  with the Lanczos-3 warp alone (versions 2.7× smaller) 78.5 → 98 %. Now: 0.03 px, 98.3 / 98.5 / 99 %.
- Focus stack, edges: a textured disc near the camera in front of a textured background (5 frames, defocus as blurred
  layers, so a background-focused frame shows the disc's blur spilling over the background). Around the disc's edge
  the low-frequency error is 0.0025 (√ units) and 88 % of the texture is kept (100 % away from the edge). Measured
  worse and not kept: one sharpness map per frame used for every band (a depth map: 0.0063 and 84 %, 1.7× slower),
  and the activity window at σ = 2 or 5 (within noise of 3).
- Focus stack, real: every 2nd frame (88) of a Sony A7R VI macro bracket (FE 100 mm, f/5.6), a
  2400 × 1500 crop of petal edges and out-of-focus background (a throwaway harness):
  background noise (8-bit high-pass) 0.39 / 0.33 / 0.56 (R / G / B) against 1.7 / 1.3 / 2.6 for a
  single frame and 1.84 / 1.38 / 2.92 for a maximum selection; petal texture (band-pass, noise
  discounted) 95–100 % of the maximum selection's.
- Focus stack, real, full size: all 175 frames of that bracket (67 MP lossless ARW) in 6 min 58 s
  wall / 1388 s CPU on a 4-CPU quota, peak 5.0 GB, measured with two speed-ups not in main yet: a
  faster AHD (the rework of #1320) and DNG Deflate at level 1 (#1731); 15 min 38 s / 2468 s before
  those, the binned small loads and the faster pyramid. Every frame aligned (median 1352 inliers,
  RMS 2 px at full size), the auto-ISO change measured as none; no ghosting, and the cyan fringe
  a maximum selection left along petal edges against the background is gone.

## Limits (known gaps)

- No lens-distortion model in the panorama bundle adjustment (rectilinear lenses assumed); no
  360° wrap-around; flat (translating-camera) scans are fitted with the rotation model. Panoramas and HDR
  panoramas carry no lens corrections, nor does an HDR whose frames don't share an orientation (they are turned
  before merging).
- No camera response calibration for JPEG brackets (sRGB decoding is taken as linear light).
- AI Denoise doesn't run on merged DNGs (they are linear RGB; the denoiser takes Bayer raws), unlike Lightroom's.
- Boundary Warp is a separable stretch, not a content-preserving mesh warp; Fill Edges is diffusion,
  not patch synthesis.
- *Create Stack*: the result goes on top of a collapsed stack of its sources (`Catalog::stack_with_ops`); its
  caption also lists them.
- Panorama output is limited to `maxMegapixels` (default 40) — blending memory is ~25 bytes/pixel.
- HDR memory: every frame is held as 32-bit float RGB (~0.86 GB per 67 MP frame at the peak, plus ~1.2 GB); full-size
  photos are decoded one at a time. Measured on 67 MP raws: 3 frames 3.8 GB, 4 frames 4.7 GB, 5 frames 5.5 GB (was
  4.8 / 6.3 GB for 3 / 4). Half-float frames would halve the per-frame share.
- The browser build has no file access for merges yet.
- Focus stacks: a similarity only, so a camera moved between
  shots (rotation about the lens's side, tilt) is not followed; no retouching from single frames,
  no depth-map mode, no halo control; frames must share their size. A preview keeps its ≤ 1024 px
  frames between the two passes (≈ 8 MB each).
