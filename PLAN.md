# Lightroom Classic clone in pure Rust: feature list, inputs and plan to completion

*Written 2026-10-10 from a survey of the five folders next to this one: `darktable`, `ansel`, `lightcraft`,
`photocraft` and this empty `nonameyet`.*

**Detailed phase plans:** [Phase 0](PLAN_phase_0.md) · [Phase 1](PLAN_phase_1.md) · [Phase 2](PLAN_phase_2.md) ·
[Phase 3](PLAN_phase_3.md) · [Phase 4](PLAN_phase_4.md). Phases 5–6 are described in §6 below.

**Product name:** not chosen yet, and never hard-coded. It lives in one file, `brand.toml`, and can be changed at any
time; see [Phase 0 §0.2](PLAN_phase_0.md#02-the-name-lives-in-one-config-file-brandtoml). "NoNameYet" below is a
placeholder.

---

## 0. TL;DR

1. **Fork LightCraft; don't start from zero.**
   - LightCraft is about 174k lines of pure Rust and compiles cleanly (`cargo check --workspace` passes).
   - It already covers about 80% of Lightroom features by row count (P0 core features 98%). Its own honest estimate
     is 60–70% of a real day-to-day replacement, and about 45% for a *Lightroom Classic* power user.
   - Rebuilding that from scratch would cost hundreds of agent-hours and gain nothing.
   - "From scratch in Rust" still holds: every line of the product stays Rust we wrote. No C/C++ is linked and no
     foreign code is pasted in.
2. **LightCraft is built on the *cloud* Lightroom model.** It has a single window with view modes and a right-hand
   panel. Classic is a **module** application (Library | Develop | Map | Book | Slideshow | Print | Web) with a
   **catalog** and Classic panels and keys. The fork's first real job is the Classic shell and a catalog that scales.
3. **What remains is mostly quality, data and output modules, not buttons.** In order of pain:
   - camera colour calibration
   - lens-profile database
   - CR3 and compressed ORF/SRW raws
   - full-resolution zoom
   - the output modules: Map, Print, Book, Slideshow, Web, publish services and tethering
   - AI masks and denoise
   - HDR and video
4. **PhotoCraft donates finished parts.** These are a real Remove tool (PatchMatch content-aware fill), a full ICC
   colour-management engine (printer soft proofing), text shaping (Print, Book, identity plate), pressure-sensitive
   brushes, a layered PSD writer ("Edit in…" round trips), a WASM plugin sandbox and action recording.
5. **Licence: MIT (decided).** darktable and Ansel are GPL-3.0+, so they serve as **behaviour and design references
   only**: run the apps, read their manuals and prose design notes, never their source code or GPL data tables
   (section 4). Camera colour, noise and white-balance data are measured by us (Phase 2).

---

## 1. What is in this folder

| Project | Language / licence | Size | What it is | Role in this plan |
|---|---|---|---|---|
| **lightcraft** | Rust, MIT OR Apache-2.0 | ~174k LoC, 1,117 commits, active (last commit 2026-10-09) | Clean-room Lightroom (cloud-style) clone: egui UI, wgpu compute pipeline, command engine, MCP, WASM build | **Fork base** |
| **photocraft** | Rust, MIT OR Apache-2.0 | ~365k LoC, ~1,049 commits | Clean-room Photoshop clone, same conventions as LightCraft (no shared code; crates were copied and have diverged) | **Donor of crates** |
| **darktable** | C/C++, GPL-3.0+ | 96 IOP modules | The most complete open-source raw developer: lighttable, darkroom, map, print, slideshow, tethering, Lua, ONNX AI, MCP | **Behaviour and feature reference** (app and manual only, no source: §4) |
| **ansel** | C, GPL-3.0+ | darktable fork from May 2022 | darktable re-engineered: incremental pipeline cache, simplified UI, LensSerious lens DB, dependency-free ML raw denoise | **Design reference** (prose notes in `ansel/doc/`, no source) |

---

## 2. Complete Lightroom Classic feature list

Status column `LC`: LightCraft today. ✅ works · 🟡 partial · ⬜ missing · ? not verified.
"Src" is where the missing piece can come from: **PC** = PhotoCraft, **DT** = darktable, **AN** = Ansel (both as running app, manual or prose notes only, never source: §4), **new** = write it.

### 2.1 Application shell and cross-cutting features

| Feature | LC | Src / notes |
|---|---|---|
| Module picker (Library, Develop, Map, Book, Slideshow, Print, Web), with configurable hidden modules | ⬜ | new: LightCraft has no module system |
| Identity plate (text or graphic, custom fonts) and activity centre/progress | 🟡 | PC `text` for identity plate; LightCraft has an Activity panel |
| Panel system: left/right/top/bottom panels, auto hide/show, solo mode, Tab/⇧Tab/F5–F8, T toolbar | ⬜ | new (KEYC-PANELS) |
| Screen modes (normal, full screen, full screen with menu bar), lights out (dim/off) | 🟡 | new (KEYC-VIEWS) |
| Secondary display window: Grid, Loupe (normal/live/locked), Compare, Survey, Slideshow | 🟡 | second window exists; keys and locked/live modes missing |
| Filmstrip with source indicator, filters, thumbnail badges | ✅ | |
| Classic keyboard map (all module keys, Ctrl+Alt+1..7 module switches) | 🟡 | new: LightCraft deliberately remaps some keys; add a "Classic keymap" layer |
| Keyboard shortcut editor | 🟡 | settings exist; editor is a M16 item |
| Preferences: General, Presets, External Editing, File Handling, Interface, Performance, Display, Network | 🟡 | partial in LC |
| Catalog settings: backup schedule, preview size and quality, auto-discard of 1:1 previews, XMP auto-write, address lookup, face detection | 🟡 | new pieces |
| Undo/redo across modules, history | ✅ | engine op log |
| Plugin manager and SDK (Lua in Lightroom Classic) | ⬜ | PC `plugins` (WASM sandbox); see §6.5 |
| Help, keyboard cheat-sheet overlay per module (⌘/), What's new | 🟡 | |
| Localisation, accessibility | 🟡 | 8 languages in LC |

### 2.2 Catalog

| Feature | LC | Src / notes |
|---|---|---|
| Single-file catalog of hundreds of thousands of photos, crash safe | 🟡 | LC op-log plus snapshot works to about 85k photos; the whole catalog sits in RAM. Needs an on-disk indexed store for over 500k (§6.2) |
| Multiple catalogs: open, create, open recent, choose at startup | ? | |
| Backup on exit with integrity test and optimise | ⬜ | new |
| Export as Catalog (subset, with or without negatives and previews) / Import from another catalog (merge, conflict policy) | ⬜ | new; LC already has a `.lrcat` read-only importer |
| Upgrade/migrate from a Lightroom Classic `.lrcat` (ratings, keywords, collections, develop settings, history) | 🟡 | LC has it (pure-Rust SQLite reader); history replay and Adobe-profile mapping are incomplete. mapping comes from published `crs:` descriptions and our own measurements (Phase 4.5) |
| Previews: minimal, embedded and sidecar, standard, 1:1; Smart Previews (lossy DNG) for offline editing | 🟡 | smart previews exist in LC |
| Missing file and folder detection, relink, "Find missing photos" | 🟡 | relink exists for the catalog import |
| Metadata ↔ XMP sync, conflict badges ("metadata changed on disk"), read/save metadata from/to file | 🟡 | XMP read/write exists; conflict UI missing |

### 2.3 Import

| Feature | LC | Src / notes |
|---|---|---|
| Import dialog: source panel (devices, folders), Copy as DNG / Copy / Move / Add | ✅ | |
| Grid with checkboxes, "new photos" / destination-folder views, suspected duplicates greyed out | ✅ | |
| File handling: build previews, smart previews, don't import duplicates, second copy (backup) to, add to collection | 🟡 | verify "make a second copy" |
| File renaming templates (template editor with tokens) | ✅ | |
| Apply during import: develop preset, metadata preset, keywords | ✅ | |
| Destination: into subfolder, organise by date (formats) | ✅ | |
| Import presets | ? | |
| Auto Import (watched folder) | ✅ | |
| Tethered capture: camera controls bar, shot naming, session, develop preset on capture, live view | ⬜ | new pure-Rust PTP/USB (`nusb`); AN's "studio capture" folder watch is a cheap first step |
| Formats: all raws (CR3, compressed ORF/SRW and the long tail), DNG, JPEG, TIFF, PSD, PNG, HEIC/HEIF, AVIF, JXL, video files | 🟡 | see §2.6 Raw |

### 2.4 Library module

| Feature | LC | Src / notes |
|---|---|---|
| Views: Grid (compact/expanded cells, cell styles, badges), Loupe (info overlays 1/2), Compare (select/candidate, linked zoom), Survey, People | 🟡 | People view lacks cluster separators |
| Navigator panel with zoom levels (Fit/Fill/1:1/custom) | ? | |
| Catalog panel: All Photographs, Quick Collection, Previous Import, Missing photos | 🟡 | Quick Collection ⬜ |
| Folders panel: volumes with free space, hierarchy, show parent, add/move/rename/remove folders, synchronise folder, update location, show in Finder | 🟡 | rename/move pending |
| Collections: collection, smart collection (full rule editor, nested AND/OR), collection set, target collection (B key), sync with mobile (out of scope) | 🟡 | smart albums ✅; target collection and sets need checking |
| Publish Services panel | ⬜ | §2.12 |
| Library Filter bar: Text / Attribute / Metadata (columns: date, camera, lens, keyword, label, etc.) / None, filter presets, lock | ✅ | KEYC filter-bar keys partial |
| Flags (P/X/U, ` to toggle), auto-advance, star ratings, colour labels and label sets, `[`/`]` rating up/down | 🟡 | `[`/`]` and flag cycling missing |
| Stacking: group, unstack, collapse/expand, auto-stack by capture time, move to top of stack | ✅ | |
| Virtual copies, set copy as master | ✅ | |
| Quick Develop panel (relative ± buttons, presets, crop ratio, treatment) | ? | check; Classic-specific |
| Keywording panel: keyword tags, suggestions, keyword sets (Ctrl+Alt+0–9) | 🟡 | keyword sets ⬜ |
| Keyword List: hierarchy, synonyms, export flags, import/export keyword lists, filter by keyword | 🟡 | |
| Painter tool (spray keywords, labels, flags, ratings, metadata, settings, rotation, target collection) | ⬜ | |
| Metadata panel: default, EXIF, IPTC, IPTC Extension, Location, Large Caption, custom; metadata presets; edit many; copy/paste metadata | 🟡 | metadata copy/paste ⬜ |
| Comments panel (from shared collections) | ⬜ | low value without cloud; can drop |
| Edit capture time (shift, set, use file date), batch | ? | |
| Convert to DNG, Find previous process version, Update DNG previews | 🟡 | DNG export exists |
| Face detection and People view (named, unnamed, cluster, confirm) | 🟡 | LC faces MVP |
| Show in Explorer/Finder, Edit In external editor, Open as Layers / Smart Object (Photoshop round trip) | 🟡 | external editor ✅; PC `psd` writer for layered round trip |
| Slideshow (impromptu, ⌘↵) | ✅ | basic |

### 2.5 Develop module

| Feature | LC | Src / notes |
|---|---|---|
| Left panel: Navigator, Presets (groups, favourites, amount), Snapshots, History (named steps, clear), Collections | ✅ | snapshots and versions exist |
| Toolbar: Before/After (left-right, top-bottom, split), loupe, grid overlays, soft proofing toggle | ✅ | |
| Histogram with clipping indicators (J), RGB readout, interactive drag on histogram | 🟡 | KEY-HISTOGRAM partial |
| Tool strip: Crop and Straighten (aspect lock, angle, auto, constrain), Remove/Spot Removal (content-aware, heal, clone; visualize spots), Red Eye/Pet Eye, Masking | 🟡 | content-aware is heal from an auto source: port PC `algo/inpaint.rs` (PatchMatch) and `content_aware.rs` |
| Basic: treatment (colour/B&W), profile browser, WB (as shot, auto, presets, eyedropper, temp/tint), exposure, contrast, highlights, shadows, whites, blacks, texture, clarity, dehaze, vibrance, saturation, Auto | ✅ | tone tuned by eye; fidelity suite is §6.3 |
| Tone Curve: parametric (regions, split points), point curve (RGB, R, G, B), targeted adjustment, refine saturation | ✅ | curves apply after display encoding: check against LR behaviour |
| HSL/Color: hue/saturation/luminance per 8 bands, targeted adjustment; B&W mix | ✅ | |
| Point Color (sample, range, ranges visualisation) | ✅ | |
| Color Grading (3 wheels plus global, blending, balance) / legacy split toning | ✅ | |
| Detail: sharpening (amount, radius, detail, masking with Alt preview), NR (luminance, detail, contrast; colour, detail, smoothness) | ✅ | profiled NR with our own measured noise profiles (Phase 2.6); DT's profiled denoise is the behaviour benchmark |
| Enhance: AI Denoise, Raw Details, Super Resolution (outputs a DNG) | 🟡 | Bayer AI denoise only; design from AN's prose note on `rawdenoiseai` (deterministic U-Net, no runtime) |
| Lens Corrections: profile (auto lens lookup, custom), remove CA, defringe (purple/green with eyedroppers), manual distortion and vignetting | 🟡 | **No lens-profile DB**: §6.3 |
| Transform: Upright (Auto, Level, Vertical, Full, Guided), manual transforms | ✅ | |
| Effects: post-crop vignette (styles), grain | ✅ | |
| Calibration: process version, shadow tint, RGB primaries hue/saturation | ✅ | process versions are LC's own |
| Profiles: Adobe-style looks, camera-matching profiles (Camera Standard, Portrait…), creative profiles with amount, LUT profiles, legacy | 🟡 | own looks ✅; **camera-matching** and **measured camera colour** ⬜ |
| Masking: brush (auto mask, flow, density, pressure), linear, radial, luminance range, colour range, depth range, Select Subject, Select Sky, Background, Objects, People (face, eyes, lips, hair, body, clothes), Landscape (water, vegetation, mountains…), add/subtract/intersect, invert, mask overlay modes, update AI masks, adaptive presets | 🟡 | AI masks are heuristics; SAM 3 path exists without hosted weights; pressure brushes from PC `paint`/`tablet` |
| Lens Blur (AI depth, bokeh shapes, focal range) | ⬜ | needs a depth model |
| Generative Remove | 🚫 | Adobe cloud; local PatchMatch instead |
| Copy/paste/sync settings (selective), Auto Sync, Previous, Reset | ✅ | |
| Presets: create from groups, import (`.xmp`, `.lrtemplate`), amount slider, adaptive presets | ✅ | adaptive ⬜ |
| Reference View (side by side with a reference photo) | ✅ | |
| Soft proofing: profile, rendering intent (perceptual/relative), Simulate Paper & Ink, destination gamut warning, create proof copy | 🟡 | **use PC `cms`** (ICC v2/v4, LUT profiles, BPC, gamut check) |
| HDR editing (extended range, HDR display, visualise, SDR preview, HDR export) | ⬜ | §6.6 |
| Photo Merge: HDR (deghost, auto align, auto settings), Panorama (projections, boundary warp, fill edges), HDR Panorama | ✅ | |
| Develop view options (info overlay, show messages) | ⬜ | small |
| Zoom: true 1:1 and higher from the original pixels | 🟡 | 2,560 px preview cap; region renderer exists for loupe only (§6.2) |

### 2.6 Raw and camera support

| Feature | LC | Src / notes |
|---|---|---|
| DNG all variants incl. 1.7 JXL, opcodes, semantic masks | ✅ | |
| CR2, NEF, ARW, RAF incl. X-Trans compressed, RW2/RWL, PEF, uncompressed ORF/SRW | ✅ | |
| **CR3** (all CRX variants, sRAW/mRAW), **compressed ORF**, **compressed SRW**, NEF lossy-after-split | 🟡/⬜ | LC: only M50/R100/R8 verified. Most common missing format |
| Long tail: 3FR, IIQ, MOS, ERF, KDC, DCR, MRW, X3F, NRW, etc. | ⬜ | |
| Per-camera colour matrices, WB presets, noise profiles, black/white levels, crop masks | 🟡 | **biggest quality gap**; measured by us (Phase 2.1, rules in §4) |
| Lens-correction profiles for camera/lens pairs | 🟡 | DNG and RW2 embedded only |

### 2.7 Map module

| Feature | LC | Src / notes |
|---|---|---|
| Map view with styles (road, satellite, terrain, light/dark), zoom, search location | ⬜ | new: slippy-map tile renderer in egui, configurable tile server plus disk cache |
| Pins and clusters for geotagged photos, hover previews | ⬜ | |
| Drag photos onto the map to geotag | ⬜ | |
| Tracklog import (GPX), auto-tag with time offset | 🟡 | GPX geotagging ✅; map display ⬜ |
| Saved locations (radius, private flag to strip GPS on export) | ⬜ | |
| Location filter bar (visible on map, tagged, untagged) | ⬜ | |
| Reverse geocoding (city, state, country fill) | 🚫 in LC | opt-in online service, or offline GeoNames (CC-BY) |

### 2.8 Book module

| Feature | LC | Src / notes |
|---|---|---|
| Book settings: type (Blurb/PDF/JPEG), size, cover, paper, logo page | ⬜ | Blurb upload is proprietary; ship PDF and JPEG |
| Auto Layout with presets, page templates (1–4+ photos, text), custom pages | ⬜ | new |
| Page and spread editing, page numbers, guides (bleed, safe text, filler text) | ⬜ | |
| Cell padding, photo zoom/pan in cell, photo text and page text, text styles (font, size, tracking, baseline, leading, kerning, alignment) | ⬜ | **PC `text`** (parley shaping, CJK) |
| Background (graphic, colour, photo), multi-page and spread views | ⬜ | |
| Export book to PDF (embedded fonts, ICC, multi-page) and JPEG | ⬜ | needs a real PDF writer (PC's is single page and has no font embedding) |
| Saved books (special collection type) | ⬜ | |

### 2.9 Slideshow module

| Feature | LC | Src / notes |
|---|---|---|
| Templates (built-in, user), template browser preview | ⬜ | |
| Options: zoom to fill, stroke border, cast shadow | ⬜ | |
| Layout guides and margins, overlays (identity plate, rating stars, text overlays with tokens, watermark) | ⬜ | PC `text` |
| Backdrop: colour wash, image, background colour | ⬜ | |
| Titles: intro/ending screens | ⬜ | |
| Music: multiple tracks, sync slides to music, audio balance | ⬜ | pure-Rust decode (`symphonia`) + output (§6.5 notes) |
| Playback: duration, fades, random order, repeat, pan and zoom, quality | 🟡 | impromptu basic only |
| Export to PDF, JPEG sequence, video (MP4/H.264) | ⬜ | video encode: pure-Rust AV1 (`rav1e`) feasible; H.264 is a risk (§6.6) |
| Saved slideshows | ⬜ | |

### 2.10 Print module

| Feature | LC | Src / notes |
|---|---|---|
| Layout styles: Single Image/Contact Sheet, Picture Package, Custom Package | ⬜ | new |
| Image settings: zoom to fill, rotate to fit, repeat one photo per page, stroke border | ⬜ | |
| Layout: margins, page grid, cell spacing and size, keep square; rulers, guides, page bleed, dimensions | ⬜ | |
| Page: background colour, identity plate, watermark, page options (numbers, info, crop marks), photo info, text | ⬜ | PC `text`; PC `print_cmds.rs` has crop/registration marks |
| Print job: to printer or JPEG file, draft mode, print resolution, sharpening (low/standard/high, matte/glossy), 16-bit output, colour management (printer ICC, rendering intent, print adjustment brightness/contrast) | ⬜ | **PC `cms`**; OS printing through IPP/CUPS (pure-Rust `ipp`) on Linux and macOS; Windows needs Win32 print API in an isolated `unsafe` crate |
| Page setup / printer settings, Print One | ⬜ | |
| Templates and saved prints | ⬜ | |

### 2.11 Web module

| Feature | LC | Src / notes |
|---|---|---|
| Layout styles (HTML grid, track/square/etc. galleries) with templates | 🚫 in LC | new: static HTML/CSS/JS templates we write; DT's web-gallery export is a behaviour reference |
| Site info, colour palette, appearance, image info, output settings (size, quality, watermark, metadata) | ⬜ | reuse export pipeline |
| Upload settings (FTP/SFTP) and Export to folder | ⬜ | pure-Rust SFTP (`russh`); plain FTP optional |
| Saved web galleries | ⬜ | |

### 2.12 Export, publish and sharing

| Feature | LC | Src / notes |
|---|---|---|
| Export dialog: location, naming, video, file settings (JPEG/PSD/TIFF/PNG/DNG/AVIF/JXL/HEIF/Original), colour space, bit depth, resize, output sharpening, metadata policy, watermark, post-processing action | ✅ | JXL encode and HDR ⬜; PSD export via PC `psd` |
| Export presets, export with previous, background export queue | ✅ | |
| Watermark editor (text/graphic, shadows, anchors) | ✅ | text shaping upgrade with PC `text` |
| Publish services: Hard Drive, Flickr, Adobe Stock, Facebook, etc.; publish collections, "modified photos to re-publish", comments | ⬜ | new: framework plus Hard Drive first; third-party APIs as plugins |
| Email photos | ⬜ | low priority (mailto with attachments or SMTP) |
| Post-processing: open in other app, export actions | ? | |

### 2.13 Video

| Feature | LC | Src / notes |
|---|---|---|
| Import and catalog video | ✅ | |
| Playback, trim, poster frame, capture frame as JPEG | ⬜ | **riskiest pure-Rust item** (§6.6) |
| Quick Develop/presets on video, export video | ⬜ | |

### 2.14 Immich integration (beyond Lightroom)

[Immich](https://immich.app) is a self-hosted photo server (AGPL-3.0). We talk to it only over its HTTP API, with a
client we write ourselves. Tracker section **IMM**.

| Feature | Id | Phase |
|---|---|---|
| Connect to one or more Immich servers (URL + API key in the OS keychain), version check, self-signed TLS trust | IMM-CONNECT | 1 |
| Link catalog photos to Immich assets by SHA-1 checksum; "in Immich" badge, filter, smart-collection rule, Open in Immich | IMM-LINK | 1 |
| Immich as an Import source (timeline, albums, people, favourites); copy originals or add linked | IMM-IMPORT | 1 |
| Shared-originals mode: Immich external library over the same folders, path mapping, no duplicate uploads | IMM-EXTLIB | 1 |
| Immich shared links from Web/Slideshow/Publish (expiry, password, download) | IMM-SHARELINK | 3 |
| Immich publish service: collections → albums; renders, originals, or both stacked; re-publish; trash-only removal | IMM-PUBLISH | 4 |
| Two-way metadata sync: rating, favourite ↔ pick, caption ↔ description, keywords ↔ tags, collections ↔ albums, GPS/time; conflict view | IMM-SYNC | 4 |
| Import Immich people and face regions; push names back | IMM-PEOPLE | 4 |
| Immich smart (CLIP) and metadata search from the Library filter bar | IMM-SEARCH | 4 |
| All Immich actions as commands (CLI, control channel, MCP) | IMM-MCP | 4 |

### 2.15 Automation (beyond Lightroom)

| Feature | LC | Notes |
|---|---|---|
| Every action is a command; CLI; JSON control channel; MCP server; headless UI snapshots | ✅ | Strong differentiator; keep it. DT's `src/mcp` tool list is a good checklist for MCP coverage |
| Recordable actions / batch scripts | ⬜ | PC `engine/src/actions_cmds.rs` |

---

## 3. What each existing project gives us

### 3.1 LightCraft (fork base): keep almost everything

- **Workspace and quality bar.** It has layered crates enforced by `cargo xtask layers` (L0 geom/color/raster through
  L5 UI). Production code may not panic, and `unsafe` is confined to one crate. CI runs fmt, clippy, tests, layers
  and wasm, plus a parity tracker with an automatic checker.
- **Develop pipeline** (`crates/pipeline`, `crates/develop`, `crates/gpu`):
  - scene-referred float pipeline in linear Rec.2020, with OkLab/OkLCh colour tools;
  - per-stage cache;
  - a wgpu compute twin that matches the CPU within 1/255.
- **Raw** (`crates/raw`, 18.5k lines): the most complete pure-Rust clean-room decoder set around. It covers DNG 1.7,
  CR2, partial CR3, NEF, ARW, RAF X-Trans, every RW2 format and PEF, plus AHD/PPG/X-Trans demosaic and a DNG writer.
- **Library and catalog:**
  - crash-safe op-log catalog;
  - albums, smart albums, stacks, virtual copies;
  - filter and search, XMP interop;
  - `.lrcat` importer, GPX geotagging, faces MVP.
- **Edit tools:** every global slider, masking, heal/clone, red eye, Upright, HDR/panorama merge, presets, versions
  and sync.
- **Export:** JPEG, PNG, TIFF, WebP, AVIF and DNG, with naming templates, watermarks and background jobs.
- **Automation:** the command engine, CLI, control channel and MCP server.
- **Platforms:** desktop builds plus a web build.
- **Must change or remove:**
  - The ArtCraft brand (`docs/brand/` is not open licensed).
  - Names (`lightcraft-*` crates).
  - The cloud-style UI shell.
  - The in-RAM catalog at very large scale.
  - Rebuild the missing local `plan/` reference folder that the tracker ids point at.
  - The 45k-line `engine` crate needs splitting before Print, Book and the other new modules go into it.

### 3.2 PhotoCraft (donor): port these crates with copy-and-rename, as LightCraft already did with `heif`

| PhotoCraft piece | Gives the clone | Where it lands |
|---|---|---|
| `algo/src/inpaint.rs`, `content_aware.rs` (PatchMatch, Wexler EM, Poisson blend) | Real content-aware Remove, auto heal source | Develop → Remove |
| `algo/src/segment/` (GrabCut, maxflow, SLIC, saliency, sky) | Classical fallback and edge refinement for AI masks | Masking |
| `cms` (5k lines, own ICC v2/v4 engine, LUT/CMYK, BPC, gamut check, profile writing) | Printer soft proofing, Print module colour management, gamut warning | Develop soft proof, Print |
| `text` (parley shaping, CJK/vertical, craft-fonts) | Print/Book/Slideshow text, identity plate, watermarks | Output modules |
| `paint`, `tablet`, `ui-egui/src/stylus.rs` | Pressure/tilt-aware masking brush (macOS and X11; Windows still to write) | Masking brush |
| `psd` + `io/src/psd_export.rs` | Layered PSD/PSB export, "Edit In → Open as Layers", PSD import | Export / external edit |
| `plugins` (wasmi sandbox) | Plugin SDK for publish services, export filters, metadata providers | §6.5 |
| `engine/src/actions_cmds.rs` | Recordable actions / batch automation | Automation |
| `codecs` (EXR, deep EXR) | EXR import/export (HDR) | Export |
| `vector` | Book/Print layout shapes, mask paths | Output modules |
| `engine/src/print_cmds.rs` (minimal PDF writer, crop marks) | Starting point only; replace with a multi-page PDF writer with font embedding | Print/Book |

Not worth taking: PhotoCraft's `raw` (weaker than LightCraft's), its `gpu` (a compositor, not a develop pipeline) and
its `codecs` JXL (it has none).

### 3.3 darktable and Ansel (behaviour and design reference only)

The project is MIT (§4), so darktable and Ansel are used **as running applications, user manuals and prose design
notes**. Their source code is never opened. File paths that appear below only show *where a feature lives*, so it can
be tried in the running app. They are not reading material.

**Features and design to emulate:**

- The **output views** exist and work, so they are a functional reference for Map, Print, Slideshow, tethering,
  web gallery, photo book and publishing. Try them in the app: the map, print, slideshow and tethering views; the
  print settings, map locations and geotagging panels; and the gallery, LaTeX book and Piwigo export storages.
- **Colour-science algorithms** that are published in papers and documented in the darktable and Ansel docs:
  - colour calibration / CAT with chart fitting (`channelmixerrgb`);
  - filmic, sigmoid and AgX tone mapping;
  - diffuse-or-sharpen (PDE-based detail);
  - tone equalizer;
  - profiled denoise (Poisson-Gaussian noise model);
  - highlight reconstruction (Ansel's harmonic version, `ansel/doc/highlights-reconstruction.md`).
- **Ansel's engineering notes** (`ansel/doc/*.md`):
  - incremental pipeline cache that recomputes only downstream stages (5–40× faster);
  - lazy-loaded SQLite lens DB;
  - "studio capture" folder-watch tethering;
  - a deterministic, dependency-free ML raw denoiser (`rawdenoiseai`).
  - LightCraft already does most of the pipeline-cache part.
- **darktable-chart**, as described in the darktable manual: chart shots, thin-plate-spline fitting, ΔE report.
  This is the method for making our own camera profiles; we implement it from the published description.
- **darktable's MCP feature list**, as documented for users: a checklist for agent coverage.

**Data that is not used.** It is listed here so nobody imports it by accident; only the last row is allowed:

| Data | Path | Licence | Gap it fills |
|---|---|---|---|
| ~100 chart-measured camera matrices | `darktable/src/common/colormatrices.c` | GPL-3+ | Camera colour calibration |
| Noise profiles, 437 cameras (Poisson-Gaussian per ISO) | `darktable/data/noiseprofiles.json` | GPL-3+ | Profiled denoise, AI denoise sigma maps |
| WB presets, 485 models | `darktable/data/wb_presets.json` | GPL-3+ | WB presets per camera |
| Per-camera base curves (535 styles) | `darktable/data/styles/`, `src/iop/basecurve.c` | GPL-3+ | "Camera Standard"-like default looks |
| Colour checker reference values, illuminants, CIE observer | `src/common/colorchecker.h`, `illuminants.h`, `src/external/cie_colorimetric_tables.c` | GPL-3+ (CIE data itself is public) | Our own chart calibration |
| rawspeed `cameras.xml` (matrices, crops, black/white levels, CFA) | submodule **not checked out**; `git submodule update --init` | LGPL-2.1 | Camera coverage and colour |
| LibRaw (CR3 and many long-tail decoders) | submodule **not checked out** | LGPL-2.1 / CDDL | CR3, compressed ORF and other decoders |
| lensfun lens DB (~1,500 lenses) | not bundled; separate download | **CC-BY-SA 3.0** (data) | **Allowed**: shipped as a separate, attributed CC-BY-SA data file (Phase 2.5); lensfun's LGPL *code* is not read |

---

## 4. Licence: MIT (decided 2026-10-10)

The fork is licensed **MIT**. LightCraft and PhotoCraft are "MIT OR Apache-2.0", and the fork takes the MIT option of
that dual licence.

### 4.1 What MIT means in practice

- **Upstream copyright stays.** `LICENSE` names the LightCraft and PhotoCraft contributors (for inherited code) and
  "the project contributors" (for new code). No product name appears in it; the name is in `brand.toml`.
- **Two exceptions stay Apache-2.0 only, and that's allowed.**
  - LightCraft's `crates/segment` ports Hugging Face SAM 3 code that is Apache-2.0, so it can't be relicensed.
  - `crates/fetch` was split out of it.
  - Both keep `license = "Apache-2.0"` and their `NOTICE` entries. The app as a whole is MIT with two
    Apache-2.0-licensed components, which is compatible.
  - The planned `net` crate (Phase 1.7) is **written fresh under MIT** rather than grown from `fetch`.
- **Merging continues both ways.**
  - Upstream LightCraft changes can be merged at any time, taking the MIT option.
  - Our changes could go back upstream if wanted.
- **Commercial use and app stores are unrestricted.**
- **Dependencies:** `cargo deny` allows only MIT, Apache-2.0, BSD-2/3, ISC, Zlib, Unicode-3.0, CC0-1.0 and MIT-0.
  GPL, LGPL, AGPL, MPL and SSPL are denied in the dependency tree.
- **Data files** may be CC-BY or CC-BY-SA if they ship as separate, attributed data files, never compiled into code.
  This covers the lensfun DB and GeoNames.

### 4.2 Clean-room rules (inherited from LightCraft, unchanged)

- **Never read or copy GPL, LGPL or AGPL source:** darktable, Ansel, RawTherapee, ART, LibRaw, rawspeed, lensfun's
  code, dcraw-derived code, Immich. Agents must not open their `.c`, `.cc`, `.h` or `.rs` files.
- **Allowed sources:**
  - running darktable and Ansel as *applications* and watching their behaviour;
  - their user manuals and prose design notes (for example `ansel/doc/*.md`), skipping any code excerpts in them;
  - published papers and format descriptions written in prose.
  - LightCraft has decided the same for raw formats (2026-10-05).
- **No GPL data tables** such as darktable's `noiseprofiles.json`, `wb_presets.json`, `colormatrices.c` or
  basecurves. The equivalents are measured ourselves (Phase 2.1).
- **Adobe:**
  - no assets, DCP, LCP, presets, profiles or matrices;
  - never look inside Adobe application bundles;
  - camera matrices are never harvested from Adobe-converted DNGs. Matrices in camera-native DNGs (written by the
    camera maker's own software, checked via the `Software` tag) are manufacturer data and may be used.

### 4.3 Cost of this choice

Camera colour, noise profiles, white-balance presets and long-tail raw decoders must be **measured or
reverse-engineered by us**. That makes Phase 2 larger (110–180 h instead of 80–160 h) and turns it partly into a
**data-collection project**: chart shots, flat-field and ISO series, and CC0 raw samples. It needs a community
contribution pipeline (Phase 2.2). The upside is a clean, permissively licensed camera and lens database that we own.

---

## 5. Target architecture (deltas from LightCraft)

```
L0  geom color raster tiff sysmem fetch heif            (LightCraft)
L1  raw codecs meta develop denoise faces scenes        (LightCraft)  + cms (from PhotoCraft)
    camdb      ← NEW: camera matrices, WB presets, noise profiles, crops, levels (data crate)
    lensdb     ← NEW: lens profiles (lensfun data + our own), lookup by EXIF
    text       ← from PhotoCraft (parley)        pdf ← NEW: multi-page PDF writer, font embedding, ICC
L2  pipeline                                            (LightCraft) + region/tiled full-res renders
    inpaint    ← from PhotoCraft algo (PatchMatch, content-aware, GrabCut refinement)
L3  gpu catalog preview merge segment                   (LightCraft) catalog → indexed on-disk store
    layout     ← NEW: page/cell/template model shared by Print, Book, Slideshow, Web, Contact sheet
    geo        ← NEW: tile fetch/cache, projections, clustering, GPX, saved locations, geocoding
    tether     ← NEW: PTP over USB (nusb) / PTP-IP; folder-watch fallback
    media      ← NEW: audio (slideshow), video decode/encode (highest risk)
L4  engine  → split: engine-core (commands, session, jobs), engine-library, engine-develop,
              engine-export, engine-output (print/book/slideshow/web), engine-publish
    plugins    ← from PhotoCraft (wasmi), publish/export SDK
L5  ui-egui → Classic shell: Module trait {Library, Develop, Map, Book, Slideshow, Print, Web},
              panel framework (left/right/top/filmstrip, solo, auto-hide), Classic keymap layer
    mcp
    net        ← promoted from fetch's pure-Rust HTTPS client: JSON, multipart upload, proxies, custom CAs
    credentials← OS keychain / encrypted-file secrets          immich ← NEW: Immich client, link, publish, sync
    brand      ← NEW (L0): every product name/id, generated from brand.toml
Apps: internal binaries `app`, `app-cli` (and optional web build), renamed from brand.toml at packaging time
```

**Upstream strategy.** LightCraft is moving fast: 1,117 commits, many in the last week. Treat the fork as a
**downstream** that merges `lightcraft/main` regularly.

- Keep LightCraft's crates as unchanged as you can.
- Put Classic-only work in **new crates and new UI modules**, so merges stay cheap.
- Rename crates only once, mechanically, at fork time.

---

## 6. Plan to completion

Estimates are in **agent-hours**, calibrated on LightCraft's own log (4–6 parallel agents built M0–M13 in about 25
active hours, roughly 105–145 agent-hours in total). Treat them as relative sizes: the heavy items depend on data and
licences, not on typing speed. Each phase ends with an **exit gate** checked by tests or measurement, not by
ticking boxes.

### Phase 0: Fork, brand config and foundations (≈ 12–22 h) → [PLAN_phase_0.md](PLAN_phase_0.md)

1. Fork `lightcraft` into `nonameyet/` with full git history (`git clone` + new remote).
2. **Product name in one config file (`brand.toml`)**, read by a `brand` crate. No source file contains the name; a CI
   check enforces it, and settings/libraries migrate automatically on rename. Remove the ArtCraft brand
   (`docs/brand/`, README hero, logos). Give crates and binaries neutral internal names.
3. **Licence (MIT, §4):** `LICENSE` (MIT), `NOTICE`, `cargo deny` policy; keep the clean-room rules in
   `AGENTS.md`/`CLAUDE.md`; `segment` and `fetch` stay Apache-2.0.
4. Rebuild `plan/`: a Lightroom Classic reference (feature catalog, menus, Classic shortcuts) **written in our own
   words**, the Classic module map from §2, and `STATUS.md`. Extend `docs/parity.md`:
   - reopen the 🚫 Classic rows (KEYC-MODULES, Web module, reverse geocoding);
   - fix the garbled "Top gaps" list.
5. Bring in shared infrastructure: `craft-fonts` (CJK) and the photo corpus (`cargo xtask corpus`). Add an
   `upstream` remote and a merge routine (merge, `cargo xtask ci`, fix, commit).
6. Add Immich tracker rows (IMM-*), the `plan/immich.md` research note, and a pinned Immich test server
   (`cargo xtask immich up`).

**Gate:** `cargo xtask ci` green; renaming = editing `brand.toml` only; the app launches; parity tracker counts Classic rows.

### Phase 1: The Classic shell, a catalog that scales, Immich link (≈ 50–85 h) → [PLAN_phase_1.md](PLAN_phase_1.md)

1. **Module system** in `ui-egui`:
   - a `Module` trait with its own panels, toolbar and keymap;
   - module picker, identity plate, panel framework (auto hide/show, solo mode, Tab/⇧Tab/F5–F8/T), screen modes,
     lights out;
   - a secondary window with Grid/Loupe(live/locked)/Compare/Survey.
   - Re-home LightCraft's existing views into **Library** and **Develop**.
2. **Classic keymap layer** (KEYC-*), with LightCraft's layout as an option.
3. **Library panels Classic-style:**
   - Navigator; Catalog panel (Quick Collection, Previous Import);
   - Folders (rename/move/sync, volumes);
   - Collections with sets and target collection; Quick Develop;
   - Keywording (keyword sets, suggestions); Keyword List (synonyms, export flags, import/export);
   - Metadata panel views plus presets and copy/paste;
   - Painter tool.
4. **Catalog v4:**
   - benchmark the current op-log at 250k and 1M photos;
   - move to an on-disk, indexed store (pure Rust, e.g. `redb`, with secondary indices) behind the existing
     `catalog` API;
   - lazy photo records;
   - backup-on-exit with integrity check and optimise;
   - multiple catalogs; Export as Catalog / Import from Catalog with conflict rules;
   - metadata-conflict badges against XMP.
5. **Split `engine`** (45k lines) into the sub-crates of §5 before the output modules land.
6. **Full-resolution zoom:**
   - generalise `ui-egui/src/region.rs` to tile/region rendering at 1:1 and above, for loupe, compare, soft proof and
     overlays;
   - 1:1 preview cache with auto-discard.

7. **Network groundwork and Immich link:**
   - `net` and `credentials` crates; `remote_identity` catalog table; SHA-1 checksums at import;
   - IMM-CONNECT, IMM-LINK, IMM-IMPORT and IMM-EXTLIB.

**Gate:**
- A 500k-photo synthetic catalog opens in under 2 s and filters in under 100 ms.
- Every Library and Develop panel and every Classic shortcut row in the tracker is ✅.
- 1:1 zoom matches export pixels.

### Phase 2: Image quality and camera coverage (≈ 110–180 h, partly data collection) → [PLAN_phase_2.md](PLAN_phase_2.md)

1. **`camdb` crate:** per-camera matrices (dual illuminant), WB presets, noise profiles, black/white levels and crops.
   - sources: data the file carries itself, our own chart calibration, noise profiles measured from ISO series and
     the corpus, WB presets read from maker notes, and LightCraft's JPEG fitting as the fallback;
   - our own chart calibration tool, built from the published description of the darktable-chart method
     (ColorChecker / IT8, thin-plate-spline LUT), for "measured" profiles;
   - a community pipeline for CC0 chart and ISO-series submissions.
2. **Camera-matching profiles** (Standard/Portrait/Landscape/Neutral/Vivid per maker): fitted to camera JPEGs with
   LightCraft's existing fitter, stored as our own LUT profiles.
3. **Raw decoders:**
   - remaining CR3 CRX variants and sRAW/mRAW;
   - compressed ORF and SRW;
   - NEF lossy-after-split;
   - then the long tail (3FR, IIQ, MOS, ERF, KDC, DCR, MRW, NRW, X3F if feasible).
   - clean-room: black-box analysis and prose format descriptions, as LightCraft does now.
   - Corpus: one CC0 sample per model from raw.pixls.us, with a decode and colour sanity test per model.
4. **`lensdb` crate:**
   - lensfun DB (CC-BY-SA, attributed), converted at build time to a compact lazy-loaded table (Ansel's lesson:
     don't parse 1,500 XML entries at startup);
   - auto lookup from EXIF lens id, distortion/TCA/vignetting models, custom profile import;
   - later, our own calibration tool.
5. **Remove tool:** port PhotoCraft's PatchMatch inpaint and content-aware fill; spot pin editing; Detect (dust spots).
6. **Denoise:**
   - profiled classical NR using noise profiles (Poisson-Gaussian variance stabilisation);
   - AI denoise across Bayer, X-Trans and linear raws, following the deterministic no-runtime design in Ansel's prose
     notes, and needing
     permissively licensed or our own trained weights;
   - Raw Details and Super Resolution (DNG output).
7. **Render-fidelity suite:**
   - reference renders made locally from CC0 raws (never committed);
   - ΔE2000 and SSIM per slider sweep;
   - tune tone/highlights/clarity/texture/NR/sharpening against the numbers.
   - Also settle tone-curve placement (LightCraft applies curves after display encoding).
8. **Soft proofing:** switch to PhotoCraft `cms`:
   - printer ICC profiles, perceptual/relative intent, BPC;
   - Simulate Paper & Ink;
   - destination gamut warning, create proof copy.

**Gate:**
- ≥ 95% of models in the corpus decode from sensor data.
- Median ΔE2000 ≤ 2 against camera JPEGs for supported bodies.
- Lens profile found for ≥ 80% of corpus lens EXIFs.
- Fidelity suite runs in CI on a small set.

### Phase 3: Output modules (≈ 72–124 h; can run in parallel with Phase 2 after Phase 1) → [PLAN_phase_3.md](PLAN_phase_3.md)

Shared first: **`layout`** (pages, cells, guides, templates, text blocks), **`text`** (from PhotoCraft) and a real
**`pdf`** writer (multi-page, font subsetting and embedding, ICC output intents, JPEG/Flate images).

1. **Print:**
   - Single Image/Contact Sheet, Picture Package, Custom Package;
   - guides, rulers, crop marks, page/photo info, identity plate, watermark;
   - print sharpening and 16-bit output;
   - colour management with printer ICC and intent;
   - print to JPEG/PDF;
   - OS printing: IPP/CUPS in pure Rust on Linux and macOS; Win32 spooler on Windows in an isolated `unsafe` crate;
   - templates and saved prints.
2. **Map:**
   - egui slippy map with a configurable tile server, disk cache and attribution;
   - pins and clusters, drag-to-geotag;
   - GPX tracklog display plus auto-tag;
   - saved locations with privacy (strip GPS on export);
   - location filter;
   - opt-in reverse geocoding (online service or offline GeoNames).
3. **Slideshow:**
   - templates, overlays, backdrop, titles, pan and zoom, fades;
   - music with sync-to-music (pure-Rust decode with `symphonia`; audio output through `cpal`, which links system
     ALSA on Linux, so accept it or add a pure-Rust PipeWire/PulseAudio backend);
   - export to PDF and JPEG;
   - video export (AV1 via `rav1e` in WebM/MP4 first, H.264 depends on §6.6);
   - saved slideshows.
4. **Book:**
   - settings (size, cover, paper);
   - Auto Layout with presets, page templates, custom pages;
   - cells, page and photo text with full type controls;
   - backgrounds, guides, spread view;
   - export to PDF and JPEG (Blurb is proprietary, so out of scope);
   - saved books.
5. **Web:**
   - gallery templates we write ourselves (HTML/CSS/JS, responsive, no external CDN);
   - site info, palette, image info, output settings;
   - export to folder plus SFTP upload (`russh`);
   - saved galleries;
   - "Share via Immich": album plus shared link (IMM-SHARELINK).

**Gate:** each module's tracker rows are ✅; golden-image tests of the layout engine (PDF raster compare); a print
round trip through a known ICC profile measures within ΔE tolerance.

### Phase 4: Workflow power features and full Immich integration (≈ 60–105 h) → [PLAN_phase_4.md](PLAN_phase_4.md)

1. **Tethered capture:**
   - start with "studio capture" (watched folder plus auto-apply preset), which is cheap and works with any vendor
     app;
   - then native PTP over USB (`nusb`) and PTP-IP for Canon/Nikon/Sony: shoot, settings bar, live view, naming,
     sessions.
2. **Publish services framework:**
   - publish collections, the modified-for-republish state, remote id tracking;
   - built-in **Hard Drive** publisher;
   - Immich built in (item 8); WebDAV/Piwigo/PhotoPrism/Nextcloud as plugins;
   - optional Flickr and other OAuth services.
3. **Plugin SDK:**
   - PhotoCraft's wasmi sandbox extended with a host API (catalog read/query, export hooks, metadata providers,
     publish service interface, menu commands);
   - capability-based permissions (network, filesystem roots);
   - pure Rust, unlike Lightroom's Lua.
4. **Edit In:**
   - external editor presets;
   - "Open as Layers" / "Open as Smart Object"-style round trip to PhotoCraft, using PhotoCraft's layered PSD writer;
   - re-import and stack the result.
5. **Actions:** record and replay of command sequences, built on PhotoCraft's `actions_cmds`; batch on a selection.
6. **People:** face clusters with separators, write face regions to XMP (MWG regions), small-face detection on
   large images.
7. **`.lrcat` migration v2:** history replay, snapshot import, profile mapping table, and a fidelity report per
   photo.

8. **Immich, two-way:**
   - publish service (albums; renders, originals, or both stacked);
   - metadata sync with conflict handling;
   - people and face import;
   - smart search;
   - MCP commands.

**Gate:** a scripted end-to-end session through MCP: tethered shoot → import preset → cull → develop → publish to
folder, SFTP and Immich → rating changed in Immich syncs back → print to PDF. It runs headless in CI (with a simulated camera).

### Phase 5: AI, HDR and video (≈ 100–200 h, highest uncertainty)

1. **AI model strategy (an owner decision).** Use permissively licensed weights, or train our own; pure-Rust
   inference on candle or LightCraft's own interpreter; host the downloads.
   - Subject, Sky, Background, People parts, Landscape parts, depth (for Depth Range and Lens Blur).
   - Update AI masks after edits; adaptive presets.
   - Semantic search with CLIP-like embeddings on device.
   - Assisted culling: eyes closed, focus, near duplicates.
   - darktable's `src/ai` shows a working model set (SAM 2.1, NIND denoise, RealPLKSR), but check each model's
     licence on its own.
2. **HDR:**
   - HDR editing range (extended highlights in the scene-referred pipeline, which LightCraft already has);
   - HDR display via wgpu with an HDR swapchain (platform-dependent);
   - visualise high dynamic range, SDR preview settings;
   - export as JPEG with gain map (ISO 21496-1), AVIF/JXL HDR, and EXR (PhotoCraft codecs).
3. **Video:**
   - playback, trim, poster frame, frame capture, presets on video, export.
   - **Pure-Rust decoding is the risk.** AV1 via `rav1d` (Rust) is fine, but there is no mature pure-Rust H.264 or
     HEVC decoder, and HEVC also has patent issues.
   - Options:
     - (a) write a pure-Rust H.264 baseline/main decoder: large;
     - (b) allow OS decoders (VideoToolbox, Media Foundation, VA-API) in an isolated FFI crate, as LightCraft
       already does for one macOS call;
     - (c) share FilmCraft's crates if and when they exist.
   - Recommendation: (b). It breaks "pure Rust" only at a thin, optional platform edge.

**Gate:** AI masks reach IoU ≥ 0.85 on an annotated CC0 test set; an HDR export round-trips gain maps; MP4 from the
main camera brands plays and trims.

### Phase 6: 1.0 hardening (≈ 30–50 h)

- Performance budgets:
  - slider ≤ 16 ms at preview size on 24 MP;
  - 1:1 tile ≤ 100 ms;
  - import 1,000 raws with standard previews in under 2 min;
  - catalog at 1M photos.
- Robustness: fuzzing raw/XMP/catalog/plugin inputs (the never-crash rule), corrupt-catalog recovery, and a disk-full
  test.
- Accessibility (AccessKit through egui), localisation, a Classic-style help overlay per module.
- Packaging: AppImage/Flatpak, .deb/.rpm, dmg (notarised), msi. Decide whether to keep or drop the web build (it
  costs CI time and constrains crates).
- Documentation: user manual, plugin SDK docs, migration guide from Lightroom Classic.

**Gate:** tracker ≥ 98% of in-scope rows; fidelity and performance suites green; a beta with real Lightroom Classic
users migrating real catalogs.

### Totals

| Phase | Agent-hours |
|---|---:|
| 0 Fork, brand config and foundations | 12–22 |
| 1 Classic shell, catalog, Immich link | 50–85 |
| 2 Quality and camera coverage | 110–180 |
| 3 Output modules | 72–124 |
| 4 Workflow power features, Immich two-way | 60–105 |
| 5 AI, HDR, video | 100–200 |
| 6 Hardening | 30–50 |
| **Total** | **≈ 434–766** |

Order and parallelism:
- Phases 0 → 1 must come first; they are the architectural changes everything else sits on.
- Phases 2, 3 and 4 then run in parallel on separate crates.
- Phase 5 depends mostly on the model decision and can start research early.
- With 4–6 parallel agents at about 70% efficiency, that is roughly **110–190 hours of wall-clock work**, plus the
  non-coding time for data (chart shots, corpus), model licensing and testing with real users.
- Without Phase 5 (AI, HDR, video), a complete Classic-module application with Lightroom-grade colour is about
  **335–565 agent-hours**.

---

## 7. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Clean-room data collection is slow (MIT, no GPL tables) | Some cameras stay "estimated" for a long time | Community CC0 chart/ISO-series submissions; automatic noise estimation from corpus raws; maker-note WB presets; JPEG fitting fallback |
| Contamination: an agent reads GPL source | Licence breach | Clean-room rule in AGENTS.md; GPL trees are never referenced by path in tasks; every algorithm PR names its sources |
| LightCraft keeps moving; merges get painful | Lost upstream fixes | New work in new crates; merge weekly; keep shared crate names unchanged after the one-time rename |
| Catalog redesign breaks existing libraries | Data loss | Keep the catalog API; migration with backup; crash/fuzz tests from LightCraft |
| AI weights not licensable | AI masks stay heuristic | Classical fallbacks (PhotoCraft GrabCut/maxflow); train our own small models |
| Video decoding in pure Rust | No video | Isolated OS-decoder FFI crate (§Phase 5) |
| Look fidelity vs Adobe never matches | Users notice a different look | Measured fidelity suite; camera-matching profiles; accept "different but good" where needed |
| Printing on Windows needs FFI | Print module incomplete on Windows | Isolated `unsafe` crate like `sysmem`; PDF export always works |
| Immich API changes between releases | Sync breaks after a server upgrade | Pin and test the minimum and latest releases nightly; version check on connect; feature-detect optional endpoints |
| Name hard-coded again by upstream merges or new code | Rename gets expensive | `cargo xtask brand check` in CI plus a throw-away-brand build |
| Adobe IP | Legal | Keep LightCraft's rules: no Adobe assets, DCP, LCP, presets or bundle inspection; feature names in our own words |

---

## 8. Immediate next steps

1. ~~Licence~~: **MIT** (decided 2026-10-10). The name can wait: `brand.toml` makes it a one-line change at any time.
2. Phase 0 ([PLAN_phase_0.md](PLAN_phase_0.md)): fork, brand config, debrand, rebuild `plan/`, update the tracker
   (including Immich).
3. Spike in Phase 1 (both inform everything after):
   - the `Module` trait and panel framework in the UI;
   - the catalog benchmark at 500k photos.
