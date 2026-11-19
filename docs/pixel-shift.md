# Pixel Shift Merge (Nikon)

> Names in angle brackets (`<app>`, `<cli>`, …) are the values set in [`brand.toml`](../brand.toml); see the README.

Merges the frames of a Nikon pixel-shift series into one image with full colour at every pixel, without NX Studio.
The merge runs on the raw mosaics, **before** demosaicing, and writes a LinearRaw DNG that is then developed like any
raw. Measured on a Z 8 (16-shot series, FX 3:2 and 1:1 image areas); other bodies and shot counts are refused until
they have been measured.

## Using it

- **App:** select all the frames of one series, then *Photo ▸ Photo Merge ▸ Pixel Shift*. The merge runs in the
  background (a row in the activity stack, with ✕ to stop it). The result `<frame 01 name>-PixelShift.dng` is written
  next to frame 01, imported, selected, and stacked on top of all the frames. It opens like a freshly imported NEF: the
  dialog's Auto Settings and Create Stack options belong to the other merges and are not used (`merge::parse_dialog`).
- A selection that is not one complete series is refused with the reason, in a message on the window and before any
  frame is decoded: a frame missing or twice, a file that is not a pixel-shift frame, frames of another shot or another
  camera, different exposure settings or image sizes, frames that were not shot one right after the other, or a shot count
  that is not supported yet. A failure while the merge runs (decoding, writing, a full disk) and a cancel are said the same
  way.
- **A result that exists:** when `<frame 01 name>-PixelShift.dng` is already next to frame 01 the merge is refused ("A
  pixel-shift merge of this series already exists"): the file is not replaced, no second name is made and the old result is
  not reused silently. Delete or rename it to merge again.
- **Other merge kinds:** choosing HDR…, Panorama…, HDR Panorama… or Focus Stack… with exactly one complete series selected
  asks "This selection is a complete pixel-shift series. Merge as Pixel Shift instead?": *Pixel Shift* merges it with
  Pixel Shift's own defaults, *Continue with …* opens the chosen merge as usual, *Cancel* does nothing. Any other selection
  is not asked.
- **Delete Merge Sources** (*Photo ▸ Photo Merge*, the grid's context menu, the command palette): on a merged photo it
  moves its frames other than frame 01 to Recently Deleted like any deleted photo (the *Confirm before deleting* setting
  applies); frame 01 and the merged photo stay stacked. Frames that are already gone are counted in the message. It is
  greyed out on any other photo and when no frame but frame 01 is left. Command: `merge.deleteSources {id?}`.
- **Command:** `merge.pixelShift {ids?, autoSettings=false, stack=true, outputDir?}`; **CLI:**
  `<cli> merge pixel-shift [--output-dir DIR] FILES…`.

## How a series is recognised

There is no series id in the files. The Nikon maker note of each frame has the shot count (`0x0034`), the frame's number
in the series (`0x0056` byte 8 and `0x005e` byte 5, which agree), a per-exposure counter (`0x00a7`) and the sensor
position of the frame (`0x0054`, reported only). A selection is one series when its frames have the same camera, lens,
exposure (shutter, aperture, ISO, exposure compensation, white balance), raw size and image area, the numbers 1..N once
each, consecutive counters, and capture times spaced evenly relative to the series' own median interval (the camera's
interval setting runs from none, about 0.12 s, to many seconds). Only the first 512 KB of each file is read; the whole
file when the start does not hold the tags. Code: `crates/raw/src/pixel_shift.rs`.

## What the merge does

`crates/merge/src/pixelshift.rs`. The camera moves the sensor by one pixel between the four frames of a *set* and by half a
pixel between sets (16 shots = 4 sets). One set sees every scene point through a red, two green and a blue filter. A
fixed table by frame number says where each frame's samples land on a grid twice as fine as the sensor; each merged
pixel takes its red from one sample, its blue from one and its green as the mean (rounded down) of two. Nothing is
interpolated, nothing is registered, and the maker-note positions are not used to place anything (they are good to
about 0.1 px). Black and white levels stay the camera's (1008 / 16383 on the Z 8): the DNG's samples are the camera's
values. Frames are decoded, placed and dropped one at a time; peak memory is the merged image plus one frame.

The merged DNG covers the frames' whole active area; its default crop is the frames' crop at twice the size, which is the
area NX Studio's merge covers (16512 × 11008 for a Z 8 FX frame, 11008 × 11008 for the 1:1 area). The geometry comes from
the raw size, active area and crop tags, never from full-frame constants.

## The DNG

16-bit LinearRaw, three samples per pixel, tiled, lossless JPEG. It carries frame 01's make and model (so the file is a
Nikon Z 8 file for the colour look, camera profiles and white balance), its colour as DNG tags (the look the NEF opens
with, fitted as for the other merges), its as-shot white balance, exposure data, capture time, lens data, Nikon distortion
correction (as `OpcodeList3`; the optical-centre offset is relative to the image, so it is the same in merged pixels), and
a provenance block in the XMP.

