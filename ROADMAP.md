# LightCraft roadmap

**Stage: alpha** · next: beta, ~14 points and ~340–600 h away

> **Last reviewed:** 2026-10-10 · **Last updated:** 2026-10-17 · **Change:** minor (watermark completeness reassessed after #2052; current checklist recount synchronized) · **Target:** Adobe Lightroom Classic 15.5 / Lightroom 9.6

One-page summary of how close LightCraft is to Adobe Lightroom. Agents: read this, then pick work
from [docs/gaps.md](docs/gaps.md). The full assessment, method and evidence are in
[docs/target-app-parity.md](docs/target-app-parity.md).

## Headline numbers

| | Value | Kind |
|---|---:|---|
| **Feature breadth** (checklist) | **83.3%** of 539 rows (P0 98.3%, P1 94.7%, P2 55.7%) | measured: [parity-checklist.md](docs/parity-checklist.md) recount |
| **Feature breadth** (weighted by use) | **~77%** | estimated |
| **Ready for real work** (full target) | **~61%** (56–66%) | estimated: weighted sum over the dimensions |
| **Mainstream practitioner** | **~51%** (46–56%) | estimated |
| **Essentials user** | **~65%** (60–70%) | estimated |
| Remaining to **beta** | **~340–600 h** (Opus 5.5 agent hours, ~60% parallel) | estimated |
| Remaining to **full parity** | **~1,000–1,800 h** (~70% parallel) | estimated |

Each audience with its own hours (Opus 5.5 agent wall-clock hours to ~95%):

| Audience | Ready | Opus 5.5 agent wall-clock hours to ~95% | Work that dominates |
|---|---:|---:|---|
| Full target (ready for real work) | **~61%** | **1,000–1,800 h** (~70% parallelizes) | raw colour, lens and camera data; AI models; Classic output modules; localization; plug-in ecosystem |
| Mainstream practitioner | **~51%** | **450–840 h** (~60% parallelizes) | camera colour and render fidelity; CR3 / ORF / HE NEF; lens database; subject / sky masks; Lightroom XMP exchange; stability |
| Essentials user | **~65%** | **160–300 h** (~50% parallelizes) | default raw look for popular cameras; launch stability; basic-slider feel; opening CR3 / HEIC files people send |

The gap between breadth and readiness is quality and coverage, not missing buttons: camera colour is
estimated rather than measured, CR3 decodes on 26 verified Canon bodies (rarer CRX variants still open as previews), compressed ORF and HE NEF
open as previews, rendering is tuned by eye, AI masks are heuristics, there is no lens database, and
tests run in CI on FreeBSD only. **Why alpha:** core workflows run end to end, but readiness is below
the ~75% beta bar (it is above the ~40% alpha bar) and Lightroom's main inputs, raw files, still have blocking gaps (compressed ORF and HE NEF open as previews; Canon,
~30% of raw users, decodes CR3 on 26 verified bodies). It passes the alpha gate: all six core workflows
(import, cull, develop, save and reopen, batch looks, export) work end to end on macOS
([docs/roadmap.md](docs/roadmap.md#alpha-gate)). The three readiness numbers and their weights and discounts are in
[target-app-parity.md](docs/target-app-parity.md#ready-for-real-work-mainstream-practitioner-and-essentials-user).
Hours are calibrated on this repo's PR history; see
[target-app-parity.md](docs/target-app-parity.md#how-hours-are-calibrated).

## By dimension

| Dimension | Ready | Breadth | Remaining | Doc |
|---|---:|---:|---:|---|
| Features | ~61% | ~77% (checklist 83.3%) | 600–1,100 h | [target-app-parity](docs/target-app-parity.md#features) |
| ↳ RAW cameras, colour, lens profiles | ~60% | 83% of 1,446 Adobe models decode, 38% verified | 255–480 h | [raw-parity](docs/raw-parity.md) |
| UI/UX fidelity | ~70% | ~85% | 32–54 h | [ui-parity](docs/ui-parity.md) |
| File formats (non-raw) | ~70% | ~85% | 30–55 h | [file-format-parity](docs/file-format-parity.md) |
| Hardware | ~40% | ~50% | 40–80 h | [hardware-parity](docs/hardware-parity.md) |
| Localization | ~55% of the 12 key languages | 7 of 12 present | 130–210 h (+145–255 h new scripts) | [localization-parity](docs/localization-parity.md) |
| Performance | ~70% | ~80% | 30–50 h | [target-app-parity](docs/target-app-parity.md#by-dimension) |
| Stability | ~60% | — | 30–60 h | [target-app-parity](docs/target-app-parity.md#by-dimension) |
| Platforms | ~60% | macOS, Windows, Linux, FreeBSD, web | 25–45 h | [target-app-parity](docs/target-app-parity.md#by-dimension) |
| Ecosystem / plug-ins | ~15% | ~25% | 40–80 h | [target-app-parity](docs/target-app-parity.md#by-dimension) |
| AI features | ~20% | ~30% | 120–220 h (overlaps features) | [target-app-parity](docs/target-app-parity.md#ai) |

## Features

| Area | Ready | Breadth | Remaining |
|---|---:|---:|---:|
| Import and library | 80% | 90% | 25–45 h |
| RAW decoding and camera coverage | 77% | 82% | 120–220 h |
| Colour, camera profiles, render fidelity | 35% | 65% | 80–150 h |
| Global develop adjustments | 70% | 93% | 30–60 h |
| Optics, lens profiles, crop and geometry | 50% | 80% | 40–80 h |
| Masking | 45% | 67% | 60–110 h |
| Remove, heal, Enhance | 30% | 45% | 70–130 h |
| Presets, profiles, versions, sync | 85% | 95% | 8–15 h |
| Export | 75% | 89% | 15–30 h |
| Merge and HDR | 60% | 85% | 20–35 h |
| Views, compare, zoom, culling | 70% | 90% | 15–30 h |
| Classic output modules (Map, Book, Slideshow, Print, publish, tethering) | 10% | 15% | 90–150 h |
| Video | 0% | 0% | 25–45 h |

Weights and evidence: [target-app-parity.md](docs/target-app-parity.md#features).

## Languages

Measured from `crates/ui-egui/locales` (share of 2,304 catalog messages translated); none is `full`
while lower-layer errors stay English. Detail: [localization-parity.md](docs/localization-parity.md).

| Language | Code | Translated | Status |
|---|---|---:|---|
| English | en | 100% | full (source) |
| Simplified Chinese | zh-hans | 87% | partial |
| Spanish | es | 86% | partial |
| Hindi | hi | 0% | none (no Devanagari shaping) |
| Arabic | ar | 0% | none (no RTL / shaping) |
| French | fr | 88% | partial |
| Portuguese (Brazil) | pt-br | 85% | partial |
| Indonesian | id | 0% | none |
| Japanese | ja | 87% | partial |
| German | de | 91% | partial |
| Korean | ko | 0% | none (no Hangul face) |
| Vietnamese | vi | 0% | none |

Also shipped: Traditional Chinese (87%), Russian (74%), Ukrainian (99%), Italian (92%), Georgian (100%).

## Upcoming

Ranked toward beta; detail in [docs/roadmap.md](docs/roadmap.md), gaps in [docs/gaps.md](docs/gaps.md).

1. Camera colour of our own: measured calibration for the popular models (50–90 h)
2. CR3 for every Canon body (25–45 h)
3. Render-fidelity suite against Lightroom, then tuning (30–60 h)
4. ~~Full-resolution zoom~~ (done 2026-10-12)
5. AI masks: Subject / Sky / People (40–70 h + model decision)
6. Compressed ORF and HE NEF (28–55 h)
7. Lens profile database of our own (40–80 h)
8. Tests in CI on macOS, Windows and Linux (6–12 h)
9. Per-model raw verification (20–35 h)

## Progress log

- 2026-10-14: Camera-coverage sweep verifies 30 more models on 40 CC0 files, with file and sensor
  hashes pinned (`docs/raw/coverage.py`: 515 → 545 verified). Eleven candidates excluded after rechecking
  identity, provenance and camera-image agreement. Phone and drone DNG tone differences remain tracked in #1277.
- 2026-10-14: 97 Canon CR2, Pentax and Ricoh models verified, one CC0 raw.pixls.us file each pinned and
  checked against the camera JPEG (`crates/raw/tests/canon_pentax_corpus.rs`). 334 models verified, ~50% of
  photographers on a verified body; headline numbers unchanged.
- 2026-10-12: Mamiya MEF (ZD) decodes from sensor data and is imported; verified on a CC0 file. 241 models
  verified, 35 unsupported; headline numbers unchanged.
- 2026-10-12: Epson ERF (R-D1, R-D1s, R-D1x) decodes from sensor data, white balance measured from each
  file's preview; 3 bodies verified on CC0 files. 240 models verified; headline numbers unchanged.
- 2026-10-11: Leica Q3 verified by a recorded check of 38 private DNGs (all decode and develop at full
  size; mean ΔE to the camera JPEG 6.3–13.9). 236 models verified; headline numbers unchanged.
- 2026-10-12: 22 more Canon CR3 bodies verified (EOS R, RP, R3, R5, R6, R6 Mark II / III, R7, R10, R50,
  R50 V, 90D, 250D, 850D, M200, M50 Mark II, M6 Mark II, 1D X Mark III, PowerShot G5 X II, G7 X III, SX70,
  V1): one CC0 file each pinned and checked against the camera JPEG; all 110 CC0 CR3s on raw.pixls.us
  decode. The EOS R6 Mark III gets its as-shot white balance (`ColorData` 66). 259 models verified; RAW
  decoding 77% → 83%; mainstream ~49% → ~51%, essentials ~64% → ~65%, full ~61%.
- 2026-10-12: 117 Nikon and Fujifilm bodies verified, one CC0 raw.pixls.us file each pinned and checked
  against the camera JPEG (`crates/raw/tests/body_corpus.rs`); the sweep found COOLPIX 5700 / 8400 / 8800
  decoding wrongly. 354 models verified, ~58% of photographers on a verified body; headline numbers unchanged.
- 2026-10-11: Canon EOS R5 Mark II C-RAW decodes from sensor data (57% of its files opened as a
  preview): the CRX QP map's first-symbol literal and limited-length escape; four CC0 R5 Mark II files
  pinned, the QP maps of 33,665 private C-RAW files decode exactly ([cr3.md](docs/cr3.md)). 235 models
  verified; RAW decoding 76% → 77%; headline numbers unchanged.
- **2026-10-10 (UI docking):** Library, Tools, and Presets panels can be regrouped, split, floated within the app window, closed, and reopened. Tools keeps the current Edit/Crop/Masking mode; the photo view and filmstrip stay together. Panel moves and reset are available as UI commands, and saved UI preferences retain arrangements. Widget state styling uses the shared `craft-ui` theme helper, retaining the dark palette, typography, spacing and per-state borders. No readiness percentage change.
- 2026-10-10 (RAW): RAW measured model by model against Adobe's 1,446-model list (234 verified, 943
  unverified, 233 preview-only, 36 unsupported; ~84% of photographers' cameras decode, ~40% on a
  verified body). Full ~61%, mainstream ~49%, essentials ~64%; beta ~14 points away.
- 2026-10-10 (final): full ready is a weighted sum over the dimensions (~59%; method aligned with
  the standard); mainstream practitioner ~51% (450–840 h), essentials user ~63% (160–300 h);
  alpha gate passes.
- 2026-10-10: full re-measure for the progress-docs standard: stage alpha, ready ~55%, checklist
  81.1%; new docs (target-app-parity, gaps, roadmap, architecture, raw / file-format / hardware / UI
  / localization parity); `docs/parity.md` renamed `docs/parity-checklist.md`. Landed today: Nikon
  lossy-after-split NEF (#633), ORF 12.8 bpp (#651), HDR gain map JPEG / PQ AVIF / float TIFF export
  (#646), Lightroom-like Auto (#628), activity stack.
- 2026-10-11: Sony ILCA-99M2 (α99 II) camera colour: bundled profile from 144 photos; on 30 withheld photos
  mean ΔE vs the camera JPEG 5.11 → 4.87, 25 of them closer.
- 2026-10-09: Sony ILCE-7CR camera colour: bundled profile from 125 photos; on 26 withheld photos mean
  ΔE vs the camera JPEG 4.07 → 3.29. HEIC in release builds (#609), Samsung uncompressed SRW,
  contact-sheet PDF export, Ukrainian and French UI, release v0.5.0.
- 2026-10-08: independent Rust CR3 lossless and version 0x100/0x200 C-RAW decoding, six exact
  full-sensor regressions on M50 / R100 / R8; guarded CR3 camera-JPEG colour fitting
  ([cr3.md](docs/cr3.md)).
- 2026-10-08: Fujifilm lossless / lossy compressed RAF: pure-Rust striped decoder, full sensor arrays
  verified on 17 CC0 and 7 supplied files (13 bodies). Fujifilm camera colour: bundled X-H2S / X-T4
  profiles from 201 / 545 photos; on 150 withheld files mean ΔE X-H2S 1.70 → 1.62, X-T4 2.43 → 2.34.
  AI RAW denoise (#384), Lightroom catalog import, German, Spanish and Russian UI.
- 2026-10-07: Panasonic RW2 / Leica RWL / Panasonic RAW decode in every raw format, established on
  178 CC0 files from 118 bodies; RW2 file-local camera look (140 of 174 files). Faces: recognition in
  pure Rust and People view.
- 2026-10-03 – 10-05: community PRs (#42–#51, #60) and 25+ issues: compressed NEF, Move import,
  folder templates, Local roots and cleanup, 85k-photo grid and catalog performance, background
  compaction, failed-save errors, GPU export hardening. Added the honest *Where we stand* assessment
  and tracker rows for camera colour, camera coverage and render fidelity.
- 2026-10-02: parity estimate added (`cargo xtask parity` prints weighted completion); milestones
  refreshed.
- 2026-10-01: RAW II formats: RAF (uncompressed Bayer + X-Trans), RW2 (packed), PEF (incl. Huffman),
  ORF (uncompressed); embedded previews for every container incl. CR3; raw corpus test with 37 CC0
  samples.
- 2026-09-30 (later): app running with the full Lightroom-style UI; pipeline v0; DNG / CR2 / ARW;
  README showcase. ≈ 8 h elapsed.
- 2026-09-30: roadmap created; M0 in progress; research docs (Lightroom reference, Rust imaging
  ecosystem) complete.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-17 | minor | LR-EXP-WATERMARK reassessed as partial for existing shaping, bidi and font limits (#2052); current checklist recount 83.4% to 83.3% of 539 rows. Stale headline figures synchronized to that recount; readiness and effort estimates unchanged |
| 2026-10-12 | minor | Italian interface (`it`, #1192): 2,201 of 2,398 messages, locale, menu command and tests; headline numbers unchanged |
| 2026-10-13 | minor | Sony ILCA-99M2 camera colour profile in the log; numbers unchanged |
| 2026-10-14 | minor | Localization ~52% → ~55% (mean of the 12 key languages): de, es, fr, ja and uk catalogs filled |
| 2026-10-14 | minor | 21 Sony and Leica models verified (258 models, 18%); headline numbers unchanged |
| 2026-10-12 | minor | UI/UX remaining 34–58 h → 32–54 h (canvas tools by keyboard); headline numbers unchanged |
| 2026-10-14 | minor | 17 uncompressed Olympus and Samsung models verified on one CC0 file each (254 models verified, 18%); headline numbers unchanged |
| 2026-10-12 | minor | Mamiya MEF decoded and imported: ZD verified (241); headline numbers unchanged |
| 2026-10-12 | minor | Epson ERF decoded: 3 more verified models (240); headline numbers unchanged |
| 2026-10-12 | minor | Georgian (`ka`) interface language added (every catalog message) |
| 2026-10-14 | minor | Full-resolution zoom (gap 4) closed: soft proofing gets zoom windows; headline numbers unchanged |
| 2026-10-12 | minor | UI/UX remaining 40–70 h → 34–58 h (accessibility audit landed); headline numbers unchanged |
| 2026-10-14 | minor | Camera-coverage sweep (#1327): 30 models verified on 40 CC0 files (coverage.py 515 → 545 verified); sensor verification does not establish colour parity (#1277); headline numbers unchanged |
| 2026-10-12 | minor | 22 more CR3 bodies verified, EOS R6 Mark III white balance: RAW decoding 77% → 83%, RAW ~56% → ~60%; mainstream 49% → 51%, essentials 64% → 65%; full unchanged at ~61% |
| 2026-10-12 | minor | 117 Nikon and Fujifilm bodies verified (354 models, 24%); headline numbers unchanged |
| 2026-10-11 | minor | EOS R5 Mark II CR3 verified: CR3 on four verified bodies, RAW decoding 76% → 77%; headline numbers unchanged |
| 2026-10-10 | minor | RAW measured model by model; RAW decode and colour as explicit rows: full 59% → 61%, mainstream 51% → 49%, essentials 63% → 64%; beta ~14 points |
| 2026-10-10 | minor | Full ready restored to the additive weighted sum (~59%; method aligned with the standard, no new evidence); hours per audience; beta ~16 points away |
| 2026-10-10 | minor | Mainstream practitioner and essentials user numbers; full ready 55% → 46% (method written down); beta 340–600 h |
| 2026-10-10 | minor | Alpha gate added (core-workflow check): passes, stays alpha |
| 2026-10-10 | major | Restructured to the progress-docs standard; re-measured; *Where we stand*, *Parity estimate* and *Totals* moved to docs/target-app-parity.md, milestones and risks to docs/roadmap.md, raw coverage to docs/raw-parity.md, catalog migration to docs/file-format-parity.md, CJK notes to docs/localization-parity.md |
| 2026-10-08 | minor | *Where we stand* by dimension and by kind of user |
| 2026-10-02 | major | Parity estimate and effort table |
| 2026-09-30 | major | Created |
| 2026-10-14 | minor | 97 Canon CR2, Pentax and Ricoh models verified (334 models, 23%); headline numbers unchanged |
| 2026-10-14 | minor | RAW: 82% of Adobe's models decode, 36% verified (integration batch 36's CC0 per-model checks) |
