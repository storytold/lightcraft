# LCC "Uniform Light" (flat-field) — design notes

> Status: exploration only, no code yet (2026-11-05). The maintainer has approved an LCC feature. Scope: a
> flat-field correction like Capture One's LCC "Uniform Light" with an Amount slider. A reference shot through a
> diffuser gives a smoothed **luminance** gain map; the same gain multiplies R, G and B, so there is **no colour-cast
> correction**. Dust removal is out of scope.

**Naming gotcha before anything else:** every crate in Chromaforge is `lcc-*` / `lcc_*` (the legacy brand prefix,
[`AGENTS.md`](../AGENTS.md) "The name lives in `brand.toml` only"; e.g. `lcc_develop`, `lcc_raw`, `lcc_brand`). A
grep for "lcc" finds the brand, not lens cast calibration, and code named `lcc_*` would read as a crate. The code
should call the feature **flat field** (`vignetting_calibration`, `optics.vignettingCalibration…`); "LCC" stays a UI/docs word.

## 1. How the Optics vignetting correction works today

**What it computes.** `Warp` in [`crates/pipeline/src/optics.rs`](../crates/pipeline/src/optics.rs) carries two
vignetting terms:

- Manual Vignetting: `gain = 2^(vig_stops · r^vig_power)` with `vig_stops = 1.5 · optics.vignetting / 100` and
  `vig_power = 1 + 6 · midpoint/100`, `r` = distance from the image centre over the half diagonal, capped at 1.5
  (`Warp::from_settings`, optics.rs:74–92; `Warp::gain`, optics.rs:237–251; `VIGNETTE_STOPS`, optics.rs:30).
- Profile vignetting: DNG `FixVignetteRadial` (`1 + k0 r² + … + k4 r¹⁰`) from the file or the lens database,
  scaled by `optics.profile_vignetting / 100` (`embedded_vignette_gain`, optics.rs:396–409; struct
  `EmbeddedVignette`, [`crates/develop/src/settings.rs`](../crates/develop/src/settings.rs):1162–1170). It is applied
  only when `optics.lens_profile` is on (optics.rs:84–90).

Both are one scalar per pixel, multiplied into R, G and B alike — exactly the shape a "Uniform Light" gain has.

**Where in the pipeline.** Stage 1, *geometry*, inside the single resample
([`crates/pipeline/src/lib.rs`](../crates/pipeline/src/lib.rs):7–14 lists the stage order). `sample_warped`
([`crates/pipeline/src/geometry.rs`](../crates/pipeline/src/geometry.rs):502–528) finds each output pixel's source
position `g`, samples it bilinearly and multiplies by `wp.gain(g)` (geometry.rs:521–524). The same gain is used by
generative-fill patches (`crates/pipeline/src/genpatch.rs`:75). The GPU renderer duplicates it: the warp's
parameters are packed into 65 floats (`crates/gpu/src/render.rs`:565–587, `debug_assert_eq!(p.len(), 65)`) and
`warp_gain` in `crates/gpu/src/wgsl/geom.wgsl`:137 is applied at geom.wgsl:194. CPU/GPU agreement is tested in
`crates/gpu/tests/it/equivalence.rs` (33 vignetting references).

A frame whose warp is the identity has no warp at all (`warp: (!warp.is_identity()).then_some(warp)`,
geometry.rs:94; `is_identity = !moves_pixels() && !has_gain()`, optics.rs:129–131), so a new gain must be part of
`has_gain()` (optics.rs:125–127) or it would be skipped on the Copy/Affine sampling paths.

Not to be confused with **Effects ▸ Vignette** (post-crop, creative): that runs in stage 5 on display-linear values
after the tone map (`crates/pipeline/src/finish.rs`:109–125, 861–880; GPU `finish.wgsl`:951).

**Is the data linear there?** Yes. The pipeline's input is "scene-referred, linear Rec.2020 … already EXIF-oriented"
(lib.rs:3–5). For a raw, `load_bytes_now` ([`crates/engine/src/files.rs`](../crates/engine/src/files.rs):715–903)
builds it as: decode → pixel repair on the mosaic (files.rs:738) → optional outlier suppression (740–746) →
`RawImage::develop` / `develop_binned_masked` (normalise, demosaic, `OpcodeList3`, default crop; files.rs:787–802) →
`finish` closure (files.rs:822–841): highlight rebuild → `CameraColourModel::apply` (white balance, camera matrix or
fitted look, DNG profile tables, baseline-exposure gain; files.rs:408–440) → fit to `max_edge` → orient. Exposure is
applied later, in stage 2 (lib.rs:19–21). So at the vignetting stage the values are linear Rec.2020 but already white
balanced, matrixed and scaled by baseline exposure, and not clipped (floats above 1.0 survive).

**Before or after highlight clipping/recovery?** After. Highlight reconstruction runs in the source load
(`rebuild_highlights` / `rebuild_highlights_masked`, files.rs:420–433, called at 826–829) on camera RGB with the
sensor clip `HIGHLIGHT_CLIP = 0.99` (files.rs:406), before white balance; the algorithm is in
`crates/raw/src/highlight.rs` (header 1–22, `clip_neutral` 28–31). The vignetting gain then multiplies the
reconstructed values. Process versions do not reach the source load ([`process-versions.md`](process-versions.md),
"Changes to how a file is decoded … the process doesn't reach that stage today").

