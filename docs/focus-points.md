# Focus points

Most cameras record where they focused. LightCraft reads that point from the file, draws it over the photo and
zooms to it, so checking whether the subject is sharp takes one key while culling. Lightroom shows no focus points
(photographers add plug-ins for it); this goes beyond parity (tracker row `LR-VIEW-FOCUSPOINT`).

## Using it

- **View ▸ Focus Point** (⌥F; `view.focusPoint {on?}`) draws the camera's AF frame as green corner brackets in
  Detail, Compare and Survey. It follows Rotate, flips and the crop; a point cropped away is not drawn. Off by
  default; the choice is saved with the app's settings.
- **View ▸ Zoom to Focus Point** (`view.zoomToFocus {on?}`, on by default): Toggle Zoom (Z or Space) zooms in
  centred on the focus point instead of the last position. A click on the photo still zooms where you click, and a
  photo without a focus point zooms as before. On by default because it changes nothing for photos without a point,
  and for those with one the focus point is what a 1:1 check is for. In Compare the two panes share one pan, so Z
  centres on the active photo's point; per-pane focus zoom is a follow-up.
- **Info panel**: a *Focus* row says how the camera focused, e.g. `AF-C · Wide · Animal Eye Tracking` (focus mode,
  the AF area set on the camera, and what it tracked when the camera says).
- **Agents**: `photo.inspect` (and the MCP `lightcraft://photo/active` resource) returns it as `meta.focus`:
  `{rect: {x0, y0, x1, y1}, mode?, area?, tracking?}`, the box normalized to the upright (EXIF-oriented) photo, y down.
  Photos without one have no `focus` key. `view.zoomToggle` replies `{focus: [x, y]}` (the pan it centred on) when it
  zoomed to one.

The point is read when a photo is imported. Photos imported earlier get it in the background (below). It is
read-only camera data: never written to XMP, and absent from the catalog JSON when unknown, so older catalogs read
unchanged (no catalog format change).

## Photos imported earlier

A library imported before focus points were read gets them by itself: when a library opens and some of its photos
from a supported body have no point and haven't been looked at, LightCraft reads them in the background, with a
**Reading focus points** row in the activity stack (n of N; ✕ stops it). **Photo ▸ Read Focus Points**
(`photo.readFocusPoints`) starts the same job by hand. Photo ▸ Reload from Disk still fills a missing point too, but it
re-reads and hashes whole files and runs while you wait, so it is for a few photos, not a library.

- **What is opened**: only photos whose stored camera is a supported body (`SONY ILCE-1`, `SONY ILCE-7SM3`), whose
  file is an ARW or JPEG, that are in the library and have no point yet. Nothing else is opened.
- **What is read**: the few kilobytes that hold the focus data, not the file
  (`lightcraft_meta::focus::read_header`). It walks the file's structure as the decoder does — the TIFF header and
  IFD0 (Make, Model), the Exif IFD (the MakerNote entry), the maker note's IFD and the values of the focus tags; in a
  JPEG, the segments up to the Exif one — asking only for the ranges the bytes read so far point at, and decodes them
  with the same code as a whole-file read. On the α1 and the A7S III everything sits in the first 16 KB, so one 32 KB
  read per photo does it (instead of 50–120 MB). Rounds (12), bytes (2 MB) and the file size are bounded; a file that
  wants more is treated as having no point. On 109 real α1 / A7S III ARWs (83 with a point) the header read gave the
  same point as the whole-file read for every file.
- **Pace**: one file at a time on one worker thread (the drive may be a spinning disk), in path order so neighbouring
  files are read together. Nothing new is read while you drag, type or scroll, and it rests briefly between files
  while you are around, so your own previews come first. Measured on a 5400 rpm USB drive, cold cache, 200 α1 / A7S III
  ARWs: 1.9 s (about 100 photos a second, 32 KB each), so a library of 58,796 raws takes about ten minutes at full
  pace instead of re-reading 2.25 TB.
- **Saving**: what was read is committed every 200 photos (or every 3 seconds) as LightCraft's own bookkeeping: no
  undo step, no sidecar written, and only `meta.focus` changes, on a photo that still has none and still points at the
  file that was read (a photo deleted, moved or relinked meanwhile is left for another run). Photos that turned out to
  have no point (manual focus, a centre-only Wide report) are listed in `focus-scanned.bin` beside the library, so they
  are not read again at every launch. The list is headed by the decoder's tag (the bodies it knows): when a body is
  added, the list starts afresh and those photos are looked at again. A file that can't be read (an offline drive, a
  file gone or replaced during the read) is not listed, and is tried again the next time the library opens.
- **Resuming**: quitting mid-run keeps what was committed; the next launch reads the rest.
- **Agents**: `photo.readFocusPoints {ids?, wait?}` — without `ids`, every photo not looked at yet; with `ids`, those
  photos, looked at again even if listed. `wait: true` returns when done with `{last: {total, done, found, none,
  unavailable, bytesRead, seconds, cancelled}}` (`lightcraft-cli run --library DIR photo.readFocusPoints wait=true`).
  The app's frame loop calls `focus.pump {pace}`, which also starts the job once per library opening.

