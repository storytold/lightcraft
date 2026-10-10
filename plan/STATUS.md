# Status

*Last updated 2026-10-10.*

## Where we stand

- Forked from upstream; Phase 0 (brand configuration, licence, `plan/`, tracker, Immich test server) in progress.
- Tracker (`docs/parity.md`, `cargo xtask parity`): 581 counted rows, 394 ✅ · 51 🟡 · 109 ⬜ · 27 🚫. Weighted
  completion 75.7% of 554 in-scope rows (P0 98.2% of 200 · P1 91.4% of 162 · P2 38.7% of 191). The drop from 80.0%
  is the Classic reopenings and the rows added from PLAN.md §2, not a regression.
- The 10 `IMM-*` rows are in the document but **not counted yet**: `xtask/src/parity.rs` accepts only the `LR`,
  `LRC`, `MENU`, `KEY` and `KEYC` prefixes (add `"IMM"` to `is_row_id`).

## Phase 1 (first pass merged 2026-10-10)

All tracks 1.1–1.8 are merged on main with `cargo xtask ci` green; exit-gate detail in `PLAN_phase_1.md`. Open gaps:
- 1.1: move Session/registry/commands out of the `engine` facade (needs a Session context trait).
- 1.2/1.3: secondary-window filmstrip + filter; panel drag-reorder; image identity plate; KEYC-COMPARE/DEVELOP keys.
- 1.4: Folders lists only folders with photos, no Windows free space; grid View Options panel.
- 1.5: lazy photo loading / grid paging (1M photos 2.3 GB); migration progress; unfiltered sort at 500k borderline.
- 1.6: closed (Compare/Reference tiled, drags at ≥1:1 back to one stage-cached window, corpus 1:1 test green on 87
  raws); the second window's loupe is fit-only, so it has no tiles.
- 1.7/1.8: native macOS/Windows keychains (FFI helper crate). (Library menu with Library ▸ Immich and the
  Immich/key-storage translations: done.)

## Phase 0 checklist

| Item | State |
|---|---|
| 0.1 Fork, `upstream` remote, `fork-base` tag, `docs/upstream-merge.md` | in progress (other agents) |
| 0.2 `brand.toml` + `brand` crate + brand check | in progress (other agents) |
| 0.3 Remove upstream branding | in progress (other agents) |
| 0.4 MIT licence, `cargo deny` | in progress (other agents) |
| 0.5 `plan/` written and committed (only `plan/observations/` ignored) | done |
| 0.5 Tracker: Classic rows reopened, PLAN.md §2 rows added, "Top gaps" de-duplicated, IMM section | done (IMM counting needs the parity.rs change above) |
| 0.5 `plan/immich.md` | done (checked against Immich v3.3.1) |
| 0.5 `xtask/immich/compose.yml` + `cargo xtask immich up|down|seed` | done; `seed` verified against a live v3.3.1 server. Wiring into `xtask/src/main.rs` pending |
| 0.6 Shared infrastructure | see below |

## Shared infrastructure (Phase 0.6)

### craft-fonts (`CRAFT_FONTS_DIR`)

- An **optional build input** for CJK and other wide-coverage fonts, from a checkout of the craft-fonts repository.
- `crates/engine-export/build.rs` reads `CRAFT_FONTS_DIR` (re-run when it changes). A relative path is resolved against the
  workspace root as well as the crate directory. If the directory is unreadable the build warns and continues
  without the fonts; with `CRAFT_FONTS_REQUIRED` set, a missing directory is a build error (use that in release
  builds).
- With the fonts, export watermarks fall back to the bundled Japanese faces (Mincho on desktop, a Gothic face on the
  web build); without them, `watermarks_work_without_craft_fonts` keeps watermarks working with the default face.
- The checkout lives outside the repository or in the git-ignored `/craft-fonts`; it is never committed. Typical use:
  `CRAFT_FONTS_DIR=../craft-fonts cargo xtask run`.
- Phase 3's `text` crate takes over font loading and will read the same variable.

### Raw corpus (`cargo xtask corpus`)

- `cargo xtask corpus` prints where test corpora live: `corpus/raw/` (CC0 raw.pixls.us samples) and
  `corpus/images/` (optional CC0 / public-domain JPEG, PNG, TIFF, HEIC). The folder is git-ignored.
- `cargo xtask corpus --download` fetches PngSuite and the pinned CC0 raw samples with `curl` and checks each file's
  SHA-256; a mismatching file is deleted and reported so it is fetched again next time.
- Tests that need a corpus skip cleanly when it is absent, so CI without the corpus stays green; corpus-backed
  checks (for example `crates/raw/tests/cr3_corpus.rs`) run where it is present.

### Never-crash standard

