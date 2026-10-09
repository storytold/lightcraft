# Phase 2: Image quality and camera coverage

Part of [PLAN.md](PLAN.md). **Estimate:** 110–180 agent-hours, partly data collection. The project is MIT, so
all camera data is measured or reverse-engineered by us (PLAN.md §4). **Depends on:** Phase 1 (engine split and the 1:1 region renderer).
**Runs in parallel with:** Phases 3 and 4.

**Goal:** a photographer opening their own raws sees **correct colour**, **correct lens geometry** and
**Lightroom-grade detail**, on nearly every camera they own. This is the gap that decides whether people switch.

Clean-room rules hold throughout (PLAN.md §4.2):
- No Adobe matrices, DCP, LCP, presets or profiles.
- No darktable, Ansel, rawspeed, LibRaw or lensfun source or GPL data tables.
- Each data source is recorded in `docs/sources.md` with its licence and how it was obtained.

---

## 2.1 `camdb` crate: per-camera data (≈ 15–30 h)

**Contents per camera model** (keyed by make, model and the EXIF unique model id):
- colour matrices for two illuminants;
- forward matrices when known;
- white-balance presets;
- noise profile (Poisson-Gaussian a/b per ISO per channel);
- black and white levels;
- default crop and masked areas;
- CFA layout;
- aliases (marketing names, regional variants).

**Format:** a compact binary table generated at build time from human-editable TOML in `data/cameras/`, lazily
loaded. One file per maker.

**Sources, in priority order:**
1. **Data the file carries itself:**
   - DNG ColorMatrix/ForwardMatrix at runtime;
   - Olympus and Pentax maker-note matrices;
   - **camera-native DNGs** (Leica, Pentax, Ricoh, Hasselblad, phones; checked via the `Software` tag, never from
     Adobe-converted DNGs) can seed `camdb` entries.
2. **Our own chart calibration** (2.2). This is the main source of "measured" entries.
3. **Measured by our tools from raw files:**
   - **WB presets** read from each maker's own maker notes (many bodies store multipliers for every preset), via
     `xtask camdb-harvest` over the CC0 corpus;
   - **black and white levels, crops, CFA layout** from the files and our decoders;
   - **noise profiles**:
     - `app-cli noise-profile` fits a Poisson-Gaussian model (a, b per channel) from an ISO series of a defocused
       flat target;
     - the same model can also be estimated automatically from flat regions of ordinary corpus raws, as a fallback.
4. **LightCraft's per-file embedded-JPEG fit**, which already exists. It is the fallback when nothing above has the
   model, and is marked "estimated" in the Info panel.

Entries record provenance (`source = "chart" | "native-dng" | "maker-note" | "estimated"`) and the contributor.

**Pipeline use:**
- the input transform chooses the matrix by illuminant (interpolating by CCT between the two);
- WB presets fill the white-balance menu;
- noise profiles feed 2.6.

**Done when:** every corpus camera resolves to a `camdb` entry or an explicit "estimated"; no camera falls back to
the neutral matrix silently.

---

## 2.2 Own camera calibration tool (≈ 8–12 h)

- `app-cli calibrate` takes a chart shot (ColorChecker 24 in its 2000 and 2014 versions, ColorChecker SG, IT8.7/2),
  either auto-detected or with corners picked by the user.
- It fits a 3×3 matrix (least squares on ΔE2000) and an optional 2.5D hue/sat LUT (thin-plate spline, the method
  darktable manual describes; implemented from that description, not from its code).
- Output is a TOML `camdb` entry or a user camera profile.
- Reference chart values come from published CIE/manufacturer data. CIE observer and illuminant tables are public
  data.
- A UI wizard in Develop → Profile → Calibrate Camera… gives users their own profiles.
- **Community data pipeline.** This is on the critical path, because there are no GPL tables to fall back on:
  - a documented kit: which chart, lighting, ISO series, flat field and lens grid;
  - `app-cli contribute` packages the shots plus EXIF and a CC0 licence statement;
  - an upload target (Git LFS repo or object store);
  - a CI job re-fits submitted data and opens a PR with the new `camdb`/`lensdb` entries.
- **Priority:** the 50 most-used bodies, from public usage statistics, get a chart shot even if the maintainers have
  to buy or rent time on them.

---

## 2.3 Camera-matching profiles (≈ 6–10 h)

