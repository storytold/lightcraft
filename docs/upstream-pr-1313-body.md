Fixes #1313

One change: the Image Quality Scanner (issue #1313). Nothing else is bundled in.

## Problem

Lightroom Classic can score photos for *technical* quality and let you sort, filter and rule on those
scores. The fork had nothing of the kind: assisted culling measures focus/clipping/eyes/aesthetics for
burst picking, but there is no per-photo quality score a user can keep, filter by, sort by, or use in a
smart collection or a rule. Issue #1313 asks for that, plus a deep-learning aesthetic engine.

## Change

- **pipeline** — `quality::QualityFactors::measure`: four classical measures, each 0–100 where 100 is
  clean — sharpness (assisted culling's own focus measure), noise (grain in the flattest half against a
  clean reference), exposure (clipping share plus distance from a well-exposed middle), and JPEG
  compression (the 8-pixel grid's step against the photo's ordinary edge energy). Combined by
  per-factor weights (0–10) into one score. `cull::luma` is shared and made short-buffer safe, which
  also hardens assisted culling.
- **catalog** — `Analysis.quality: Option<QualityScan>` (catalog format **20**), `Filter::quality` /
  `qualityMin` / `qualityMax`, `SortKey::Quality`, `Tally.quality_{accepted,pending,rejected,unscanned}`,
  and six rule fields (Quality Score, each factor, Quality Verdict) under a new **Image Quality** group.
- **engine** — `photo.qualityScan`: scopes selection / view / folder (with subfolders) / catalog;
  unscanned-only unless `rescan`; clips skipped; a cancellable activity row; a whole run written as
  **one undo step**; `applyFlags` writes Pick / Reject only onto photos the user has not flagged
  (`QualityScan::flag_written` records the flag a scan wrote, so a rescan may change its own and never
  the user's). `photo.qualityScanPump` is the app's per-frame work.
- **UI** — the Library menu entry and dialog (scope, cutoffs, weights, rescan, write flags; remembered),
  the sort menu's **Quality**, the filter bar's quality picker and score floor, filter chips, and a toast
  summarising a finished run. 17 languages.
- **CLI** — `<cli> quality-scan [FOLDER]` with `--accepted`, `--rejected`, `--apply-flags`, `--rescan`,
  `--weights`, `--connect`; every option becomes a `photo.qualityScan` param.
- **docs** — manual page, CLI / Menus / not-built pages, parity rows (`LR-LIB-QUALITY`,
  `LR-FILT-QUALITY`), **gap 39**, what's-new and changelog entries.

**Not in this PR:** the issue's deep-learning aesthetic engine (NIMA / MUSIQ on ONNX). The fork is pure
Rust with no ONNX runtime, and `crates/ml`'s candle CLIP has `Role::AestheticHead` unimplemented. The
score is the weighted mean of the four technical measures — honest, but blind to subject. Say so in the
manual page, `reference/not-built.md`, parity row and gap 39. **This is the one scope deviation, and it
is the thing a maintainer should sign off on** (with the format bump below).

## How it was checked

- [ ] `cargo xtask ci --fast`, `cargo xtask sil` and `cargo test -p` for the touched crates
      (`lcc-catalog`, `lcc-engine`, `lcc-pipeline`, `lcc-ui-egui`, `lcc-cli`) — **not yet green.**
      The branch is being iterated on CI until it is; the first compile is there, not here (no Rust
      toolchain and no crates.io on the machine this was written on).
- [x] A test that fails without the change: `crates/pipeline/src/quality.rs` (the measures and the
      weighting, including empty / one-pixel / short buffers), `crates/catalog/src/tests_quality.rs`
      (verdicts, filter, sort, tally, JSON round trip, rule fields),
      `crates/engine/src/tests_quality_scan.rs` (a scan end to end, the background pump, folder scope,
      the cutoffs), `crates/ui-egui/src/tests_quality_scan_ui.rs` (picker, score floor, dialog),
      `apps/cli/tests/sil.rs` (a scan in the SIL workflow, then filtering and sorting by it).
- [ ] User-visible change: ran the app and looked at the result — **not done** (no display, no build).

What was checked here instead of a whole-workspace build:

- **The pipeline measures were compiled and run.** `quality.rs` and `cull.rs::luma`/`sharpness` were
  compiled with a local `rustc` against a stubbed `Rgb32f`, and their tests run for real. That found
  two defects, now fixed and covered by a new test: sharpness was measured on the raw luminance, so
  grain counted as detail and a photo of pure noise read 100 (it is measured on the 3 × 3 smoothed
  luminance now, which is exactly what the grain smoothing removes), and the noise measure's "flattest
  half" was picked by comparing each block's spread to the median's *value* — blocks tie in runs, so
  every detailed block that tied with the median was taken in as well, and a clean photo of a fine
  pattern scored freedom-from-noise 0. It is picked by rank now.
- **The notes gate and the brand/docs/i18n gates were run for real or ported and validated on
  pristine `main`**, then run on this branch: `lcc-notes` was compiled and its `read_entries` run over
  both new entries; the brand checker scans 3,748 files with 0 problems; the docs templates render to
  0 stale files; all 13 new UI strings are present in all 17 catalogs.
- **`rustfmt` 1.8.0-stable** against the repo's `rustfmt.toml`: 0 diffs.
- Every changed file parsed with a Rust grammar; every external symbol, field and signature the new
  code names checked against its definition.

CI says the same about the library code: the **throw-away brand job passes** on every push, and it
runs `cargo build -p lcc-cli` — which builds `lcc-catalog`, `lcc-pipeline`, `lcc-engine` and
`lcc-ui-egui` inside it. So the production code compiles; what still fails is clippy's
`--workspace --all-targets`, i.e. code only `cfg(test)` (or a bench or an example) sees. That is where
the next error is, and it is being worked through on CI.

## Screenshots

Not available: the change could not be run here. The manual page describes the dialog and the filter
bar controls, and its picture is a `<NeedsImage>` placeholder saying what to capture.

## Merging

- **Overlap:** none found. Searched open pull requests for #1313 and for the culling/AI work; assisted
  culling (`photo.analyze`) is the nearest neighbour and is untouched except that `cull::luma` gained
  short-buffer guards, which changes no measurement on a valid buffer. `cmd/cull.rs` writes
  `Analysis.quality` through when it rewrites an analysis, so a cull cannot throw a scan away.
- **Existing edits and libraries:** unchanged. A catalog written by this build gains an optional
  `analysis.quality` field; an older build ignores it. Nothing renders or develops differently, and no
  process version is involved.
- **Shared numbers taken:** catalog format `VERSION` **20** (`crates/catalog/src/journal.rs`; `main` is
  at 19 today). If another pull request lands with 20 first, this takes 21 when `main` is merged in.
  No `RENDER_CACHE_VERSION`, `LOOK_VERSION` or process version change.
- **Where it came from:** original work. No third-party code or data; the measures are classical and
  documented in the module docs. No Adobe material of any kind.
- **New dependencies, `unsafe`, build scripts, network access, spawned processes, deleted files, CI or
  `xtask` gate changes:** none. No new dependency (`rayon` was already a `lcc-pipeline` dependency), no
  `unsafe`, no build-script change, nothing spawned that the app did not already spawn, no gate changed.

## Checklist

- [x] No new panic path in non-test code: no `unwrap` / `expect` / `panic!`; every read from an image
      buffer or a catalog goes through `get` / `if let` / `let…else` (`cull::luma` and the measures read
      short or hostile buffers without indexing past `data`, and there is a test for it); numbers from a
      command are range-checked before use; `clippy.toml`'s allow-list is what lets tests `unwrap`.
- [x] New text on screen goes through `tr()` / `tr_format!` and has its key in
      `crates/ui-egui/locales/fork/*.json` — all 17 languages, including the format strings (each is a
      valid `format!` string with exactly the call site's placeholders). No new shortcut.
- [x] Parity rows and gap 39 updated in the same commit; the summary table regenerated
      (654 rows in all with the summary; 527 ✅ / 82 🟡 / 20 ⬜ / 25 🚫 — 90.3% of 629 in-scope).
- [x] Manual updated in the same commit: `library/quality-scanner.mdx` (new), plus `browse.mdx`,
      `filters.mdx`, `library-tools.mdx`, `automate/cli.md`, `reference/menus.md`,
      `reference/not-built.md`. Its picture is `<NeedsImage>` — it cannot be captured here (no build).
- [x] New code has a caller: every command is in `command_specs()` and reachable from the Library menu,
      the CLI and MCP; the filter bar and sort menu reach the catalog fields; the rule fields are in
      `FIELDS`.
- [x] No new image, icon, font or other asset.
- [ ] Ready to merge — **no**, and not only for CI: this needs a maintainer's design sign-off on #1313
      before it is green-lit, per the triage note.

🤖 Generated with [Arena Agent](https://arena.ai)
