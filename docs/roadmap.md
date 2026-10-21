# Roadmap: milestones, current focus and what's next

> **Last reviewed:** 2026-10-10 · **Last updated:** 2026-10-10 · **Change:** major (created; milestones moved here from ROADMAP.md, focus re-ranked after the 2026-10-10 re-measure) · **Target:** Adobe Lightroom Classic 15.5 / Lightroom 9.6

Forward-looking plan. The summary with the headline numbers is [ROADMAP.md](../ROADMAP.md); the
ranked shortfalls are [gaps.md](gaps.md); the numbers come from [target-app-parity.md](target-app-parity.md).
LightCraft has no `cargo xtask scorecard`; the measured checklist is
[parity-checklist.md](parity-checklist.md) (`cargo xtask parity`).

## Current focus

Toward **beta** (~340–600 Opus 5.5 agent hours; ranked, each a gap in [gaps.md](gaps.md)):

1. **Camera colour of our own** (gap 1, 50–90 h): chart-based calibration for the most-used models,
   building on the per-file JPEG fits and pooled profiles (`lightcraft-cli calibrate`); never Adobe
   data.
2. **CR3 for every Canon body** (gap 2, 25–45 h): the remaining CRX variants, sRAW / mRAW, HEIF CR3;
   clean-room from prose descriptions (decided 2026-10-05; compressed NEF is the template).
3. **Render-fidelity suite** (gap 3, 30–60 h): score our output against Lightroom's on the same CC0
   raws (references local in `plan/`), then tune; a retune ships as a new process version.
4. **Full-resolution zoom** (gap 4, 8–15 h): visible-region rendering so 100% shows original pixels.
5. **AI masks** (gap 5, 40–70 h): Subject / Sky / People on licensable weights; configure the SAM 3
   mirror. Needs the maintainer's model decision.
6. **Compressed ORF and HE NEF** (gaps 6–7, 28–55 h).
7. **Lens profiles of our own** (gap 8, 40–80 h).
8. **Tests in CI on macOS, Windows and Linux** (gap 9, 6–12 h).
9. **Per-model verification** (gap 10, 20–35 h).

## Alpha gate

The target's core workflows: what a typical Lightroom user does every day. Alpha requires each to
work end to end on the main platform (macOS) and the work to save and reopen. Checked on 2026-10-10
against the code, tests, issues and [gaps.md](gaps.md).

| Workflow | Works end to end? | Evidence | Hours to pass |
|---|---|---|---:|
| 1. Import a shoot (raw + JPEG) from a card or folder into the library | yes | Add / Copy / Move import with templates, duplicates, devices with DCIM, background import with cancel (`crates/engine/src/import.rs`, `tests_import.rs`, `tests_import_move.rs`); checklist A 83% | 0 |
| 2. Cull: rate, flag, label, compare / survey, filter, check focus | yes (partial: focus check) | ratings / flags / labels with advance, Compare, Survey ≤ 48, filter bar, smart albums (checklist B–D 88–92%); 100% zoom is capped by the preview size (2,560 px default, adjustable in Settings), gap 4 — degrades focus checks on large files but does not block culling | 0 (8–15 h to close gap 4, beta) |
| 3. Develop raws non-destructively (global sliders, crop, masks, heal) | yes for most cameras (partial: Canon CR3 outside the verified variants, compressed ORF, HE NEF open as embedded previews) | Sony, Nikon (except HE), Fujifilm, Panasonic, Pentax, CR2, DNG decode from sensor data (raw-parity.md); every Edit slider, crop / Upright, brush / gradient / range masks, heal / clone (checklist F, H, K); colour estimated per file rather than measured, gaps 1–3 | 0 (coverage and colour gaps are beta blockers 1–3, 6–7) |
| 4. Save, quit and reopen the work | yes | crash-safe journal + snapshots, torn-append and crash tests (`crates/catalog/src/journal.rs`, `tests_journal.rs`, `tests_torn_append.rs`); XMP sidecars read / write; failed saves reported | 0 |
| 5. Batch-apply a look: presets, copy / paste / sync settings | yes | presets (incl. `.xmp` / `.lrtemplate` import), copy / paste / sync, auto sync (checklist L 86%, N 100%) | 0 |
| 6. Export deliverables (JPEG / TIFF, size, sharpening, metadata, watermark, naming) | yes | one encoder for app, CLI and MCP (`crates/engine/src/export.rs`, `tests_export.rs`); checklist S 89% | 0 |