## Cameras

Sony first, on bodies whose files have been checked: **ILCE-1 (α1)** and **ILCE-7SM3 (A7S III)**
(`lightcraft_meta::focus::SONY_MODELS`). Other Sony bodies store the same tags, but a body is added only after its
files are checked, so an unchecked one shows nothing rather than a misleading box. Fujifilm, Canon, Nikon, OM System
and Panasonic record similar data and are later steps.

### Sony layout and evidence

Tag identities and value names come from the public ExifTool tag-name documentation (Sony MakerNotes table). No
third-party code was read or ported. Every field was confirmed on real files by comparing LightCraft's decoder with
ExifTool's output, run as a black box: 333 ARWs sampled across four years of a photographer's library (180 α1,
153 A7S III; 12 portrait or upside-down, 54 in APS-C crop or reduced-size modes), with no mismatch in the point, frame or names.

| MakerNote tag | Layout | Notes |
|---|---|---|
| `0x2027` FocusLocation | 4 × int16u: frame width, frame height, focus x, focus y | in the sensor's landscape frame (a portrait shot still says 8640 × 5760); the frame is the image size (5616 × 3744 in α1 APS-C mode) |
| `0x2037` FocusFrameSize | 6 bytes: int16u width, int16u height, then 2 bytes that are zero when no frame was shown | zero in manual focus; one AF-C α1 file had no frame (a small box is drawn: 3 % of the short side) |
| `0x201b` FocusMode | int8u: 0 Manual, 2 AF-S, 3 AF-C, 4 AF-A, 6 DMF, 7 AF-D | |
| `0x201c` AFAreaModeSetting | int8u: 0 Wide, 1 Center, 3 Flexible Spot, 11 Zone, 12 Expanded Flexible Spot, 13 Custom AF Area | |
| `0x2021` AFTracking | int8u: 0 Off, 1 Face tracking, 2 Lock On AF | |
| `0x9402` bytes 0x16 / 0x17 | enciphered: focus mode (low 7 bits) and the AF area mode used (0 Multi, 2 Spot, 3 Flexible Spot, 11 Zone, 14 Tracking, 15 Face Tracking, 20 Animal Eye Tracking, 21 Human Eye Tracking, 255 Manual…) | read only when its focus mode equals `0x201b`'s |

The documentation says the `0x94xx` blocks use "a simple substitution cipher". On the files each byte `p < 249` is
stored as `p³ mod 249` (bytes 249–255 unchanged): ExifTool's decoded AFAreaMode 20 is stored as `0x20`, a decoded
`0x83` as `0x77`. Cubing is a bijection modulo 249 (= 3 × 83), so the inverse is a table lookup.

On the α1 files the box sits on the subject: drawn on the embedded previews, it lands on the bird's eye in eye-AF
shots and follows spot, zone, wide and tracking shots, in portrait and landscape.

### When there is no real point

Nothing is stored (so nothing is drawn and Z zooms as before) when:

- the body is not one of the checked ones, the tags are missing or malformed, the frame size is zero, or the centre
  lies outside its frame;
- **manual focus** (`0x201b` = 0, or the block's area mode is Manual). The camera still records where the AF frame was
  parked: 83 of the 91 sampled manual-focus files carry an off-centre location that says nothing about focus;
- the location is the **exact frame centre while the area used was Multi** (Wide with nothing tracked; without the
  block, Wide with tracking off). The wide area reports the centre when it picked no particular spot; 7 of the 34
  sampled autofocus Multi files did. A centred Spot, Zone or Flexible Spot frame is a choice the photographer made, and
  is kept (49 of the 111 sampled spot and zone files were centred, the camera's default position).

DMF (AF, then manual touch-up) keeps its point.

## Code

- `crates/meta/src/focus.rs`: the decoder (bounded; hostile values give no point, never a panic) and its tests,
  including an opt-in test over real files (`LIGHTCRAFT_FOCUS_SAMPLES=<photo folder>`).
- `crates/meta/src/focus_read.rs`: the header-only read; its tests check it against the whole-file read on intact,
  truncated and corrupted files and count what it reads.
- `crates/engine/src/focus_scan.rs`: the background job (`photo.readFocusPoints`, `focus.pump`) and
  `crates/engine/src/tests_focus_scan.rs`, with an opt-in comparison over real files
  (`LIGHTCRAFT_FOCUS_FILES=<a list of paths>`); `crates/ui-egui/src/focus_scan.rs` drives it from the frame loop.
- `crates/ui-egui/src/panels/detail.rs` (`focus_box`, `focus_pan`, `draw_focus_point`): mapping through the user's
  rotation and crop, the overlay, and the zoom target; `crates/ui-egui/src/tests_focus_point.rs`.
