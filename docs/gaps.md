# Gaps: where LightCraft falls short of Lightroom

> **Last reviewed:** 2026-10-11 · **Last updated:** 2026-10-24 · **Change:** minor (gap 30: the Info panel shows the selected photos' shared metadata and marks mixed fields; gap 28: ⌘⌥⇧C / ⌘⌥⇧V copy and paste metadata; gap 13: before a Lightroom catalog import the app asks merge or new library, #2672; gap 28: F8 shows / hides the right panel; import review album hierarchy, #2362; gap 20: Minolta MRW decodes; gap 10: 551 models verified; gap 15: Print Page Options — page numbers, page info, crop marks; gap 28: `\` shows / hides the Filter Bar in the grids; gap 21: Indonesian interface shipped, every message but What's New; gap 27: the Windows x64 installer verified on Windows 11; the Settings → Apps uninstall confirmation remains; gap 28: ⌘L Enable Filters switches the filter off and on; gap 15: Japanese photo and postcard paper sizes in the Print page setup and PDF)

Every known shortfall, one entry each, ranked by user impact (how many Lightroom users hit it, and
how hard it blocks them), then by effort. **Agents pick work from the top** (the first entry nobody is
working on). When you close a gap, delete or shrink its entry, update its rows in
[parity-checklist.md](parity-checklist.md), and the numbers in [target-app-parity.md](target-app-parity.md)
and [ROADMAP.md](../ROADMAP.md) if they move.

