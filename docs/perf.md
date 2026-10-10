# Performance budgets (P6.1)

Measured 2026-10-11 on the integration branch (`integ/batch2` + this task's fixes). Machine: AMD Ryzen AI Max
(Strix Halo, 32 threads, Radeon 8060S over Vulkan/RADV), Linux, btrfs on NVMe. **Caveat:** the machine was shared with
four other build agents during the runs (load average 30–90), so wall times are noisy (±50 % between runs of the same
binary); CPU-time columns and before/after ratios taken minutes apart are more reliable than the absolute numbers.
Re-measure on an idle machine before quoting.

Test file: `corpus/raw/arw-sony-a7m3-compressed.arw` (6000×4000, 24 MP), from `cargo xtask corpus --download`.

## Summary

| Budget | Target | Measured | Status |
|---|---|---|---|
| Slider update, draft preview, 24 MP | ≤ 16 ms | GPU 4.7–8.7 ms (exposure, highlights, clarity); CPU 34–47 ms; NR drag GPU 27 ms / CPU 297 ms | ✅ GPU, ❌ CPU fallback, ❌ NR |
| Loupe update (1920×1280) | ≤ 60 ms | GPU 8.6 ms; CPU 72 ms | ✅ GPU, ❌ CPU |
| 1:1 tile (1024², `Session::region_job`) | ≤ 100 ms | 530–1140 ms per window, every window | ❌ (upstream candidate U1) |
| Export 24 MP JPEG | ≤ 1 s | GPU render 865 ms + encode 386 ms = 1.25 s; CPU 2.8 s + 0.4 s | ❌ slightly |
| Import 1,000 raws + standard previews | < 2 min | import 16 s + previews 354 s = **370 s** | ❌ (upstream candidate U2) |
| Catalog at 1M photos | interactive | v3: open 20.7 s, filter ≤ 98 ms, 7.7 GB peak RSS; v4: open 13.2 s, filter ≤ 339 ms, 1.9 GB peak, first snapshot 44 s | 🟡 (see below) |
| Map, 50k geotagged photos | frame ≤ 16 ms | frame 6–15 ms (noisy), cached pins 4.0 ms → **0.8 µs** (fixed); re-cluster on zoom step 6–49 ms | ✅ after fix |
| Print / Book page render (300 dpi, raws) | — | 1 photo 0.7–2.2 s, 4 photos 3.5 s → **0.6 s**, 20-photo contact sheet 8.1 s → **2.0–3.1 s** (fixed) | 🟡 |
| Publish 1,000 raws to Hard Drive (2048 px JPEG) | — | 512 s → **157 s** (fixed) | 🟡 |

## How to reproduce

```sh
cargo xtask corpus --download                    # CC0 raws into corpus/
cargo xtask bench                                # render: slider / loupe / export rows (CPU and GPU)
<PREFIX>_PROFILE=1 cargo run --release -p dac-cli -- render corpus/raw/arw-sony-a7m3-compressed.arw -o /tmp/o.jpg --size 2048
ONLY=source N=3 cargo run --release -p dac-engine --example render_bench -- corpus/raw/arw-sony-a7m3-compressed.arw
cargo run --release -p dac-engine --example perf_tile -- $PWD/corpus/raw/arw-sony-a7m3-compressed.arw   # 1:1 tile
cargo xtask bench-catalog --photos 100000,1000000 [--backend v4]
cargo test --release -p dac-ui-egui map_50k -- --ignored --nocapture                                   # Map, 50k pins
```

Import / previews / publish / print: 1,000 unique raws are made as reflink copies of the 24 MP ARW with a few bytes
appended (so their content hashes differ), then driven headless with the CLI:

```sh
<cli> run --library L --import DIR library.buildPreviews size=standard wait=true
<cli> run --library L --script publish.jsonl   # publish.createService (dir, export {jpeg, longEdge 2048}), library.selectAll,
                                               # publish.createCollection {addSelected}, publish.run {service}
<cli> run --library L --script pages.jsonl     # print.render / book.render with the built-in templates at 300 dpi
```

## Render (`cargo xtask bench`)

```
decode full: 4206 ms (cold, first run, machine under load; warm best-of-3 below)
loupe 1920×1280 cold:                  cpu: 956.3 ms wall, 1011.5 ms cpu  |  gpu: 85.2 ms wall, 19.0 ms cpu
loupe draft 1152×768 cold:             cpu: 381.5 ms wall, 362.4 ms cpu  |  gpu: 43.4 ms wall, 14.8 ms cpu
loupe 1920×1280 exposure drag (warm):  cpu: 71.7 ms wall, 472.9 ms cpu  |  gpu: 8.6 ms wall, 4.7 ms cpu
loupe draft highlights drag (warm):    cpu: 34.4 ms wall, 175.4 ms cpu  |  gpu: 4.7 ms wall, 4.3 ms cpu
loupe draft clarity drag (warm):       cpu: 47.2 ms wall, 176.4 ms cpu  |  gpu: 8.7 ms wall, 3.7 ms cpu
loupe draft NR drag (warm):            cpu: 296.8 ms wall, 316.0 ms cpu  |  gpu: 26.7 ms wall, 8.5 ms cpu
loupe 1920×1280 NR drag (warm):        cpu: 358.4 ms wall, 861.2 ms cpu  |  gpu: 70.1 ms wall, 12.2 ms cpu
export render 6000×4000:               cpu: 2839.8 ms wall, 10338.1 ms cpu  |  gpu: 864.9 ms wall, 436.9 ms cpu
export JPEG encode:                    386.4 ms wall, 1177.7 ms cpu
```

Source stages (`ONLY=source`, best of 3):

```
raw decode:                 5.1 ms wall,   136.9 ms cpu
normalize:                  9.0 ms wall,   261.0 ms cpu
demosaic AHD:             193.5 ms wall,  4844.3 ms cpu
load_bytes(2560):          93.0 ms wall,  1855.4 ms cpu
load_bytes(full):         425.3 ms wall, 10448.3 ms cpu
```

## Catalog (`cargo xtask bench-catalog`)

| backend | photos | open | peak RSS | filter (worst) | first snapshot | snapshot after 1000 edits | import | disk |
|---|---|---|---|---|---|---|---|---|
| v3 | 100000 | 2817 ms | 779 MB | 15 ms | 2233 ms | 1623 ms | 21696/s | 444 MB |
| v3 | 1000000 | 20658 ms | 7744 MB | 98 ms | 8481 ms | 16615 ms | 49434/s | 4445 MB |
| v4 | 1000000 | 13235 ms | 1903 MB | 339 ms | 43976 ms | 2832 ms | 58359/s | 2178 MB |

v4 (owned) uses a quarter of v3's memory and checkpoints after edits 6× faster, but its first full snapshot (44 s)
and worst filter (339 ms, vs 98 ms) are follow-ups for the catalog owner; not changed in P6.1.

## Map (50k geotagged photos, headless frame loop, 1400×900)

The pin list was cached per zoom step but **cloned every frame** (all clusters and all 50k photo ids). It is now an
`Arc` (`crates/ui-egui/src/map/mod.rs`):

| zoom | pins | drawn | cluster (zoom step) | cached pins before → after | frame |
|---|---|---|---|---|---|
| 3 | 42 | 42 | 6–24 ms | 17 µs → 0.8 µs | 6–20 ms |
| 5 | 440 | 352 | 6–27 ms | 165 µs → 1.2 µs | 5–19 ms |
| 8 | 22,821 | 385 | 14–64 ms | 1.5 ms → 0.8 µs | 8–21 ms |
| 12 | 50,000 | 4 | 18–73 ms | 4.0 ms → 0.8 µs | 9–24 ms |

(Frame ranges span runs under varying load; the Library grid with the same 50k photos ran at 1.2–3.2 ms per frame.)
Follow-up (owned, `dac-geo`): clustering on a zoom step costs up to ~50 ms at 50k photos (a `BTreeMap` of cells); a
hash map plus a final key sort, or clustering off the UI thread, would remove the hitch.

## Print / Book page render (raws, 300 dpi, built-in templates)

| render | before | after |
|---|---|---|
| print `1 Large, Letter` 150 dpi (1 photo) | 345 ms | 213–226 ms |
| print `1 Large, Letter` 300 dpi (1 photo) | 2178 ms | 733–750 ms |
| print `2×2 Cells, A4` 300 dpi (4 photos) | 3549 ms | 566–672 ms |
| print `4×5 Contact Sheet` 300 dpi (20 photos) | 8054 ms | 2023–3102 ms |
| print `2×2 Cells, A4` → PDF (photos cached) | 545 ms | 408–1863 ms |
| book `Full Bleed Spread` 300 dpi | 802 ms | 865–1844 ms |
| book `Two Up with Caption` 300 dpi | 413 ms | 415–480 ms |

Fix: `creations::run_jobs` rendered a layout's photos one by one; it now renders up to `render_width()` (a quarter of
the cores, 1..=4) at once, results in order. Single-photo rows only vary with machine load.

