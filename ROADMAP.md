# LightCraft roadmap

Milestones toward full Adobe Lightroom parity (cloud Lightroom first, then every Lightroom Classic module), with wall-clock
estimates for continuous (24/7) agent-driven development with 4–6 parallel agents. Estimates are calibrated on the sibling
projects (DrawCraft reached its first four milestones in ≈ 4½ h) and are revised as milestones land.

## Where we stand

*Honest assessment, 2026-10-08. Agents: read this before picking work. The checklist in
[`docs/parity.md`](docs/parity.md) counts features that **exist**; this section is about whether a photographer can
**switch** from Lightroom. Update it when a gap below closes.*

**In one line:** the checklist says **79.7%** (P0 98.2%, P1 95.6%, P2 41.6%), but measured by whether a working
photographer could replace Lightroom without noticing, we are at roughly **60–70%**. The remaining gap is mostly
**quality of results and camera coverage**, not missing buttons.

Caveat on the checklist: ✅ is set by whoever lands a feature, and nobody has systematically checked rows against
Lightroom's behaviour or output. Bugs keep turning up in ✅ areas (CR2 colour-filter phase on some Canon models #85,
duplicate Local entries #22, black GPU exports on an Intel iGPU #78).

### By dimension

| Dimension | Estimate | What's true today | Biggest gaps |
|---|---:|---|---|
| **Feature checklist** | 79.7% | P0 core and P1 nearly complete: import (Add / Copy / Move, templates, devices), library, grid/loupe/compare/survey, every Edit slider, curves, colour grading, masking tools, crop/Upright, heal/clone, presets/profiles, versions/history, sync, export, menus, shortcuts | P0: full-resolution zoom detail; P1: lens-profile database, content-aware fill (patch synthesis), video playback/trim |
| **RAW coverage** (formats people shoot) | ~55% | DNG (all kinds), CR2, CR3 lossless Bayer and version 0x100/0x200 C-RAW (M50/R100/R8 pixel-exact corpus), ARW, NEF (uncompressed + Huffman lossless/lossy), RAF (uncompressed, lossless/lossy compressed Bayer and X-Trans), uncompressed ORF, uncompressed Samsung SRW (NX5 to NX1100, EX1, WB2000), RW2 / RWL / Panasonic RAW (every raw format, checked on 178 files from 118 Panasonic and Leica bodies), PEF; every container's embedded preview (incl. CR3) | **CR3** (every Canon since ~2018), compressed ORF, compressed Samsung SRW (NX1, NX30, NX300, NX500, NX2000, NX3000, NX3300, NX mini), Nikon lossy-after-split, Canon sRAW, HEIC/AVIF. NEFs labelled compressed but stored uncompressed (Z 6 packed 14-bit, D850 12-bit uncompressed). Nikon High Efficiency NEF (HE / HE★: Z 8, Z 9, Z 6III, Z f; JPEG XS-based, preview only, #193). Per-model verification is thin (~55 corpus files vs >1,000 models) |
| **Colour & image quality** | ~55–65% | Pipeline is complete and fast; GPU path CPU-exact within 1/255 | **No measured camera calibration database**: ARW, NEF, RW2, RAF, CR3, CR2 and PEF have guarded camera-JPEG colour estimates, with bundled ILCE-7CR, ILCE-7M4, X-H2S and X-T4 profiles (docs/camera-preview-colour.md); 52 models whose JPEG fit is rejected start from matrices fitted to measured spectral sensitivities; other non-DNG raws and rejected estimates use a neutral matrix. Colour fidelity remains incomplete. No lens-profile database. No measured fidelity against Lightroom (tone, highlights, texture/clarity, NR, sharpening are tuned by eye) |
| **AI & computational** | ~15–20% | Assisted culling (focus, bursts), auto tone, HDR/panorama merge; subject/sky/background masks as classical heuristics, or the photo's own segmentation mattes when it ships them (DNG semantic masks, e.g. iPhone ProRAW's sky matte); non-destructive Bayer RAW denoise with opt-in weights on pure-Rust CPU/GPU; **faces (MVP, [docs/faces.md](docs/faces.md))**: a pure-Rust detector (a 232 KB opt-in download), opt-in recognition models (one-click download or add a file; they run on the shared pure-Rust CPU interpreter; pinned SFace/AuraFace embeddings agree with an independent reference runtime on 40 public crops; one-thread medians 34–36 / 233–252 ms (AuraFace remains expensive)), a background scan that finds and embeds faces, name suggestions, a person's page with look-alike faces to confirm, an Unnamed faces section (look-alikes together, select a group and name it at once) | Real segmentation masks (subject, sky, people, objects, landscape, depth), denoise model policy / linear RGB / quality verification, super resolution, lens blur, generative remove, natural-language search; for faces: clusters with separators between groups, small faces in large photos (the detector looks at 640 px), thresholds calibrated on live faces, writing face regions back to XMP. **Blocked on a model strategy** (licensable weights or our own training; pure-Rust inference is feasible) |
| **Workflow & library** | ~85% (single machine) | Robust catalog (journal + snapshots, background compaction, crash-tested), 85k-photo libraries stay responsive, Local browsing with automatic cleanup, XMP interop, keywords, smart albums, Move import | Opening an 85k library takes 1.7–4.7 s; no cloud sync (out of scope), no tablet companion (#74, roadmap), shared albums, publish services, tethering |
| **Classic modules** | ~30% | Geotagging from GPX track logs, soft proofing (partial), slideshow (basic), paginated contact-sheet PDF export (A4/Letter, layout/captions, current edits, 150 dpi sRGB) | **Map view, Book, full Print module, Slideshow module, Web, publish services**: contact sheets work, custom print packages and printer colour management remain missing |
| **HDR & video** | ~25% | HDR editing (headroom limit, SDR rendition, Visualize HDR); gain map JPEG, PQ AVIF and float TIFF export | On-screen HDR display, HLG, HDR settings from crs XMP; video play/trim/edit/export |
| **Platform & robustness** | ~70% | macOS native; Windows/Linux builds; web via WASM; no-panic lints workspace-wide, `unsafe` confined to `crates/sysmem`; failed saves are reported; GPU errors fall back to CPU | Windows installer UI unverified on Windows (PR #79); GPU path proven only on Apple + user reports; English, Simplified/Traditional Chinese, Japanese, Brazilian Portuguese, Spanish, German, Russian and Ukrainian UI (see docs/localization.md); remaining technical errors; Traditional Chinese uses the Simplified Chinese font until craft-fonts has a TC face; accessibility partial; headless UI tests time out under machine load |

### By kind of user

| User | Readiness | What blocks them |
|---|---:|---|
| JPEG / DNG shooter, single machine | ~85% | Fidelity polish, AI masks |
| Nikon / Sony / Panasonic / older-Canon raw shooter | ~65% | Camera colour fidelity and coverage (ARW, NEF and RW2 preview estimates are only a starting point); embedded distortion is supported for Panasonic / Leica RW2 and the Sony ILCE-7RM4A (Bayer ARW, 24–105 / 200–600 tested); other lens metadata needs coverage and validation |
| Fujifilm raw shooter | ~65% | Uncompressed and lossless/lossy compressed RAF decoded; camera colour calibration remains missing. Verified on 13 bodies, including 14/16-bit GFX; see `docs/raf-compression.md` |
| Canon CR3 / Olympus compressed-raw shooter | ~45% | CR3 lossless Bayer and version 0x100/0x200 C-RAW develop from sensor data (M50, R100, R8 verified pixel for pixel); other CRX variants and compressed ORF open as embedded JPEG previews. Camera colour and broader model verification remain |
| Lightroom Classic power user | ~45% | Print, Book, Map, publish, tethering |
| Relies on AI (masks, denoise) | ~25% | Object / Describe masks via optional SAM 3 (download not yet hosted); Bayer AI denoise needs separately installed weights; X-Trans/linear RGB, model policy and quality parity remain open |

## Where we're going

Priorities, in order. Each points at tracker rows in [`docs/parity.md`](docs/parity.md) → *Top gaps*.

1. **Camera colour calibration of our own** (LR-PROF-CAMERACOLOR, P0): fit each camera to its own embedded JPEG, use
   matrices the files carry themselves, then chart shots. Sony ARW, Nikon NEF, Panasonic RW2, Fujifilm RAF, Canon CR3/CR2,
   Pentax PEF, Samsung SRW and Olympus ORF have the file-local fit (matrix + tone curve from their own JPEG; CR2 and PEF
   since issue #310, not yet checked on the corpus); 52 models whose fit is rejected fall back to matrices fitted to
   measured spectral sensitivities (rawtoaces-data); then validate fidelity.
2. **Raw formats, clean-room** (LR-IMP-FORMATS, P0): Remaining **unverified CR3 CRX** variants, compressed ORF, NEF
   lossy-after-split, sRAW. Decided 2026-10-05: write our own decoders from prose descriptions (never decoder source,
   no LGPL dependency); compressed NEF (#86) is the template.
3. **Verified camera coverage** (LR-IMP-CAMERA-COVERAGE, P0): a CC0 sample per model in the corpus, each decoded and
   checked for plausible colour; fix per-model bugs (#85).
4. **Render fidelity suite** (LR-BEHAV-RENDER-FIDELITY, P1): measure our output against Lightroom on the same CC0 raws
   (references stay in the local `plan/`), then tune against the numbers.
5. **Lens profiles of our own** (LR-EDIT-OPTICS-PROFILE, P1): DNG, Panasonic / Leica RW2 and Sony ILCE-7RM4A Bayer ARW
   embedded distortion are supported; broaden validation and decode remaining vignetting / lateral CA metadata.
6. **AI model strategy** (maintainer decision): which permissively licensed models (or our own training) for
   segmentation masks and denoise. Bayer denoise inference is implemented in pure Rust with an opt-in GPL model; release policy and independently licensed weights remain open. Unblocks M12 and Enhance. Face detection and recognition run on our own checked Rust engine. Object / Describe masks now
   run Meta's SAM 3 in pure Rust (optional, user-consented download under the non-OSI SAM License; CDN mirrors still
   to be set up, docs/ai-masks.md); Subject / Sky / People, Raw Details and Super Resolution are still open.
7. **Then:** HDR (Q), the Classic output modules (expand contact-sheet PDF export into full Print, then Map view, Book, Slideshow), video (R), localisation
   and accessibility.

Already closed in the week of 2026-10-03: compressed NEF (#10), Move import (#29), per-library smart-preview folder,
Local roots / cleanup, grid and catalog performance at 85k photos (#35, #37), failed-save reporting, GPU export
hardening (#78), copyright metadata (#51), GPX geotagging (#60), import tag help and folder templates (#31, #32).

Library folders now have records of their own in the catalog (`Catalog.folder_records`, keyed by folder path; colour
labels first). Photos still store full paths, so moving a whole disk or folder relinks each photo. If that becomes a
problem (an "Update Folder Location" for a renamed or replaced drive, journal growth on large moves), the next step is
the Classic model: photos point to a folder id (root + relative path), and the records keyed by path become those rows.

Image navigation now supports native pinch zoom and two-finger pan in Detail, Compare and Reference views.
The whole-image preview still has a configurable size cap (2,560 px default), so 100% on larger photos does not
yet guarantee original-pixel detail; visible-region rendering remains a gap (LR-VIEW-ZOOM).

## Milestones

**Status legend:** ✅ done · 🚧 in progress · ⬜ not started

| # | Milestone | Scope (summary) | Estimate (h) | Status |
|---|---|---|---|---|
| M0 | Skeleton + visual shell | workspace, xtask CI + layering, geom/color/raster, develop model, pipeline v0, catalog v0, engine commands, Lightroom-look UI (grid, loupe, filmstrip, Edit panel), control channel, MCP, web build | 3–5 | ✅ |
| M1 | Library core | import (JPEG/PNG/TIFF/WebP), EXIF/XMP, persistent catalog (op log + snapshots), albums, ratings/flags/labels, filter/search/sort, thumbnail cache, 100k-photo grid | 6–10 | ✅ (100k-photo grid scale test pending) |
| M2 | Pipeline v1 (quality) | WB temp/tint, profiles, local tone mapping (highlights/shadows), curves, HSL, point colour, colour grading, texture/clarity/dehaze, vignette, grain, B&W, auto tone/WB, histogram, before/after | 10–15 | ✅ (look tuning vs our references ongoing) |
| M3 | RAW I | TIFF/DNG (LJ92, deflate, tiles, opcodes), demosaic (AHD/PPG/bilinear), highlight recovery, DNG colour model, CR2, NEF, ARW, embedded previews | 10–15 | 🚧 (DNG, CR2, ARW, NEF uncompressed + Huffman lossless/lossy, embedded previews ✅; NEF lossy-after-split ⬜) |
| M4 | Crop, geometry, optics | crop tool + overlays, straighten, Upright (auto/level/vertical/full/guided), manual transforms, CA, defringe, manual lens corrections | 6–10 | ✅ |
| M5 | Performance | source pyramids, wgpu compute pipeline (CPU oracle), draft/full renders, prefetch, budgets (16 ms slider updates on 24 MP) | 10–15 | 🚧 (stage cache, source pyramid, wgpu pipeline, prefetch, memory budget ✅; colour NR at half resolution, GPU histogram ⬜) |
| M6 | Masking | brush, linear/radial gradients, colour/luminance/depth range, add/subtract/intersect/invert, all local adjustments, masks panel | 8–12 | 🚧 (brush/linear/radial/colour/luminance range, add/subtract/intersect, masks panel ✅; depth range, AI masks ⬜) |
| M7 | Detail | sharpening + masking preview, luminance/colour NR, Denoise, Raw Details, Super Resolution | 6–10 | 🚧 (sharpening, luminance/colour NR ✅; AI Denoise 🟡 (Bayer, optional weights; policy/fidelity open); Raw Details, Super Resolution ⬜) |
| M8 | Heal / Remove | content-aware remove (PatchMatch), heal, clone, brush spots, visualize spots, red/pet eye | 6–10 | 🚧 (heal, clone, auto source, visualize spots, red/pet eye ✅; PatchMatch remove ⬜) |
| M9 | Presets, profiles, versions, sync | preset browser + amount, create/import presets, profile browser, versions, history, copy/paste/sync settings | 5–8 | ✅ |
| M10 | Export & share | export dialog (JPEG/PNG/TIFF/DNG/AVIF/JXL/original), sizing, sharpening, metadata, watermark, naming, batch jobs, XMP sidecars, HDR export | 6–10 | 🚧 (all formats incl. DNG/original, sizing, presets, background jobs, HDR gain map JPEG / PQ AVIF ✅; JXL encode ⬜) |
| M11 | RAW II | CR3, RAF (X-Trans), ORF, RW2, PEF, SRW, 3FR, IIQ + long tail; camera calibration DB; HEIC/AVIF/JXL import | 20–35 | 🚧 (CR3 lossless CRX and C-RAW, RAF uncompressed and lossless/lossy compressed, RW2 (every raw format), PEF, ORF uncompressed ✅; **camera colour calibration** 🚧 (guarded ARW, NEF, RW2 and RAF preview fitting; measured database still missing), CR3, compressed ORF ⬜) |
| M12 | AI & smart features | subject/sky/background/people/object masks, semantic search, faces/People (permissively licensed models, pure-Rust inference) | 20–40 | ⬜ |
| M13 | Merge | HDR merge (deghost), panorama (projections, boundary warp, fill edges), HDR panorama | 10–15 | ✅ |
| M14 | Video | import/playback/trim via FilmCraft crates, global edits + presets on video, video export | 6–10 | ⬜ |
| M15 | Classic modules | Map, Book, Slideshow, Print, Web; smart collections, stacks, virtual copies, publish services, tethering | 25–40 | 🚧 (smart albums, stacks, virtual copies, compare/survey ✅; Map/Book/Slideshow/Print/Web ⬜) |
| M16 | 1.0 polish | preferences, shortcut editor, accessibility, localization, packaging (dmg/msi/AppImage/web), hardening | 10–20 | 🚧 (settings, keyboard shortcuts sheet, packaging basics ✅; English/Chinese (Simplified, Traditional)/Japanese/Brazilian Portuguese/Spanish/German/Russian/Ukrainian localisation 🟡; accessibility and further locales ⬜) |

## Parity estimate (feature count updated 2026-10-08; effort estimate from 2026-10-02)

**By feature count** — from `docs/parity.md` (one row per Lightroom feature, menu item and shortcut; `cargo xtask
parity` prints this line on every run, so it stays current):

| Scope | Weighted completion | Rows | 2026-10-02 |
|---|---:|---:|---:|
| P0 (core) | **98.2%** | 200 | 98.7% |
| P1 (important parity) | **95.6%** | 149 | 94.4% |
| P2 (later / AI / niche, incl. Classic modules) | **41.6%** | 160 | 29.4% |
| **All in-scope rows** | **79.7%** | 509 | 75.6% |

The 2026-10-05 count includes three new 🟡 rows that make quality gaps visible (camera colour calibration, verified
camera coverage, render fidelity), which is why P0 dipped slightly.

✅ counts 1, 🟡 ½, ⬜ 0; out-of-scope rows (cloud sharing, Adobe accounts…) are left out.

**By remaining effort** — rows are not equal: a shortcut and the whole Book module are one row each, and what is
left is the heavy part (AI, video, Classic output modules, undocumented raw codecs). Remaining work in **Opus agent-hours**
(one agent working continuously; calibrated on this project — a single lead agent closed ≈ 45 tracker rows of
UI/feature work in ≈ 5 h on 2026-10-01, and ≈ 60 more (preset import incl. `.lrtemplate` / DNG / zip and masks, smart-album
rule editor, auto sync, keyword and label sets, import options and DNG conversion, smart previews, external-editor round
trip, slideshow, auto import…) in ≈ 12 h on 2026-10-02; four to six parallel agents built M0–M13 in ≈ 25 active hours):

| Work package | Tracker rows | Agent-hours | Risk |
|---|---|---:|---|
| Remaining P0/P1 UI and library features (folder rename/move, keyword painter, people view…) | ≈ 8 | 5–10 | low |
| Raw codecs: CR3 (CRX), compressed ORF, NEF lossy-after-split, HEIC/AVIF decode | LR-IMP-FORMATS | 40–80 | **high** — clean-room black-box analysis, no permissive specs |
| Lens-profile database of our own (calibration targets, fitting, data) | LR-EDIT-OPTICS-PROFILE | 15–30 | data collection |
| Video: playback, trim, edits, export (pure-Rust decode, ideally shared with FilmCraft) | R. Video | 20–40 | medium |
| AI: subject / sky / background / people / object masks, object-aware remove, AI denoise, super resolution, lens blur, people & faces, natural-language search, culling | ≈ 30 | 80–150 | **high** — permissively licensed weights, pure-Rust inference, maybe training |
| HDR editing, display, visualisation and export | Q. HDR, LR-EXP-HDR | 15–25 | medium |
| Classic modules: Map, Book, Slideshow module, Print, publish services, tethering, soft proofing | ≈ 50 | 60–100 | medium (large, well-understood) |
| Smaller P2 items: Enhance dialog, export to Photos, help / what's new, accessibility, localisation, sidecar variants | ≈ 15 | 12–25 | low |
| Look tuning, performance budgets (colour NR at half resolution on CPU + GPU, 100k-photo library), packaging, hardening | — | 25–45 | medium |
| **Total remaining** | | **≈ 270–505** | |

Spent so far ≈ 105–145 agent-hours, so **by effort the project is ≈ 20–35% of the way to complete Lightroom + Classic
parity**, and ≈ 45–60% of the way for cloud-Lightroom parity without AI and the Classic modules (remaining ≈ 130–255 h,
most of it the raw codecs, lens data, video and HDR).

**Wall clock:** ≈ 270–505 h for one agent working alone; with 4–6 parallel agents (≈ 70% parallel efficiency, merges and
CI under load cost the rest) **≈ 65–125 h of continuous work**. The AI package and the raw codecs carry most of the
uncertainty: they can finish faster if suitable permissive models / documentation turn up, or stall on licensing.

## Totals

Remaining from 2026-10-02 evening (see *Parity estimate* above for the breakdown):

| Target | Remaining agent-hours | Wall clock, 4–6 parallel agents |
|---|---:|---:|
| Cloud-Lightroom parity without AI or Classic modules | ≈ 130–255 h | ≈ 35–65 h |
| Full parity incl. AI, video and the Classic modules | ≈ 270–505 h | ≈ 65–125 h |

The milestone estimates in the table above were made before work started and are kept for calibration (M0–M13 took
≈ 25 active hours with parallel agents against an estimate of ≈ 110–170 h for those milestones).

## Risks that coding hours alone don't retire

- **AI features** (subject/sky/people masks, generative remove) need model weights with licences we can ship; classical
  fallbacks first. No permissively licensed sky-segmentation or raw-denoise model was found — we may need to train our own.
- **Camera colour and lens data** is a data problem: we never use Adobe's matrices, DCPs or LCPs. DNG-embedded data first,
  then our own calibration; long-tail camera/lens coverage grows over time.
- **Raw-format sources:** decided 2026-10-05: decoders are written from *prose* format descriptions (even ones
  published alongside GPL code); decoder source is never read. Still open: freedom-to-operate review for local
  Laplacian filters, PatchMatch and HEVC (HEIC).
- **Look parity** with Adobe's default rendering is measured for the default raw response — tone *and* colour, against
  local-only Lightroom references (LR-BEHAV-RENDER-FIDELITY). Every other slider's response, and the presets, are still
  tuned by eye; the planned fidelity suite turns those into the same kind of measured comparison. Retuning changes
  existing edits, so it ships as a new process version (`docs/process-versions.md`): photos keep their look until they
  are updated.

## Raw format coverage and known gaps

Sony ILCE-7M4 downsized lossless ARW (subsampled YCbCr 4:2:0 / 4:2:2 tiles) now decodes to linear RGB;
private 3:2 & 4:3 examples verified through native rendering & full-resolution 16-bit export.

CR3 lossless Bayer and version 0x100/0x200 C-RAW sensor decoding is verified pixel-exactly on Canon M50, R100 and R8; any other CR3, or one that fails to decode, opens from its embedded JPEG as before. See [`docs/cr3.md`](docs/cr3.md) for coding limits, reference provenance and colour limitations.

Decoded (CC0 corpus from raw.pixls.us, `cargo xtask corpus --download`, `crates/raw/tests/corpus.rs`): DNG (uncompressed,
LJ92, lossy JPEG / Smart Previews, Deflate, float, linear, DNG 1.7 JPEG XL tiles: lossless ones bit-exact on synthetic files,
lossy ones unclipped in the declared linear primaries, checked on one real file), CR2, ARW (uncompressed, ARW2, LJ92; as-shot white balance and black level of pre-2017 bodies from the encrypted
`SR2SubIFD`, decrypted with a keystream recovered by black-box analysis, `crates/raw/src/vendor/sr2.rs`, `crates/raw/src/vendor/arw.rs`), NEF/NRW uncompressed and Huffman-compressed (lossless, lossy type 1/2, 12/14-bit), RAF uncompressed and lossless/lossy compressed (Bayer and
X-Trans, 14/16-bit real-file verification), RW2 / Leica RWL / Panasonic RAW in every raw format (compressed formats 4 and 6, the prefix-coded strips of format 8,
packed formats 2, 5 and 7, the 16-bit words of the 2005–2007 bodies; all recovered by black-box analysis of 178 CC0 files from
118 bodies, `crates/raw/src/vendor/rw2.rs`), PEF (uncompressed and Huffman), ORF uncompressed (16-bit and 12-bit packed). Supported containers yield their embedded JPEG preview when present (including CR3). The engine uses it for RAW variants it cannot decode yet; HEVC-only CR3 previews are not decoded.

Not decoded yet — preview only (no permissively licensed description; black-box analysis incomplete):
- **Olympus compressed ORF**, **Canon CR3 unverified CRX variants** (M11.1), **Canon sRAW/mRAW**.

**Camera colour matrices:** ARW, NEF, RW2, RAF, CR3, CR2, PEF, Samsung SRW and Olympus ORF files can use guarded, separate chromaticity and tone estimates from their own embedded JPEG (see `docs/camera-preview-colour.md`); this is a per-file camera-look estimate with relative WB, not measured calibration or absolute-Kelvin WB. 52 models whose fit is rejected (or that have no usable JPEG) start from matrices fitted to their measured spectral sensitivities instead (rawtoaces-data, Apache-2.0; `crates/raw/src/spectral.rs`). Other non-DNG raws and rejected fits use the documented neutral fallback (camera RGB ≈ linear sRGB, flagged
`matrix_is_fallback`) with the file's as-shot white-balance multipliers. Clean sources to evaluate next: manufacturer
matrices stored in the files themselves (Olympus ImageProcessing `ColorMatrix`, Pentax equivalents; Panasonic RW2 files carry none) and our
own chart-based calibration (M11.4). Adobe matrices are never used.

## Lightroom Classic catalog migration

File → Import Lightroom Catalog… reads `.lrcat` plus committed WAL pages directly through a pure-Rust
SQLite reader. Originals stay in place; ratings, flags, labels, XMP metadata, hierarchical keywords,
collections/sets, virtual copies and supported develop settings migrate into LightCraft. Existing edits
are preserved by default, reimport identities are persistent, and source settings/history/snapshots are
archived before catalog mutation. Missing originals remain available for relinking.

Rendering is approximate: unsupported Adobe profiles/AI/process settings are reported and archived;
history/snapshots remain source data, and smart collections import current membership. The Lightroom
database is read-only; LightCraft owns subsequent edits. See `docs/lightroom-catalog-import.md`.

## Log
- 2026-09-30: roadmap created; M0 in progress; research docs (Lightroom reference, Rust imaging ecosystem) complete.
- 2026-09-30 (later): app running with the full Lightroom-style UI; pipeline v0; DNG/CR2/ARW; README showcase. ≈ 8 h elapsed.
- 2026-10-02: parity estimate added (xtask parity prints weighted completion); milestone statuses refreshed.
- 2026-10-08: Fujifilm camera colour: guarded JPEG matching and relative WB, plus bundled X-H2S/X-T4 profiles from 201/545 photos. On 150 withheld files the pooled profiles improve mean ΔE slightly over per-file fitting (X-H2S 1.70 → 1.62, X-T4 2.43 → 2.34); dark X-H2S samples 8.74 → 2.25. Measured calibration remains open.
- 2026-10-08: Fujifilm lossless/lossy compressed RAF: pure-Rust striped decoder; full sensor arrays verified against a black-box reference on 17 CC0 files and 7 supplied X-H2S / X-T4 files (13 camera bodies, 14/16-bit Bayer and X-Trans). Camera colour calibration remains a separate gap.
- 2026-10-01: RAW II formats: RAF (uncompressed Bayer + X-Trans), RW2 (packed), PEF (incl. Huffman), ORF (uncompressed); embedded previews for every container incl. CR3; raw corpus test with 37 CC0 samples.
- 2026-10-03 – 10-05: community PRs (#42–#51, #60) and 25+ issues landed: compressed NEF, Move import, folder templates,
  Local roots and cleanup, 85k-photo grid and catalog performance, background compaction, failed-save errors, GPU
  export hardening, clippy 1.99. Added the honest *Where we stand* assessment and *Where we're going* priorities; added
  tracker rows for camera colour, camera coverage and render fidelity.
- 2026-10-07: Panasonic RW2 / Leica RWL / Panasonic RAW decode in every raw format (compressed 4/6/8, packed 2/5/7,
  16-bit words), black levels, in-camera aspect crops and mapped-out defects established on 178 CC0 files from 118
  bodies; RW2 gets the file-local camera look (accepted on 140 of 174 files with a preview).
- 2026-10-08: independent Rust CR3 lossless and version 0x100/0x200 C-RAW decoding, six exact full-sensor regressions on M50/R100/R8; any CR3 decode error falls back to the embedded JPEG; guarded CR3 camera-JPEG colour fitting with independent framing validation (`docs/cr3.md`). Unverified CRX variants and broader model verification remain.
- 2026-10-09: Sony ILCE-7CR camera colour: bundled profile from 125 photos. On 26 withheld photos mean ΔE vs the camera JPEG 4.07 → 3.29; all 131 photos accepted, where 8 fell back to the neutral matrix without it. Measured calibration remains open.

## Chinese and Japanese interfaces, and text watermarks

English, Simplified Chinese, Traditional Chinese (Taiwan) and Japanese interface languages are persisted in UI state, and the
language table (`crates/ui-egui/src/i18n.rs`) drives the menus, settings and fonts, so another
language is a table entry plus two catalogs (docs/localization.md). Core menus and panels are
translated; untranslated text retains English. Ukrainian covers all existing catalog keys, command/control/rule labels
and format messages, with count-neutral labels and numeric dates; technical errors from lower layers remain English.
CJK glyphs (Chinese: Noto Sans CJK SC; Japanese UI:
BIZ UDPGothic; watermarks: BIZ UDMincho) come from storytold/craft-fonts, embedded by builds made
with the optional `CRAFT_FONTS_DIR` input (all releases), so no system fonts are needed. Text watermarks now accept
`vertical: true` in export JSON/presets and expose an orientation selector. Japanese
characters stay upright in top-to-bottom columns, with newlines starting columns to the
left. This is basic lettering, without tate-chu-yoko, ruby, kinsoku, or general vertical
OpenType shaping. The same coverage renderer serves 8/16/32-bit exports.