- For each supported maker, generate "Camera Standard / Portrait / Landscape / Neutral / Vivid"-style **looks** by
  fitting our pipeline to the camera's own JPEGs: tone curve plus a 3D LUT in our profile format.
- Use LightCraft's existing fitter (`docs/camera-preview-colour.md`) on many shots per body, not per file.
- Store them as our own profiles in `data/profiles/<maker>/`, and list them in the profile browser under "Camera
  Matching".

---

## 2.4 Raw decoders: coverage (≈ 25–60 h)

| Item | Today | Work |
|---|---|---|
| Canon CR3: all CRX variants, sRAW/mRAW, dual-pixel | lossless and C-RAW verified on M50/R100/R8 only | every CRX variant and subsampled RAW; verify on ≥ 20 bodies |
| Olympus/OM compressed ORF | preview only | decoder |
| Samsung compressed SRW | preview only | decoder |
| Nikon lossy-after-split NEF | preview only | decoder; also NEFs labelled compressed but stored uncompressed |
| Long tail | none | 3FR/FFF (Hasselblad), IIQ (Phase One), MOS (Leaf), ERF (Epson), KDC/DCR (Kodak), MRW (Minolta), NRW, SRF/SR2, X3F if feasible |
| HEIC/HEIF, AVIF import | HEIC behind a feature flag | default-on HEIC decode (pure-Rust HEVC, shared `heif` crate), AVIF decode |

- **Method: clean-room only.** Use LightCraft's black-box analysis (as for compressed NEF and RW2) and prose format
  descriptions. Never LibRaw, rawspeed or dcraw source. Each decoder documents its analysis in the module docs, as
  `vendor/rw2.rs` does.
- **Risk:** LightCraft rates this "high, 40–80 h". The budget covers the common formats first; the long tail falls back
  to embedded previews until done.
- **Corpus:**
  - one CC0 sample per model from raw.pixls.us in `cargo xtask corpus`;
  - `crates/raw/tests/corpus.rs` checks per model: sensor decode (not preview), plausible levels, colour sanity
    against the camera JPEG (median ΔE threshold);
  - a coverage report in `docs/cameras.md` is generated by xtask.

---

## 2.5 `lensdb` crate: lens corrections (≈ 10–18 h)

