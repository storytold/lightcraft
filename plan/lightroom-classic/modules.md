# Per-module behaviour notes

Lightroom Classic is a **module** application over one **catalog**. The window has the same frame in every module:

- **top:** identity plate on the left, module picker on the right;
- **left panel:** sources and templates (what to work on, or which layout to start from);
- **right panel:** settings for the current module;
- **toolbar** under the main view (toggle with T);
- **filmstrip** at the bottom, shared by every module, showing the current source with its filter.

Panels collapse individually; each side can be set to auto-hide (appears on hover), and a panel group can be put in
**solo mode**, where opening one panel closes the others. Tab hides both side panels, ⇧Tab hides all four.
The current selection and source carry over when you switch modules.

Tracker sections: Library and catalog rows are under `LRC-LIB-*` / `LRC-CAT-*`, Develop under `LRC-DEV-*` plus most
`LR-EDIT-*`, outputs under `LRC-MAP-*`, `LRC-BOOK-*`, `LRC-SS-*`, `LRC-PRINT-*`, `LRC-WEB-*`.

---

## Library

Purpose: organise, cull and describe. Everything that touches many photos at once lives here.

- **Views:** Grid (thumbnail cells, compact or expanded, with configurable badges and overlays), Loupe (one photo
  with two cycling info overlays), Compare (a fixed "select" and a "candidate" that you swap through, zoom linked),
  Survey (all selected photos fitted to the screen, drop the weak ones), People (faces grouped by person).
- **Left:** Navigator (zoom levels and the visible region), Catalog (all photos, the Quick Collection, the previous
  import, missing photos), Folders (volumes with free space, the folder tree, synchronise and relocate), Collections
  (collections, smart collections, collection sets, the target collection), Publish Services.
- **Right:** Histogram, Quick Develop (relative step buttons and presets that work on many photos at once without
  opening Develop), Keywording (typing, suggestions, keyword sets), Keyword List (hierarchy, synonyms, export
  flags), Metadata (several field sets, metadata presets, edit many photos at once), Comments.
- **Filter bar** above the grid: Text, Attribute (flag, rating, label, edit state, kind), Metadata (columns you
  pick: date, camera, lens, keyword, label, location…, each narrowing the next), saved filter presets, and a lock
  that keeps the filter while you change source.
- **Culling:** flags (pick, reject, none), ratings 0–5, colour labels with named label sets; auto-advance moves on
  after each rating. Stacks group related shots and collapse to their top photo. Virtual copies are extra
  develop-only versions of one file.
- **Painter** (spray can in the toolbar): click or drag across thumbnails to apply keywords, a label, a flag, a
  rating, metadata, develop settings, rotation or target-collection membership.
- **Photo-level tasks:** edit capture time, convert to DNG, rename with templates, show in file manager, edit in an
  external editor (copy as TIFF/PSD or the original), find missing files, read/save metadata to the file.

## Develop

Purpose: edit one photo at a time with full controls; settings are non-destructive and stored in the catalog
(and optionally in XMP).

- **Left:** Navigator, Presets (groups, favourites, amount slider), Snapshots (named states), History (every step,
  click to go back, clear), Collections.
- **Right:** Histogram (drag regions to adjust tone; clipping indicators), tool strip (Crop & Straighten, Remove,
  Red Eye, Masking), then panels in processing order: Basic, Tone Curve, Colour Mixer / HSL / B&W, Colour Grading,
  Detail, Lens Corrections, Transform, Effects, Calibration.
- **Toolbar:** Before/After layouts, soft proofing toggle, grid overlay.
- **Copy, paste and sync:** choose which setting groups to copy; sync applies the active photo's changes to the
  selection; Auto Sync makes every change apply to all selected photos; Previous pastes the last photo's settings.
- **Merges:** HDR, panorama and HDR panorama produce a new DNG beside the sources, with a headless variant that
  reuses the last settings.
- **Soft proofing:** preview the photo through a printer or output profile with rendering intent and paper/ink
  simulation; out-of-gamut warnings for monitor and destination; "create proof copy" makes a virtual copy for the
  output-specific edit.

## Map

Purpose: geotag and browse by place.

- A slippy map with selectable styles, search for places, zoom.
- Geotagged photos appear as pins; nearby pins cluster with a count; hovering shows a preview strip.
- Drag photos from the filmstrip onto the map to set their location.
- Track logs (GPX) tag photos by time, with a time-zone offset; the track is drawn on the map.
- Saved locations are named circles; marking one "private" strips GPS on export for photos inside it.
- A location filter bar narrows the filmstrip to photos visible on the map, tagged, or untagged.
- Reverse geocoding fills city/state/country from coordinates (we offer it as opt-in online or offline data).

## Book

Purpose: lay out a multi-page photo book.

- Book settings: output type (we ship PDF and JPEG pages), size, cover style, paper, optional logo page.
- Auto layout from layout presets; page templates by photo count and text.
- Pages and spreads with page numbers; guides for bleed, safe text area and photo cells.
- Cells have padding; photos are zoomed and panned inside their cell; photo text (captions) and page text with full
  type controls.
- Backgrounds: colour, graphic, or a photo with opacity.
- Saved books are a special collection that remembers layout and photos.

## Slideshow

Purpose: build and play or export a slideshow.

- Templates and a preview browser; user templates.
- Options: zoom to fill, stroke border, cast shadow. Layout guides and margins.
- Overlays: identity plate, rating stars, text overlays with tokens (title, caption, filename…), watermark.
- Backdrop: colour wash, background image, background colour. Intro and ending title screens.
- Music: one or more tracks, fit the slide timing to the music, audio balance with video clips.
- Playback: slide and fade duration, random order, repeat, pan-and-zoom, preview quality.
- Export as PDF, JPEG sequence or video. Saved slideshows are special collections.

## Print

Purpose: lay out pages and print with colour management.

- Layout styles: single image / contact sheet (a grid), picture package (one photo in several sizes, auto-packed),
  custom package (free cells).
- Image settings: zoom to fill, rotate to fit, repeat one photo per page, stroke border.
- Layout: margins, page grid, cell spacing and size, keep square; rulers and guides.
- Page: background colour, identity plate, watermark, page numbers, page info, crop marks, photo info text.
- Print job: to a printer or to a JPEG file; draft mode; resolution; output sharpening by media type; 16-bit
  output; colour management with a printer profile and rendering intent; print adjustment for brightness and
  contrast.
- Page setup, "print one" without the dialog, templates and saved prints.

## Web

Purpose: generate a static web gallery.

- Layout styles (grid and other gallery templates); a template browser.
- Site info (title, description, contact, link), colour palette, appearance (grid size, borders, image sizes).
- Image info (title and caption from tokens), output settings (size, quality, watermark, metadata policy).
- Upload settings (we plan SFTP) or export to a folder. Saved web galleries are special collections.
- For us, an Immich shared link is an alternative "publish" target (IMM-SHARELINK).
