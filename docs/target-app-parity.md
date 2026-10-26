# Target-app parity: LightCraft vs Adobe Lightroom

> **Last reviewed:** 2026-10-10 · **Last updated:** 2026-10-19 · **Change:** minor (Minolta MRW decoded, 6 bodies verified; RAW: 83% of Adobe's models decode, 38% verified after the camera-coverage sweep #1327; Platforms: the Windows x64 installer verified on Windows 11) · **Target:** Adobe Lightroom Classic 15.5 (August 2026) and Lightroom 9.6 (desktop)

The authoritative assessment of how close LightCraft is to Lightroom. The row-by-row checklist is
[parity-checklist.md](parity-checklist.md); the ranked work list is [gaps.md](gaps.md); deep areas
have their own documents: [raw-parity.md](raw-parity.md), [file-format-parity.md](file-format-parity.md),
[ui-parity.md](ui-parity.md), [hardware-parity.md](hardware-parity.md),
[localization-parity.md](localization-parity.md).

## Headline

| Number | Value | Kind |
|---|---:|---|
| **Feature breadth**, checklist | **81.6%** of 532 in-scope rows (P0 98.0%, P1 95.2%, P2 50.0%) | **measured**: recount of [parity-checklist.md](parity-checklist.md) rows (✅ 1, 🟡 ½, 🚫 left out), the same rule `cargo xtask parity` uses |
| **Feature breadth**, weighted by use | **~77%** | estimated: feature-area table below |
| **Ready for real work** (full target) | **~61%** (range 56–66%) | estimated: weighted sum over the dimensions with written weights, see [below](#ready-for-real-work-mainstream-practitioner-and-essentials-user) |
| **Mainstream practitioner** | **~51%** (range 46–56%) | estimated: weekly areas of a working photographer, depth 74.0% × 0.804 × colour 0.85 |
| **Essentials user** | **~65%** (range 60–70%) | estimated: core features at default settings, depth 78.0% × 0.830 |
| Remaining to **beta** | **~340–600 h** | estimated; ~60% parallelizes |
| Remaining to **full parity** | **~1,000–1,800 h** | estimated; ~70% parallelizes |
| **Stage** | **alpha** | see [Stage](#stage) |

## The target and how it was measured

- **Target:** Lightroom Classic 15.5 (August 2026; latest per Adobe's release notes) for the
  library, modules and depth; Lightroom 9.6 (the cloud-library desktop app), installed on the
  owner's Mac at `/Applications/Adobe Lightroom CC`, for the interface LightCraft's layout follows.
  Both share the Camera Raw engine.
- **Installed bundle, inspected by listing only** (the clean-room rule forbids reading contents):
  `Info.plist` document types (31 raw extensions, jpg, tif, heic, xmp, catalog bundles); 16
  `.lproj` localizations; `CameraProfiles` (1,500 Adobe Standard and 395 Camera Matching model
  names); `LensProfiles` (3,635 files, 56 makers); `ModelZoo` (37 AI model names). The app was not
  launched.
- **LightCraft side:** origin/main at `b70ae174` (2026-10-10): code counts (226 engine commands, 143
  UI commands, 122 + 10 develop controls, 2,561 tests, ≈ 202,000 Rust lines in 24 crates + 3 apps),
  the checklist rows, raw corpus manifests, locale catalogs, CI workflows, docs.
- **Behaviour and output** were not compared against a running Lightroom in this pass; the
  measured ΔE figures in [raw-parity.md](raw-parity.md) come from earlier work.

**Presence is not parity.** ✅ rows are set by whoever lands a feature and have not been checked
systematically against Lightroom's behaviour; bugs keep turning up in ✅ areas (CR2 colour-filter
phase #85, duplicate Local entries #22, black GPU exports on an Intel iGPU #78).

## By dimension

| Dimension | Breadth | Ready for real work | Remaining | Doc |
|---|---:|---:|---:|---|
| Features (weighted areas below) | ~77% (checklist 81.1%, measured) | ~61% | 600–1,100 h | [Features](#features) |
| ↳ RAW cameras, colour and lens profiles (part of features) | 83% of Adobe's 1,446 models decode (38% verified), measured | ~56% | 255–480 h | [raw-parity.md](raw-parity.md) |
| UI/UX fidelity | ~85% | ~70% | 32–54 h | [ui-parity.md](ui-parity.md) |
| File formats (non-raw: images, sidecars, presets, catalogs) | ~85% | ~70% | 30–55 h | [file-format-parity.md](file-format-parity.md) |
| Hardware | ~50% | ~40% | 40–80 h (+ tethering and printing, counted under Classic modules) | [hardware-parity.md](hardware-parity.md) |
| Localization (12 key languages) | 8 of 12 present | ~64% (mean of the 12, measured) | 130–210 h for the target's set; +135–239 h for Hindi, Arabic, Vietnamese, Korean | [localization-parity.md](localization-parity.md) |
| Performance | ~80% | ~70% | 30–50 h | 24 MP GPU slider 3.9 ms, export 0.3 s (measured, [gpu-pipeline.md](gpu-pipeline.md)); 85k library opens in 1.7–4.7 s; 100k test pending |
| Stability | — | ~60% | 30–60 h | never-crash lints and guards workspace-wide, crash-tested journal, 2,561 tests; but CI runs tests only on FreeBSD, user reports keep finding bugs in ✅ areas |
| Platforms | macOS, Windows (x64 / x86 / ARM64), Linux (AppImage, deb, rpm, Flatpak, Nix), FreeBSD, web | ~60% | 25–45 h | macOS is where it is tested; the Windows x64 installer is verified on Windows 11 (0.9.1) except for a confirmation after uninstalling from Settings → Apps (#892); web experimental (CPU only) |
| Ecosystem / plug-ins | ~25% | ~15% | 40–80 h + owner decision | no Lightroom SDK equivalent (export, publish, metadata plug-ins); beyond the target: JSON-lines control channel, MCP server with 28 tools + one per command, CLI |
| AI features | ~30% | ~20% | 120–220 h + model decisions | [AI](#ai) (overlaps the masking and Enhance rows; not added twice) |

## Features

Weights are a judgement of what Lightroom users spend their time on (library and develop dominate;
Classic output modules and video are used by few). Breadth and readiness per area are estimated
from the checklist sections and the deep-dive documents.

| Area | Weight | Breadth | Ready | Remaining | Notes |
|---|---:|---:|---:|---:|---|
| Import and library (import, folders, albums, smart albums, keywords, metadata, search, faces) | 15% | 90% | 80% | 25–45 h | checklist A–E 83–92%; robust journal; 85k-photo libraries |
| RAW decoding and camera coverage | 15% | 81% | 77% | 120–220 h | [raw-parity.md](raw-parity.md), measured model by model: 85% of photographers' cameras decode (use-weighted), × 0.9 for per-body bugs; CR3 variants, compressed ORF, HE NEF |
| Colour, camera profiles, render fidelity | 10% | 65% | 35% | 80–150 h | 5 bundled profiles vs Adobe's 1,500; per-file fits median ΔE ~5 vs the camera JPEG, ~10% far off; no fidelity suite ([raw-parity.md](raw-parity.md#colour)) |
| Global develop adjustments (light, colour, curves, grading, effects, detail) | 15% | 93% | 70% | 30–60 h | checklist F 91.7%; character tuned by eye |
| Optics, lens profiles, crop and geometry | 6% | 80% | 50% | 40–80 h | no lens database; crop / Upright complete |
| Masking | 8% | 67% | 45% | 60–110 h | checklist K 67.4%; AI masks heuristic or SAM 3 without mirror |
| Remove, heal, Enhance (denoise, Raw Details, Super Resolution, Lens Blur) | 7% | 45% | 30% | 70–130 h | checklist I 75%, P 0% |
| Presets, profiles, versions, history, sync | 5% | 95% | 85% | 8–15 h | |
| Export | 6% | 89% | 75% | 15–30 h | checklist S 88.9% |
| Merge and HDR | 3% | 85% | 60% | 20–35 h | merge 88% (HDR merge 🟡: AI Denoise on merged DNGs, JPEG response, memory; gap 29), HDR 70% (no HDR display) |
| Views, compare, zoom, culling | 5% | 90% | 70% | 15–30 h | zoom detail P0 gap |
| Classic output modules (Map, Book, Slideshow, Print, Web, publish, tethering) | 4% | 15% | 10% | 90–150 h | Classic extras 48% counts many small keys; the modules themselves are mostly ⬜ |
| Video | 1% | 0% | 0% | 25–45 h | catalogued only |
| **Weighted** | 100% | **~77%** | **~61%** (61.5%) | **600–1,100 h** | |

## AI

Lightroom 9.6 ships 37 on-device models (names listed from its `ModelZoo`), covering subject, sky,
portrait, skin and object selection with refinement, depth estimation, denoise, dust detection,
reflections removal, distraction / people detection, culling scores (eyes open, blur, aesthetics) and
search embeddings; generative remove runs in Adobe's cloud.

| Feature | LightCraft | Status |
|---|---|---|
| 2026-10-12 | minor | Mamiya MEF decoded (raw-parity.md): 241 verified, 35 unsupported; rounded numbers unchanged |
| 2026-10-12 | minor | Epson ERF decoded (raw-parity.md): 240 models verified, 229 preview-only; shares and rounded numbers unchanged |
| Subject / Sky / Background masks | classical heuristics; the file's own DNG semantic mattes | 🟡 |
| People (faces, body, hair, skin…) and Landscape masks | — | ⬜ |
| Object / Describe masks | SAM 3 in pure Rust (Metal / CPU); opt-in 3.4 GB download, no mirror configured | 🟡 |
| Depth range mask, Lens Blur | — | ⬜ |
| AI Denoise | Bayer only, pure-Rust CPU / GPU, user-installed weights | 🟡 |
| Raw Details, Super Resolution | — | ⬜ |
| Generative remove, distractions, reflections | — (generative is out of scope: cloud) | ⬜ |
| Dust detection | ✅ | ✅ |
| Faces / People | YuNet detection, SFace / AuraFace recognition (opt-in downloads), People view | ✅ (MVP, [faces.md](faces.md)) |
| Assisted culling | focus, bursts | 🟡 (no eyes-open / aesthetics scores) |
| Auto (tone), adaptive presets | Auto calibrated on Lightroom's Auto results (#628); no adaptive presets | 🟡 |
| Natural-language search | local SigLIP 2 (opt-in 1.5 GB download, pure Rust on candle): search bar Contents mode and `contents:` words, Relevance sort, Find Similar Contents, auto-tags (Library Filter ▸ Contents, Keywording suggestions, Smart Collection Contents rule) ([content-search.md](content-search.md)); one model family, no web build | 🟡 |

**Blocked on a model strategy** (maintainer decision): licensable weights or our own training;
pure-Rust inference is proven (faces, SAM 3, denoise).

## Ready for real work, mainstream practitioner and essentials user

Three readiness numbers (craftrules `standards/progress-docs.md`). The full number is a weighted
sum over the dimensions with written weights. Mainstream and essentials take the weighted depth of
the areas their user touches, then apply written multiplicative discounts for what still stops real
work. Each has its own hours to ~95%, calibrated as in [How hours are calibrated](#how-hours-are-calibrated)
(essentials ⊂ mainstream ⊂ full):

| Audience | Ready | Opus 5.5 agent wall-clock hours to ~95% | Work that dominates |
|---|---:|---:|---|
| Full target (ready for real work) | **~61%** | **1,000–1,800 h** (~70% parallelizes) | raw colour, lens and camera data; AI models; Classic output modules; localization; plug-in ecosystem |
| Mainstream practitioner | **~51%** | **450–840 h** (~60% parallelizes) | camera colour and render fidelity; CR3 / ORF / HE NEF; lens database; subject / sky masks; Lightroom XMP exchange; stability |
| Essentials user | **~65%** | **160–300 h** (~50% parallelizes) | default raw look for popular cameras; launch stability; basic-slider feel; opening CR3 / HEIC files people send |

### Full target (ready for real work): ~61%

Weighted sum over the dimensions of [By dimension](#by-dimension) (AI is left out because it
overlaps the masking and Enhance feature areas). RAW decoding and camera colour are their own rows,
valued from the model-by-model measurement in [raw-parity.md](raw-parity.md):

| Dimension | Weight | Ready |
|---|---:|---:|
| Features: the other 11 areas, weighted depth from [Features](#features) | 45% | 60.9% |
| RAW decoding and camera coverage | 9% | 83% (92% use-weighted × 0.9) |
| Camera colour and default rendering | 6% | 35% |
| UI/UX fidelity | 8% | 70% |
| File formats (non-raw) | 6% | 70% |
| Hardware | 3% | 40% |
| Localization | 3% | 64% |
| Performance | 6% | 70% |
| Stability | 8% | 60% |
| Platforms | 4% | 60% |
| Ecosystem / plug-ins | 2% | 15% |
| **Weighted sum** | 100% | **61.5% ≈ 61%** (range 56–66%) |

Mainstream comes out lower than full: its scope is the raw-develop core, where LightCraft is weakest
(camera colour, CR3 coverage), and its discounts are multiplicative, while the full sum is lifted by
strong library, presets, UI and format dimensions.

**Why the numbers moved with the RAW measurement:** decoding measured model by model against
Adobe's list is better than the earlier 50% estimate (84% of photographers' cameras decode, 76% after
a bug discount), colour is lower (35%, was 40%) and is now its own row. Net: full +2 points.
Mainstream now carries colour as a ×0.85 discount on top of the measured coverage (−2 points);
essentials replaces its guessed 45% raw look with 84% × 0.75 (+1 point).

### Discounts (evidence: open issues from users, 2026-10-10)

| Discount | Mainstream | Essentials | Evidence |
|---|---:|---:|---|
| Interaction fidelity / UI clarity | ×0.95 | ×0.95 | brush live preview gaps and feathering (#517, #518); crop artefacts and limits (#731, #742, #770, #771); Compare white-balance selector (#743–#745); low-res slider previews (#330); dialogs taller than the window (#781); mouse-wheel scroll (#714) |
| Stability on real machines | ×0.92 | ×0.92 | launch and quit crashes or silent startup failures (#250 macOS 12 Intel, #315 Windows Intel GPU, #431 Intel Mac, #260, #462, #783 Linux / Nouveau, #790 Windows installer, fixed in 0.9.1); tests run in CI on Linux x64 and FreeBSD only (gap 9) |
| File exchange with Lightroom users (mainstream) / opening files people send (essentials) | ×0.92 | ×0.95 | Adobe XMP sidecars change colours (#290), XMP CameraProfile and Upright ignored (#204, #205), catalog import fails (#599), same Light values render differently (#523, #209), no write-back to `.lrcat`; for casual users: CR3 beyond verified bodies, compressed ORF, HE NEF open as previews (#473, #485, #556), HEIC trouble (#693) |
| **Product** | **×0.804** | **×0.830** | |

### Mainstream practitioner: ~51%

A working photographer (wedding, portrait, events, landscape) shooting raw on one current camera,
weekly: import, cull, develop, a few masks, sync a look, export. Excluded: Classic output modules,
video, generative / cloud AI, plug-ins, tethering and other specialist hardware, languages other
than the user's own.

| Area | Weight | Depth |
|---|---:|---:|
| Import and library | 18% | 80% |
| Cull: views, compare, zoom, ratings | 12% | 70% |
| Raw decoding for their camera | 27% | 92% (measured use-weighted share whose camera fully works, [raw-parity.md](raw-parity.md#by-use-estimated)) |
| Global develop adjustments | 15% | 70% |
| Crop, geometry, lens corrections | 6% | 50% |
| Masking (manual, subject / sky; no generative) | 8% | 45% |
| Heal / remove (no generative), denoise | 5% | 40% |
| Presets, copy / paste / sync | 5% | 85% |
| Export | 4% | 75% |
| **Weighted depth** | 100% | **74.0%** |

Colour fidelity is a written discount, **×0.85**: most raws start near the camera's own look (median
ΔE ~5), about 10% far off (ΔE ~18), none calibrated to Adobe Standard, so a working photographer
corrects colour by hand (33 open colour issues). 74.0% × 0.804 × 0.85 = **~51%** (range 46–56%).

### Essentials user: ~65%

A casual photographer with default settings: import, look through, rate, fix a few photos with the
basic sliders, crop, apply a preset, undo, export JPEGs.

| Feature | Weight | Depth |
|---|---:|---:|
| Import photos | 15% | 80% |
| Browse grid and detail view | 10% | 85% |
| Rate and flag | 5% | 90% |
| Basic sliders (exposure, contrast, highlights, shadows, white balance, vibrance / saturation) | 22% | 70% (slider strength differs from Lightroom, #196, #521) |
| Auto | 5% | 65% (#318) |
| Crop and straighten | 10% | 80% |
| Presets and profiles | 8% | 80% |
| Undo and history | 5% | 90% |
| Their camera's raws: decode × default look | 10% | 69% (92% whose camera fully works × colour factor 0.75) |
| Export JPEG | 10% | 85% |
| **Weighted depth** | 100% | **78.0%** |

78.0% × 0.830 = **~65%** (range 60–70%).

### User evidence (GitHub issues, excluding the four maintainers)

- 324 issues from 159 users; 162 open. 8,719 stars, 2,698 forks (2026-10-10).
- Of the 162 open: **~92 core-path bugs** (33 colour / rendering, 16 decoding, ~36 UI / workflow, 7
  launch / stability), ~11 developer / test infrastructure, **~59 feature requests or niche asks**
  (plug-ins, mobile, sync servers, tablets, MIDI, geocoding, themes).
- Praise in issue text (love / amazing / great work / awesome): ~14 issues; "thank you" in 27.
  **No explicit "switched from Lightroom" report** was found ("switched" matches 6 issues, none a
  switch report).
- Reading: strong interest, but most open user issues are core-path, and raw colour dominates them,
  so the mainstream number is held down by default rendering rather than missing features.

### Why the full number moved (55% → 46% → 59%)

The first figure (~55%) was the feature depth "pulled down" by an unwritten judgement. A later pass
applied the mainstream discounts to it (~46%). The standard defines the full number as a weighted
sum over the dimensions, so it is now that sum with the weights written above: ~59%. Method aligned
with the standard, no new evidence. The stage stays **alpha**.

## Stage

**Alpha.** Core workflows exist end to end (import, cull, develop, mask, export, catalog migration),
but ready-for-real-work is ~61%, below the ~75% beta bar, and the main file formats have blocking
gaps: Canon CR3 decodes from sensor data on three verified bodies only, compressed ORF and HE NEF
open as previews, and colour on every non-DNG raw is estimated rather than measured.

**Alpha gate:** all six core workflows (import, cull, develop, save and reopen, batch looks, export)
work end to end on macOS; see the table in [roadmap.md](roadmap.md#alpha-gate). The partial rows (CR3
variants, compressed ORF, HE NEF as previews) degrade but don't block a workflow.

**To beta:** ~14 points of readiness, **~340–600 h**: the `B` gaps in [gaps.md](gaps.md)
(~230–430 h), plus burning down the ~92 open core-path user issues (40–80 h) and stability work
around them. Beta needs: CR3, ORF and
HE NEF decoding from sensor data; measured colour for the popular models and a fidelity suite with
scores; Subject / Sky / People masks; a lens profile database for the
popular lenses; tests in CI on macOS, Windows and Linux.

## How hours are calibrated

Hours are Opus 5.5 agent wall-clock hours, one session working sequentially, including tests and
verification. Calibrated on this repository's history (git log and GitHub PR open-to-merge times):

- **Project pace:** first commit 2026-09-30; by 2026-10-10, 1,066 non-merge commits and 325 merged
  PRs, commits in 159 distinct clock hours, with 4–6 parallel agents plus community contributors.
  On 2026-10-02 the ROADMAP recorded ≈ 105–145 agent-hours spent; M0–M13 took ≈ 25 active hours with
  parallel agents against an up-front estimate of 110–170 h.
- **UI and library rows:** a lead agent closed ≈ 45 checklist rows in ≈ 5 h (2026-10-01) and ≈ 60 in
  ≈ 12 h (2026-10-02): 6–12 minutes per small row.
- **Raw decoders:** RW2 in every format 6.5 h (#215), compressed RAF 8.6 h (#282), CR3 CRX 10 h
  (#279), NEF lossy after split 4.7 h (#633), ORF 12.8 bpp 1.7 h (#651), HEIC 7 h (#609).
- **Large features:** AI RAW denoise 8.3 h (#384, 12.8k lines), SAM 3 masks 14.9 h (#162),
  HDR editing and export 2.3 h (#646, on existing groundwork), contact-sheet PDF (#563), German UI
  13.8 h (#267).
- PR open time includes queueing and review, so these are upper bounds per feature; the ranges add
  a long-tail factor of 1.3–1.5× for verification against real files.

### Why the estimate went up

The previous estimate (2026-10-02, kept below) was ≈ 270–505 h to full parity. It counted checklist
rows only. New evidence since: the target's own lists (≈ 1,000 dedicated camera models, 3,635 lens
profiles, 37 AI models, 16 languages) measured from the installed bundle; per-maker verification
counts; that CI tests only FreeBSD; and the standard's added dimensions (localization, stability,
platforms, ecosystem). The raw-codec package it estimated at 40–80 h took roughly that and closed
most of its list; what remains is the data-heavy part (colour, lenses, models) it did not count.

## History of the numbers

| Date | Checklist (all / P0 / P1 / P2) | Ready for real work | Remaining to full parity | Source |
|---|---|---|---|---|
| 2026-10-12 | 81.1% (unchanged) | ~61% full · ~51% mainstream · ~65% essentials | unchanged | 22 more CR3 bodies verified (raw-parity.md) |
| 2026-10-10 (RAW) | 81.1% (unchanged) | ~61% full · ~49% mainstream · ~64% essentials | unchanged | RAW measured model by model against Adobe's list; RAW decode and colour as explicit rows |
| 2026-10-10 (final) | 81.1% (unchanged) | ~59% full (weighted sum) · ~51% mainstream · ~63% essentials | full 1,000–1,800 h · mainstream 450–840 h · essentials 160–300 h | method aligned with the standard |
| 2026-10-10 (later) | 81.1% (unchanged) | ~46% full · ~51% mainstream · ~63% essentials | 1,000–1,800 h | full × discounts (superseded) |
| 2026-10-10 | 81.1% / 98.0% / 95.1% / 46.9% (517 rows) | ~55% | 1,000–1,800 h | this re-measure |
| 2026-10-08 | 79.7% / 98.2% / 95.6% / 41.6% (509 rows) | 60–70% | — | ROADMAP *Where we stand* |
| 2026-10-02 | 75.6% | — | 270–505 h (cloud parity without AI / Classic: 130–255 h) | ROADMAP *Parity estimate* |

The 2026-10-08 assessment by dimension, kept for evidence: feature checklist 79.7%; RAW coverage
~55%; colour and image quality ~55–65%; AI and computational ~15–20%; workflow and library ~85%
(single machine); Classic modules ~30%; HDR and video ~25%; platform and robustness ~70%. By kind of
user: JPEG / DNG shooter ~85%; Nikon / Sony / Panasonic / older-Canon ~65%; Fujifilm ~65%; Canon
CR3 / Olympus compressed ~45%; Classic power user ~45%; relies on AI ~25%. Today's numbers per user:
JPEG / DNG ~75%; Sony, Panasonic, Fujifilm ~60%; Nikon ~55% (HE NEF); Canon CR3 ~40%; OM / Olympus
~30%; Classic power user ~40%; relies on AI ~20%.

The 2026-10-02 effort table, kept for calibration: remaining P0/P1 UI 5–10 h; raw codecs 40–80 h;
lens database 15–30 h; video 20–40 h; AI 80–150 h; HDR 15–25 h; Classic modules 60–100 h; smaller
P2 items 12–25 h; look tuning, performance, packaging, hardening 25–45 h.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-19 | patch | Stability note: tests run in CI on Linux x64 (every PR) as well as FreeBSD; macOS and Windows remain (gap 9) |
| 2026-10-19 | minor | Localization ~55% → ~64% (mean of the 12 key languages): Indonesian (`id`) catalog, every message but What's New; full target 61.2% → 61.5% (still ~61%) |
| 2026-10-19 | patch | Platforms: the Windows installer is no longer listed as unverified (the 0.9.1 x64 MSI is tested on Windows 11; a confirmation after uninstalling from Settings → Apps, #892, remains); #790 noted as fixed in 0.9.1; percentages and hours unchanged |
| 2026-10-15 | minor | Natural-language search stays 🟡: auto-tags added (Contents filter, Keywording suggestions, Smart Collection Contents rule); one model family, no web build; headline numbers not re-derived |
| 2026-10-15 | minor | Natural-language search ⬜ → 🟡 (local content search from the search bar; auto-tags still missing); headline numbers not re-derived |
| 2026-10-14 | minor | Localization ~52% → ~55% (mean of the 12 key languages): de, es, fr, ja and uk catalogs filled |
| 2026-10-12 | minor | UI/UX remaining 34–58 h → 32–54 h (canvas tools by keyboard); headline numbers unchanged |
| 2026-10-14 | minor | Merge and HDR: merge 100% → 88% (LR-MERGE-HDR ✅ → 🟡 after testing on real 67 MP brackets; gap 29); area percentages unchanged |
| 2026-10-14 | minor | Gap 4 (full-resolution zoom) closed: removed from the interaction-fidelity notes, the alpha-gate partial rows and the beta needs; headline numbers and the ×0.95 interaction factor unchanged |
| 2026-10-12 | minor | UI/UX remaining 40–70 h → 34–58 h (accessibility audit landed, ui-parity.md); percentages unchanged |
| 2026-10-18 | minor | Minolta MRW decodes from sensor data; 6 Konica Minolta bodies verified; headline numbers unchanged |
| 2026-10-14 | minor | Camera-coverage sweep (#1327): 30 models verified on 40 CC0 files (`coverage_sweep_corpus.rs`, `coverage-sweep.tsv`; `docs/raw/coverage.py` 515 → 545 verified rows); the counts above are not re-measured here. Sensor verification does not establish colour parity (#1277); headline numbers unchanged |
| 2026-10-12 | minor | 22 more CR3 bodies verified on one CC0 file each (raw-parity.md): use-weighted share whose camera decodes 85% → 92%, RAW decoding 77% → 83%; full 60.7% → 61.2% (~61%), mainstream depth 72.1% → 74.0% (~49% → ~51%), essentials depth 77.5% → 78.0% (~64% → ~65%) |
| 2026-10-12 | minor | 117 Nikon and Fujifilm bodies verified (raw-parity.md): use-weighted share whose camera decodes 85% → 86%, RAW decoding 77% (unchanged); mainstream depth 72.1% → 72.4%, essentials depth 77.5% → 77.6%; headline numbers unchanged |
| 2026-10-11 | minor | EOS R5 Mark II CR3 verified (raw-parity.md): use-weighted share whose camera decodes 84% → 85%, RAW decoding 76% → 77%; full 60.6% → 60.7%, mainstream depth 71.8% → 72.1%, essentials depth 77.4% → 77.5%; the rounded numbers (~61%, ~49%, ~64%) are unchanged |
| 2026-10-10 | minor | RAW measured model by model (raw-parity.md): RAW decoding (76%) and colour (35%) are explicit rows of the full sum (59% → 61%); mainstream uses the use-weighted 84% plus a ×0.85 colour discount (51% → 49%); essentials uses 84% × colour 0.75 (63% → 64%); beta ~14 points |
| 2026-10-10 | minor | Full number restored to an additive weighted sum over the dimensions (~59%; method aligned with the standard, no new evidence); hours to ~95% for each of the three audiences; beta distance ~16 points |
| 2026-10-10 | minor | Added mainstream practitioner (~51%) and essentials user (~63%) numbers; full ready re-derived from written weights × discounts (55% → 46%); user-issue evidence; beta hours 340–600 |
| 2026-10-10 | minor | Stage re-checked against the core-workflow alpha gate: passes, stays alpha |
| 2026-10-10 | major | Created per the progress-docs standard: full re-measure (checklist recount, installed Lightroom 9.6 bundle listing, code counts, catalogs, CI), feature areas with weights, dimensions, AI, stage, calibration; merged ROADMAP's *Where we stand* and *Parity estimate* |
| 2026-10-14 | minor | RAW: 82% of Adobe's models decode, 36% verified (integration batch 36) |
