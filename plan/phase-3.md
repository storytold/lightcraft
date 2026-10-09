# Phase 3: Output modules (Print, Map, Slideshow, Book, Web)

*Copy of `PLAN_phase_3.md` at the repository root, taken 2026-10-10. The root file is the working copy; refresh this one when it changes.*

Part of [PLAN.md](../PLAN.md). **Estimate:** 72–124 agent-hours. **Depends on:** Phase 1 (module system).
Print also needs Phase 2.9 (`cms`); until then it uses the existing output-space transform.
**Runs in parallel with:** Phases 2 and 4.

**Goal:** every Lightroom Classic output module works end to end, sharing one layout and text engine and one PDF
writer.

Functional references: the darktable application's map, print, slideshow and tethering views, its web-gallery and
photo-book exports, and its user manual. Use them as **running applications and documentation only**; their source
is never read (MIT clean-room, PLAN.md §4.2).

---

## 3.1 Shared foundations (≈ 15–25 h)

| Crate | Layer | Contents |
|---|---|---|
| `text` | L1 | **Port of PhotoCraft `text`**: font database, parley shaping (complex scripts), skrifa, CJK/vertical, craft-fonts loading. Replaces LightCraft's ab_glyph watermark renderer, and the export watermarks use it too |
| `pdf` | L1 | **New** PDF 1.7 writer: multi-page, TrueType/OpenType **subsetting and embedding**, images (JPEG passthrough, Flate 8/16-bit), ICC-based colour spaces and **OutputIntent**, page boxes (media/bleed/trim), vector paths for guides and crop marks, metadata. PhotoCraft's single-page `print_cmds.rs` writer is a starting point for crop and registration marks only |
| `layout` | L3 | Page model shared by Print, Book, Slideshow and Web: page size, margins, bleed; **cells** (photo cell with fit/fill, zoom, pan and rotate; text cell; graphic cell); grids and packages; guides; snapping; **templates** (serde, user-saveable, built-ins we design); token text (`{Title}`, `{Caption}`, `{Filename}`, `{Date}`, `{Exposure}`, …, shared with the export naming templates) |
| `ui` widgets | L5 | Page canvas with zoom, rulers, guides, cell handles and drag-and-drop from the filmstrip; template browser with live previews; text style panel |

- A **"saved creation"** catalog object type (Saved Print / Book / Slideshow / Web Gallery) is stored as a special
  collection with the layout document attached, so it appears in the Collections panel exactly as in Classic.
- All of it is reachable as commands (`print.*`, `book.*`, …) for MCP and CLI. `app-cli print --template …
  --collection …` renders headless.

---

## 3.2 Print module (≈ 14–22 h)

- **Layout styles:**
  - Single Image / Contact Sheet (grid: rows, columns, spacing, cell size, keep square);
  - Picture Package (fixed cell sizes, auto layout, new page);
  - Custom Package (free cells, anchor, lock to photo aspect ratio).
- **Image settings:** zoom to fill, rotate to fit, repeat one photo per page, stroke border, inner stroke.
- **Guides:** rulers, page bleed, margins and gutters, image cells, dimensions.
- **Page:**
  - page background colour;
  - identity plate (with rotation, opacity, render on every image);
  - watermark (shared with export);
  - page options (page numbers, page info, crop marks);
  - photo info tokens;
  - font size.
- **Print job:**
  - to printer, JPEG file or **PDF file**;
  - draft mode (uses previews);
  - print resolution;
  - print sharpening (Low/Standard/High × Matte/Glossy);
  - 16-bit output;
  - colour management: printer-managed or a chosen ICC profile with Perceptual/Relative intent and BPC (`cms`), plus
    a Print Adjustment brightness/contrast compensation.
- **OS printing:**
  - Linux and macOS: **IPP to CUPS** in pure Rust. Send the rendered PDF or raster; query printer media sizes and
    margins for the page-setup dialog.
  - Windows: the Win32 print spooler in an isolated `platform-print` crate (the only `unsafe` here, documented in
    AGENTS.md as `sysmem` is). PDF/JPEG output always works.
- Page Setup, Print, Print One; templates (built-ins and user); saved prints.

**Done when:**
- A 2×2 contact sheet, a picture package and a custom layout print to PDF with embedded fonts and the correct ICC
  OutputIntent.
- A CUPS test printer (`cups-pdf` in CI) receives the job.
- Measured colour through a profile is within ΔE 1 of the soft proof.

---

## 3.3 Map module (≈ 12–20 h)

- **New crate `geo`** (L3):
  - Web-Mercator tiles: fetch through `net`, disk cache with size limit and expiry;
  - **configurable tile servers**: OpenStreetMap default, respecting its tile usage policy (User-Agent from `brand::`,
    attribution shown, no bulk prefetch), plus user-added XYZ servers, satellite and terrain;
  - clustering (grid-based, zoom-dependent);
  - GPX/KML/GeoJSON track parsing (GPX already exists);
  - saved locations (centre, radius, private flag).
- **Map view (egui):**
  - smooth zoom/pan, style picker, search box (geocoding);
  - pins and clusters with hover previews;
  - multi-select;
  - **drag photos from the filmstrip onto the map to geotag**, and drag pins to move them.
- **Tracklogs:**
  - load, show and choose tracks;
  - time-zone offset;
  - **Auto-tag selected photos**; set the offset by matching one photo to a known position.
