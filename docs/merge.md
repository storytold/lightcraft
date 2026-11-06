# Photo Merge (HDR, Panorama, HDR Panorama)

`crates/merge` (layer L3, no UI dependencies) implements the algorithms; `crates/engine/src/merge.rs`
plans, runs and finishes merges in the library; the desktop UI shows a dialog with a live preview.
All algorithms are our own implementations from published papers (cited in the module docs).

## Using it

- **App:** select the photos, then *Photo Merge ▸ HDR…* (`Ctrl+H`), *Panorama…* (`Ctrl+M`) or
  *HDR Panorama…* from the photo context menu (or the `dialog.merge*` commands). The dialog previews
  the merge at ≤ 1024 px and re-runs the preview when an option changes; as in Lightroom, a click on the preview
  zooms in on that point (drag to pan, click again to fit), and it stays there while options change; *Merge* runs the full merge
  in the background (a row in the activity stack shows its progress, with ✕ to stop it). The result is written next to the first photo as
  `<name>-HDR.dng`, `-Pano.dng` or `-HDR-Pano.dng`, imported and selected. *Photo Merge ▸ HDR for Each Bracket Set*
  merges many brackets in one go with the last HDR options: one HDR per stack among the selected photos, then per
  bracket (photos taken less than a second apart, after the shutter time, at exposures at least ½ EV apart; a burst at
  one exposure is skipped), one after the other in the activity stack.
- **Commands** (CLI, MCP, control channel):
  - `merge.hdr {ids?, align=true, deghost=none|low|medium|high, autoSettings=true, stack=false, preview=false, showOverlay=false, previewPath?}`
  - `merge.panorama {ids?, projection=auto|spherical|cylindrical|perspective, boundaryWarp=0..100, autoCrop=false, fillEdges=false, autoSettings=true, maxMegapixels=512 (also the most: a larger one, or 0 or less, is taken as 512; the output keeps the photos' own resolution up to it), preview, previewPath?}`
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
- Memory is planned before a merge decodes anything (`crates/engine/src/merge/plan.rs`), from each photo's size and
  file size in the catalog (when it has none, its file's header; when that can't be read either, the largest known),
  against the merge's memory: three-quarters of the RAM, less the cache budget. Each phase is followed in the order it
  runs. Photos are decoded one at a time, always at full size, then made smaller: each holds its file and ~36 bytes a
  pixel (48 when it is made smaller; a raw whose sensor has many more samples than its picture has pixels, 8 bytes a
  sample instead when that is more; and one that demosaics its whole active area before cropping it, 12 bytes a pixel
  of that area besides: every raw but a Bayer mosaic without an `OpcodeList3`, and every raw in a preview) beside the
  frames already held (12 bytes a pixel each). Issue #3708: until then a merge was sized only after every photo was
  decoded, which could run out of memory first. A merge that fits runs as it always did; one with no size known at all
  runs unplanned. When the computer's memory can't be read (Windows, for now) the merge's memory is a guess (that of 8
  GB of RAM): the plan follows it but refuses nothing on it, and what it would refuse runs as it did before, one
  photo at a time: a full merge at full size, a preview at its 1024 px frames. Just before each photo is decoded its header is read again and the plan checked with it
  (the catalog doesn't keep a raw's sensor size, and a file can have changed since it was imported): when the plan no
  longer holds the merge stops before decoding it, naming the file (and, when its header differs from the catalog, saying it
  looks changed since import, so its folder needs synchronizing).
- A preview's photos are planned at full size too (a JPEG other than a lossless one may decode at a smaller DCT scale;
  the plan keeps the full size as a margin), and decoded several at once: one at a time when several don't fit, and the
  preview is refused when not even that, or the merge or stitch of its small frames after it, fits. A preview's
  panorama is made down to 1 MP (or its frames' size, when less), not to the floor of the panorama it previews.
- HDR memory: every frame is held as 32-bit float RGB (~0.86 GB per 67 MP frame at the peak, plus ~1.2 GB); full-size
  photos are decoded one at a time. Measured on 67 MP raws: 3 frames 3.8 GB, 4 frames 4.7 GB, 5 frames 5.5 GB (was
  4.8 / 6.3 GB for 3 / 4). Planned as 17 bytes a pixel of every frame and 12 of the largest, plus deghosting's 2048 px
  maps (20 bytes a pixel), or the frames and two saturation maps of each (20 bytes a pixel, measured for every frame
  at once) when that is more: 3.9 / 5.0 / 6.1 GB for those without deghosting, 3–10 % above the measurements. An HDR
  keeps its photos' resolution: one that doesn't fit is refused with the memory it needs and the merge's budget. Half-float frames
  would halve the per-frame share.
- Panorama output keeps the photos' resolution, up to `maxMegapixels` (default and most 512: a larger one, or 0 or
  less, is taken as 512), a longest edge of 65,000 px, and what fits in the merge's memory: the blend peaks at ~68
  bytes per output pixel, and projecting needs ~36 beside the photos' 12. When the result comes out smaller than the
  photos' resolution, `limit` says why: `"memory"` when what fits in memory is below both the `maxMegapixels` asked
  for and the photos' resolution; otherwise `"asked"` when a `maxMegapixels` below 512 is below the photos'
  resolution (so `maxMegapixels: 20` with 50 MP fitting in memory and 100 MP of photos says `"asked"`); otherwise
  `"size"`, the 512 MP or 65,000 px cap. It is made no smaller than its floor: 40 MP, or the smaller `maxMegapixels` asked for (a positive `maxMegapixels` below 0.1 is refused:
  the stitch keeps no smaller size limit), or the photos' own pixels when they have less, even under 0.1 MP (the stitch never scales photos up). A
  merge whose memory can't hold the floor is refused with the memory it needs and the merge's budget (that share of the
  RAM: closing other apps doesn't raise it), and, when a smaller `maxMegapixels` would fit, the largest that does
  (issue #3708; until then the 40 MP floor was kept whatever the memory, and could run out of it).
- Photos for a panorama that don't fit at full size are decoded smaller, but only while the stitch still reaches
  its floor (which follows the photos' own size, not the smaller frames'); a merge that would have to go below it is
  refused before any photo is decoded (only the header of a photo the catalog has no size for is read first). They keep at least twice the floor's pixels (a panorama is taken to keep half
  its frames' pixels, their overlap aside; for an HDR panorama, in whichever bracket makes the fewest), and a
  panorama of smaller frames that still comes out under its floor (the overlap is known only after the stitch) is
  refused then, when the memory is known; on a guessed memory it is kept, below its floor. They are decoded as large as fits and still leaves the panorama as large as
  memory allows (frames above about 1/25 of the memory in pixels leave less room for projecting it), and never under
  1024 px on the long side (the preview's frames): the result says `limit: "memory"` (or `"asked"`, when a
  smaller `maxMegapixels` holds it below what memory allows), and `nativeMegapixels` is still the photos' own
  resolution. On a guessed memory a panorama the stitch would refuse is made at its floor, as
  before merges were planned. An HDR panorama holds all its brackets' photos before merging them, one bracket at a
  time; with the bracket left to EXIF, every size it could be is planned for.
- The browser build has no file access for merges yet.
