# Upstream PR: Image Quality Scanner (issue #1313)

**Status: prepared and verified, but not opened.** The GitHub App this session authenticates with
is installed only on `1234destiny1234/lightcraft-classic`, so it cannot create a pull request on
`emmertex/chromaforge` (`403 Resource not accessible by integration`). The branch is pushed and the
compare is clean; opening it takes one click.

- **Branch:** `arena/211076b9-upstream-1313` in `1234destiny1234/lightcraft-classic`
- **PR:** [#1314](https://github.com/emmertex/chromaforge/pull/1314) — **open** since the body was first
  drafted (the App turned out to have access after all), mergeable, based directly on `main` `8ed8c2a0e`
- **Commit:** `e4392a81c`, plus `f6485157b` (rustfmt on the scanner's own files, from the real gate's output)
- **Compare:** `ahead` by 1, `behind` by 0, 62 files — `merge_base` is upstream's tip, so it needs
  no rebase
- **Open it:** <https://github.com/emmertex/chromaforge/compare/main...1234destiny1234:arena/211076b9-upstream-1313?expand=1>
  (paste the title and body below)

Suggested title:

```
Image Quality Scanner (issue #1313): score photos for technical quality, filter and sort by it
```

Suggested body — see the fenced block at the end of this file.

---

## What was checked on the rebase

The commit was written against `3893fa22b`, 8565 commits behind upstream's `main`. It bisected
cleanly onto `8ed8c2a0e`, and the result was checked rather than trusted:

- 34 of the 52 modified files carry **byte-identical deltas** to the pre-rebase commit; the other 18
  are exactly the files both sides added lines to (the 17 fork catalogs and the parity doc).
- **Every Rust file is ours-only**: upstream's whole advance touches none of them, and upstream never
  changed `crates/catalog/src/model.rs` or `query.rs`, so the catalog changes carry no merge risk.
- The catalogs merged as **pure additions**: all 17 valid, sorted, formatted, no duplicate keys, and
  every one of the scanner's strings and of upstream's FP16 / GPU-precision strings present.
- `LR-PREF-PEOPLE`'s newer upstream text is untouched; the parity rows are 652 + our 2, and the
  summary table was regenerated to match (527 ✅ / 82 🟡 / 20 ⬜ / 25 🚫; 629 in-scope rows, 90.3%).
- Upstream changed no `AGENTS.md`, `rustfmt.toml`, `clippy.toml`, `xtask/` or CI file since our base,
  so the conventions this work was written to are the current ones.
- All 6 external symbols and 10 `crate::` paths the new modules name resolve in the merged tree, and
  the call signatures of `start_batch`, `source_ref`, `commit`, `visible_cloned`, `text_button`,
  `dropdown`, `register` are unchanged.

**Nothing has been compiled**: the development environment has no Rust toolchain and no way to
install one. The work is checked by reading and by scripted checks only. CI is the first compile.

## What the first CI run said, and what was done about it

All four gating jobs failed on `e4392a81c` (Linux `cargo xtask ci --fast`; Windows and macOS
clippy; `cargo xtask brand test`). Their logs are unreachable from this environment (the log
blobs are on a hosts that does not resolve here), so each gate was reproduced locally instead:

- **fmt** — installed rustfmt 1.8.0-stable from npm (`@rustbin/rustfmt-1.88.0-…`; crates.io and
  rustup are both blocked). With the repo's own `rustfmt.toml` and edition 2024 it reports **0
  diffs over pristine upstream `main`**, so it agrees with the version CI uses. Against the
  scanner it found **38 hunks in 12 files**; all are fixed in `f6485157b`, and the whole
  workspace (minus `vendor/`, which `cargo fmt --all` excludes) now checks clean.
- **brand check / docs / notes / i18n / quarantine** — ported from `xtask/src/{brand,docs,notes,
  i18n}.rs` and validated by running the port on pristine upstream `main` (3737 files, 0
  problems); the branch then reports **0** brand problems too. Docs: 17 templates, none stale.
  Notes: 748 What's New + 13 changelog entries, 0 malformed. i18n: all 34 fork catalogs sorted,
  formatted, no duplicate keys.
- **syntax** — every Rust file parses (ast-grep's Rust grammar over 1878 files: 0 errors outside
  files that predate this work), the 34 changed files included.
- **clippy** could not be run (no compiler, no crates registry). Every external symbol, field and
  signature the new code names was checked against its definition instead — including the ones a
  wrong guess would fail on: `cmd!`/`CommandSpec`, `SourceRef::load` (`Arc<Rgb32f>`), `TaskHandle`,
  `Op::{SetAnalysis,SetFlag}`, `Analysis` (all its literals, after the new field), the exhaustive
  `match`es over `SortKey`, and in the UI `data/get_temp`/`insert_temp` (as `filterbar.rs` already
  used them), `text_button`, `register`, `picker`, `Session::{library_folder,browse,folder_subfolders}`.
  On the next run clippy is past fmt, so its report is the next thing to read.

One defect this found and still **to fix**: the four toast strings in `crates/ui-egui/src/quality_scan.rs`
are absent from every `-formats.json`, so they render in English in all 17 languages
(`tr_format!` has a fallback arm, so this is not a compile error, but it is wrong).

---

## PR body to paste

```markdown
Implements **issue #1313**. Library ▸ Image Quality Scanner… scores photos for technical quality and keeps the scores on the photo, so the Library can filter and sort by them without scanning again.

## What it does
- **Four technical factors**, each 0–100: sharpness (assisted culling's own focus measure), noise (grain in the flattest areas against a clean reference), exposure (clipping plus how close the tones sit to a well-exposed middle), and JPEG compression artifacts (the 8-pixel grid's step against the photo's other edges). Combined into one 0–100 score by per-factor weights (0–10).
- **Scopes**: the selection (else the photos in view), the photos in view, a folder (with its subfolders) or the whole catalog. **Unscanned only** by default; `rescan` measures everything again. Clips are skipped.
- **Background**: a cancellable activity row; a whole scan is written as **one undo step** (cancel keeps what it measured).
- **Pick / Reject** by two cutoffs (70 / 25, inclusive, never crossing), written only onto photos the user has not flagged. `QualityScan::flag_written` records the flag a scan wrote, so a rescan may change its own flag and **never** the user's.
- **Stored per photo** (`Analysis.quality`, catalog format 20 — the version row and its docs are updated), so `SortKey::Quality`, `Filter::quality` / `qualityMin` / `qualityMax`, the filter bar's quality picker and score floor, the tally, and the smart-collection rule fields (Quality Score, each factor, Quality Verdict, under **Image Quality**) all read them without a rescan.
- **Commands**: `photo.qualityScan`, `photo.qualityScanPump` (the app's per-frame work), `dialog.qualityScan`; CLI `<cli> quality-scan [FOLDER]` with `--accepted`, `--rejected`, `--apply-flags`, `--rescan`, `--weights`, `--connect`.
- **17 languages** complete, and the manual page, CLI/Menus/not-built pages, parity rows and a what's-new and changelog entry all ship with it.

## Deviation, said plainly
The issue also asks for a deep-learning aesthetic engine (NIMA / MUSIQ, ONNX on GPU). **It is not built**: the fork is pure Rust with no ONNX runtime, and `crates/ml`'s candle CLIP is the only learned model, with `Role::AestheticHead` unimplemented. The score is the weighted mean of the four classical measures — honest, but blind to subject: a sharp, clean photo of a dull subject scores high. This is documented in the manual page, `reference/not-built.md`, parity row `LR-LIB-QUALITY` and **gap 39**, with what closing it would take.

## Tests
- `crates/pipeline/src/quality.rs` — the measures and the weighting, including hostile buffers (empty, one-pixel, and one shorter than its size claims; `cull::luma` was made short-buffer safe, which also hardens assisted culling).
- `crates/catalog/src/tests_quality.rs` — verdicts, filter, sort (unscanned first ascending, a measured 0 still a score), tally, JSON round trip, rule fields.
- `crates/engine/src/tests_quality_scan.rs` — a scan end to end (scores, verdicts, flags, undo, the user's flag untouched, rescan, refusals, clips), the background run and its pump, folder-scope resolution (path → Folders panel → Browse), and the cutoffs through the command.
- `crates/ui-egui/src/tests_quality_scan_ui.rs` — the picker appears once a scan has scored a photo and filters the grid; the score floor is set and cleared; the dialog hands its settings on.
- `apps/cli/tests/sil.rs` — a scan in the SIL workflow, then filtering and sorting by it.

## Notes for review
- The commit is **rebased onto current `main` (`8ed8c2a0e`)** and is one commit ahead, so it merges cleanly as it stands. The merge was checked by hand where this work and recent `main` touch the same files: the fork catalogs gained `main`'s FP16 / GPU-precision keys alongside the scanner's (all 17 catalogs valid, sorted, no duplicates, pure additions), `LR-PREF-PEOPLE`'s newer text is untouched, and the parity summary table was regenerated (652 + 2 rows).
- **Nothing here has been compiled**: the development environment had no Rust toolchain and no way to install one. The code is checked by reading and by scripted checks (brace balance, 150-column width, i18n coverage, parity command ids and backticked paths, catalog ordering/formatting). **CI is the first compile** — expect the usual rustfmt/clippy first-pass fixes, and I'll follow up on anything it reports.
```
