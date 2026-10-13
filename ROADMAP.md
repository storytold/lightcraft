# LightCraft roadmap

**Stage: alpha** · next: beta, ~14 points and ~340–600 h away

> **Last reviewed:** 2026-10-10 · **Last updated:** 2026-10-13 · **Change:** minor (Canon CRW decoded clean-room; 241 models verified) · **Target:** Adobe Lightroom Classic 15.5 / Lightroom 9.6

One-page summary of how close LightCraft is to Adobe Lightroom. Agents: read this, then pick work
from [docs/gaps.md](docs/gaps.md). The full assessment, method and evidence are in
[docs/target-app-parity.md](docs/target-app-parity.md).

## Headline numbers

| | Value | Kind |
|---|---:|---|
| **Feature breadth** (checklist) | **81.1%** of 517 rows (P0 98.0%, P1 95.1%, P2 46.9%) | measured: [parity-checklist.md](docs/parity-checklist.md) recount |
| **Feature breadth** (weighted by use) | **~77%** | estimated |
| **Ready for real work** (full target) | **~61%** (56–66%) | estimated: weighted sum over the dimensions |
| **Mainstream practitioner** | **~49%** (44–54%) | estimated |
| **Essentials user** | **~64%** (59–69%) | estimated |
| Remaining to **beta** | **~340–600 h** (Opus 5.5 agent hours, ~60% parallel) | estimated |
| Remaining to **full parity** | **~1,000–1,800 h** (~70% parallel) | estimated |

Each audience with its own hours (Opus 5.5 agent wall-clock hours to ~95%):

| Audience | Ready | Opus 5.5 agent wall-clock hours to ~95% | Work that dominates |
|---|---:|---:|---|
| Full target (ready for real work) | **~61%** | **1,000–1,800 h** (~70% parallelizes) | raw colour, lens and camera data; AI models; Classic output modules; localization; plug-in ecosystem |
| Mainstream practitioner | **~49%** | **450–840 h** (~60% parallelizes) | camera colour and render fidelity; CR3 / ORF / HE NEF; lens database; subject / sky masks; Lightroom XMP exchange; stability |
| Essentials user | **~64%** | **160–300 h** (~50% parallelizes) | default raw look for popular cameras; launch stability; basic-slider feel; opening CR3 / HEIC files people send |

