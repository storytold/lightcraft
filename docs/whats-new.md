# What's new in LightCraft

## October 2026

### Library keyboard culling
- Colour labels tint thumbnail surrounds in Square Grid and the Detail filmstrip, and a translucent footer along the bottom of the photo in Photo Grid. Label confirmations use a matching pale colour.
- Setting or clearing a colour label shows a brief bottom confirmation, like rating a photo; custom label names appear in the message too.
- On macOS, ratings `0–5`, labels `6–9` and pick/unflag `P/U` now reach the app even when shown in the native menu (issue #283; adapted from PR #261).
- `Shift+6–9` labels and advances. `Shift+P` picks and advances in Photo Grid and Square Grid; it opens Presets in other views. With Auto Advance on, Shift still moves only once.
- Help → Keyboard Shortcuts includes the number-key bindings. See [library shortcuts](library-shortcuts.md).

### Keyboard shortcuts
- Shortcuts are editable: Help ▸ Keyboard Shortcuts (⌘/) lists every command with a search box; click a shortcut and
  press the new keys (Esc cancels), × removes it, ↺ restores the original, Reset All undoes every change. A key that
  belonged to another command moves to the new one. Menus show the new keys; agents use `app.setShortcut`.

### RAW decoding
- Panasonic and Leica raws (RW2, RWL) are now corrected for lens distortion the way the camera corrects its own JPEG
  (issue #256): the correction the camera records in the file is applied under Lens Corrections ▸ Enable Profile
  Corrections, on by default for newly imported photos, with the same framing as the camera's JPEG. At 12 mm the
  12–32 mm kit zoom was off by about 5 % of the image width at the corners before. Files shot with the correction off
  are unchanged; photos imported before this change get it when imported again.
- Canon CR3 raws now develop from their sensor data: lossless RAW and C-RAW, checked sample for sample on the EOS
  M50, R100 and R8. CR3 files the decoder can't read yet still open from their embedded JPEG, as before.
- Sony ILCE-7M4 downsized lossless ARWs now decode subsampled YCbCr tiles into linear RGB,
  preserving RAW editing & full-resolution export instead of using embedded JPEG previews.
- Sony A7R II (and other) raws whose camera JPEG is lens-corrected no longer open grey and too dark (issue #232): the
  starting look is fitted to the camera JPEG away from edges when the misaligned edges spoil the fit on all pixels.

### Lightroom Classic catalogs
- File → Import Lightroom Catalog… opens `.lrcat` directly, with originals referenced in place.
  Ratings, flags, labels, keywords, collections/sets, virtual copies and supported edits migrate;
  existing LightCraft edits are preserved by default. Source settings/history are archived, unsupported
  fields are reported, and the original Lightroom database stays read-only. Rendering is approximate.

### Presets and profiles
- Import presets from other editors: XMP presets, classic `.lrtemplate` files, "DNG presets" from mobile apps and `.zip`
  bundles of any of these — whole folders at once, grouped by pack. Masks inside presets come along.
- Luminar looks: `.lmp` files and `.mplumpack` collections import as presets (grouped by collection); the sliders
  with a counterpart here come along, the rest is listed.
- 23 new built-in presets: Portrait, Landscape, Urban, Food, Seasons, Vintage and B&W toners.
- Imported `.cube` LUT profiles appear in the Profile menu and the profile browser, grouped by their folder, and stay
  favourites across restarts (issue #328).

### Editing
- The Tint slider works the right way round (issues #188, #321): left adds green, right adds magenta, as its track
  shows and as in Lightroom, and Tint values in Lightroom XMP sidecars now render as they do there. A custom Tint
  saved in an earlier version now shifts the other way; set it again (or re-run Auto / the white-balance picker).
- Crop (issue #295): a Lock toggle keeps the aspect ratio on every handle, Custom takes your own ratio (Apply), and
  dragging a handle into the image edge stops there instead of pushing the crop out of shape.

### Library and views
- Trackpads: pinch to zoom around the pointer and scroll with two fingers to pan the photo; panning keeps the photo
  inside the view. A plain mouse wheel over a zoomed photo pans it too.
- A Folders section in the sidebar lists the folders your photos were imported from; choose one to see its photos.
- Select All and multi-selection show every selected photo in the grid and filmstrip, not only the active one
  (issues #187, #298). Importing files that are in Recently Deleted asks whether to leave them there, restore them
  (with their edits) or import them as new; the trash view's Photo menu has Empty Recently Deleted.
- The Import Photos review opens bigger and can be resized; its photo grid fills it (issue #337). Shift-click checks
  or unchecks a range of photos (issue #338).
- When a folder holds several file types, the Import Photos review has a toggle per type (`ARW · 120`, `JPG · 120`):
  import only the raws and leave the JPEGs beside them (issue #344).

### Languages
- The interface is available in Spanish (issue #371), German and Russian (Edit ▸ Language), alongside English,
  Chinese (Simplified and Traditional), Japanese and Brazilian Portuguese.

### Editing
- Type an exact value into any slider (issue #322): click the number next to its name, type (`1.5`, `-20`, `5600`)
  and press Return; Esc keeps the old value.

### Editing
- The eye on the Light, Color and Detail section headers now hides their adjustments, as it already did for Effects,
  Optics, Geometry and Calibration (issue #316).

### Library
- Choosing a date under By Date or a keyword under Keywords shows those photos from All Photos, as their counts
  promise, instead of filtering whatever album or folder was open, which often showed nothing (issue #341).

### Reliability
- If the desktop app can't open its window (for example when no graphics device can be used), it now says so in a
  message box that names the log file, instead of quitting without a trace (issue #260).
- On macOS, single-key shortcuts that appear in the menu bar now work: E, C, H, M, ⇧P, I, K, D, ratings 0–5,
  labels 6–9, P / U and the rest did nothing, because macOS only passes ⌘ / ⌃ combinations and function keys
  to the menu bar and the app ignored those keys, assuming the menu bar would handle them. Keys outside the menus
  (Space, G, X, ⌫) and ⌘ shortcuts were not affected.
- `--memory` sessions keep their promise to save nothing (issues #164, #169): UI changes made in one no longer
  land in `ui.json` (where they replaced the saved settings), and the GPU crash sentinel no longer creates the
  settings folder there. The same goes for the temporary session offered when the library can't be opened.
- The desktop app keeps a log file: `logs/lightcraft.log` in its settings folder (Linux `~/.config/lightcraft/logs/`),
  with the logs of the two previous runs beside it, so warnings and crashes of a run started from a desktop menu or the
  Dock can be attached to a bug report. `LIGHTCRAFT_LOG` works as before; `RUST_LOG` takes env_logger-style
  directives. See README → Quick start → Logs.
- `lightcraft-cli` logs warnings on stderr too (issue #168); `LIGHTCRAFT_LOG` or `RUST_LOG` picks another level.
- LightCraft no longer crashes at launch on Windows PCs whose Vulkan driver is broken (issue #136, e.g. some Intel UHD
  630 drivers): on Windows the window and GPU rendering use DirectX 12 only and never load the Vulkan driver unless
  asked to. `LIGHTCRAFT_GPU_BACKEND=dx12 | vulkan | metal | off` (or wgpu's `WGPU_BACKEND`, which GPU rendering
  ignored before) chooses the graphics backend; `off` renders on the CPU. The GPU now starts after the window
  is up, and only when Settings ▸ Performance ▸ Use the GPU for rendering is on; if LightCraft ever dies while
  starting the GPU, the next launch starts with GPU rendering off and says how to turn it back on.
- Exports and renders never write over a photo's original (issue #93): exporting into the photo's own folder with
  the same name and "Overwrite" (or Export with Previous repeating it), an exact output path from the control
  channel or MCP, a merge preview path or `lightcraft-cli render IMG.jpg -o IMG.jpg` is refused with a clear
  message, and the original is left byte for byte. Ordinary earlier exports are still overwritten when asked.
  Exported files are written to a temp file and then renamed into place, so a full disk or an unplugged drive
  never leaves a truncated file; the XMP sidecar of an "Original" export follows the "If file exists" choice too.
- Convert to DNG, Copy as DNG, Photo Merge and smart previews no longer write straight to the final file (issue
  #106): a DNG is checked against the raw data, written to a temp file, synced and read back before it gets its
  name (never replacing a file), and only then is the photo relinked or the raw copy removed — a failed write
  leaves no DNG and keeps the raw. Smart previews are written the same way; a damaged one (cut short by a crash or
  a full drive) no longer counts as built and Build Smart Previews replaces it.
- Import ▸ Copy verifies every copy, like Move (issue #96): each file is written as a new file, synced to disk and
  checked against the content read from the card. A copy that fails or differs is removed and reported as a failed
  import — so "import complete" means the copies are good before you format the card — and a name that is taken
  gets -1, -2… instead of being replaced.
- Faster exports and card imports, with the same protection for what can't be recreated (issue #134): exports,
  renders and screenshots are still written to a temp file and renamed into place, but no longer forced to disk
  one by one — they can always be exported again, and on a USB drive or a NAS that per-file sync dominated a large
  export. The catalog, XMP sidecars, settings, DNGs, merges, smart previews and the copies an import makes are still
  synced. Import ▸ Copy now checks each copy against the content hash taken while scanning the card instead of
  reading the card a third time (Move still compares byte for byte before it deletes a source); a file that changed
  on the card after Review Import is reported instead of being imported with stale details. Checking an export path
  against the library's originals no longer scans the whole library when nothing is at that path.
- Saving metadata to an XMP sidecar another application wrote no longer replaces it (issue #92): LightCraft merges its
  fields in and keeps the rest — e.g. that application's develop settings and edit history — byte for byte. A
  sidecar that isn't valid XMP is copied to `<name>.xmp.bak-<time>` first. With the default stem naming, a raw and a
  JPEG with the same name (`IMG_0001.CR3` + `IMG_0001.JPG`) no longer share one sidecar: the raw keeps `IMG_0001.xmp`,
  the JPEG uses `IMG_0001.JPG.xmp`.
- A save that fails part-way (a full disk, a network share that drops) no longer looks like a damaged catalog
  afterwards (issue #101): the partial write is cut off before LightCraft retries, so the next launch replays every
  change. Catalogs already holding such a fragment load in full. Quitting while the catalog log can't be written
  still saves your queued changes in the closing snapshot.
- The catalog has a format version (issue #102). Opening a library from an older LightCraft upgrades it; a library
  written by a newer LightCraft is refused with "this library was written by a newer version of LightCraft" and left
  untouched — older versions no longer read part of it as a damaged log. Once this version has opened a library,
  LightCraft 0.2.0 and older refuse it ("unsupported format … v2").
- The control port (`--control`) closes a connection as soon as it receives anything that isn't a JSON request
  (issue #94): an HTTP request from a web page can no longer carry a command in its body. Lines are capped at
  4 MiB and connections at 16.
- A sleeping NAS, a dropped network share or a USB drive spinning up no longer freezes the window (issue #104):
  whether originals are there is checked on a worker thread (grid thumbnails, the Info panel, the photo menu, Missing
  Photos); importing (also by drag and drop), Find Missing Photos, Build / Discard Smart Previews, the Rename preview,
  the Local folder tree and Auto Import read the disk on worker threads too. The import progress window has Cancel.
- Browser version (experimental), keeping a library safe (issue #107): File ▸ Back Up Library… downloads the catalog
  and every imported photo as one zip, and File ▸ Restore Library from Backup… brings it back (the current library
  is kept). A failed save (storage full) now shows the unsaved warning and is retried, a photo that can't be stored
  isn't added, a second tab shows a message instead of overwriting the first, `?reset` asks first, and the page says
  when the browser may evict the library. Hosting: the sample cache headers no longer mark the (unhashed) files
  immutable, and HOSTING.md describes the actual build.
- Canon CR2 photos from the EOS 7D, 50D, 60D, 550D, 600D, 1200D, 1300D, 5D Mark II and 1D Mark IV (and other
  models whose sensor starts on a green-blue row) no longer come out magenta (issue #85): the colour-filter
  layout is read from each file instead of assumed.
- Exports are never black because of the GPU (issue #78): a GPU render that runs out of device
  memory, exceeds the GPU's buffer limits, hits a driver error or reset, or comes back
  incomplete is redone on the CPU — the file is the same image either way. Work is sent to the GPU
  in short pieces so slow integrated GPUs aren't reset by their watchdog. `ui.inspect` → `perf`
  (`gpuReason`, `gpuFallback`), Help ▸ System Info and Settings ▸ Performance say why the GPU isn't
  used (e.g. a skipped software adapter such as llvmpipe) and why the last render fell back.
- The thumbnail cache only ever counts and deletes its own files (issue #98): a library opened on a folder that
  already has a `thumbs/` folder of other pictures no longer loses them when the cache is trimmed or cleared.
- Settings files are never quietly reset (issue #103): a damaged `prefs.json`, `presets.json` or `view.json` is kept
  as `<name>.corrupt-<time>` and you're told; one that can't be read (e.g. locked by another program) is left alone
  for the session instead of being overwritten with defaults. The app settings (`ui.json`, which remembers your
  library) are written atomically and saved as soon as you open another library, not only at quit. Quitting while
  changes couldn't be saved tries once more, then asks: Try Saving Again, Quit Anyway or Cancel.

### Library
- Photos in Recently Deleted can be restored from the app: right-click ▸ Restore (or Delete Permanently), also in the
  Photo menu. The filmstrip has the photo context menu too. Adding a file again that is in Recently Deleted no
  longer just says "duplicate skipped": it opens the side panel on Recently Deleted with the photo selected and says
  how to restore it or delete it permanently and import it afresh. (For a fresh start on a photo, Reset Edits,
  Cmd+Shift+R, keeps the photo and clears its edits.)
- Canon CR3 files show their full-size embedded JPEG (e.g. 6960 × 4640 on an EOS R6 Mark III) instead of the
  1620 × 1080 preview, and import with their metadata: capture time, camera, lens, exposure, GPS and XMP. Their raw
  data is not decoded yet, so they stay preview-only. For CR3s imported earlier, Photo ▸ Reload from Disk (now in
  the Photo menu and the photo context menu) picks up the full-size preview and fills in the camera metadata they
  were missing, without touching anything already set.
- Rename Photos never overwrites another photo when only the letter case changes (issue #95): on case-sensitive
  volumes (Linux, case-sensitive APFS) `img_1.JPG` next to `IMG_1.JPG` is a different photo and the renamed one gets
  `img_1-1.JPG`; on case-insensitive volumes the case change still goes through.
- Rename Photos reports files it could not move back after a failure (issue #105), e.g. when a network share drops
  mid-batch: the error lists them (old → new) and the library points at their new names (an undoable partial rename),
  so none shows as missing. Renaming one of a raw + JPEG pair copies their shared `IMG_0001.xmp` instead of taking
  it away from the other (issue #92). Find Missing Photos also finds renamed files by their content, prefers a content match
  over a same-name same-size look-alike, and skips (and reports) photos it can't tell apart instead of guessing.
- Rename Folder… and Move Folder To… (Local) can be undone and redone (issue #97): the folder moves back on disk with
  its photos and XMP sidecars, and the photos point at it again. If something now occupies the old place, the undo
  is refused and nothing is overwritten.
- Smart albums with a rule editor: match all / any / none, nested groups, 26 fields.
- Quick Collection and target album (B in the grid), keyword sets (⌥1–⌥9), colour-label sets.
- Colour-label filter with several labels at once; expandable folder tree in Local.
- Import: copy to any folder, by day / by month / one folder, rename on import, metadata preset, Copy as DNG.
- Import ▸ Move: photos go into the destination (with the same folders and renaming as Copy, e.g.
  `Photos/2026/20260114/20260114_001.jpg`) together with their XMP sidecars; each original leaves the card only after its
  copy is verified and in the library. Duplicates and files that fail stay where they were.
- Watched-folder auto import; Convert to DNG; Duplicate; Build Standard / 1:1 / Smart Previews.
- Export file names use the Rename Photos tokens ({title}, {seq:2}, {date:%Y-%m-%d}…), plus new {num}, {folder}, {lens}, {iso}, {rating}, {creator}.
- Copyright status, rights usage terms and copyright info URL in Info, metadata presets and exports.
- Auto-Tag from Tracklog: GPS locations for your photos from a GPX track log, matched by capture time.
- A change that can't be saved to disk (full or unplugged drive) is no longer silent: the command reports
  "saved in memory but not written to disk", the top bar shows a warning, and LightCraft keeps retrying until the
  save goes through.
- Smaller, faster catalogs: photos you only looked at in Local (never added, rated or edited) are forgotten once
  their folder has not been browsed for 30 days — your files and sidecars stay, and browsing the folder shows them
  again. Change the period (or turn it off) in Settings → Performance.
- A library is open in one program at a time (issue #99): opening a library that LightCraft or `lightcraft-cli` already
  has open — on this computer or another one sharing the folder — says who has it ("already open in LightCraft (process
  123 on studio-mac)") instead of letting both write and silently drop each other's edits. A crash never leaves the
  library locked: the lock is the operating system's and goes away with the program.
- If your library can't be opened at launch (open in another program, unreadable, on a drive that isn't connected,
  written by a newer LightCraft), LightCraft says so and why, and offers Try Again, Choose Another Library…, Continue
  Without Saving and Quit (issue #100). It no longer quietly starts a demo session that looked like a reset library and
  lost everything at quit; a temporary session shows a banner the whole time and never writes to your library.

### Editing
- AI masks with SAM 3 (Object and Describe in the Masking panel): click an object to select it (⌥-click leaves a
  part out), or type what to select ("sky", "the red car", "car, road"); both combine with other masks, have an
  Edge setting, and get a sharper zoomed-in pass in the background. The model runs inside LightCraft in pure Rust
  and never freezes the window. It is optional and not part of LightCraft (Meta's SAM License, about 3.4 GB): the
  first time you use an AI mask, LightCraft asks before downloading it, shows the progress, can cancel and resume,
  and checks the file before using it. Masks keep their selection, so they render and export without the model.
- Auto Sync: edits apply to every selected photo. Auto B&W mix. Automatic versions.
- Colour-range masks: click the photo to sample. Luminance ranges: range bar, smoothness, luminance map.
- ⌘-drag to straighten, ⇧G Guided Upright, a grid while transforming.
- Nikon NEFs start from a colour and tone look fitted to the camera's own JPEG, as Sony ARWs do, instead of a muted,
  greenish neutral rendering (issue #150); white balance is adjusted relative to the as-shot look. 12-bit NEFs
  (e.g. D750, D780, D850, D7500, Z 50) no longer render nearly black or with crushed shadows: their black level was
  read in the wrong units.

### Viewing and sharing
- Slideshow, second window, All Metadata, System Info.
- Edit in External Editor (⇧⌘E): a 16-bit TIFF copy, stacked, refreshed when you come back.
- Lossy DNG files and Smart Previews open as raw photos.
- Compressed Nikon NEFs (lossless and lossy compressed, 12- and 14-bit — e.g. D3200, D5100, D7000, D750, D850, Z 50)
  now develop from the raw data instead of the camera's embedded JPEG, so a B&W or other picture style set in the
  camera no longer gets baked in. Photos already imported as "preview only" switch over on Reload. (Files that
  Nikon splits into two differently compressed halves still use the preview for now.)
- Panasonic and Leica raws (RW2, RWL and the older RAW files, DMC-LX1 to DC-S1R II) develop from the raw data in
  every format the cameras write. Most bodies opened as preview only before: GH1–GH5, the G, GX, GF, GM, FZ, LX and
  TZ/ZS series, Leica D-Lux, V-Lux and C-Lux, and the GH6, GH7, G9 II, S5 II and S9 generation. They start from a
  colour and tone look fitted to the camera's own JPEG, as Nikon and Sony raws do, framed in the aspect ratio set in
  the camera; mapped-out sensor defects are filled in. `.rwl` and `.raw` files are imported too. Photos already
  imported as "preview only" switch over on Reload.
- Sony ARWs from before about 2017 (RX100, RX100 II–V, RX10, NEX, SLT, ILCE-6000, A7 / A7 II / A7R II and their
  siblings) no longer open bright green (issue #148): their as-shot white balance and black level are read from the
  file (Sony stores them only in scrambled maker-note data on these bodies), the few columns of padding at the right
  edge are cropped away, and "12-bit uncompressed" files are no longer clipped. The RX100 series renders much closer
  to the camera's own JPEG.