- **lensfun database** (CC-BY-SA 3.0, ~1,500 lenses), used as **data only**. lensfun's LGPL library code is never read;
  the correction models are implemented from their published mathematical descriptions.
  - Shipped as a **separate data file**, not compiled into the binary, so the MIT code stays separate from the
    CC-BY-SA data. It is converted at packaging time into a compact, lazily loaded table (Ansel's notes measured about
    100 ms just to parse lensfun's XML).
  - It can also be downloaded or updated at runtime from upstream.
  - Attribution and the CC-BY-SA terms go in `ATTRIBUTION.md`; edits we make to that data stay CC-BY-SA in
    `data/lenses/` and are offered back upstream.
  - Our **own** measured profiles (below) are CC0 and kept in a separate file.
- **Lookup:** EXIF lens id, lens model string, focal length and aperture, with crop-factor handling and mount
  filtering. Users can pick manually from make/model menus (the Lens Corrections → Profile UI).
- **Models:**
  - distortion: poly3, poly5, PTLens;
  - TCA: linear, poly3;
  - vignetting: pa model.
  - Applied in the existing geometry stage with a single resample.
  - Amount sliders for distortion and vignetting.
- **Our own profiles:** `app-cli lens-calibrate` with distortion from straight-line or grid shots, vignetting from flat
  fields and TCA from edges, writing `data/lenses/` entries. Users can save custom profiles.
- **Done when:** a profile is found for ≥ 80% of lens EXIFs in the corpus; straight lines stay straight within 0.5 px
  on test charts.

---

## 2.6 Detail: noise reduction, sharpening, Enhance (≈ 12–25 h)

- **Profiled classical NR:**
  - use the `camdb` noise profiles (a variance-stabilising transform, then wavelet/non-local-means denoise in the
    stabilised domain);
  - keep the Luminance/Detail/Contrast and Color/Detail/Smoothness controls as they are;
  - run colour NR at half resolution on both CPU and GPU (an open LightCraft M5 item).
- **AI Denoise:**
  - LightCraft has Bayer-only inference with **opt-in GPL weights** that the user downloads separately; they are never
    bundled with the MIT app. The goal is our own CC0/MIT weights.
  - Follow the **design described in Ansel's prose note** `doc/rawdenoiseai.md` (not its code): a small U-Net
    conditioned on a per-pixel sigma map from the noise profile, deterministic, no external runtime.
  - Extend it to X-Trans and linear DNG.
  - Output, as Lightroom does: a new DNG stacked with the original, plus Amount.
  - Weights: train our own on CC0 / own pairs (preferred), or choose a permissively licensed model. That is an owner
    decision shared with Phase 5.
- **Raw Details:** better demosaic with reduced false colour and artifacts; offer the demosaic choice (AHD, PPG, and
  later RCD/AMaZE-class) as an "Enhance" output.
- **Super Resolution (2×):** classical edge-directed upscaling first; an ML model later (Phase 5).
- Sharpening keeps its masking preview (Alt-drag) at true 1:1, which Phase 1.6 makes possible.

---

## 2.7 Remove tool: content-aware (≈ 6–10 h)

- Port PhotoCraft:
  - `algo/src/inpaint.rs`: multi-scale Wexler EM with PatchMatch NNF and `best_offset`;
  - `algo/src/content_aware.rs`: restricted sampling area, rotation/scale/mirror adaptation, Poisson colour
    adaptation.
- Put them in a new L2 **`inpaint`** crate and copy-rename them as LightCraft did with `heif`.
- Make it resolution independent:
  - store strokes in normalised coordinates;
  - synthesise at preview scale for interaction and at full scale for export;
  - cache results per stroke hash.
- **Spot pin editing:** drag the source, change the size, switch Remove/Heal/Clone, feather and opacity.
- **Detect (dust spots):**
  - a "Visualize Spots" threshold already exists;
  - add automatic spot detection as a sensor-dust finder: same position across photos from one body, low-contrast
    blobs;
  - add sync of spots across a selection.

---

## 2.8 Render fidelity suite (≈ 8–14 h)

- **References:** renders made by the maintainer's own Lightroom Classic copy, from CC0 raws, with settings sweeps.
  - They are stored **locally only** (never committed) in `plan/observations/fidelity/`.
  - The reference pixels themselves are never shipped.
- **Metrics:** ΔE2000 (mean, p95), SSIM, histogram distance, and highlight-clipping location per sweep step.
- **Tuning targets, in order:**
  1. default rendering;
  2. exposure, highlights, shadows, whites, blacks;
  3. texture, clarity, dehaze;
  4. NR and sharpening;
  5. vignette and grain.
- **Decide tone-curve placement.** LightCraft applies curves after display encoding; compare against reference
  behaviour and move the stage if needed, behind a process-version bump so old edits render unchanged.
- **CI:** a small public subset compared against *our own* golden renders, which guards against regressions.

---

## 2.9 Soft proofing with real ICC (≈ 6–10 h)

- Port PhotoCraft's **`cms`**: pure-Rust ICC v2/v4 parse and write, LUT profiles including CMYK↔Lab, rendering
  intents, black point compensation, gamut check.
  - Replace or complement `moxcms` for output transforms.
  - Keep one CMS path, so the preview equals the export.
- **Soft proof:**
  - choose any installed or added printer/paper ICC;
  - Perceptual or Relative intent;
  - **Simulate Paper & Ink** (paper white plus black point);
  - monitor and destination **gamut warnings**;
  - "Create Proof Copy" (a virtual copy).
  - Before/after shows proof against the original.
- **Display profile:** read the OS monitor profile (ICC from the platform, via an isolated crate if FFI is needed) and
  render through it.
- **Done when:** proofing through a known profile matches a reference transform within ΔE 1. Print (Phase 3) uses the
  same code.

---

## Exit gate

- [ ] ≥ 95% of corpus camera models decode from sensor data; coverage report generated.
- [ ] Median ΔE2000 ≤ 2 against camera JPEGs (camera-matching profile) for supported bodies; no silent neutral-matrix
      fallback.
- [ ] Lens profile found for ≥ 80% of corpus lens EXIFs.
- [ ] Content-aware Remove ✅, spot editing ✅, dust detection ✅.
- [ ] Fidelity suite runs; default rendering mean ΔE2000 ≤ 3 against the local references.
- [ ] Soft proofing with printer ICC and Simulate Paper & Ink ✅.