Sizes for a 16-shot Z 8 FX series (16560 × 11040 samples):

| packing | size | write | decode |
|---|---|---|---|
| lossless JPEG (used) | 569 MB | 2.7 s | 0.8 s |
| deflate with prediction | 750 MB | 2.5 s | 0.5 s |
| uncompressed | 1097 MB | 4.2 s | 0.4 s |

Previews and thumbnails bin the three samples as they do for a NEFX (`crates/raw/src/binned.rs`); the DNG is recognised by
its provenance block.

### Provenance block (`lcps`)

XMP namespace `<project homepage>/ns/pixelshift/1.0/` (like the project's other own namespaces) in the DNG's own packet (`crates/raw/src/merge_provenance.rs`):
`Engine` (version of the merge engine), `ShotCount`, `Reference` (the frame the file belongs to: always 1), `Scale`
(merged pixels per sensor pixel), the stored frames' place on the sensor (`SensorWidth/Height`, `WindowX/Y/Width/Height`,
for a later Vignetting Calibration), and `Frames`: for each frame its `Index`, `FileName`, `Captured` and `Counter`. The
catalog keeps the same (`Photo.merge_sources`, catalog format 19: the frames' photos and the reference). When a merged DNG is
imported on its own (a rebuilt catalog, a re-imported folder) the catalog list is restored from this block: each frame is
found in the library by file name and capture time; a frame that is not there stays in the list as not found, and the
exposure counter is the file's own record of it.

## Known differences from NX Studio's merge

Measured on two 16-shot series, against NX Studio output with chromatic-aberration correction off:

- Red and green are bit-identical over the whole frame. Blue is identical except at blue sites of every twelfth sensor
  row (rows ≡ 5 mod 12), where about half of NX Studio's values differ from ours by about the noise. The camera already
  stores smoothed values in those rows (their noise is a third lower than the other blue rows'); we keep the stored samples. No
  pattern shows on real images.
- **Lens correction starts on.** A merged DNG carries the lens data (Nikon distortion correction as `OpcodeList3`), and a
  DNG with lens data opens with Enable Profile Corrections on, as every such DNG does; a NEF or NEFX opens with it off.
  This is kept on purpose: the pixels are merged on the sensor grid and the correction is the lens's own, so the merged
  file looks right without a step. Switch it off in the Lens Corrections panel like any other setting.
- **Restoring a deleted frame does not put it back into the stack.** This is how every deleted photo behaves, not
  something of the merge: deleting a photo removes it from its stack (`remove_from_stacks_ops`) and Restore only clears its
  deleted flag. A restored frame is a normal photo again; stack it with the merged photo by hand (Photo ▸ Stacking). A
  general "restore stack membership" fix would help every stack and is left for a follow-up.
- No chromatic-aberration correction and no registration: frames are placed on the nominal grid. A series shot on an
  unsteady tripod drifts by up to about 0.6 px over the 16 shots and shows it as softness and colour fringes at fine detail.

## Metadata of the merged file

The merged DNG gets frame 01's capture time with its sub-seconds and time-zone offset, in its Exif (`DateTimeOriginal`,
`SubSecTimeOriginal`, `OffsetTimeOriginal`, with the exposure, focal length and lens) and in its XMP, so the library dates
it with its series. Keywords are the union of the catalog keywords of all the frames (hierarchy kept, each once whatever its
case), written as a sidecar writes them. Creator, rights, title, caption, the IPTC descriptive fields, location and GPS are
frame 01's as the catalog has them. Rating, colour label, flag and edit settings are not carried.