**Coordinates.** Neither sensor nor output coordinates: the gain is evaluated at the **source position in the
user-oriented, EXIF-oriented, default-cropped source**, in pixels, normalised by its half diagonal (optics.rs:237–243;
the warp's `w × h` are the oriented source, optics.rs:34–38). So crop, straighten, Upright/Transform and flips don't
move it (the warp maps output → transformed → lens-corrected → source, optics.rs:3–11), and preview/export sizes
agree (resolution independence, AGENTS.md "Non-negotiables"). But it is not sensor-aligned:

- the active-area crop happens in `RawImage::normalized` and the default crop in `finish_demosaiced`
  (`crates/raw/src/lib.rs`:859–929), and non-square pixels are stretched (`stretched_height`, lib.rs:925–928);
- the source is EXIF-oriented at load (`img.into_oriented(raw.orientation)`, files.rs:837) and user orientation is
  applied in `Frame::with_lens` (geometry.rs:70–75).

File-embedded lens data is converted for this: `EmbeddedLens` is stored "converted to the oriented,
default-cropped image" (settings.rs:1112–1116) and re-expressed for the user orientation by `reorient_lens`
(optics.rs:411–446). A flat-field map recorded in sensor coordinates needs the same treatment.

## 2. Where linear, black-subtracted raw data exists, and could a gain map go there?

**Where.** `RawImage::normalized` (`crates/raw/src/lib.rs`:859–901) applies `OpcodeList1` on raw values, subtracts
each site's black, scales each site's white to 1.0, crops to the active area, then applies **`OpcodeList2` on the
normalised CFA data** (lib.rs:899). That is the earliest linear, black-subtracted point, still a mosaic and not white
balanced. After demosaic, `develop` gives camera RGB with white = 1.0, not white balanced (lib.rs:903–914), which is
what highlight reconstruction reads.

**There is already a gain-map mechanism here.** `crates/raw/src/opcodes.rs` parses, writes and applies DNG
`GainMap` (opcodes.rs:88, parse 404, `gain_at` 550, applied in `apply_list` 609–635), including binning/cropping of
its area (opcodes.rs:135–177). A DNG flat field is exactly a `GainMap` in `OpcodeList2`. (Not to be confused with
`crates/raw/src/gaintable.rs`, ProRAW's tone-mapping `ProfileGainTableMap`.)

**Could ours go there?** It could, but it costs more than it gains for an achromatic map:

- **Amount would re-decode the raw.** Develop settings that change the source go through `LoadOptions::of`
  (`crates/engine/src/media.rs`:179–188) and `DecodedSource::serves` (media.rs:277–285), as `sensor.pixelFix` and
  `detail.nr_outliers` do; any change drops the decoded source. A slider drag would then mean a full decode per step,
  far past the 16 ms slider budget (AGENTS.md "Testing & performance").
- **Previews lose the fast path.** `develop_binned` falls back to a full `normalized()` whenever `OpcodeList1/2` are
  non-empty (`crates/raw/src/binned.rs`:70), so injecting a map there slows every thumbnail and preview unless the
  binned path learns the map.
- **Denoise and highlights must follow.** The AI-denoised twin is made from `raw.normalized()`
  (`crates/engine/src/denoise.rs`:1245) and read back as a cached product (files.rs:697–713), so its key would need
  the map; highlight reconstruction's clip test (0.99) would need dividing by the gain, or brightened corners read as
  clipped.
- **It buys nothing for a uniform gain.** Everything between the mosaic and stage 1 is linear and per-pixel except
  highlight reconstruction and the colour tables (`CameraColourModel::apply`, files.rs:436–440). A scalar gain
  commutes with white balance and the 3×3 matrix; the hue/sat tables are indexed by hue and saturation, which a
  scalar gain leaves alone (a DNG `LookTable` with a value axis is the exception). Raw-stage placement matters for a
  **colour-cast** LCC (per-channel gains, which the sensor's colour crosstalk produces); that is out of scope here.

So the current stage is not "too late" for Uniform Light. The open question is only highlights (see §7).

## 3. How develop settings are stored and versioned

**Schema.** `DevelopSettings` (settings.rs:271–347) is one serde struct, `#[serde(default)]` everywhere: missing
fields take defaults, unknown ones are ignored (settings.rs:1–4); `SCHEMA_VERSION = 1` (settings.rs:9). Field names
are snake_case in JSON (`optics.profile_vignetting`); control ids are camelCase (`optics.profileVignetting`).
Optics fields: settings.rs:1050–1107. Fields added later avoid changing old settings' JSON with
`skip_serializing_if = "…::is_default"` (`hdr`, `skin`, `retouch`, `sensor`; settings.rs:286–336), because
`DevelopSettings::hash64` hashes the JSON (`crates/develop/src/lib.rs`:99–106) and that hash keys preview caches and
the catalog's content-addressed `develop_settings` table ([`catalog.md`](catalog.md) "develop settings are
content-addressed").

**Process versions.** `ProcessVersion` / `Process` (settings.rs:146–270), now V1…V7, LATEST = V7
(settings.rs:216–269); rules and table in [`process-versions.md`](process-versions.md): a change to how *existing*
settings render needs a new process; a new setting that is neutral by default does not. A flat field at Amount 0 / no
reference is neutral, so it needs no process.

**Catalog format.** The catalog format is **16** now, not 13: `VERSION = 16` in
`crates/catalog/src/journal.rs`:91, table at journal.rs:43–60 (13 was `Meta.gps_removed`; [`catalog.md`](catalog.md)
line 112 still says 13 and is stale). The rule (journal.rs:62–66): bump `VERSION` when adding a serialized field "to …
develop settings that an older build would misread or silently drop". In practice recent develop fields (`sensor`,
`hdr`, `skin`) shipped without a row; see open questions.

**XMP sidecars.** `lc:settings` holds the complete `DevelopSettings` JSON (exact round trip;
[`xmp-interop.md`](xmp-interop.md):32, `crates/engine/src/sidecar.rs`:6, 29, 244), so a new field travels
automatically. `crs:` fields are mapped approximately in `crates/engine/src/crs.rs` (`VignetteAmount` →
`optics.vignetting`, crs.rs:479–480); Adobe has no flat-field equivalent to map. Pixel-filter stacks are the one
thing written under their own namespace instead (sidecar.rs:749).

**Copy / paste / sync / presets.** `SettingsGroup` in `crates/develop/src/presets.rs`: the Optics group copies the
whole `optics` object (presets.rs:131–134), and `OPTICS_ITEMS` lists sub-items that can be ticked one by one
(presets.rs:228–245). The process is in no group (presets.rs:9–11; process-versions.md "Copy, paste, sync and
presets").

**What a new setting needs** (following `sensor.pixelFix` and `optics.vignetting`):

1. The field with a neutral `Default`, ideally skipped while default, in `Optics` (settings.rs:1050–1107).
2. A control spec for the numeric Amount in `crates/develop/src/controls.rs` (the table at ~295–347; e.g.
   `"optics.vignetting" => optics.vignetting, "Vignetting", Optics, -100, 100, 0, 1, 0, Centered;` at 320). Every
   slider is a control spec (AGENTS.md "Everything is a command"); clamping of hostile values is tested in
   `crates/develop/src/lib.rs`:544–546.
3. A sub-item in `OPTICS_ITEMS` (presets.rs:228–245) so it can be copied without the other optics settings.
4. The reference itself (not a number) set through `develop.merge` like the lens picker's `optics.lens`
   (`ChosenLens`, settings.rs:1072–1090) — plus commands for making and listing references (engine command registry,
   `crates/engine/src/cmd/`), so UI, CLI, control channel and MCP share them.
5. The renderer: CPU (`Warp`, optics.rs) and GPU (`render.rs` warp params, `geom.wgsl`), with the equivalence test.
6. Catalog `VERSION` bump if the maintainer applies journal.rs's rule to develop fields; `RENDER_CACHE_VERSION`
   (`crates/engine/src/media.rs`:65, now 77) only if existing renders change (they shouldn't).

## 4. The Optics panel in each layout, and locale strings

- **Desktop (Lightroom-style right panel):** `panels/right.rs`:91 calls `edit::show_controls`, which draws the
  section `section(app, ui, &d, "optics", "Lens Corrections", …)` in
  [`crates/ui-egui/src/panels/edit.rs`](../crates/ui-egui/src/panels/edit.rs):418–498: Remove CA and Enable Lens
  Corrections checkboxes (`develop.merge`), lens note and picker, profile sliders when enabled (435–437), the "New
  photos start from" default (442–468), then **Manual**: `optics.distortion`, `optics.vignetting`,
  `optics.vignettingMidpoint` (470–474), and the Defringe flyout (476–496). Sliders are `control()` / `control_as()`
  (edit.rs:41–50), built from the control specs. Menu entry `section.optics` = Cmd+6 (`crates/ui-egui/src/menus.rs`:199).
- **Modern (touch) layout:** [`crates/ui-egui/src/modern_dock.rs`](../crates/ui-egui/src/modern_dock.rs) — the
  Optics dock group's pill sliders are a fixed list `&["optics.distortion", "optics.vignetting", "optics.caRed",
  "optics.caBlue"]` (modern_dock.rs:130); its "more" button opens the same Edit-panel sections, `("optics", "Lens
  Corrections")` and `("geometry", "Transform")` (modern_dock.rs:108), via `edit::show_section` (modern_dock.rs:472);
  a long press on any `optics.*` slider opens the `optics` section (modern_dock.rs:139–148). Docs:
  [`android.md`](android.md):15.
- There are two layouts, Desktop and Touch/Modern (Settings ▸ General ▸ Layout; [`android.md`](android.md):15).
  `panels/classic.rs` is not a third one: it is the Lightroom Classic–style panel stack of the Library side
  (Navigator, Folders, Quick Develop…; classic.rs:1–5), and the Desktop Develop column (`panels/right.rs`:54–93, with
  Classic's Previous / Sync… bar from `classic_tools.rs`) shows the same `edit::show_controls`. So there is one Optics
  section to change (edit.rs) plus the Modern pill list (modern_dock.rs:130), if the Amount should be a pill too.
- UI tests touching Optics: `tests_develop_panels.rs`, `tests_panels.rs`, `tests_compact_layout.rs`,
  `tests_solo_sections.rs`, `tests_lens_db.rs` in `crates/ui-egui/src/`.

**Locale strings.** Text goes through `crate::i18n::tr("…")`; keys are the English strings
([`localization.md`](localization.md) "How a message is found"). Upstream's catalogs are
`crates/ui-egui/locales/<code>.json`; **Chromaforge's own strings go in `crates/ui-egui/locales/fork/<code>.json`**,
merged over upstream's (`crates/ui-egui/src/i18n_fork.rs`:1–3). `tests_i18n_coverage.rs` fails when a fork string is
missing from a complete language (every language except the `PARTIAL` list: pl, cs, it, ka, sv, fi, id, vi;
tests_i18n_coverage.rs:23–28); the fast gate also checks the fork catalogs are sorted ([`ci.md`](ci.md)). Control
labels ("Vignetting") are translated the same way (e.g. `locales/de.json`:1589).

## 5. Capture One catalog import

`crates/engine/src/capture_one_catalog.rs` (1001 lines) reads library data only: "Develop settings are not read yet"
(capture_one_catalog.rs:9–11). `read_with_progress` (279–) loads `ZENTITIES`, `ZIMAGE`, `ZPATHLOCATION`,
`ZVARIANT`, `ZVARIANTMETADATA`, `ZKEYWORD`, collections and `ZDOCUMENTCONTENT` (291–303). The only per-variant
layer lookup is metadata: rating, colour tag, IPTC from the `ZVARIANTMETADATA` rows of the variant's
`ZCOMBINEDSETTINGS`, `ZADJUSTMENTLAYER` and `ZDEFAULTLAYER` (362–366). **Nothing in the importer reads
`ZVARIANTLAYER` or anything LCC-like**; a grep for `ZSTYLESOURCE`, `ZVARIANTLAYER`, `.lcc` or flat field in `crates/`
finds nothing.

Outside the code (from inspecting a Capture One catalog, read-only):

- In `ZVARIANTLAYER`, LCC is applied as a *style* layer (entity 19 `VariantStyleLayer`, `ZSTYLESOURCE = 'LCC'`).
- The catalog's `Adjustments/LCC/` folder holds the profiles as `<32-hex>.lcc` files (12–520 KB each). Their header
  is a tagged binary container naming "CaptureOne LCC", a version, camera make, model and **body serial**, a raw hash
  and a "Chroma" block. The format is undocumented; reading it would be black-box analysis of a user's own files
  (allowed for data, AGENTS.md "Clean-room sources"), and is a separate task.

A Capture One develop import would wait for this tool, so it can bring LCC across in one pass.

## 6. Tests, CI and PR / changelog conventions

- **Tests** ([`AGENTS.md`](../AGENTS.md) "Testing & performance"): unit/property tests next to the code (crates use
  `tests*.rs` siblings, e.g. `crates/pipeline/src/tests_geometry.rs`, `tests_window.rs`); behaviour and invariants,
  no change-detector or golden-image tests; procedural images from `lcc-scenes`, never committed media; hostile-input
  no-panic checks for anything parsed (an imported reference or `.lcc`). CPU/GPU parity in
  `crates/gpu/tests/it/equivalence.rs`. Process rules have `crates/develop/src/tests_process.rs`. End-to-end:
  `apps/cli/tests/`, `crates/mcp/tests/it/e2e.rs`; SIL harness `apps/cli/tests/sil.rs`, `cargo xtask sil`
  ([`testing-sil.md`](testing-sil.md)). Tests that can't run here use `lcc_testkit::skip!`.
- **Never crash:** no `unwrap`/`expect`/`panic!`/indexing on input-derived numbers in non-test code, enforced by
  clippy (AGENTS.md "Never crash"). A gain map from a file needs `get()`, checked sizes and capped allocations.
- **CI** ([`ci.md`](ci.md)): PRs run `cargo xtask ci --fast` (fmt, brand, docs, notes, i18n sort, clippy
  `-D warnings`, parity refs, layers, assets, deny, wasm and Android checks) plus SIL; the full `cargo xtask ci`
  runs before releases. One `ci` per machine (`xtask/src/ci_lock.rs`). Per commit: `--fast` + `cargo test -p` of the
  touched crates. Perf: `cargo xtask bench`, `<PREFIX>_PROFILE=1`.
- **Commits / PRs:** one task id per commit (`M2.3: …`); PR title starts with the checklist id or `Issue #n:`
  ([`CONTRIBUTING.md`](../CONTRIBUTING.md) "Open a pull request"); template `.github/PULL_REQUEST_TEMPLATE.md` (Problem,
  Change, How it was checked, "Existing edits and libraries", "Shared numbers taken" — catalog `VERSION`,
  `RENDER_CACHE_VERSION`, `LOOK_VERSION`, process). A **Sources:** line for every algorithm (here: "own design" / the
  DNG spec's GainMap). Branch from latest `main`, merge (not rebase) `origin/main` before pushing.
- **Parity docs in the same commit:** the row for this belongs next to `LR-EDIT-OPTICS-MANUAL`
  ([`parity-checklist.md`](parity-checklist.md):299) — a new beyond-Lightroom row; `docs/gaps.md` if listed;
  `cargo xtask parity` checks cited ids and paths.
- **Release notes:** `docs/whats-new/<YYYY-MM-DD>-<slug>.md` (one `### Title` + bullets; `docs/whats-new/README.md`)
  and `docs/changelog/unreleased/<YYYY-MM-DD>-<slug>.md` (one `## Develop` section + bullets with `PR #n`;
  `docs/changelog/unreleased/README.md`). Never edit `docs/whats-new.md` or a released changelog;
  `cargo xtask notes --check` enforces it.
- **Docs** name the fork "Chromaforge" and never write binary names or the env prefix outside `.md.in` templates
  (AGENTS.md "The name lives in `brand.toml` only"; `cargo xtask brand check`).

## 7. Open questions

1. **Highlights in dark corners.** With the gain after highlight reconstruction, a corner that clipped on the sensor
   is rebuilt, then brightened past the rest of the frame's clip level. Is that acceptable (it matches today's
   vignetting slider), or should reconstruction see the gain? Needs a test on a real flat-field + clipped-corner raw.
2. **Where reference profiles live.** Options: in the settings JSON (a coarse grid, e.g. 64 × 43 f32 ≈ 11 KB, bloats
   every edited photo and preset; travels in `lc:settings`), or a per-body store referenced by id, like
   `crates/engine/src/sensor_maps.rs` (`<config>/sensor-maps`, per body serial) and camera profiles
   (`crates/engine/src/camera_profiles.rs`, `dcp:<id>`). A store keeps settings small but a sidecar or copied
   library then carries a dangling id. What happens on the web build, which has no config folder?
3. **Grid and smoothing.** Grid size, smoothing radius, and how the reference is normalised (centre = 1.0, or
   brightest = 1.0 so the map only brightens?). Amount: 0–100 linear in stops (`gain^(amount/100)`) or in gain?
4. **Coordinates of the map.** Store it relative to the active area (sensor), then convert per photo for default
   crop, pixel aspect, EXIF and user orientation, as `reorient_lens` does; and refuse a reference whose raw size
   doesn't match (sensor maps refuse other models/sizes the same way).
5. **Interaction with lens-profile vignetting.** A flat field already removes lens fall-off; should applying one
   switch Profile Vignetting off (C1 does not touch its own lens correction), or multiply both?
6. **Catalog format bump.** journal.rs's rule says a new develop field older builds would drop needs a `VERSION`
   bump (17), but `sensor`, `hdr` and `skin` shipped without one. Which does the maintainer want here?
7. **Making a reference.** A command on a selected raw (`vignettingCalibration.create`?), batch per lens/aperture, and how the
   C1 importer maps an `.lcc` (decode its grid, or ask the user to re-shoot)?
8. **Copy semantics.** Should the reference be in the Optics copy group (so syncing a shoot copies it) — C1's own
   workflow copies LCC across the shots of one lens and aperture.

## 8. Recommendation: where the flat-field stage goes

**Put the gain in stage 1, inside `Warp::gain`** (`crates/pipeline/src/optics.rs`:237), as a third factor next to
manual and profile vignetting, sampled bilinearly from a smoothed grid at the source position:

- it is where Chromaforge already applies achromatic fall-off corrections, linear and before every spatial and
  tonal stage (noise reduction, local tone, masks), so masks and Auto see the evened-out picture;
- the Amount slider stays cheap: the decoded source is reused and only the geometry resample reruns, like dragging
  Vignetting today (no re-decode, no lost binning fast path, no denoise-product key);
- resolution independence, crops, Upright and windowed renders come for free from the existing framing
  (geometry.rs:502–528), and generative fill picks it up through the same `gain()` (genpatch.rs:75);
- for a same-gain-on-all-channels map the result equals a raw-stage gain except for highlight reconstruction
  (open question 1).

What it requires: extend `has_gain()` so the warp isn't dropped as identity (optics.rs:125–131); carry the grid on
`SourceInfo` / `Frame` already converted to the oriented, default-cropped frame (like `EmbeddedLens`); add it to the
GPU path (a small storage buffer or texture beside the 65-float params in `render.rs`:565–587, sampled in
`geom.wgsl` `warp_gain`) with an equivalence test; and keep the settings field neutral and skipped while default so
no existing photo's JSON, hash or render changes (no new process).

Revisit the raw stage (`OpcodeList2`-style `GainMap` in `RawImage::normalized`, `crates/raw/src/lib.rs`:899) only
if colour-cast correction comes into scope later; the GainMap opcode code in `crates/raw/src/opcodes.rs` is the
starting point then.

## 9. Phase 1: measuring maps (2026-11-05)

**Amount and what we validate against.** Recent Capture One versions overcorrect at LCC amount 100 (users set
about 43 to look right), while older versions were correct at 100. Chromaforge does not emulate either:

- The map is measured on linear data and applied on linear data, so amount 1.0 is the physically correct full
  correction. `DEFAULT_VIGNETTING_CALIBRATION_AMOUNT = 1.0`, and the range is 0 to 1.2 (Capture One's slider).
- The amount scales the correction in EV: `gain^amount`.
- A map is checked against a second reference of the same setup (`--check` below), not against Capture One's
  output.

**A map is valid only for an identical active area.** It is a grid over the sensor's active area, unrotated
(`VignettingMeasurement::sensor`). Phase 2 must refuse a photo whose active area differs. This includes the same body in
another image area: a Z 7 shot in 5:4 records 6912 × 5520 px instead of 8288 × 5520. It also includes another body
of the same lens, even a near-identical sensor: Z 7 is 8288 px wide, Z 8 is 8280. `--check` always refuses a size
mismatch.

**What landed** (`crates/raw/src/vignetting_calibration.rs`, CLI `apps/cli/src/vignetting_calibration.rs`):

- `VignettingMeasurement` (serde, grid of gains ≥ 1, never pre-scaled) and `VignettingCalibrationSource` (camera, serial, lens, focal
  length, aperture, SHA-1).
- `build`:
  - Takes the green samples of `RawImage::normalized`, which already has any DNG `OpcodeList2` GainMap applied, so
    the map measures the fall-off that remains.
  - Per-cell trimmed mean; any cell with more than 1 % clipped green samples refuses the reference (`Clipped`,
    with the cell and percentage).
  - log2, 3 × 3 median and Gaussian, both with odd-reflection padding; normalised to the brightest cell.
- Sampling (`gain_at`, `ev_at`) is bilinear in log space, with linear extrapolation over the last half cell to the
  frame edge.
- `SensorFrame` maps between `Warp::gain`'s coordinates (EXIF and user orientation, default crop, pixel aspect, any
  fitted size) and the sensor.
- `fit_centre` fits `a + b·d² + c·d⁴` with a coarse-to-fine centre search and reports the offset and RMS.
- `check` reports the residual against the median corrected cell. It passes at max |residual| < 0.05 EV and
  peak-to-peak < 0.1 EV.
- `<cli> flat-field REF.raw [--check B.raw [--force]] [--save MAP.json] [--json]` prints corners, the radial table,
  the optical centre, and the lens database's vignetting evaluated as `Warp::gain` does.

Nothing is in the pipeline, the settings or the UI yet.

**First real references** (17 NEFs, Z 7 and Z 8):

- All measure without clipping. Corner fall-off ranges from 0.8 EV (500 mm f/5.6) to 3.6 EV (24 mm f/1.8).
- The only same-setup pair with an identical active area (28 mm f/2.8 on the Z 7) fails `--check`: 0.5 EV
  peak-to-peak, with centres 4 % apart and fit RMS around 0.05 EV. One of the two shots has uneven light (a sky
  gradient or the diffuser).
- The other same-setup pairs differ in image area (5:4 vs 3:2), and the Z 7 / Z 8 pairs differ in width, so they
  can't be compared under the identical-area rule.
- Better references are needed: two diffuser shots per setup at the same image area.

### Phase 1 follow-up: image areas, centre fit, lens database (uncommitted)

**Image areas of one sensor.** Nikon records where the stored frame sits on the sensor in maker-note tag `0x001b`
(CropHiSpeed: mode, sensor width and height, crop width and height, crop x and y):

| Camera and image area | CropHiSpeed |
|---|---|
| Z 7, 3:2 | `11, 8288, 5520, 8288, 5520, 0, 0` |
| Z 7, 5:4 | `3, 8288, 5520, 6912, 5520, 688, 0` (centred) |
| Z 8, 3:2 | `11, 8280, 5520, 8280, 5520, 0, 0` |

- `lcc_raw::sensor_window(bytes, active)` reads it (`crates/raw/src/sensor_window.rs`, `vendor/nef.rs`; it came in with the lens-profile crop-mode fix, PR #856). It returns a window only when the
  window's size equals the active area of `normalized()`.
- A map keeps its reference's window.
- `VignettingMeasurement::placement` lets a map serve a smaller image area of the same sensor: offset = origin difference.
  Another sensor, an unknown position, or an area the map doesn't cover are refused.
- `relative_offset` is the comparison-only version: a 5:4 map can be checked against a 3:2 shot over the area they
  share.
- `find_offset` searches ±64 px (steps of 4, then 1) for the placement with the smallest spread of corrected cells.

Z 7 results:

| Pair | Direction | Result | Offset from the file | Best fit |
|---|---|---|---|---|
| 85 mm f/1.8 | 3:2 map → 5:4 shot | PASS (max 0.024 EV, p2p 0.047) | +688, 0 | +648, −13 |
| 85 mm f/1.8 | 5:4 map → 3:2 shot | PASS (max 0.027, p2p 0.047) | −688, 0 | −648, +13 |
| 50 mm f/1.8 | 3:2 map → 5:4 shot | FAIL (max 0.097, p2p 0.128) | +688, 0 | +663, −2 |
| 50 mm f/1.8 | 5:4 map → 3:2 shot | FAIL (max 0.091, p2p 0.119) | −688, 0 | −665, +1 |

- The 85 mm best fit is about 40 px off the file's offset, consistently in both directions. That is the same as the
  difference between the two shots' fitted optical centres (−0.5 % of 8288 vs 0.0 % of 6912 ≈ 41 px). The lens's
  centre moved between the shots (in-body stabilisation recentres the sensor between frames), not the crop. The
  crop offset itself holds: with no offset the spread is 0.17 EV instead of 0.011.
- The 50 mm pair fails at any offset (the minimum is shallow, 0.021 vs 0.022 EV). Its two shots simply had different
  fall-off: different light, or focus distance.

**Centre fit:** `a + b·d² + c·d⁴ + e·d⁶`, solved with partial pivoting (`None` when nearly singular). "Uneven"
means RMS > max(0.03 EV, 2 % of the mean corner fall-off). With this rule only the 28 mm pair still disagrees; the
known sky shot (a Z 7 300 mm f/2.8 reference) is marked as such.

**Lens database:** a folder of lensfun XML files.
- Set it in Settings ▸ Import ▸ Lens profiles: Download fetches lensfun's `data/db` into `<settings>/lensfun/db`, or
  Choose Folder… points at an existing copy.
- The choice is stored in `<settings>/lens-profiles.json` (`{"dir": …}`); `<PREFIX>_LENSFUN_DIR` overrides it
  (`docs/lens-profiles.md`).
- Where a profile has vignetting, the measured corner fall-off is 1.10–1.28× the profile's on full-frame shots.

**Found on the way, fixed separately (PR #856, `fix/lens-profile-crop-modes`):** the lens-profile lookup scaled the
profile by the developed frame's half diagonal and the camera's crop factor. A 5:4 frame of a full-frame body
therefore got the profile's full-frame corner fall-off at its own, nearer corners: the same 1.912 EV for the 50 mm
at 3:2 and at 5:4. That over-corrected 5:4 by 0.46 EV at the corners (1.447 EV is right) and 16:9 by 0.20 EV.
`lookup_framed` now evaluates the profile for the camera's normal frame and re-expresses it for the image area
exactly. The `flat-field` diagnostic uses it too: after the fix the 85 mm's measured/profile ratio is 1.28 in both
3:2 and 5:4 (it was 1.02 in 5:4).

Image-area evidence from three Z 8 diffuser shots (50 mm f/1.8; 1:1, DX, 16:9): every cross-check between them
passes (max |residual| 0.005–0.011 EV, peak-to-peak 0.008–0.022 EV), and the offset search finds the CropHiSpeed
offsets within 1–10 px. Ignoring the offset makes the spread 0.18–0.36 EV. An older 3:2 Z 8 shot fails against all
three (peak-to-peak 0.23–0.54 EV), with its centre 1–2 % off theirs: it was lit differently.

## 10. Phase 2 plan: applying a map in the pipeline, and the UI

Written 2026-11-05; revised 2026-11-10 against `main` a974417dc, after PR #856 (the crop-mode lens-profile fix) was
merged. Phase 2 makes a measured map part of an edit. It builds on `lcc_raw::SensorWindow`, `lookup_framed` and
`ImageArea` from #856.

### Name and UI (2026-11-11, later): Vignetting Calibration, Create and Apply

- **Name:** "Vignetting Calibration" in the UI; in code `vignetting_calibration` (modules, settings
  `optics.vignetting_calibration` / `_amount`, commands `vignettingCalibration.*`, library folder
  `<config>/vignetting-calibration`, CLI `vignetting-calibration`) and `VignettingMap…` for the stored map
  (`VignettingMapId`, `EmbeddedVignettingMap`, `Op::AddVignettingMap`, catalog table `vignetting_maps`). It works like
  Capture One's LCC "Uniform Light". Upstream's Library ▸ Flat-Field Correction… (PR #1079, `lcc_raw::flatfield`,
  `photo.flatField`) is a separate tool that writes corrected DNG copies; the names keep the two apart. Earlier names in
  this section (flat field, LCC, `lens_cast`) are the same feature.
- **Panel:** two buttons. **Create** measures this raw (on a worker thread) and reports its exposure. **Apply** gives
  each photo of the selection (or this one) the reference that fits it, picked by `vignettingCalibration.apply`
  without a map: same camera model and lens named on both, a zoom within 5 % (at least 2 mm) of the focal length, the
  same aperture (since 2026-11-13: the same one only, any body; see *One reference per combination* below). It is off, with a
  tooltip, when no reference fits; photos without one are skipped and counted. With a calibration the panel shows its
  reference (Show Reference), what differs in one quiet line, the Amount and Remove. Choosing a map by hand is for
  commands, the CLI and MCP only (`map` / `key`, `vignettingCalibration.list`).
- **Shooting a reference** (What's New): an OLED screen or a real diffuser, not LCD/IPS (it darkens off-axis and
  makes the map too strong); histogram peak at 2/3 to 3/4; 1/30 s or slower on a screen; infinity, tripod, the photos'
  lens, focal length and aperture.

### Withdrawn references, filter and marker (2026-11-12)

- **Why:** Apply prefers the newest reference, so a bad new one took over silently, and without a list there was no
  way back.
- **Withdrawn:** `VignettingMapRecord.withdrawn` (serde default, left out while false) and
  `Op::SetVignettingMapState { id, withdrawn }`, whose inverse restores the state before (format 18 still: 18 hasn't
  shipped). The table stays append-only. Apply's automatic pick skips withdrawn maps; photos that use one keep it.
  Create's batch adds the map withdrawn and then puts it in use, so undoing a Create withdraws its map instead of
  leaving it to be picked as the newest; a Create that measures a map the catalog holds puts it back in use.
- **Store:** the v4 store compared vignetting-map rows by id only; it now keeps the records and rewrites one whose
  `Arc` changed (an op replaces the record it changes), so a withdraw reaches the next checkpoint.
- **Library:** each library file has `withdrawn` too. Within a catalog the catalog's state wins; a library-only map
  uses its file's flag. Every step that sets a state (commit, undo, redo, take-back) rewrites that map's library file,
  best effort (`Session::vignetting_after`); a map the library doesn't hold isn't added to it.
- **Derived reference index:** `Catalog::vignetting_reference_states()` (photo → Reference | Withdrawn), built from the
  table through `reference_photo` on first use and dropped by every op kind that can change it (the table, files,
  SHA-1s, deletion). It feeds the panel's reference block, the Library filter `vignettingReference`
  (`reference | none`), the smart-collection field `vignettingReference` (Develop group; Reference / Not a Reference)
  and the grid/filmstrip marker, so none of them can be stale. Both map ops touch the reference photo, so
  smart-collection tallies follow.
- **The UI shows only references in use** (simplified the same day): the panel offers Stop Using as Reference on an
  active reference; a withdrawn one is a normal photo to the panel, the filter, the rule (it counts as "not a
  reference") and the marker. Create on it measures the same map (the same id) and puts it back in use, through the
  plausibility check. The state itself stays (undo, library files, other catalogs).
- **Commands:** `vignettingCalibration.withdraw` / `.restore` (`map` or the photo's own maps; one undo step; a
  library-only map changes its file) and `vignettingCalibration.references` (catalog and library, with state).

### One reference per combination, the same aperture only (2026-11-13)

- **Matching:** the aperture must be the same (within `SAME_STOPS` = 1/6 stop); the nearest-aperture order, the
  "another body" note and the own-body and newest rules are gone. Apply's automatic pick is the reference in use that
  fits; with none, the reason says whether only the aperture is missing (`noReferenceAtAperture`, the panel's
  "No reference at f/13 for this lens yet").
- **One in use per combination** (`same_combination`): camera model, lens (or both unnamed), aperture, a zoom's focal
  length within `same_zoom_focal`, and for now the same image area (sensor size and window). Create finds the ones in
  use in the catalog and the library (`conflicts`) and replaces them only when asked (the panel's Replace question,
  the command's `replace: true`): they are withdrawn in the Create's own undo step (one only in the library joins the
  catalog first, in use, so undo restores it there and in its file). Photos that use the old one keep it. Among
  references from before this rule that overlap, the one shot last is picked.
- **Image areas** (approved and built the same day): an FX reference serves DX and other crops on the same sensor
  (`sensor_at`), not the other way round. Within a setup (`same_setup`: camera model, lens, aperture, zoom focal
  length) one reference is in use per image area (`coverage`: the window on the full sensor). Create replaces those
  its new reference covers (`covers`: the same area or one inside it), asking first, and adding to the question that
  the new one covers the old one's area too; nothing is lost. Those it doesn't cover (a new DX reference beside an FX
  one, or partly overlapping areas) stay in use without a question, and the report says so (`keptForOtherAreas`).
  Apply uses the tightest image area that covers a photo (`sort_for`).
- **Tests and the reference library:** the app's tests give their sessions no reference library
  (`vignetting_calibration_dir_override`) or a temporary one, so they never read or write the settings folder's.
- **VR:** `lcc_meta::Metadata::stabilization` reads the Nikon maker note's VRInfo (`0x001F`, byte 4: 1 on, 2 off).
  Every Z 7 / Z 8 raw of a 331-raw sample had it, and it matched ExifTool on all 22 test references (6 on). Create still
  creates the reference; its report adds "VR was on: turn it off for references for the most precise result". Three
  of the four references with the "unevenly lit" note were shot with VR on.

### Plausibility check in Create (2026-11-12)

- `lcc_raw::vignetting_plausibility::check(&Normalized)`: on a 96-cell grid of per-cell trimmed means, the largest
  and 99th-percentile deviation of log2 green from its smoothed grid (3 × 3 median, Gaussian σ 1, odd-reflection
  padding), the colour structure (robust RMS of log2 R/G, B/G less their smoothed grids), the brightest area's offset,
  rings brighter than the ring inside them, `fit_centre`'s RMS, the fall-off, the level and the clipped share.
- **Limits** (borderline / fail): detail max 0.05 / 0.12 EV, detail p99 0.008 / 0.015, colour 0.004 / 0.010, peak
  offset 0.10 / 0.25 half-diagonals, rising rings any / 10 %, fit RMS 0.08 / 0.15, clipped 0.1 % / 1 %; fail below a
  level of 0.02 or a fall-off of 0.05 EV; a clean pass with fit RMS above 0.03 gets the "unevenly lit" note.
- **Tuning data:** 22 references (20 + two screen shots) and 2 astro flats against 553 ordinary raws: 318 at
  random and 235 chosen as hard (fog, snow, dawn, dusk, sunsets, the moon, deep sky, sky, clouds, walls). All flat
  fields pass (4 with the note); every photo fails, all on detail. Flat fields reach at most 0.035 EV of detail (a dust
  spot), the closest photo 0.159 EV (an 800 mm sky); the worst reference is at 0.7 of the borderline limits.
- **Create:** `measure` runs the check before the map is made and refuses a Fail (`MeasureError::Implausible`, the
  reasons and the detail measurement in the message); `commit_measured(…, force)` keeps a Borderline one only with
  `force` (the command's `force`, the panel's Create Anyway dialog, which keeps the map measured on the worker thread
  so confirming doesn't measure again). The result carries `plausibility: {verdict, reasons, metrics}`.

### As built (2026-11-11): one PR, maps by reference

Implemented on `flat-field-render` (rebased on `main` b1d04be13). Where this differs from the plan below, this
section wins.

- **One PR, not three.** Steps 1–6 (settings, placement, render, library, commands, UI) are one PR; the stabilisation
  shift (step 7) stays a later one.
- **Maps by reference, not inline** (replacing decision 1). A catalog stores each map once, in a `vignetting_maps` table
  keyed by the map's id (the SHA-256 of its canonical JSON), linked to the reference photo it was measured from
  (`lcc_catalog::vignetting_maps`, `Op::AddVignettingMap`). A photo's settings hold only the id and the amount, so History
  steps, Versions, copy, paste and sync inside a catalog carry the reference.
  - **Append-only.** No op removes a map: removing the reference photo, deleting its file, clearing History and undo
    all keep it, and a map is self-contained (nothing reads the reference raw again). A later "clean up unused maps"
    would be one op that refuses any id still reachable from a photo's settings, History, Versions or Before view.
  - **The reference link is trusted only within one catalog.** Photo ids are unique per catalog only, so the portable
    form never carries one; Export / Import as Catalog remaps it or drops it; and `reference_photo` uses a stored id
    only when that photo's file has the map's reference SHA-1, else finds the photo by the SHA-1.
  - **Portability.** Settings that leave the catalog (XMP sidecars, presets, the clipboard, the control channel and
    MCP, `--save-settings`, peer sync) carry the map at `optics.vignetting_calibration_map`; settings that come in store it with
    the edit that uses them (`vignetting_calibration_io`). Peer sync leaves photos with a map out of an exchange with a device that
    doesn't announce `vignettingCalibration` (an older build would drop the map).
  - **Catalog format 18** (17 is upstream's Export status) covers the table, the op and the settings field.
- **The per-body library** (`<config>/flat-fields`) is the cross-catalog store: every map made, under the same id as
  in a catalog. Choose from Library lists the library's and the catalog's maps merged by id.
- **Grid: 96 cells along the long side** by default (was 48). On the Z 7 85 mm reference corrected with its own map,
  the residual against the centre is a step inside the inner 15 % of the radius, flat outside it: 0.019 EV at 48
  cells, 0.013 EV at 48 without smoothing, 0.007 EV at 96, 0.005 EV at 96 without smoothing. Cell averaging and linear
  interpolation flatten the peak at the optical centre; smoothing adds about a third.
- **Several photos at once:** Apply to Selected Photos (`vignettingCalibration.apply` on the selection), Copy / Paste and Sync
  Settings (the "Lens Cast Calibration (LCC)" item).
- **No menu item:** Library ▸ Flat-Field Correction… is upstream's separate DNG-copy tool (PR #1079), so LCC is
  reached from Develop ▸ Lens Corrections. The names (`vignetting_calibration` in code, Lens Cast Calibration in the UI) keep the
  two apart.

### Where the map multiplies in

- **CPU:** `Warp::gain` (`crates/pipeline/src/optics.rs`, the `gain` method). It gains a third factor next to
  manual vignetting (`vig_stops`) and profile / embedded vignetting (`lens_vig`).
  - `Warp` carries an `Option<Arc<FieldGain>>`: the map already re-expressed in `Warp`'s own frame (the
    user-oriented, EXIF-oriented, default-cropped source, at full size) as a small EV grid, plus the amount.
  - Doing the coordinate work once, when the frame is built, keeps `gain()` cheap: bilinear in log space, then
    `2^(amount · ev)`. It also lets the CPU and the GPU sample the very same numbers.
  - The grid is 64 × 64 nodes resampled from the sensor map. It's exact under orientation changes (nodes map to
    nodes) and loses nothing visible for a smooth field.
- **`has_gain()`** (optics.rs, next to `is_identity`) must include the flat field. Otherwise a photo with no other
  warp takes the Copy / Affine sampling path and the map is silently skipped (`geometry.rs`, the
  `(!warp.is_identity()).then_some(warp)` in `Frame::with_lens`). `genpatch.rs` already multiplies by `wp.gain(g)`,
  so generative-fill patches follow.
- **Windowed renders:** `Frame::sample_windows` (new on `main`, `f4ae084e8` / `0a99b96be`) renders several windows of
  one frame through the same `sample_base` → `sample_warped` → `wp.gain()`. The windows share the frame's `Warp`, so
  the field needs nothing extra, but the tests cover that path too.
- **GPU:**
  - `warp_params` (`crates/gpu/src/render.rs`) packs the warp into 65 values, guarded by a
    `debug_assert_eq!(p.len(), 65)`. It grows by the grid's size, origin and spacing and the amount (in a new
    parameter range documented in `geom.wgsl`'s layout comment).
  - The `sample_warp` kernel gets a fourth buffer next to `base`, `out` and `coverage`: the EV grid as `f32`.
  - `warp_gain` in `crates/gpu/src/wgsl/geom.wgsl` multiplies by `exp2(amount · bilinear(grid, s))`. `pu(64u)`
    ("gain?") becomes true when a map is active.
- **Parity tests** (`crates/gpu/tests/it/equivalence.rs`, which already has ~35 vignetting cases), CPU against GPU
  within the file's existing tolerance:
  - a synthetic map on a rotated, 5:4-windowed source;
  - the map plus manual vignetting;
  - a `sample_windows` render against the same windows rendered one by one.

### Coordinates: `SourcePlacement`, extended with the sensor window

The map is in sensor coordinates (the active area, unrotated). Chromaforge already places a sensor-space map on the
developed source: Apple ProRAW's gain table. It uses `lcc_raw::gaintable::SourcePlacement`, applied per output pixel
in `crates/pipeline/src/local_tone.rs` (`apply_frame`). The flat field reuses that type instead of a parallel path.

- **What `SourcePlacement` already carries:** `rect`, the developed default crop relative to the active area, and
  `orientation`, the EXIF orientation. `active(u, v)` maps normalised source coordinates to the active area.
- **What it handles without changes:**
  - **User orientation:** the caller undoes it first, as `local_tone::apply_frame` does
    (`frame.orient.inverse()`).
  - **Pixel aspect:** everything is in normalised coordinates, where a row stretch is a uniform scale.
- **What it can't carry, and is therefore added:** the **sensor window**. An optional field
  `window: Option<lcc_raw::SensorWindow>` (serde-default, skipped when `None`) says where the active area sits on the
  full sensor. Without it, `rect` alone can't turn active-area coordinates into a position on a map measured in
  another image area of the same sensor (a 3:2 map on a 5:4 photo). The window's `size` also gives the active area
  in pixels, so nothing else is needed.
- **The chain:** output pixel → `Warp` source position → undo user orientation → `SourcePlacement::active` →
  × active size, + `window.origin` (sensor) → − the map's `window.origin` (`VignettingMeasurement::placement`) →
  `VignettingMeasurement::ev_at`.
- **Phase 1's `SensorFrame`** (`crates/raw/src/vignetting_calibration.rs`) becomes a thin wrapper over `SourcePlacement`, kept for
  the `flat-field` diagnostic, so there is one mapping.
- **Requirements:**
  - Phase 1's orientation round-trip test (8 EXIF × 8 user orientations, a crop offset, pixel aspect 2.0) and the
    window tests run through the `SourcePlacement` path.
  - With `window` unset, ProRAW gain tables behave exactly as before: the existing `local_tone` / gain-table tests
    unchanged, and a byte-identical render of the corpus ProRAW sample (`raw/dng-apple-iphone12pro-proraw.dng`,
    CC0, raw.pixls.us) before and after.
- The decode path (`crates/engine/src/files.rs`, `load_bytes_now`) builds a `SourcePlacement` with the window for
  every raw, the way `local_tone()` builds one for ProRAW today, and keeps it in `SourceInfo`, next to the new
  `lens_from_file` (upstream #3781). The raw is still oriented at decode (`img.into_oriented(raw.orientation)`), so
  the chain above holds.

### Interaction with other vignetting

- **The flat field is independent of "Enable Lens Corrections".** A map applies whenever the photo has one, whether
  profile corrections are on or off. Since `92ddacfe3` ("LENS-OFF", 2026-11-10), new photos start with lens
  corrections off except DNG, RW2 / RWL and RAF built-in corrections, so the flat field alone is the common case.
- **When both are on**, profile and embedded vignetting are off (`lens_vig` = 0 whatever
  `optics.profile_vignetting` says): the map is the measured truth for this lens and aperture, and both would correct
  twice. Profile **distortion and lateral CA stay**, since the map is luminance only.
- **Manual lens vignetting** (`optics.vignetting`, `vig_stops`) stays as a fine-tune on top. Post-crop vignette
  (Effects) is unaffected.
- The Optics section says so: "Profile vignetting is replaced by the flat field" under the profile sliders, which
  are disabled for vignetting only.
- **The camera-look fit:** since upstream #3402 (5bc88f079), the per-file camera look
  (`camera_preview::correct_vignetting`) is fitted on a sensor proxy corrected with the lens-profile vignetting the
  render applies. On `main` that's `LOOK_VERSION` 22, and `7f470722a` (DCP-KEEPS-TONE) passes the same correction
  into its new `tone_without_profile` path.
  - That fit runs at decode, before any develop setting is known.
  - **Proposal for Phase 2:** keep it on the lens profile, so the fitted look doesn't depend on a develop setting
    (switching the flat field on or off must not refit the look or change the decoded source).
  - The cost: a photo whose profile vignetting is off because of a flat field gets a look fitted with a slightly
    different correction than the one rendered.
  - **Maintainer question:** is that acceptable, or should the fit take the flat field when the photo has one?

### Storage

- **In the develop settings.**
  - Fields: `optics.vignetting_calibration: Option<EmbeddedVignettingMap>` (the map) and `optics.vignetting_calibration_amount: f64` (0–120,
    default 100). Both are serde-skipped while there is no map, so every existing photo keeps its JSON and
    `hash64`.
  - The map is stored compactly: grid size, sensor, window, source (body model and serial, lens, focal length,
    aperture, SHA-1), and the gains as log2 in 1/4096-EV `u16`, base64. A 48 × 32 grid is 3 KB of JSON.
  - Copy, paste, sync and presets carry it through the Optics group (a new `OPTICS_ITEMS` entry), and XMP sidecars
    through `lc:settings` (the full settings JSON).
  - **Cost:** the catalog stores each distinct settings value once (`develop_settings` table, content-addressed).
    But every History step and Version of an edited photo is a distinct value, each with its own 3 KB copy. On a
    photo with 50 steps that's ~150 KB. Acceptable: maps are inline (decision 1 below), not referenced by hash.
- **Partial presets** (upstream #2207: "a preset changes only the settings it names"). The rule, explicitly:
  - A preset that names only `optics.vignetting_calibration_amount` sets the amount. On a photo with a map that changes the
    correction. On a photo without one it changes the stored amount and has no visible effect, and no map is
    created.
  - A preset that names `optics.vignetting_calibration` sets the map and the amount together. If it doesn't name the amount,
    the amount becomes the default (100 %), so a map is never applied at a stale amount from an earlier map.
  - A preset that names `optics.vignetting_calibration` as `null` removes the map and leaves the amount alone.
  - Applying a preset never refuses a map: a map that doesn't fit the photo (see *Matching*) is stored and shown as
    not applied.
- **A per-body library of maps to pick from**, like the hot-pixel maps (`crates/engine/src/sensor_maps.rs`: one JSON
  file per body in `<config>/sensor-maps`, keyed by `body_key`, damaged files set aside as `.bad`).
  - New `<config>/flat-fields/<body>/<lens>_<focal>_<aperture>_<sha8>.json`, each a `VignettingMeasurement`, validated on
    read.
  - "Create from this photo" adds the map there and to the photo. "Choose from library" lists the body's maps,
    best match first.
  - The library is a convenience; a photo's own copy in its settings is what renders.
- **Web build:** `config::config_dir()` gives no settings folder there, so there is no per-body library on disk.
  - The settings copy works unchanged (it lives in the catalog and in exported sidecars).
  - "Choose from library" there lists the maps of photos already in the library (the distinct `optics.vignetting_calibration`
    values).
  - "Create from this photo" works if the source raw is available (the web build decodes raws).

### Matching a map to a photo

| Check | Result |
|---|---|
| Active area not the map's, and no window that places it (`VignettingMeasurement::placement` → `Mismatch`) | **refused**: not applied, with a notice in Optics |
| Another sensor (window's sensor size differs, e.g. Z 7 8288 vs Z 8 8280), or camera model differs | **refused** |
| Same model, another body serial | not checked (2026-11-13): any body of the model serves |
| Lens differs (both named) | **refused** |
| Focal length differs (zoom) | within 5 % (at least 2 mm) a note with the difference, further **refused** |
| Aperture differs (2026-11-13) | **refused** beyond a sixth of a stop (every third-stop step stays apart): the fall-off changes with the aperture, so a reference serves only its own aperture. Recorded on one side only: **refused**; on neither (adapted or manual lenses): applies, with "can't be checked" |
| Focus distance | not checked: NEFs don't give a usable one yet (see *Focus distance* below) |

A refused map is kept on the photo (so a later fix or another photo can use it) but has no effect. "Choose from
library" shows only maps that can be placed.

### Focus distance

Fall-off depends on focus distance as well as aperture, so matching could use it. What the 20 Z 7 / Z 8 NEFs
record:

- **EXIF SubjectDistance (`0x9206`):** absent in every file.
- **Nikon maker note `0x0098` LensData:** present in every file, 108 bytes, version `0800` / `0801` (Z 7) and `0802`
  (Z 8).
  - Per ExifTool's Nikon tag-name documentation, LensData from version 0201 on is enciphered with the body serial
    (`0x001d`) and the shutter count (`0x00a7`), both of which these files record.
  - Earlier versions carry a one-byte, log-scale focus distance. Whether 0800–0802 do, and at what resolution, can't
    be checked without deciphering.
- **Deciphering, under the licence policy of 2026-11-10** (AGENTS.md, "Third-party sources"):
  - Both sources are now usable: dcraw's Nikon decipher (its `xlat` tables; all of dcraw is allowed), and ExifTool's
    code and tables (ExifTool is distributed under Perl's terms, Artistic or GPL).
  - Either needs provenance: a `Sources:` line, module docs saying "translated from", and a `NOTICE` credit with the
    licence text.
  - A port from a share-alike source stays Chromaforge-only (never offered upstream), and a binary that ships it is
    under the GPL's terms.
- **Reliability**, even once decoded: Nikon reports the lens's focus position, quantised. It is typically coarse at
  long distances, "infinity" past a few tens of metres, and meaningless or absent with manual and adapted lenses
  (four of the 20 files name no lens at all).
- **Proposed experiment** before relying on it: shoot one lens at known distances (0.5 m, 1 m, 3 m, infinity), several
  frames each. Decipher LensData and check the value is monotone in distance and repeatable at each one.
- Until then matching ignores focus distance, as the lens-profile lookup does (`Shot::distance` is `None`).

### Z 8 in-body stabilisation centre (NEFInfo `0x0008`)

- `crates/raw/src/vendor/nef_lens.rs` already reads the optical centre's offset from the image-area centre (entry
  `0x0008`, "where the in-body stabilization left the sensor"). It's validated for the Z 6II, Z 8 and Z f, and is up
  to ~100 px with Z lenses (−90.1, +18.6 in one file).
- Vignetting is centred on the lens axis, so between a reference and a photo shot with different sensor positions
  the map should move by the difference of their offsets.
- **How big:** a 50 mm f/1.8 corner fall-off of 1.9 EV has a slope of about 4.8 EV per half diagonal at the corner,
  ~0.001 EV per px on a 45 MP sensor. 100 px of shift is about ±0.1 EV at opposite corners, 40 px about ±0.04 EV.
  That matches the ~40 px seen between the two Z 7 85 mm shots, which left 0.011 EV of spread against 0.002 at the
  best placement.
- **Proposal:**
  - Shift by `photo offset − reference offset` where both files record it (Z 6II / Z 8 / Z f), as an extra
    placement offset; otherwise no shift.
  - Validate with two Z 8 references shot handheld (IBIS on) against one on a tripod (IBIS off): the `--check`
    residual with the shift should drop to the tripod pair's.
  - The Z 7 isn't validated for `0x0008` yet (it isn't in `VALIDATED`); check its files first.

### UI

- **Desktop, Lens Corrections section** (`crates/ui-egui/src/panels/edit.rs`, the `section(app, ui, &d, "optics",
  …)` block): a "Flat Field (LCC)" sub-section under the profile controls, shown whether or not Enable Lens
  Corrections is on.
  - With no map: **Create from This Photo**, and **Choose from Library…** (a list: body, lens, focal length,
    aperture, date, match warnings).
  - With a map: its source line (lens, focal length, aperture, body, any warning), the **Amount** slider (control
    `optics.vignettingCalibrationAmount`, 0–120, default 100, step 1), and **Remove**.
  - The compact layout's short-label map (`edit.rs`, the match from control id to short name, e.g.
    `"optics.profileVignetting" => "Vignetting"`) gains `"optics.vignettingCalibrationAmount" => "Amount"`.
  - Create on a clipped or too-dark photo shows the builder's error (`Clipped` names the cell and the percentage).
- **Modern dock:** the Optics group (`crates/ui-egui/src/modern_dock.rs`, `Group::Optics` in `control_ids`) gains the
  `optics.vignettingCalibrationAmount` pill while the photo has a map. The rest is in the Lens Corrections section the group
  already opens.
- **Library menu:** `main` already lists a placeholder, `soon.flatField`, "Flat-Field Correction…"
  (`crates/ui-egui/src/lrc_menus.rs`, the `COMING_SOON` table; issue #983).
  - PR B wires it to Create from This Photo for the selected raw, and opens the Lens Corrections section on it.
  - Its tooltip changes, because the current one ("Correct lens cast and vignetting from a flat-field reference
    frame") promises colour-cast correction, which this feature doesn't do. New text: "Even out lens fall-off from a
    flat-field reference shot."
- **Commands** (everything is a command; UI, CLI, control channel and MCP share them):
  - `vignettingCalibration.create {id?}` (from the photo's raw; adds to the library and applies);
  - `vignettingCalibration.list {body?}`;
  - `vignettingCalibration.apply {map, ids}`;
  - `vignettingCalibration.remove {ids}`;
  - the amount through `develop.set` / `develop.merge`.
- **Copy and sync:** `OPTICS_ITEMS` (`crates/develop/src/presets.rs`) gains `item("optics.vignettingCalibration", "Flat Field
  (LCC)", &["optics.vignetting_calibration", "optics.vignetting_calibration_amount"])`. Syncing across a shoot then carries the map to
  every photo of that lens and aperture, as Capture One users expect.
- **Locale strings:** every new label and message goes into all 17 fork overlays
  (`crates/ui-egui/locales/fork/<code>.json`).
  - #946 holds every language to the coverage tests (the old partial-language list is gone), and #935 scans every
    source file for text shown without `tr`.
  - No em dash (U+2014) in UI copy: `ui_text_has_no_em_dashes` (`62b588ade`) fails on one. Use a comma, colon,
    full stop or parentheses.
  - Labels: "Flat Field (LCC)", "Create from This Photo", "Choose from Library…", "Remove", "Amount".
  - Notices: "Profile vignetting is replaced by the flat field", "Made with another lens", "Made at another aperture
    ({diff} stops)", "Made on another camera body", "This flat field doesn't fit this photo's image area".

### Process version, catalog version

- **No process version:** the fields are new and off by default, so no existing photo renders differently
  (`docs/process-versions.md`; V8 is the latest process on `main`).
- **Catalog `VERSION`** (`crates/catalog/src/journal.rs`, still 16 on `main`): bump to 17. An older build would
  silently drop `optics.vignetting_calibration` when it saves the photo, which loses the user's map, which is the case the rule
  ("a serialized field … an older build would misread or silently drop") is for. Earlier develop fields (`sensor`,
  `hdr`, `skin`) shipped without a bump, but dropping them cost a setting, not measured data.
- **`RENDER_CACHE_VERSION`:** no bump; photos with a map have a new `hash64`.

### Phase 2 steps, each separately testable

1. **Schema:** `EmbeddedVignettingMap` (compact encoding, serde round trip, hostile input), the two Optics fields, the
   control spec, the `OPTICS_ITEMS` entry, the partial-preset rule, the catalog `VERSION` bump. Tests: settings
   without a map keep their JSON and hash; copy/paste carries it; damaged maps load as "no map".
2. **Placement:** `SourcePlacement` gains `window`; `SourceInfo` carries one for every raw from the decode path;
   the field grid is built from it and `placement`. Tests: all 8 × 8 orientations and the 5:4 / 1:1 / 16:9 / DX
   windows give each sensor position its map value, through `SourcePlacement`; ProRAW unchanged; a refused map gives
   no field.
3. **CPU render:** `Warp::gain` and `has_gain`, profile vignetting off while a map is active, independent of Enable
   Lens Corrections. Tests: a synthetic fall-off source corrected flat to < 0.01 EV; amount 0 / 100 / 120; manual
   vignetting still applies; `sample_windows`; no map → byte-identical renders.
4. **GPU render:** the extra buffer and parameters, `warp_gain`; the equivalence tests.
5. **Library and commands:** `<config>/flat-fields`, `vignettingCalibration.create/list/apply/remove`, MCP schema, the activity
   stack for Create (a full decode). Tests: hostile files set aside; a matching table for the warnings; the web
   build without a folder.
6. **UI:** the Optics sub-section, the short label, the Modern pill, the Library menu item and its tooltip, locale
   strings in all 17 overlays, What's New. Verify by running the app and taking screenshots; a SIL step that creates,
   applies and removes a map.
7. **IBIS shift** (Z 6II / Z 8 / Z f): the placement offset from NEFInfo `0x0008`, after the tripod / handheld
   validation above.
8. **Later:** importing Capture One's LCC from the catalog (the paused develop import), and writing the map into
   DNG export as an `OpcodeList2` GainMap.

Every commit passes `cargo xtask ci --fast`, the targeted `cargo test -p` and `cargo xtask sil` before it is pushed
(AGENTS.md, quality gates of 2026-11-10).

### Decisions (2026-11-06, extended 2026-11-10)

1. **Maps are inline** in the develop settings, about 3 KB each, not referenced by hash. Inline travels by itself
   through XMP, copy, sync and presets, and needs no catalog table or import path.
2. **The field in `Warp`'s frame is a 64 × 64 `f32` grid**, on the CPU and in the GPU buffer alike (16 KB; no
   `f16`).
3. **Nearest aperture only in Phase 2.** With references at several apertures, a photo gets the one nearest in
   stops, with the aperture warning. Interpolating between two references (a log-space blend by stops) comes later.
4. **Uneven references warn, they don't refuse.** A reference the centre fit flags (`CentreFit::uneven`), or one
   whose centre is far off, is accepted with a notice in Optics. Clipped and too-dark references are still refused
   by the builder.
5. **The Z 7's stabilisation offset** (NEFInfo `0x0008`) is checked in step 7, together with the shift.
6. **Amount range 0–120 %**, default 100 %.
7. **The camera-look fit stays on the lens profile**, pending the maintainer (see *Interaction with other
   vignetting*).
8. **The flat field doesn't depend on Enable Lens Corrections.** When both are on, profile vignetting is off; profile
   distortion and lateral CA stay.
9. **Placement reuses `SourcePlacement`**, extended with an optional sensor window, instead of a parallel type.
10. **Partial presets** follow the rule under *Storage*: the amount alone never creates or applies a map; a map
    without an amount comes with 100 %.

### PR split

- **PR A: steps 1–4.** Schema, placement, CPU and GPU render, so a map in the settings renders, and renders the same
  on both paths. Reviewable on its own with synthetic maps; nothing visible in the UI yet.
- **PR B: steps 5–6.** Library, commands and UI: creating, choosing and removing maps, the Amount slider, the Library
  menu item, locale strings and What's New. This is what users see.
- **PR C: step 7.** The stabilisation shift, after the tripod / handheld validation. It changes renders of photos
  that already have a map, so it lands on its own with its measurements.
- Step 8 stays later.

### Questions for the maintainer (issue #857)

1. **Catalog `VERSION`:** bump it for `optics.vignetting_calibration`, since an older build would drop the user's map when it
   saves? (`sensor`, `hdr` and `skin` shipped without a bump.)
2. **Camera-look fit:** keep the per-file look fitted with the lens-profile vignetting even when a flat field
   replaces it in the render, so decoding doesn't depend on a develop setting? Or fit with the flat field?
3. **PR split:** A (render path) → B (library, commands, UI) → C (stabilisation shift), as above?