**Result: passes.** Every core workflow runs end to end on macOS and the work survives quit and
reopen; the partial rows degrade fidelity for some cameras and large files but don't stop the
workflow, so LightCraft stays **alpha** (ready for real work ~61%, above the ~40% bar).

## After beta

Enhance (Raw Details, Super Resolution, X-Trans denoise), content-aware remove, HDR display, the
Classic output modules (Print, then Map, Book, Slideshow, publish services), tethering, video,
plug-in ecosystem, localization to `full` and new scripts (RTL, shaping). Estimates in
[gaps.md](gaps.md) entries 11–28; ~500–1,300 h after beta.

## Milestones

Original wall-clock estimates (made before work started, 4–6 parallel agents) are kept for
calibration: M0–M13 took ≈ 25 active hours against an estimate of ≈ 110–170 h.

**Status legend:** ✅ done · 🚧 in progress · ⬜ not started

| # | Milestone | Scope (summary) | Estimate (h) | Status |
|---|---|---|---|---|
| M0 | Skeleton + visual shell | workspace, xtask CI + layering, geom/color/raster, develop model, pipeline v0, catalog v0, engine commands, Lightroom-look UI (grid, loupe, filmstrip, Edit panel), control channel, MCP, web build | 3–5 | ✅ |
| M1 | Library core | import (JPEG/PNG/TIFF/WebP), EXIF/XMP, persistent catalog (op log + snapshots), albums, ratings/flags/labels, filter/search/sort, thumbnail cache, 100k-photo grid | 6–10 | ✅ (100k-photo grid scale test pending) |
| M2 | Pipeline v1 (quality) | WB temp/tint, profiles, local tone mapping (highlights/shadows), curves, HSL, point colour, colour grading, texture/clarity/dehaze, vignette, grain, B&W, auto tone/WB, histogram, before/after | 10–15 | ✅ (look tuning vs our references ongoing) |
| M3 | RAW I | TIFF/DNG (LJ92, deflate, tiles, opcodes), demosaic (AHD/PPG/bilinear), highlight recovery, DNG colour model, CR2, NEF, ARW, embedded previews | 10–15 | ✅ (DNG, CR2 incl. sRAW, ARW, NEF uncompressed / lossless / lossy / lossy after split, embedded previews) |
| M4 | Crop, geometry, optics | crop tool + overlays, straighten, Upright (auto/level/vertical/full/guided), manual transforms, CA, defringe, manual lens corrections | 6–10 | ✅ |
| M5 | Performance | source pyramids, wgpu compute pipeline (CPU oracle), draft/full renders, prefetch, budgets (16 ms slider updates on 24 MP) | 10–15 | 🚧 (stage cache, source pyramid, wgpu pipeline, prefetch, memory budget ✅; colour NR at half resolution, GPU histogram ⬜) |
| M6 | Masking | brush, linear/radial gradients, colour/luminance/depth range, add/subtract/intersect/invert, all local adjustments, masks panel | 8–12 | 🚧 (brush/linear/radial/colour/luminance range, add/subtract/intersect, masks panel ✅; depth range, AI masks ⬜) |
| M7 | Detail | sharpening + masking preview, luminance/colour NR, Denoise, Raw Details, Super Resolution | 6–10 | 🚧 (sharpening, luminance/colour NR ✅; AI Denoise 🟡 (Bayer, optional weights; policy/fidelity open); Raw Details, Super Resolution ⬜) |
| M8 | Heal / Remove | content-aware remove (PatchMatch), heal, clone, brush spots, visualize spots, red/pet eye | 6–10 | 🚧 (heal, clone, auto source, visualize spots, red/pet eye ✅; PatchMatch remove ⬜) |
| M9 | Presets, profiles, versions, sync | preset browser + amount, create/import presets, profile browser, versions, history, copy/paste/sync settings | 5–8 | ✅ |
| M10 | Export & share | export dialog (JPEG/PNG/TIFF/DNG/AVIF/JXL/original), sizing, sharpening, metadata, watermark, naming, batch jobs, XMP sidecars, HDR export | 6–10 | 🚧 (all formats incl. DNG/original, sizing, presets, background jobs, HDR gain map JPEG / PQ AVIF ✅; JXL encode ⬜) |
| M11 | RAW II | CR3, RAF (X-Trans), ORF, RW2, PEF, SRW, 3FR, IIQ + long tail; camera calibration DB; HEIC/AVIF/JXL import | 20–35 | 🚧 (RAF all, RW2 every format, PEF, ORF uncompressed and 12.8 bpp, SRW uncompressed, HEIC ✅; CR3 🟡 (3 bodies verified); **camera colour calibration** 🚧 (per-file JPEG fits, 5 bundled profiles, 52 spectral matrices; measured database missing); compressed ORF / SRW, HE NEF, IIQ, X3F ⬜) |
| M12 | AI & smart features | subject/sky/background/people/object masks, semantic search, faces/People (permissively licensed models, pure-Rust inference) | 20–40 | 🚧 (faces: detection, recognition, People view ✅; Object / Describe masks via SAM 3 🟡; Subject / Sky / People masks, semantic search ⬜) |
| M13 | Merge | HDR merge (deghost), panorama (projections, boundary warp, fill edges), HDR panorama | 10–15 | ✅ |
| M14 | Video | import/playback/trim via FilmCraft crates, global edits + presets on video, video export | 6–10 | ⬜ |
| M15 | Classic modules | Map, Book, Slideshow, Print, Web; smart collections, stacks, virtual copies, publish services, tethering | 25–40 | 🚧 (smart albums, stacks, virtual copies, compare/survey ✅; contact-sheet PDF 🟡; Map/Book/Slideshow/Print/Web ⬜) |
| M16 | 1.0 polish | preferences, shortcut editor, accessibility, localization, packaging (dmg/msi/AppImage/web), hardening | 10–20 | 🚧 (settings, keyboard shortcuts sheet, packaging basics ✅; English/Chinese (Simplified, Traditional)/Japanese/Brazilian Portuguese/Spanish/German/Russian/Ukrainian/Georgian localisation 🟡; accessibility and further locales ⬜) |