## Publish 1,000 raws to Hard Drive (JPEG, long edge 2048)

`Plan::execute` (`crates/engine/src/cmd/publish.rs`) rendered and wrote one photo at a time: **512 s**. It now renders
batches of `render_width()` photos concurrently and writes them in order on the worker: **157 s** (3.3×). A render
thread that dies is reported as that photo's failure, and cancel is checked before each batch and each upload.

## Upstream-PR candidates (shared code, not changed here)

**U1 — 1:1 tiles re-decode the whole raw (`crates/engine/src/media.rs`, `crates/pipeline`).** Every
`Session::region_job` window, even the same window twice, loads and develops the full 6000×4000 source
(`<PREFIX>_PROFILE`: develop 226–292 ms, highlights 50–57 ms, colour 138–161 ms, then the GPU window ~60 ms), so a
1:1 tile costs 530–1140 ms against a 100 ms budget, with a `StageCache` attached as the loupe does. The GPU part alone
would fit. Proposal: keep the developed full-resolution source (per photo + develop-source key) across region jobs, as
the whole-frame loupe path keeps its preview-level source, so a pan or a slider change re-runs only the window.
Expected: ~60–100 ms per tile.

**U2 — `library.buildPreviews` builds one photo at a time (`crates/engine/src/cmd/previews.rs`, `run_all`).**
1,000 24 MP raws: 354 s of previews after a 16 s import (budget: 2 min in total). A single 2048 px render takes ~320 ms
(`load_bytes(2560)` alone is 93 ms wall / 1.9 s CPU, so it already uses many cores). Running the same work as N separate
processes gave N=4: 202 s, N=8: 144 s — so concurrency helps but does not reach the budget on its own. Proposal:
(a) render a bounded number of photos at once (as P6.1 did for publish), (b) derive the grid thumbnail from the loupe
preview instead of a second job per photo, (c) for an untouched raw, take the standard preview from the embedded
camera JPEG (16.7 ms) and render the real one lazily. (b)+(c) should bring import + previews under 2 min.

**U3 — CPU fallback misses the slider budget.** Without a GPU, draft-size drags take 34–47 ms (exposure/highlights/
clarity) and NR 297 ms. Proposal: a smaller draft size during drags on the CPU path (e.g. 768 px), and skipping NR in
draft renders while a slider moves.

**U4 — export 24 MP JPEG at 1.25 s** (GPU render 865 ms of which most is the full-size source develop, plus 386 ms
encode). Overlapping the JPEG encode of strip *n* with the render of strip *n+1*, or a faster demosaic for export
previews, would get under 1 s.