The gap between breadth and readiness is quality and coverage, not missing buttons: camera colour is
estimated rather than measured, CR3 decodes on four verified Canon bodies, compressed ORF and HE NEF
open as previews, rendering is tuned by eye, AI masks are heuristics, there is no lens database, and
tests run in CI on FreeBSD only. **Why alpha:** core workflows run end to end, but readiness is below
the ~75% beta bar (it is above the ~40% alpha bar) and Lightroom's main inputs, raw files, still have blocking gaps (Canon, ~30% of
raw users, decodes CR3 on four verified bodies). It passes the alpha gate: all six core workflows
(import, cull, develop, save and reopen, batch looks, export) work end to end on macOS
([docs/roadmap.md](docs/roadmap.md#alpha-gate)). The three readiness numbers and their weights and discounts are in
[target-app-parity.md](docs/target-app-parity.md#ready-for-real-work-mainstream-practitioner-and-essentials-user).
Hours are calibrated on this repo's PR history; see
[target-app-parity.md](docs/target-app-parity.md#how-hours-are-calibrated).

## By dimension

| Dimension | Ready | Breadth | Remaining | Doc |
|---|---:|---:|---:|---|
| Features | ~61% | ~77% (checklist 81.1%) | 600–1,100 h | [target-app-parity](docs/target-app-parity.md#features) |
| ↳ RAW cameras, colour, lens profiles | ~56% | 81% of 1,446 Adobe models decode, 16% verified | 255–480 h | [raw-parity](docs/raw-parity.md) |
| UI/UX fidelity | ~70% | ~85% | 40–70 h | [ui-parity](docs/ui-parity.md) |
| File formats (non-raw) | ~70% | ~85% | 30–55 h | [file-format-parity](docs/file-format-parity.md) |
| Hardware | ~40% | ~50% | 40–80 h | [hardware-parity](docs/hardware-parity.md) |
| Localization | ~52% of the 12 key languages | 7 of 12 present | 130–210 h (+145–255 h new scripts) | [localization-parity](docs/localization-parity.md) |
| Performance | ~70% | ~80% | 30–50 h | [target-app-parity](docs/target-app-parity.md#by-dimension) |
| Stability | ~60% | — | 30–60 h | [target-app-parity](docs/target-app-parity.md#by-dimension) |
| Platforms | ~60% | macOS, Windows, Linux, FreeBSD, web | 25–45 h | [target-app-parity](docs/target-app-parity.md#by-dimension) |
| Ecosystem / plug-ins | ~15% | ~25% | 40–80 h | [target-app-parity](docs/target-app-parity.md#by-dimension) |
| AI features | ~20% | ~30% | 120–220 h (overlaps features) | [target-app-parity](docs/target-app-parity.md#ai) |

## Features

| Area | Ready | Breadth | Remaining |
|---|---:|---:|---:|
| Import and library | 80% | 90% | 25–45 h |
| RAW decoding and camera coverage | 77% | 81% | 120–220 h |
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

Also shipped: Traditional Chinese (87%), Russian (74%), Ukrainian (99%).

## Upcoming

Ranked toward beta; detail in [docs/roadmap.md](docs/roadmap.md), gaps in [docs/gaps.md](docs/gaps.md).

1. Camera colour of our own: measured calibration for the popular models (50–90 h)
2. CR3 for every Canon body (25–45 h)
3. Render-fidelity suite against Lightroom, then tuning (30–60 h)
4. Full-resolution zoom (8–15 h)
5. AI masks: Subject / Sky / People (40–70 h + model decision)
6. Compressed ORF and HE NEF (28–55 h)
7. Lens profile database of our own (40–80 h)
8. Tests in CI on macOS, Windows and Linux (6–12 h)
9. Per-model raw verification (20–35 h)

## Progress log

- 2026-10-13: Canon CRW (the 2000–2004 CIFF bodies) decodes from sensor data: compression
  derived clean-room against a black-box reference and frozen (`clean-room/crw/06-crw-format.md`);
  EOS D30/D60/10D/300D and PowerShot G1/G6 verified pixel-exact on 28 of 29 corpus files (18 of
  them the maintainer's D60 shots); as-shot white balance, black level, rotation and capture time
  from the CIFF records; the PowerShot G5's anomalous coding stays preview-only. 241 models
  verified; use-weighted coverage 85% unchanged, Canon bucket 73% → 76%.
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
| 2026-10-13 | minor | Canon CRW decoded (clean-room): 6 bodies verified pixel-exact, G5 preview; 241 models verified |
| 2026-10-11 | minor | EOS R5 Mark II CR3 verified: CR3 on four verified bodies, RAW decoding 76% → 77%; headline numbers unchanged |
| 2026-10-10 | minor | RAW measured model by model; RAW decode and colour as explicit rows: full 59% → 61%, mainstream 51% → 49%, essentials 63% → 64%; beta ~14 points |
| 2026-10-10 | minor | Full ready restored to the additive weighted sum (~59%; method aligned with the standard, no new evidence); hours per audience; beta ~16 points away |
| 2026-10-10 | minor | Mainstream practitioner and essentials user numbers; full ready 55% → 46% (method written down); beta 340–600 h |
| 2026-10-10 | minor | Alpha gate added (core-workflow check): passes, stays alpha |
| 2026-10-10 | major | Restructured to the progress-docs standard; re-measured; *Where we stand*, *Parity estimate* and *Totals* moved to docs/target-app-parity.md, milestones and risks to docs/roadmap.md, raw coverage to docs/raw-parity.md, catalog migration to docs/file-format-parity.md, CJK notes to docs/localization-parity.md |
| 2026-10-08 | minor | *Where we stand* by dimension and by kind of user |
| 2026-10-02 | major | Parity estimate and effort table |
| 2026-09-30 | major | Created |