- Confirmed: `AGENTS.md` carries the standard in its "Never crash (outranks feature work)" section: non-test code
  never panics (no `unwrap`, `expect`, `panic!`, `unreachable!`, `todo!`, `unimplemented!`), malformed input becomes
  an error the user or agent can act on, and a panic hook plus `catch_unwind` around command dispatch and
  import/export is the last-resort guard (`panic = "unwind"` on native). It links to the external full standard;
  that repository is not needed at build time.
- `AGENTS.md` still names the upstream product in that section; the brand clean-up (Phase 0.2/0.3) owns that wording.

## Immich test server

- `cargo xtask immich up` starts Immich v3.3.1 on `http://127.0.0.1:2284` (docker volumes `xtask-immich_*`),
  `seed` creates `admin@example.invalid`, an API key in `target/immich/api-key` and an album of 8 generated PNGs,
  `down [--volumes]` stops it (and wipes data). Re-running `seed` is safe: uploads come back as duplicates.

## Metrics

### P1.6 slider drags at ≥ 1:1 (`profile_slider_drag`)

`cargo test -p dac-ui-egui --release profile_slider_drag -- --ignored --nocapture` (2026-10-10, Ryzen AI Max+ PRO 395
/ Radeon 8060S, Linux, GPU), demo 24 MP photo, tick → shown median (p90), ms:

| canvas, zoom | old single window (pre-tiles) | tiles drafted per tile | one drag window per pane (now) |
|---|---|---|---|
| 1400 × 900, fit | 3.7 (5.2) | 9.1 (12.1) | 2.6 (5.0) |
| 1400 × 900, 100 % | 9.3 (11.2) | 128.2 (236.1) | 9.5 (11.4) |
| 1400 × 900, 400 % | 6.4 (7.2) | 119.1 (134.1) | 5.9 (7.6) |
| 2800 × 1700, 100 % | 19.6 (21.1) | 254.6 (315.1) | 19.2 (21.5) |
| 2800 × 1700, 400 % | 8.9 (10.0) | 119.6 (123.0) | 8.8 (11.3) |

During a drag the visible window (2304 × 2048 at 1:1 on the small canvas) is drafted as one `Slot::Window(pane)` job
with its own stage cache (4.4 ms) and drawn over the tiles; the final look's tiles follow when the drag ends. On a
~2.5 MP view a tick is within the 16 ms budget; the 2800 × 1700 canvas at 1:1 (≈ 10.8 MP window) is about 19 ms.

### P1.6 corpus raws: 1:1 equals export

`DAC_CORPUS_RAW_COUNT=87 cargo test --release -p dac-engine --lib one_to_one_pixels_equal_export_pixels_on_corpus_raws`:
all 87 raw.pixls.us samples (`cargo xtask corpus --download`): three 1:1 windows and tiles per photo equal the
full-size export within 2 levels (145 s). The test had silently decoded nothing before (no filesystem loader).

### Bench 2026-10-10 (`cargo xtask bench`, a7m3 24 MP, CPU | GPU wall)

First run in this checkout (no history to compare; the pipeline is unchanged by P1.6's UI work): loupe 1920 × 1280
cold 56.3 | 34.4 ms, exposure drag warm 13.8 | 2.7 ms, draft clarity drag 5.5 | 2.0 ms, export render 6000 × 4000
418 | 347 ms, JPEG encode 64.7 ms; GPU vs CPU max 1 LSB.

### P1.6 panning at 1:1 (45 MP)

`cargo test --release -p dac-engine --lib pan_at_one_to_one -- --ignored --nocapture` (2026-10-10, Ryzen AI Max+
PRO 395 / Radeon 8060S, Linux): an 8256 × 5504 procedural photo with exposure and clarity, a 2560 × 1440 canvas at
1:1, panned 40 px a frame for 180 frames, with the tile grid: 1024 px tiles rendered with a 512 px margin and
cropped back, the view and a ring of one tile around it asked for. The loupe draws the tiles it has every frame
(textures moved by the GPU, so the frame rate does not depend on the photo); the pan renders only the 20 tiles it
moves onto. The first view (25 tiles incl. the ring) takes 0.95 s on the CPU with one worker (the app runs up to 6;
a built 1:1 preview shows at once in the meantime).

| path | mean tile render | worst | sharp panning up to (one worker) |
|------|------------------|-------|----------------------------------|
| CPU  | 29.0 ms | 43.1 ms | ≈ 308 fps |
| GPU  | 77.8 ms | 92.5 ms | ≈ 115 fps |

Both stay well above the 30 fps target. Tiles render without stage caches, so per tile the GPU's fixed costs dominate
and it is slower than the CPU here (the earlier one-window design: CPU 95.5 ms / GPU 39.0 ms per 3584 × 2560
window, ≈ 270 / 660 fps).
