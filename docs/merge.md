# Photo Merge (HDR, Panorama, HDR Panorama)

`crates/merge` (layer L3, no UI dependencies) implements the algorithms; `crates/engine/src/merge.rs`
plans, runs and finishes merges in the library; the desktop UI shows a dialog with a live preview.
All algorithms are our own implementations from published papers (cited in the module docs).

## Using it

- **App:** select the photos, then *Photo Merge ▸ HDR…* (`Ctrl+H`), *Panorama…* (`Ctrl+M`) or
  *HDR Panorama…* from the photo context menu (or the `dialog.merge*` commands). The dialog previews
  the merge at ≤ 1024 px and re-runs the preview when an option changes, reusing the photos it decoded (the
  session's `FrameCache`, ≤ 512 MB, keyed by file, size and modification time), so switching a projection or a
  deghost amount reads nothing again; as in Lightroom, a click on the preview zooms in on that point (drag to pan,
  click again to fit), and it stays there while options change; *Merge* runs the full merge
  in the background (a row in the activity stack shows its progress, with ✕ to stop it). The result is written next to the first photo as
  `<name>-HDR.dng`, `-Pano.dng` or `-HDR-Pano.dng`, imported and selected. *Photo Merge ▸ HDR for Each Bracket Set*
  merges many brackets in one go with the last HDR options: one HDR per stack among the selected photos, then per
  bracket (photos taken less than a second apart, after the shutter time, at exposures at least ½ EV apart; a burst at
  one exposure is skipped), one after the other in the activity stack.
- **Commands** (CLI, MCP, control channel):
  - `merge.hdr {ids?, align=true, deghost=none|low|medium|high, autoSettings=true, stack=false, preview=false, showOverlay=false, previewPath?}`
  - `merge.panorama {ids?, projection=auto|spherical|cylindrical|perspective, boundaryWarp=0..100, autoCrop=false, fillEdges=false, autoSettings=true, maxMegapixels (default: the photos' own resolution), preview, previewPath?}`
  - `merge.hdrPanorama {ids?, bracket=0 (from EXIF), …both sets of options}`
  - `merge.hdrBatch {ids?, align, deghost, autoSettings, stack}` → `{merged: [{id, path, …}], failed: [{photos, error}]}`
- **CLI:** `lightcraft-cli merge hdr|panorama|hdr-panorama|hdr-batch [options] FILES…` (`hdr-batch` finds the brackets
  among the files, e.g. a whole folder);
  `lightcraft-cli synth-merge hdr|panorama -o DIR` writes synthetic inputs (procedural scene).

## What it does

**HDR** (`hdr.rs`): frames ordered by EXIF exposure (or median luminance); the middle one is the
reference. Auto Align: features on exposure-normalised luminance, RANSAC translation / similarity /
homography (the simplest model that explains ≥ 97 % of the matches). Frames are resampled with Lanczos-3, held to
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

**DNG output** (`output.rs`): LinearRaw, 3 samples, Deflate tiles; values scaled below 1.0 with the
scale in `BaselineExposure`; raw sources keep their DNG colour tags (camera RGB), other sources get
`ColorMatrix1`/`ForwardMatrix1` of their RGB space.

## Measured (synthetic tests, `cargo test -p lightcraft-merge`)

- HDR, 3 brackets ±2 EV with 14-bit noise and handheld shifts: alignment error < 0.3 px, exposure
  error < 0.02 EV, radiance median log error < 0.03 (noise-limited, bias < 0.01), recovered
  highlights < 0.05; moving object: ghost error 0.2–2.0 without deghosting, < 0.06 with High; a star that moves
  across a night sky keeps its clipped core and, with High, leaves no ghost of the brightest frame.
- Real brackets (Sony A7R VI, 67 MP, 2026-10-14): frames agree with the merged radiance within ~1 % from 3 to 90 % of
  their clip in all three channels; measured exposure steps match EXIF within 0.03 EV.
- Panorama, 5 views over 182° with a 1.3× exposure drift: relative rotations < 0.1°, focal < 1 %,
  BA RMS ≈ 0.35 px; median log error vs ground truth < 0.02; no seam steps.

## Memory

- Preview photos are binned straight from the mosaic (whole `k × k` blocks, `RawImage::develop_binned`), also when
  the raw carries an `OpcodeList3` gain map (lens shading, as DJI drones write) or lens warps: those apply to the
  binned image (`Opcode::binned`). A 48 MP DJI DNG took 1.7 GB and 2 s to load for a preview (a full-size
  demosaic), now 0.35 GB and 0.3 s. Previews decode at most 4 photos at once; full-size merges one at a time.
- An HDR panorama loads, merges and frees one bracket at a time. 12 DJI DNGs (4 brackets of 3, 48 MP): preview
  1.9 s and 1.9 GB peak (40 GB and minutes before); the full 97 MP merge 27 s, peak 18.5 GB, set by the stitch
  (the projected frames and the multi-band blend at full size), which `maxMegapixels` and the memory fit bound.

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