## Risks that coding hours alone don't retire

- **AI features** (subject / sky / people masks, generative remove) need model weights with
  licences we can ship; classical fallbacks first. No permissively licensed sky-segmentation or
  raw-denoise model was found; we may need to train our own.
- **Camera colour and lens data** is a data problem: we never use Adobe's matrices, DCPs or LCPs.
  DNG-embedded data first, then our own calibration; long-tail coverage grows over time and needs
  people with cameras, lenses and charts.
- **Raw-format sources:** decoders are written from prose format descriptions (even ones published
  alongside GPL code); decoder source is never read (decided 2026-10-05). Still open:
  freedom-to-operate review for local Laplacian filters, PatchMatch and HEVC (HEIC).
- **Look parity** with Adobe's default rendering is tuned by eye until the fidelity suite exists.
  Retuning changes existing edits, so it ships as a new process version
  ([process-versions.md](process-versions.md)).

## Design notes kept from the old ROADMAP

- **Folder records:** library folders have records of their own in the catalog
  (`Catalog.folder_records`, keyed by folder path; colour labels first). Photos still store full
  paths, so moving a whole disk relinks each photo. If that becomes a problem (an "Update Folder
  Location" for a renamed drive, journal growth on large moves), the next step is the Classic model:
  photos point to a folder id (root + relative path).
- **Zoom:** native pinch zoom and two-finger pan work in Detail, Compare and Reference views; the
  whole-image preview still has a configurable cap (2,560 px default), see gap 4.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-10 | minor | Beta hours 340–600; full ready ~59% in the gate note |
| 2026-10-10 | minor | Added the Alpha gate table (core-workflow gate from the revised standard): passes, stays alpha |
| 2026-10-10 | major | Created: milestones moved from ROADMAP.md (statuses refreshed: M3 ✅, M11 / M12 / M15 detail), current focus re-ranked toward beta, risks and design notes kept |