- **Saved Locations panel:** create from the map, radius circle, **private** flag (GPS stripped on export of photos
  inside it), photo counts.
- **Location filter bar:** visible on map, tagged, untagged, by saved location.
- **Reverse geocoding** (fills Sublocation/City/State/Country/ISO code):
  - opt-in, with a choice of provider: an online Nominatim-compatible endpoint with rate limits, or an **offline
    GeoNames** cities dataset (CC-BY 4.0), downloaded on request and attributed;
  - never sends anything without consent.
- If Immich is connected, an option shows Immich-only assets (linked but not in the catalog) as ghost pins. Off by
  default.

---

## 3.4 Slideshow module (≈ 10–16 h)

- **Options:** zoom to fill, stroke border, cast shadow (opacity, offset, radius, angle).
- **Layout:** guides, margins (linked or independent), aspect preview (screen, 16:9, 4:3).
- **Overlays:** identity plate, rating stars, watermark, text overlays with tokens (positioned and anchored to the
  photo), shadow on text.
- **Backdrop:** colour wash with angle, background image with opacity, background colour.
- **Titles:** intro and ending screens with colour and identity plate.
- **Playback:**
  - manual or automatic; slide and fade durations;
  - colour fade; random order; repeat;
  - **pan and zoom** (Ken Burns, amount);
  - quality (draft or full).
- **Music:**
  - several tracks;
  - **fit slide durations to the music**;
  - volume and balance.
  - Decode with `symphonia` (pure Rust: MP3, AAC, FLAC, Vorbis, WAV). Output with `cpal`; on Linux this links the
    system ALSA library, so accept that and document it, or add a pure-Rust PipeWire/Pulse backend later.
- **Export:**
  - PDF slideshow;
  - JPEG sequence;
  - **video**: AV1 in MP4/WebM with pure-Rust `rav1e` encode and Opus/FLAC audio.
  - H.264 MP4 depends on the Phase 5 video decision, where an OS encoder is an option.
- Templates and saved slideshows; preview in the content area; full-screen play on the main or secondary display.

---

## 3.5 Book module (≈ 12–20 h)

- **Book settings:**
  - book types **PDF** and **JPEG** (Blurb ordering is a proprietary service and out of scope);
  - sizes (small square, standard landscape/portrait, large landscape, letter, A4, custom);
  - cover type (hardcover, softcover, none);
  - paper type as a note;
  - JPEG quality, colour profile, file resolution, sharpening.
- **Auto Layout:** presets (one photo per page, left blank, with text, fill), "Auto Layout" and "Clear Layout".
- **Page:** page templates by photo count (1–4+, with or without text); favourite templates; add page or blank; page
  numbers (position, display, apply to all).
- **Guides:** page bleed, text safe area, photo cells, filler text.
- **Cells:** padding (linked or per side), zoom and pan inside the cell, swap photos by dragging.
- **Text:**
  - photo text (tokens or custom, offset);
  - page text;
  - **Type panel**: font, style, size, opacity, colour, tracking, baseline, leading, kerning, columns, gutter,
    alignment;
  - text style presets.
  - Uses the `text` crate.
- **Background:** apply to all pages, graphic or photo with opacity, colour.
- **Views:** multi-page, spread, single page, zoomed page.
- **Export:** a PDF with embedded fonts and ICC (one file, cover plus pages, spreads as single pages) or a JPEG per
  page.
- Saved books.

---

## 3.6 Web module (≈ 8–14 h)

- **Gallery templates** we write ourselves: grid, square, track and a single-image viewer. They are static
  HTML/CSS/minimal JS with no external CDN, responsive, with keyboard navigation and accessible alt text from the
  captions.
- **Panels:**
  - Site Info: title, collection title and description, contact, web or mail link;
  - Colour Palette;
  - Appearance: grid size, cell numbers, photo borders, image page size;
  - Image Info: title and caption tokens;
  - Output Settings: large image size, quality, metadata (all / copyright only), watermark, sharpening.
- **Preview:** live in-app preview of the rendered HTML.
- **Output:** export to a folder, or **upload** via **SFTP** (pure-Rust `russh`/`russh-sftp`); plain FTP is optional
  and off by default. Upload server presets with credentials in the keychain (Phase 1.7).
- **IMM-SHARELINK:** as an alternative output, "Share via Immich":
  - upload the gallery's photos to an Immich album (Phase 4 IMM-PUBLISH);
  - create an **Immich shared link** with expiry, password, allow-download and show-metadata options;
  - copy the URL.
  - The same action is offered in Slideshow (share the selection) and in the Publish Services context menu.
- Saved web galleries.

---

## Exit gate

- [ ] `text`, `pdf` and `layout` crates done, with golden PDF tests (rasterised compare) and font-embedding checks.
- [ ] Print: all three layout styles, colour-managed PDF/JPEG/printer output; CUPS CI job green.
- [ ] Map: tiles, pins/clusters, drag-to-geotag, tracklogs, saved locations with privacy, location filter, opt-in
      reverse geocoding.
- [ ] Slideshow: templates, overlays, music sync, PDF/JPEG/AV1 video export.
- [ ] Book: auto layout, templates, text, PDF/JPEG export.
- [ ] Web: templates, live preview, folder and SFTP output; Immich shared link (IMM-SHARELINK ✅).
- [ ] Saved creations appear in Collections; every module scriptable via MCP.
- [ ] All LRC-PRINT/MAP/SS/BOOK/WEB tracker rows ✅.