Hours are Opus 5.5 agent wall-clock hours (see [target-app-parity.md](target-app-parity.md#how-hours-are-calibrated)).
`B` marks a gap that blocks **beta**.

## Ranked

| # | Gap | Evidence | User impact | Hours | Doc |
|---:|---|---|---|---:|---|
| 1 B | **Camera colour is estimated, not measured** (LR-PROF-CAMERACOLOR). Raws start from a per-file fit to the camera JPEG, 5 bundled profiles or 52 spectral matrices (plus same-sensor bodies, e.g. the X-T30 through the X-T3); others use a neutral matrix (a black-and-white camera JPEG gives its tone curve only, #577; dark JPEG tones also guide the toe, #1307) | ΔE vs camera JPEG 2.9–10 after fitting (raw-parity.md); 0 chart-calibrated models vs 1,500 Adobe Standard profiles | every raw shooter sees colour that is close but not Lightroom's; some get neutral-matrix colour | 50–90 | [raw-parity](raw-parity.md#colour) |
| 2 B | **CR3 variants no public file uses** (LR-IMP-FORMATS): `crx.rs` refuses vertical-tile C-RAW, partial-subband and other plane coding, multi-tile 0x200 and adaptive low-frequency gains, and those files fall back to the preview. All 110 CC0 CR3s on raw.pixls.us (31 folders) decode from sensor data, so none of these variants has a public sample; 26 CR3 bodies are verified (M50 / R100 / R8 exact, R5 Mark II pinned, 22 more pinned on one CC0 file each and checked against the camera JPEG, [cr3.md](cr3.md#further-bodies-one-cc0-file-each)). HEIF stills are `.HIF` files, not CR3; no CR3 sRAW / mRAW sample exists | measured: Canon's share whose camera fully works 73% → 94% (raw-parity.md); 18 of 22 popular Canon bodies write CR3, all 18 now verified | +0.3 points of photographers; the variants surface only as user files that open as a preview | 10–25 | [raw-parity](raw-parity.md#remaining-work-ranked-by-photographers-unlocked-per-hour) |
| 3 B | **Render fidelity unmeasured** (LR-BEHAV-RENDER-FIDELITY): tone, highlights, texture / clarity / dehaze, NR and sharpening are tuned by eye | no side-by-side suite; a retune needs a new process version (only V1 exists) | edits imported from Lightroom look different; the look "isn't Lightroom's" | 30–60 | [raw-parity](raw-parity.md), [process-versions](process-versions.md) |
| 4 | ~~Original-pixel detail at 100%~~ (LR-VIEW-ZOOM): closed 2026-10-12, see Closed recently | checklist row ✅ | — | 0 | [ui-parity](ui-parity.md) |
| 5 B | **AI masks: Subject, Sky, People, Background, Landscape** are heuristics; Object / Describe run SAM 3 but its download has no mirror configured | `crates/segment`; ai-masks.md; MASK rows 67% | masking is how most Lightroom users edit today | 40–70 + model decision | [target-app-parity](target-app-parity.md#ai) |
| 6 B | **Compressed ORF** (every Olympus / OM body since ~2008) | measured: 66 of 78 Olympus / OM models preview-only; `orf.rs` refuses; OM bucket 3% of photographers, 3% of them fully work | +2.6 points; best gain per hour of any raw gap (~0.2) | 8–15 | [raw-parity](raw-parity.md#compressed-orf-a-clean-room-starting-point) (the two Olympus patents that describe the method) |
| 7 | **Nikon High Efficiency NEF** (HE / HE★ on Z 8, Z 9, Z 6III, Z f, Z50II, Z5II when set to HE) | named and preview only (#193); needs a pure-Rust JPEG XS decoder | +1.1 points (only shots taken in HE; lossless works on the same bodies); low gain per hour (~0.04), so not a beta blocker | 20–40 | [raw-parity](raw-parity.md) |
| 8 B | **No lens profile database** (LR-EDIT-OPTICS-PROFILE): only DNG opcodes, RW2 in-file distortion, Sony ILCE-7RM4A, full-frame ILCE-7CR distortion (Auto only) and ILCE-7RM6 distortion (`docs/sony-ilce7rm6-lens-corrections.md`) and Nikon Z 6II / Z 8 / Z f distortion (`docs/nikon-lens-corrections.md`) | Adobe ships 3,635 lens profiles; we never use them | distortion / vignetting correction is a default step for many | 40–80 + calibration shots | [raw-parity](raw-parity.md#lens-profiles) |
| 9 B | **Tests don't run in CI on macOS or Windows**: Linux x64 runs `cargo xtask ci` on every PR and push to main (`.github/workflows/ci.yml`) and FreeBSD runs `cargo test`; the others build packages | `.github/workflows/` (ci, freebsd, packaging-lint, release, windows-arm64); the linux-riscv64 release job added in 0.6.0 (#844) runs only `lightcraft-cli --version` under QEMU; macOS / Windows jobs: #1148 | regressions in `cfg(target_os)` code reach macOS and Windows users | 6–12 | [target-app-parity](target-app-parity.md#by-dimension) |
| 10 B | **Per-model verification thin, plus cheap per-body decode fixes**: 551 of 1,446 Adobe models verified (38%), ~85% of photographers on a verified body; Nikon 5 bodies verified, Canon 34, Leica 14 (the Q3 by a recorded check of 38 private DNGs). Cheap fixes: Z 6II packed 14-bit NEF (#769, PR #671), Sony A100 regression (#702); F828 SRF now discovered and imported as preview-only, sensor decoding still missing, DSLR-A200…A390 / A850 12-bit ARW (#535) | measured: [raw/coverage.tsv](raw/coverage.tsv) | per-body bugs (levels, crops, WB) surface only from users | 25–45 | [raw-parity](raw-parity.md#by-maker-measured) |
| 11 | **Enhance: Raw Details, Super Resolution; AI denoise without shippable weights or X-Trans** | ENH rows 0%; denoise Bayer only, RawNIND weights GPL and opt-in | Fujifilm users get no AI denoise; nobody gets it out of the box | 30–60 + model decision | [raw-parity](raw-parity.md#demosaic-levels-crops-and-lens-metadata) |
| 12 | **Content-aware / generative remove, detect distractions, reflections removal** | REM rows 75%; patch synthesis missing | cleanup work goes back to Photoshop | 25–45 (+ generative model) | [target-app-parity](target-app-parity.md#ai) |
| 13 | **Lightroom catalog migration is one-way and approximate**: unsupported Adobe profiles / AI settings archived, history kept as data; main-file pages read on demand up to 16 GiB, with dense-table and WAL bounds retained (#798); offline record counts and destination are now shown in a lasting import result; before an import the app asks whether the catalog merges into the open library or goes into a new library (opt-out in Settings, #2672) | lightroom-catalog-import.md | switching users lose some looks | 8–12 | [file-format-parity](file-format-parity.md) |
| 14 | **HDR display** (EDR / Windows HDR); the headroom limit and SDR rendition from `crs` XMP (the HDR switch, `HDREditMode`, is read). HLG AVIF export done; the display's SDR/HDR decision is tested, blocked on eframe's 8-bit surface | HDR rows 70%; LR-VIEW-HDR-DISPLAY ⬜ | HDR editing works blind on HDR monitors | 12–25 | [hardware-parity](hardware-parity.md) |
| 15 | **Print module**: identity plate / watermark / photo info overlays, Cut Guides for the packages, Custom Package Auto Layout, printer-driver Page Setup and direct printing, JPEG file output (the Print dialog writes a colour-managed PDF) | LRC-PRINT rows | Classic users who print | 15–25 | [target-app-parity](target-app-parity.md#features) |
| 16 | **Map module (saved locations), Book, Slideshow module, publish services** | LRC-MAP rows partly done (Map view, Info-panel map, geotagging by drag and the location filter bar ✅), BOOK / SS rows ⬜ | Classic users; publish services matter for Flickr / SmugMug workflows | 60–100 | [target-app-parity](target-app-parity.md#features) |
| 17 | **Tethered capture** | ⬜ | studio photographers | 25–45 + cameras | [hardware-parity](hardware-parity.md) |
| 18 | **Video**: playback, trim, global edits, export | VID rows 0% | users who mix stills and clips | 25–45 | [target-app-parity](target-app-parity.md#features) |
| 19 | **Plug-in ecosystem**: no Lightroom SDK equivalent (export / publish plug-ins, metadata plug-ins) | ⬜ | pro workflows built on plug-ins (LR/Enfuse, Jeffrey's exporters, Negative Lab Pro) | 40–80 + owner decision | [target-app-parity](target-app-parity.md#by-dimension) |
| 20 | **Long-tail raw formats**: compressed SRW, ARQ pixel shift, IIQ, Hasselblad compression, X3F, CRW, ERF, KDC, DCR, MEF, GPR, old FinePix (MRW decodes since 2026-10-18) | raw-parity.md | small user share each | 40–80 | [raw-parity](raw-parity.md) |
| 21 | **Localization**: 10 of 16 shipped languages partial (de, es, fr, ja, uk now 97–99% of catalog messages and id every message but What's New, awaiting native review; pt-BR, ru, zh-hans, zh-hant 74–92%), lower-layer errors English; Hindi, Arabic, Korean, Vietnamese absent; no RTL or complex shaping; exports add Thai glyphs only (#2052), with horizontal mark positioning still incomplete and Arabic fallback deferred until joining and bidi exist | measured from catalogs | non-English users see mixed English | 40–70 (shipped to full); 135–239 (four new) | [localization-parity](localization-parity.md) |
| 23 | **Accessibility**: AccessKit tree audited by a test; canvas handles, spots and mask pins by keyboard; left: brush and red-eye by keyboard, a session with a real screen reader | LR-BEHAV-ACCESS 🟡 | blind and low-vision users | 2–4 | [ui-parity](ui-parity.md) |
| 22 | **Pen pressure on macOS / Linux tablets** (brush size and flow follow pressure where winit reports a pen force), OS monitor profiles on Windows (Linux reads colord's, macOS the screen's), follow-window colour on Linux / Windows | hardware-parity.md | brush users with tablets; multi-monitor setups | 11–22 | [hardware-parity](hardware-parity.md) |
| 24 | **Brush live preview and masking feel** (#517, #518); Feather / Edge mask refinement (15.5) | issues | masking feels slower than Lightroom | 8–12 | [ui-parity](ui-parity.md) |
| 25 | **File writes**: AVIF import; Render to DNG as a catalog command (lossy DNG and the Render to DNG export format landed) (PSD export landed with Edit in PhotoCraft, #953) | file-format-parity.md | interchange edge cases | 19–34 | [file-format-parity](file-format-parity.md) |
| 26 | **Performance at scale**: 85k-photo library opens in 1.7–4.7 s; 100k grid test pending; colour NR at half resolution; at 100k photos stack and album lookups are indexed (an "In a Stack" or "Album" smart album counts in ~14 ms, was 0.6–2.7 s) | ROADMAP log, gpu-pipeline.md | large-catalog users | 15–30 | [target-app-parity](target-app-parity.md#by-dimension) |
| 27 | **Windows / Linux runtime coverage**: the Windows x64 installer is verified on Windows 11 (0.9.1: install and upgrade end on the completion page, a custom folder is kept, Launch works, removal is clean), but uninstalling from Settings → Apps ends with no confirmation (#892, #931); GPU path proven on Apple and on one AMD GPU on Linux (Radeon 8060S, Vulkan); NVIDIA, Intel and DX12 unverified; the Linux riscv64 tarball (new in 0.6.0, #844) is cross-compiled and never run beyond a QEMU smoke test | ROADMAP, issues, `release.yml` | non-Mac users find bugs first | 13–27 | [hardware-parity](hardware-parity.md) |
| 29 | **HDR merge leftovers** (LR-MERGE-HDR): AI Denoise doesn't run on merged (linear) DNGs; JPEG brackets are treated as linear (no camera response curve); every frame is held as 32-bit float, ~0.86 GB per 67 MP frame (half-float frames would halve it) | tested on Sony A7R VI brackets 2026-10-14 ([merge.md](merge.md)); Lightroom's Denoise supports HDR DNGs | HDR shooters who denoise their merges, merge JPEGs, or merge 7+ frames of high-resolution raws on 16 GB machines | 10–20 (+ denoise linear-RGB contract, gap 11) | [merge](merge.md) |
| 28 | **Small Classic shortcuts and keys**: Tab / ⇧Tab (hide the side panels / all panels; Tab moves the keyboard focus here) and F5. Rating `[` `]`, flag cycling (`` ` ``), `\` (Filter Bar), ⌘L (Enable Filters), ⌘⌥⇧C / ⌘⌥⇧V (Copy / Paste Metadata), F1 help, F6 / F7 / F8 (filmstrip, left panel, right panel) are bound; the grid-cycle key is shown beside both grid rows in View (#2500) | [ui-parity](ui-parity.md#shortcuts) | Classic muscle memory | 2–4 | [ui-parity](ui-parity.md) |
| 30 | **Multi-select metadata editing in the UI**: `photo.setMeta` already iterates all selected photos, so the engine applies changes to every target. The Info panel's Show Metadata For choice (Target Photo / Selected Photos) shows the values the selected photos share and marks each field where they differ as Mixed; the All Metadata dialog still shows one photo's values with no "mixed" indication | standard in Lightroom Classic's multi-select Info panel | anyone editing metadata on a batch (copyright, keywords, custom fields) | 1–3 | [ui-parity](ui-parity.md) |

Beta-blocking gaps (B) total **~230–430 h**; with the stability and platform work they need around them,
**~340–600 h** to beta (incl. 40–80 h for the ~92 open core-path user issues) (see [target-app-parity.md](target-app-parity.md#stage)).

## Gaps by kind

- **Feature:** 5, 8, 11, 12, 13, 15, 16, 18, 19, 29
- **UI / UX:** 4, 23, 24, 28, 30
- **File format:** 2, 6, 7, 10, 13, 20, 25 (raw ones in [raw-parity.md](raw-parity.md))
- **Hardware:** 14, 17, 22, 27
- **Localization:** 21
- **Quality / fidelity:** 1, 3
- **Stability and platforms:** 9, 26, 27

## Closed recently (kept for calibration)

- 2026-10-22: saving XMP preserves foreign namespaces even when they use `lc:` or `xmp:` prefixes; importing them no longer interprets their properties as native settings or ratings. Parser, merge and save-command regressions cover the correction; no ranked gap or readiness estimate changes.

- 2026-10-22: PSD/PSB imports and metadata readers now expose embedded EXIF resource 1058 and XMP resource 1060 for capture dates, GPS and All Metadata (#2519); imported PSD edit copies retain their capture date, and the `removeLocation` Original-export guard sees embedded PSD/PSB GPS. No ranked gap or headline feature count closes.

- 2026-10-20: the import review album picker follows the sidebar folder hierarchy and searches album paths (#2362, LR-IMP-ADD-DIALOG), retaining None/New album and existing destination IDs. This closes a usability subgap; no ranked gap or headline feature count closes.

- 2026-10-12: original-pixel detail at 100% (gap 4, LR-VIEW-ZOOM, #323): soft proofing gets zoom windows like
  Detail, Before/After, Compare, Reference and mask overlays; 1:1 tested equal to the full-size export.
- 2026-10-11: Sony ILCA-99M2 profile.
- 2026-10-11: Nikon edge bars on bodies without CropArea (#976, #914): 36 model / size / depth groups measured.
- 2026-10-10: Nikon lossy-after-split NEF (#633), ORF 12.8 bpp (#651), HDR gain map / PQ AVIF / float
  TIFF export (#646), activity stack (#345), JPEG XL export (#247).
- 2026-10-09: EOS R7 C-RAW with an odd-height QP map (a45c2b66; part of gap 2), HEIC in release builds (#609), Samsung uncompressed SRW (#459), contact-sheet PDF (#563),
  Ukrainian UI (#564), Sony ILCE-7CR profile.
- 2026-10-08: CR3 CRX lossless and C-RAW (#279), Fujifilm compressed RAF (#282), AI RAW denoise (#384),
  Lightroom catalog import (#218), German, Spanish, Russian UI.
- 2026-10-07: RW2 every format (#215), faces recognition in pure Rust (#174).
- 2026-10-05: compressed NEF (#86). The older history is in [ROADMAP.md](../ROADMAP.md#progress-log).

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-24 | minor | Gap 28: ⌘⌥⇧C / ⌘⌥⇧V (Ctrl+Alt+Shift+C / V on Windows and Linux) copy and paste metadata; no key leaves the entry; hours unchanged |
| 2026-10-24 | minor | Gap 30 shrinks: the Info panel's Show Metadata For (Target Photo / Selected Photos, `view.infoSelectedPhotos`) shows the selected photos' shared values and marks fields that differ as Mixed; the All Metadata dialog remains; 1–3 h (was 4–8) |
| 2026-10-23 | minor | Gap 13: before a Lightroom catalog import the app asks whether it merges into the open library or goes into a new one (opt-out in Settings, #2672); hours unchanged |
| 2026-10-22 | patch | Gap 28: View shows the current grid-cycle key beside Photo Grid and Square Grid (#2500); no new binding and no change to the remaining shortcut gap or hours |
| 2026-10-22 | minor | Gap 15: the Print page setup and PDF gain the Japanese photo and postcard paper sizes (L判 89×127, 2L判 127×178, KG判 102×152, はがき 100×148, 往復はがき 148×200, 六切 203×254, 四切 254×305, 名刺 55×91 mm), by their customary names; gap not closed |
| 2026-10-22 | minor | #2052: Thai glyphs added to shared export text; gap 21 retains Thai mark positioning, Arabic joining and bidi limits. No ranked gap closes; estimates unchanged |
| 2026-10-24 | minor | LR-LIB-SMARTALBUM: relative-date views and source totals expire on the session-clock minute, including nested smart albums; ranked gaps and readiness counts unchanged |
| 2026-10-22 | patch | XMP namespace-prefix collisions no longer overwrite foreign metadata; ranked gaps and estimates unchanged |
| 2026-10-22 | patch | Issue #2519: PSD/PSB EXIF resource 1058 and XMP resource 1060 now use the common metadata extractor, and `.psb` files are accepted by import; no ranked gap or readiness count changes |
| 2026-10-22 | minor | Gap 30 added: multi-select metadata editing — Info panel and All Metadata dialog show one photo's values with no mixed-value indication |
| 2026-10-21 | minor | Gap 28 shrinks: F8 shows / hides the right panel (`view.rightPanel`); rating `[` `]`, flag cycling and F1 help were already bound, so the entry lists only filter-bar keys, Tab / ⇧Tab and F5; 2–4 h (was 4–8) |
| 2026-10-20 | minor | #2362: import destination albums use the shared hierarchy/search picker instead of a flat name list; existing import workflows retained; ranked gaps/readiness counts unchanged |
| 2026-10-19 | minor | Gap 9 shrinks: `.github/workflows/ci.yml` runs `cargo xtask ci` on Linux x64 for every PR and push to main; macOS and Windows remain |
| 2026-10-19 | minor | Gap 21: Indonesian (`id`) shipped with every catalog message but What's New; four new languages left (Hindi, Arabic, Korean, Vietnamese), 135–239 h |
| 2026-10-19 | minor | Gap 27: the released 0.9.1 x64 MSI is tested on Windows 11 Pro 24H2 (a fresh install and an upgrade from 0.8.0 end on the completion page, so #790 is fixed; the upgrade keeps a custom folder; Launch starts the app; removal leaves nothing behind); uninstalling from Settings → Apps still ends with no confirmation (#892, #931); hours unchanged |
| 2026-10-18 | patch | Gap 1: six X-Trans CMOS 4 Fujifilm bodies (X-T30 …) take the X-T3's spectral matrices instead of the neutral matrix |
| 2026-10-15 | minor | Gap 15 shrinks: the Page panel's Page Options (Page Numbers, Page Info, Crop Marks) print on Single Image and Contact Sheet pages; identity plate, watermark, photo info, Cut Guides, direct printing and JPEG output remain |
| 2026-10-14 | minor | Gap 4 follow-up: the diagnostic overlays (Visualize Spots, Point Color range, HDR range) render zoom windows too; soft proofing runs on the GPU |
| 2026-10-12 | minor | Gap 10: SR2/SRF folder discovery fixed; F828 imports as preview-only. Sensor decoding remains a gap; readiness scores unchanged |
| 2026-10-23 | minor | Gap 28: ⌘L / Ctrl+L (Library ▸ Enable Filters) puts the filter aside and brings it back as it was; the entry drops ⌘L; hours unchanged |
| 2026-10-15 | minor | Gap 28: `\` shows / hides the Filter Bar in the grids; the entry drops `[` `]` (#509), flag cycling (#1490) and F1 (Help), already on main; ⌘L is the filter-bar key left; hours unchanged |
| 2026-10-14 | minor | Gap 26: at 100k photos stack and album lookups are indexed, so an "In a Stack" or "Album" smart album counts in ~14 ms (was 0.6–2.7 s); open time unchanged |
| 2026-10-14 | minor | Gap 10: Camera-coverage sweep (#1327): 30 models verified on 40 CC0 files (coverage.py 515 → 545 verified); sensor verification does not establish colour parity (#1277) |
| 2026-10-14 | minor | Gap 21: de, es, fr, ja and uk catalogs filled (97–99%); 55–90 h → 40–70 h |
| 2026-10-14 | minor | Gap 10: 17 uncompressed Olympus and Samsung models verified on one CC0 file each (254 models) |
| 2026-10-14 | minor | Gap 29 added: the HDR merge, tested on real 67 MP brackets, keeps moving clipped highlights and merges in batches now; AI Denoise on merged DNGs, JPEG response curves and per-frame memory remain |
| 2026-10-14 | minor | Gap 4 closed: soft proofing at 1:1 renders a window (mask overlays already did); 1:1 pixels tested equal to export; only diagnostic overlays (Visualize Spots, Point Color range, HDR range) stay whole-frame |
| 2026-10-13 | patch | Gaps 10, 22 and 23 each had two rows (two branches edited them; the merges kept both): the older copies are removed, and `cargo xtask parity` (in `ci`) now fails when a gap number is listed twice |
| 2026-10-13 | minor | Closed recently: Sony ILCA-99M2 profile |
| 2026-10-14 | minor | Gap 10: 21 Sony and Leica models verified on one CC0 file each (258 models) |
| 2026-10-12 | minor | Gap 20 shrinks: Mamiya MEF decodes (`vendor/mef.rs`) and is imported; gap 10 count 240 → 241 |
| 2026-10-12 | minor | Gap 20 shrinks: Epson ERF decodes (`vendor/erf.rs`), 3 bodies verified; gap 10 count 237 → 240 |
| 2026-10-13 | minor | Gap 28: F6 (filmstrip) and F7 (left panel) are bound, so the entry no longer lists them as missing; hours unchanged |
| 2026-10-11 | minor | Gap 10 shrinks: Leica Q3 verified by a recorded check of 38 private DNGs (236 models verified; Leica 14); gap not closed |
| 2026-10-12 | minor | Camera-look deep shadows follow the JPEG toe (#1307); camera calibration remains partial |
| 2026-10-12 | minor | Gap 23 (accessibility): crop handles, spots, gradient handles and mask pins by keyboard; 4–8 h → 2–4 h; gap not closed |
| 2026-10-12 | minor | Gap 23 (accessibility): the AccessKit tree audit and its fixes landed; what is left is canvas placement by keyboard and a real screen-reader session; 10–20 h → 4–8 h; gap not closed |
| 2026-10-12 | minor | Gap 22: Linux previews use the display profile from colord |
| 2026-10-12 | minor | Gap 27: the CPU ↔ GPU equivalence suite passes on an AMD Radeon 8060S (Vulkan, Linux), the first non-Apple GPU verified; 15–30 h → 13–27 h; gap not closed |
| 2026-10-12 | minor | Gap 25: Render to DNG lands as an export format; lossy DNG and AVIF import remain |
| 2026-10-12 | minor | Gap 10 shrinks: the Hasselblad Lunar (a Sony NEX-7 ARW under its own Make) decodes and is verified, 236 models (#1275) |
| 2026-10-12 | minor | Gap 14: HLG AVIF export (`hdrTransfer`) and a tested SDR/HDR display decision |
| 2026-10-12 | minor | Gap 15: printer colour management lands (printer / paper ICC profiles or Managed by Printer, intent, black point compensation, print adjustment, print sharpening, 16-bit ICC-tagged PDF with an output intent); page overlays and direct printing remain |
| 2026-10-12 | minor | Gap 15 shrinks: File → Print… is a Print dialog with the four layout styles, cell editing, image settings, guides, page setup and templates; printer colour management, page overlays and direct printing remain |
| 2026-10-12 | minor | Gap 15: the Print module's layout styles, cells, image settings, templates and PDF are `print.*` commands (CLI, control, MCP); the Print view, printer colour management and page overlays remain |
| 2026-10-12 | minor | Gap 15: print pages compose in the engine (fit / Zoom to Fill, Rotate to Fit, border, stroke, background) with an interim PDF writer; commands and UI still to land |
| 2026-10-12 | minor | Gap 15: the Print layout model (paper sizes, margins, grid, package cells, Auto Layout packer, snapping) is in the engine; commands, composition and UI still to land |
| 2026-10-12 | minor | Gap 25: lossy DNG export (DNG compression 34892) lands; AVIF import and Render to DNG as a catalog command remain |
| 2026-10-12 | minor | Gap 22: pen pressure scales brush size and flow (Brush ▸ Pen Pressure) where winit reports a pen force |
| 2026-10-12 | minor | Gaps 2 and 10 shrink: 22 more CR3 bodies verified on one CC0 file each (every CC0 CR3 on raw.pixls.us decodes), EOS R6 Mark III white balance read; Canon 34 verified, 259 models, ~59% on a verified body; CR3 gain +6.2 → +0.3 points |
| 2026-10-12 | minor | Gap 10 shrinks: 117 Nikon and Fujifilm bodies verified on one CC0 file each (354 models, ~58% on a verified body); COOLPIX 5700 / 8400 / 8800 decode wrongly; #702 is fixed |
| 2026-10-11 | minor | Nikon edge bars on bodies without CropArea fixed for 36 model / size / depth groups (#976, #914; survey in #1053); gap 10 notes the survey and the pending bodies; gap not closed |
| 2026-10-11 | minor | Reviewed against main after 0.6.0: CR3 entry names the CRX variants `crx.rs` still refuses and the R7 / R6 Mark II C-RAW state (decoding, not verified, so the measured numbers don't move); CI and Windows / Linux entries note the riscv64 release build (#844); R7 fix added to Closed recently. Panel docking (UI.9, #869) closes no listed gap |
| 2026-10-11 | minor | Camera colour entry: raws with a black-and-white camera JPEG take its tone curve (#577); gap not closed |
| 2026-10-11 | minor | Gap 4: Compare and Reference views get zoom windows per pane (1:1 was a 2,560 px render magnified); overlays and soft proofing remain, 8–15 h → 4–8 h |
| 2026-10-10 | minor | Compressed ORF entry links the two Olympus patents that describe the method |
| 2026-10-11 | minor | Gap 14: `crs:HDREditMode` is read from XMP (#663); the headroom limit and SDR rendition still aren't |
| 2026-10-11 | minor | Gaps 2 and 10 shrink: EOS R5 Mark II CR3 decodes and is verified (fourth CR3 body, Canon 12 verified, 235 models, ~42% on a verified body); CR3 gain +6.5 → +6.2 points |
| 2026-10-10 | minor | Map location filter bar landed (LRC-MAP-FILTER): dropped from gap 16 |
| 2026-10-10 | minor | Map geotagging by drag landed (LRC-MAP-GEOTAG): dropped from gap 16 |
| 2026-10-10 | minor | RAW gaps re-measured model by model (raw-parity.md): CR3, compressed ORF and verification entries carry measured shares; HE NEF no longer a beta blocker (+1.1 points for 20–40 h); verification entry absorbs the cheap per-body fixes (#769, #702, A200 series) |
| 2026-10-10 | minor | Beta total raised to 340–600 h to include the ~92 open core-path user issues |
| 2026-10-10 | major | Created from the checklist's Top gaps (whose items 1–2 had been duplicated by a merge) and ROADMAP's Where we stand; re-ranked; added evidence, impact, hours and beta flags; added platform-CI, lens-database, HE NEF and localization gaps |
| 2026-10-13 | minor | Gap 8 narrows: Nikon Z 6II / Z 8 / Z f NEFs use the camera's in-file distortion correction (`docs/nikon-lens-corrections.md`) |
| 2026-10-12 | minor | Catalog migration reports offline records as additions, keeps the destination/result readable, skips missing-original probes and stages only masters needed for virtual copies. Approximate rendering and database/table limits remain. |
| 2026-10-13 | minor | Catalog import review: offline Local records join All Photos, redundant operation/settings copies removed, and result invalidation across library switches covered. Approximate rendering and database/table limits remain. |
| 2026-10-14 | patch | LRC-LIB-CATALOG-IMPORT: reconciled with upstream phased progress, cancellation kinds, WAL-only import, translations and current progress tracking. Status and summary counts unchanged. |
| 2026-10-10 | minor | Gap 8: Sony ILCE-7RM6 in-file distortion added next to the ILCE-7RM4A's, its lateral CA parsed but off by default (docs/sony-ilce7rm6-lens-corrections.md); the gap stays open (no lens database), hours unchanged |
| 2026-10-14 | minor | Gap 10 shrinks: 97 Canon CR2, Pentax and Ricoh models verified on one CC0 file each (334 models, ~50% on a verified body) |
| 2026-10-14 | minor | Gap 10: 515 of 1,446 models verified (36%), ~84% of photographers on a verified body (integration batch 36) |
