# Performance budgets (P6.1)

Measured 2026-10-11 on the integration branch (`integ/batch2` + this task's fixes). Machine: AMD Ryzen AI Max
(Strix Halo, 32 threads, Radeon 8060S over Vulkan/RADV), Linux, btrfs on NVMe. **Caveat:** the machine was shared with
four other build agents during the runs (load average 30–90), so wall times are noisy (±50 % between runs of the same
binary); CPU-time columns and before/after ratios taken minutes apart are more reliable than the absolute numbers.
The idle-machine column in the summary (and the "Idle, 2026-10-11" blocks below) supersedes them.

Test file: `corpus/raw/arw-sony-a7m3-compressed.arw` (6000×4000, 24 MP), from `cargo xtask corpus --download`.

## Summary

The "Idle machine, 2026-10-11" column is the re-measure on the same machine with no other agent running (load average
< 1 at the start), same binaries' code (`main` at 488ef9ac, release build), each probe run 3× and the **median** shown.
Status: ✅ met · ❌ missed in owned code · ❌ᵘ missed in shared (upstream) code · — no budget.

| Budget | Target | Measured (loaded machine) | Idle machine, 2026-10-11 (median of 3) | Status (idle) |
|---|---|---|---|---|
| Slider update, draft preview, 24 MP | ≤ 16 ms | GPU 4.7–8.7 ms (exposure, highlights, clarity); CPU 34–47 ms; NR drag GPU 27 ms / CPU 297 ms | GPU 1.8–1.9 ms (highlights, clarity); CPU 6.3–7.1 ms; NR drag GPU 9.2 ms / CPU 22.8 ms | ✅ GPU, ✅ CPU, 🟡 CPU NR drag 15.7–16.0 ms after U3 fix (load ~25) |
| Loupe update (1920×1280) | ≤ 60 ms | GPU 8.6 ms; CPU 72 ms | exposure drag GPU 2.5 ms / CPU 16.4 ms; NR drag GPU 28.5 / CPU 54.7 ms; cold GPU 35 / CPU 69 ms | ✅ |
| 1:1 tile (1024², `Session::region_job`) | ≤ 100 ms | 530–1140 ms per window, every window | first 528 ms, same window 496 ms, pan 493 ms, after exposure 486 ms, after NR 497 ms (probe artefact, see U1); with the source kept as the app keeps it: first 372–468 ms (decode), same window 1.7 / 5.0 ms, pan 5.9 / 15.1 ms, after exposure 1.3 / 6.0 ms, after NR 12.9 / 18.5 ms (GPU / CPU) | ✅ (first tile pays the decode) |
| Export 24 MP JPEG | ≤ 1 s | GPU render 865 ms + encode 386 ms = 1.25 s; CPU 2.8 s + 0.4 s | GPU render 361 ms + encode 68 ms = **0.43 s**; CPU 477 + 68 ms = 0.55 s; whole `<cli> render` process (decode, GPU init, write) 846 ms | ✅ |
| Import 1,000 raws + standard previews | < 2 min | import 16 s + previews 354 s = **370 s** | import ~4 s + previews 172 s = **176 s**; after U2 fix (load ~25, same session A/B): previews 239 s → **75 s**, import + previews **78.5 s** | ✅ after U2 fix |
| Catalog at 1M photos | interactive | v3: open 20.7 s, filter ≤ 98 ms, 7.7 GB peak RSS; v4: open 13.2 s, filter ≤ 339 ms, 1.9 GB peak, first snapshot 44 s | v3: open 14.2 s, filter ≤ 75 ms, 7.7 GB peak; v4: open 1.4 s, filter ≤ 48 ms, 1.9 GB peak, first snapshot 10.3 s (9.8–19.5), snapshot after edits 338 ms | ✅ v4 (first full snapshot 10 s runs off the UI path); v3 open 14 s ❌ᵘ |
| Map, 50k geotagged photos | frame ≤ 16 ms | frame 6–15 ms (noisy), cached pins 4.0 ms → **0.8 µs** (fixed); re-cluster on zoom step 6–49 ms | frame 3.3–5.9 ms, cached pins 0.7 µs, re-cluster on zoom step 4.4–11.0 ms; Library grid 0.97 ms | ✅ |
| Print / Book page render (300 dpi, raws) | — | 1 photo 0.7–2.2 s, 4 photos 3.5 s → **0.6 s**, 20-photo contact sheet 8.1 s → **2.0–3.1 s** (fixed) | 1 photo 0.65 s (150 dpi 0.19 s), 4 photos 0.52 s, 20-photo contact sheet 1.81 s | — |
| Publish 1,000 raws to Hard Drive (2048 px JPEG) | — | 512 s → **157 s** (fixed) | **102.6 s** | — |

No owned-code budget is missed on the idle machine, so no product code changed in the re-measure. Every remaining miss
is in shared code and already has an upstream-PR candidate below (U1–U3). The loaded-machine numbers overstated the
CPU slider path and export (both now met); the import miss and the 1:1 tile miss are real.

## How to reproduce

```sh
cargo xtask corpus --download                    # CC0 raws into corpus/
cargo xtask bench                                # all probes + budget table (see Budget gate)
<PREFIX>_PROFILE=1 cargo run --release -p lcc-cli -- render corpus/raw/arw-sony-a7m3-compressed.arw -o /tmp/o.jpg --size 2048
ONLY=source N=3 cargo run --release -p lcc-engine --example render_bench -- corpus/raw/arw-sony-a7m3-compressed.arw
cargo run --release -p lcc-engine --example perf_tile -- $PWD/corpus/raw/arw-sony-a7m3-compressed.arw   # 1:1 tile
cargo xtask bench-catalog --photos 100000,1000000 [--backend v4]
cargo xtask bench-keywords [--listed 2000] [--photos 20000]                                             # Keywording panel
cargo test --release -p lcc-ui-egui map_50k -- --ignored --nocapture                                   # Map, 50k pins
```

Import / previews / publish / print: 1,000 unique raws are made as reflink copies of the 24 MP ARW with a few bytes
appended (so their content hashes differ), then driven headless with the CLI:

```sh
<cli> run --library L --import DIR library.buildPreviews size=standard wait=true
<cli> run --library L --script publish.jsonl   # publish.createService (dir, export {jpeg, longEdge 2048}), library.selectAll,
                                               # publish.createCollection {addSelected}, publish.run {service}
<cli> run --library L --script pages.jsonl     # print.render / book.render with the built-in templates at 300 dpi
```

## Budget gate (`cargo xtask bench --strict`)

`cargo xtask bench` runs every probe in `PROBES` (`xtask/src/bench.rs`) with `<PREFIX>_PROBES=1`, release build:

| Probe | What runs |
|---|---|
| `render` | `render_bench` example (24 MP raw, or a procedural 24 MP source): slider drags (draft), loupe drags @ 1920×1280, export |
| `grid` | `tests_grid::scale_*` in `lcc-ui-egui`: Library grid at 100k photos, filter bar at 68k |
| `tiff` | `tiff_zip::bench` in `lcc-codecs`: 24 MP 16-bit TIFF, none / ZIP / LZW |
| `keywords` | `probe_keyword_tree_cost` in `lcc-catalog`: keyword tree at 58k photos, 2,000 keywords |

It prints each budget in `BUDGETS` (draft slider ≤ 16 ms, loupe ≤ 60 ms at 2.5 MP, grid frame ≤ 16 ms, TIFF ZIP ≤ 3 s,
kept keyword tree ≤ 16 ms, …) with the measured wall time, then compares with this machine's baseline
(`target/bench/baseline.json`, or the previous run on this host when there is none) using CPU time where a probe
reports it and wall time otherwise. `--strict` fails on an enforced budget breach or a regression over 20 %
(`--threshold PCT`). The same threshold is the budgets' noise allowance: an enforced budget fails only above
ceiling × (1 + threshold), so one noisy reading just over a ceiling on a shared machine is reported as "over
(within noise allowance)" and does not fail. Budgets marked advisory are known misses and never fail; they become enforced once met. A
baseline from another host is never used: numbers compare only on one machine.

First run (Windows, shared machine under heavy concurrent build load, 2026-11-24, 24 MP ARW; wall ms):

| Budget | Ceiling | Measured | Gate |
|---|---|---|---|
| draft highlights / clarity drag, GPU | 16 | 3.9 / 4.1 | enforced |
| draft highlights / clarity drag, CPU | 16 | 78 / 51 | advisory |
| draft NR drag, GPU / CPU (4.5) | 16 | 11.3 (was 39.9, same-machine A/B, loaded) / 519 | advisory |
| loupe 1920×1280 exposure drag, GPU / CPU | 60 | 13 / 127 | GPU enforced, CPU advisory |
| loupe 1920×1280 NR drag, GPU / CPU | 60 | 255 / 573 | advisory |
| grid 100k unchanged frame | 16 | 13.2 | enforced |
| grid wheel-scroll frame, filter bar hidden (4.8) | 16 | 17.5 | advisory |
| grid wheel-scroll frame median, 100k (`wheel` probe, 4.8; 2026-11-24) | 16 | 6.2 | enforced |
| grid 100k jump to 90k / filter change | 1500 / 1000 | 971 / 232 | enforced (guessed ceilings with headroom) |
| TIFF ZIP 24 MP 16-bit | 3000 | 960 | enforced (guessed ceiling) |
| keyword tree kept / rebuilt | 16 / 400 | 0.6 / 198 | enforced (rebuilt: guessed ceiling) |

The CPU rows ran at several times the 4.5 figures (CPU NR drag 33 ms idle-ish vs 519 ms here) because of the load;
re-measure on a quiet machine before turning them into enforced budgets.

```sh
cargo xtask bench --save-baseline            # on main: record this machine's baseline
cargo xtask bench --strict                   # on the branch: budgets + regressions vs that baseline
cargo xtask bench --only grid,keywords       # a subset (with --save-baseline it updates only those rows)
```

Adding a probe or a budget is one line: `probe("name", &[cargo args…])` in `PROBES` (it must print
`label: N ms wall[, N ms cpu]` lines on stdout or stderr), and `budget("name:label/cpu/wall", ms, "why")` (or
`advisory(…)`) in `BUDGETS`; a unit test checks every budget names a probe.

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

Idle, 2026-10-11 (median of 3 `cargo xtask bench` runs; CPU wall, CPU time | GPU wall, GPU CPU time):

```
loupe 1920×1280 cold:                  cpu: 68.5 ms wall, 1469.6 ms cpu  |  gpu: 35.4 ms wall, 10.7 ms cpu
loupe draft 1152×768 cold:             cpu: 28.4 ms wall, 466.2 ms cpu   |  gpu: 13.4 ms wall, 6.7 ms cpu
loupe 1920×1280 exposure drag (warm):  cpu: 16.4 ms wall, 433.3 ms cpu   |  gpu: 2.5 ms wall, 1.6 ms cpu
loupe draft highlights drag (warm):    cpu: 6.3 ms wall, 138.6 ms cpu    |  gpu: 1.9 ms wall, 1.4 ms cpu
loupe draft clarity drag (warm):       cpu: 7.1 ms wall, 165.9 ms cpu    |  gpu: 1.8 ms wall, 1.4 ms cpu
loupe draft NR drag (warm):            cpu: 22.8 ms wall, 414.7 ms cpu   |  gpu: 9.2 ms wall, 3.2 ms cpu
loupe 1920×1280 NR drag (warm):        cpu: 54.7 ms wall, 1164.4 ms cpu  |  gpu: 28.5 ms wall, 4.9 ms cpu
finish 1920×1280 typical:              cpu: 17.2 ms wall, 476.7 ms cpu   |  gpu: 2.5 ms wall, 1.6 ms cpu
export render 6000×4000:               cpu: 476.8 ms wall, 11506.1 ms cpu |  gpu: 361.1 ms wall, 152.9 ms cpu
export JPEG encode:                    68.3 ms wall
```

`<PREFIX>_PROFILE=1 <cli> render` (full size): develop 226 ms, highlights 43 ms, colour 135 ms, fit 18 ms; GPU sample
35 ms, wb/nr 89 ms, planes 38 ms, finish + readback 23 ms. Whole process: 846 ms full size, 216 ms at 2048 px.

Source stages, idle (median of 3): raw decode 5.0 ms, normalize 8.0 ms, demosaic AHD 190 ms (5.0 s CPU),
`load_bytes(2560)` 95 ms (1.9 s CPU), `load_bytes(full)` 436 ms (10.7 s CPU) — the same as under load, since these
were already best-of-3.

Source stages (`ONLY=source`, best of 3, loaded machine):

```
raw decode:                 5.1 ms wall,   136.9 ms cpu
normalize:                  9.0 ms wall,   261.0 ms cpu
demosaic AHD:             193.5 ms wall,  4844.3 ms cpu
load_bytes(2560):          93.0 ms wall,  1855.4 ms cpu
load_bytes(full):         425.3 ms wall, 10448.3 ms cpu
```

## Start-up (`cargo xtask bench`, `startup_bench`; plan item 4.1, #979)

`cargo run --release -p lcc-ui-egui --example startup_bench -- [DIR] [PHOTOS]` makes a synthetic library (default
100 000 raws, a third edited) and starts the app on it headless: `startup first frame`, `startup library ready`
(budget ≤ 3 s, an advisory row in `BUDGETS` in `xtask/src/bench.rs` until a quiet machine shows it met), `startup slowest frame`, and `startup prep tick` (the
slowest of the background preparation's first 30 ticks after the library opened). `cargo xtask bench` runs it as the `startup`
probe (`--only startup` alone; other sizes by running the example directly).

2026-11-24, Windows 11, machine shared with other builds (wall times vary ±100 % between runs of one binary), three
interleaved before/after runs at 100 000 photos: preparation tick 12–19 ms → **4 ms**. Before, each catalog change
compared every photo's settings in one tick and copied the whole view (twice) to sign it, and a build started from a
photo not in the view asked for previews of every photo in it — about 4 s a tick at 329 000 photos in the report on
#979. Now the catalog's change log names the photos to compare, a full walk (library opened, an op over more than its
own photos) goes 16 384 photos a tick, the view is signed in place and a build covers only the stretch around the
photo. The lens database is read on a worker after a library opens (an activity-stack row) instead of by the first
photo that needs it. Library ready 1.0–3.9 s before, 1.0–8.2 s after, same noise: the remaining long frame is the
session open on the interface thread, not measured apart here.

## CPU-only stages against the loupe budget (plan 4.6, 2026-11-24)

Two stages have no WGSL kernel: soft proofs against an output-device profile (a 3D table; the GPU renderer declines
the whole render, `lcc_gpu::handles`) and Even Out / Fill spots (the GPU renderer runs the scene-linear stage on the
CPU for such a photo, `lin_needs_cpu`). Matrix soft proofs and Clone / Heal / sourced Remove spots already run on the
GPU. Probe: `cargo test -p lcc-pipeline --release cpu_only_stage_cost -- --ignored --nocapture` (1920×1280, median of 7,
synthetic source). The machine was heavily loaded by other agents (16 cores; a plain CPU loupe render took 74–681 ms
across runs against 16 ms idle in the summary), so only the quietest run is quoted, and it is an upper bound:

| Case (1920×1280) | Quietest run |
|---|---|
| CPU render, plain | 74 ms |
| + device proof (33³ table) | +6 ms; with both gamut warnings +12 ms |
| + 1 Even Out spot (size 0.05) | +12 ms in the render; spot stage alone 34 ms |
| 1 Even Out spot, size 0.20 (box ≈ the whole frame) | spot stage 223 ms |
| 10 Even Out spots, size 0.05 | spot stage 325 ms |
| 1 Fill spot, size 0.05 / 0.20 | 84 / 334 ms on first solve (cached afterwards: `fills()`) |

Conclusion: a device proof adds a few per cent to a CPU render, which fits the 60 ms loupe budget on an idle machine
(CPU loupe drag 16 ms in the summary); a WGSL port of the table lookup isn't needed for the budget. Even Out with one
small spot fits; large or many Even Out spots miss it (the three box blurs per spot over a box that grows with the
radius), and Fill pays its PatchMatch solve once per stroke. A port of Even Out's blurs to the GPU blur kernels is the
next step if the idle re-measure confirms the miss. Tried and dropped: taller `box_v` bands for wide radii (fewer
primes, less parallelism): no difference distinguishable from the load noise.

**Even Out on the GPU (2026-11-24).** Even Out spots now run on the device: `spot_prep` writes the target and the
masked surroundings, the three blurs go through the GPU box-blur `gaussian` (the CPU's three-box blur, pass for
pass), and `spot_apply` computes `flat_gain`'s clamped gain. `lin_needs_cpu` no longer sends such a photo's
scene-linear stage to the CPU (Fill / Generate / sourceless Remove still do). GPU vs CPU: `spots_match` (four Even
Out cases: small, large, brushed and cut by the edge, over a heal) within 1 LSB, mean 0.0001. Probe:
`cargo test -p lcc-gpu --release --test it even_out_render_time -- --ignored --nocapture` (whole GPU render,
1920×1280, 7 runs). Same binary-pair before/after on the same machine, but it was saturated by ten other build
lanes (the plain render ranged 1.9–5.6 s, against tens of ms idle), so only the fastest run is meaningful:

| Case (1920×1280, GPU render) | Before (fastest) | After (fastest) |
|---|---|---|
| no spots | 1947 ms | 4017 ms (load; slower than the spot cases, so absolute numbers here are noise) |
| 1 Even Out spot, size 0.05 | 2361 ms | 937 ms |
| 1 Even Out spot, size 0.20 | 2963 ms | 1507 ms |
| 10 Even Out spots, size 0.05 | 3757 ms | 1982 ms |

Before, every Even Out case paid a read-back, the CPU stage and an upload on top of the plain render (+0.4–1.8 s
here); after, no case is slower than the plain render's noise. An idle re-measure should confirm the absolute spot
cost against the 60 ms loupe budget.

## Catalog (`cargo xtask bench-catalog`)

| backend | photos | open | peak RSS | filter (worst) | first snapshot | snapshot after 1000 edits | import | disk |
|---|---|---|---|---|---|---|---|---|
| v3 | 100000 | 2817 ms | 779 MB | 15 ms | 2233 ms | 1623 ms | 21696/s | 444 MB |
| v3 | 1000000 | 20658 ms | 7744 MB | 98 ms | 8481 ms | 16615 ms | 49434/s | 4445 MB |
| v4 | 1000000 | 13235 ms | 1903 MB | 339 ms | 43976 ms | 2832 ms | 58359/s | 2178 MB |

Idle, 2026-10-11 (median of 3):

| backend | photos | open | peak RSS | filter (worst) | first snapshot | snapshot after 1000 edits | import | disk |
|---|---|---|---|---|---|---|---|---|
| v3 | 100000 | 1454 ms | 778 MB | 7 ms | 568 ms | 580 ms | 128692/s | 444 MB |
| v3 | 1000000 | 14181 ms | 7747 MB | 75 ms | 5754 ms | 5760 ms | 130304/s | 4445 MB |
| v4 | 1000000 | 1388 ms | 1926 MB | 48 ms | 10306 ms | 338 ms | 134064/s | 2178 MB |

v4 (owned) uses a quarter of v3's memory and checkpoints after edits 6× faster, but its first full snapshot (44 s)
and worst filter (339 ms, vs 98 ms) are follow-ups for the catalog owner; not changed in P6.1.

## Keywords (`cargo xtask bench-keywords`)

A library with a big nested keyword list (2,000 listed, three levels; 20,000 photos with three each), what the
Keywording panel asks of the catalog. Suggestions and the Add keyword box's offers scanned the keyword list against
every used keyword (lower-casing both for each pair) and every photo, on each selection change and keystroke, which
lagged the grid and typing once a Lightroom Classic keyword file was imported. They now use a keyword index the
catalog builds on first use and drops when an op can change a keyword (`crates/catalog/src/keywords.rs`,
`KeywordIndex`). 2026-10-20, release build:

| | before | after |
|---|---|---|
| selection change (Suggestions) | 116 ms | 0.07 ms |
| keystroke (offers + new name) | 198 ms | 0.06 ms |
| first ask after a keyword edit (rebuilds the index) | 214 ms | 23 ms |

## Map (50k geotagged photos, headless frame loop, 1400×900)

The pin list was cached per zoom step but **cloned every frame** (all clusters and all 50k photo ids). It is now an
`Arc` (`crates/ui-egui/src/map/mod.rs`):

| zoom | pins | drawn | cluster (zoom step) | cached pins before → after | frame |
|---|---|---|---|---|---|
| 3 | 42 | 42 | 6–24 ms | 17 µs → 0.8 µs | 6–20 ms |
| 5 | 440 | 352 | 6–27 ms | 165 µs → 1.2 µs | 5–19 ms |
| 8 | 22,821 | 385 | 14–64 ms | 1.5 ms → 0.8 µs | 8–21 ms |
| 12 | 50,000 | 4 | 18–73 ms | 4.0 ms → 0.8 µs | 9–24 ms |

Idle, 2026-10-11 (median of 3): zoom 3 cluster 4.4 ms / frame 3.3 ms; zoom 5 4.8 / 3.6 ms; zoom 8 8.9 / 4.6 ms;
zoom 12 11.0 / 5.9 ms; cached pins 0.7 µs at every zoom; Library grid 0.97 ms per frame.

(Frame ranges span runs under varying load; the Library grid with the same 50k photos ran at 1.2–3.2 ms per frame.)
Follow-up (owned, `lcc-geo`): clustering on a zoom step costs up to ~50 ms at 50k photos (a `BTreeMap` of cells); a
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

Idle, 2026-10-11 (median of 3; one photo selected for the single-photo rows, four for 2×2, two for the book rows,
20 for the contact sheet): `1 Large` 150 dpi 192 ms, 300 dpi 649 ms; `2×2 Cells, A4` 300 dpi 517 ms, → PDF 396 ms;
`4×5 Contact Sheet` 300 dpi 1814 ms; book `Full Bleed Spread` 300 dpi 1364 ms; `Two Up with Caption` 354 ms. (With all
20 photos selected, `1 Large` renders 20 pages: 10.8 s, i.e. 0.54 s per page.)

Fix: `creations::run_jobs` rendered a layout's photos one by one; it now renders up to `render_width()` (a quarter of
the cores, 1..=4) at once, results in order. Single-photo rows only vary with machine load.

## Publish 1,000 raws to Hard Drive (JPEG, long edge 2048)

`Plan::execute` (`crates/engine/src/cmd/publish.rs`) rendered and wrote one photo at a time: **512 s**. It now renders
batches of `render_width()` photos concurrently and writes them in order on the worker: **157 s** (3.3×). A render
Idle, 2026-10-11: **102.6 s** (102.1 / 102.6 / 103.3 s), ~0.1 s per photo. A render
thread that dies is reported as that photo's failure, and cancel is checked before each batch and each upload.

## Upstream-PR candidates (shared code, not changed here)

**U1 — 1:1 tiles re-decode the whole raw — not a product issue (2026-10-11, G6).** The probe
(`examples/perf_tile.rs`) never handed its results to `Session::accept`, so each window decoded the raw again; the
app's render queue accepts every result's source (`ui-egui` `render.rs`), so its tiles reuse the full-size source.
With the probe fixed: same window 1.7 ms, pan 5.9 ms, after exposure 1.3 ms, after NR 12.9 ms (GPU; CPU 5–18.5 ms);
only the first tile pays the ~0.4 s decode. Original analysis, kept for reference: (`crates/engine/src/media.rs`, `crates/pipeline`).** Every
`Session::region_job` window, even the same window twice, loads and develops the full 6000×4000 source
(`<PREFIX>_PROFILE`: develop 226–292 ms, highlights 50–57 ms, colour 138–161 ms, then the GPU window ~60 ms), so a
1:1 tile costs 530–1140 ms against a 100 ms budget, with a `StageCache` attached as the loupe does. The GPU part alone
would fit. Proposal: keep the developed full-resolution source (per photo + develop-source key) across region jobs, as
the whole-frame loupe path keeps its preview-level source, so a pan or a slider change re-runs only the window.
Expected: ~60–100 ms per tile.

**U2 — fixed in shared code (G6, branch `upstream/perf-previews`):** (a) photos render in `export_parallelism`
lanes (2 for 1:1), (b) the thumbnail renders from the view's decoded source (one decode per photo instead of two),
plus a cheaper hue/saturation table path in `lcc-raw` (`HsvTable::apply`: no sRGB round trip for a unit value scale, no
`fmodf`; 2048 px render 16.4 → 15.1 G instructions single-thread). 1,000 ARWs, same binaries A/B minutes apart, load
average ~25: previews 239.3 s → **75.0 s** (CPU 34m06 → 26m59), whole `<cli> run --import … buildPreviews` 243.5 s →
**78.5 s**; 100 ARWs 20.7 s → 8.4 s. (c) (embedded JPEG as the standard preview) not needed for the budget.
Original analysis: **`library.buildPreviews` builds one photo at a time (`crates/engine/src/cmd/previews.rs`, `run_all`).**
1,000 24 MP raws: 354 s of previews after a 16 s import (idle machine: 172 s after ~4 s, median of 3, still over) (budget: 2 min in total). A single 2048 px render takes ~320 ms
(`load_bytes(2560)` alone is 93 ms wall / 1.9 s CPU, so it already uses many cores). Running the same work as N separate
processes gave N=4: 202 s, N=8: 144 s — so concurrency helps but does not reach the budget on its own. Proposal:
(a) render a bounded number of photos at once (as P6.1 did for publish), (b) derive the grid thumbnail from the loupe
preview instead of a second job per photo, (c) for an untouched raw, take the standard preview from the embedded
camera JPEG (16.7 ms) and render the real one lazily. (b)+(c) should bring import + previews under 2 min.

**U3 — improved in shared code (G6, branch `upstream/perf-nr-drag`):** a noise-reduction change no longer rebuilds
the wide spatial planes (base, clarity, dehaze, chromaticity) in draft renders — the plan carries the linear key
without NR, and drafts keep those planes, redoing only log luminance and the texture band; full-quality renders never
take them (tested). Draft NR drag, CPU, A/B at load ~25: 18.5–20.2 → **15.7–16.0 ms** wall, 335 → 270 ms CPU; single
thread `prepare` 18.7 → 7 ms. At the budget edge: the rest is NR itself (~30 ms single-thread) and the finish stage
shared with every drag. Original analysis: **CPU fallback misses the slider budget for NR.** Idle, draft-size highlights/clarity drags on the CPU take
6–7 ms (met; 34–47 ms under load), but an NR drag takes 22.8 ms (297 ms under load). Proposal: a smaller draft size during drags on the CPU path (e.g. 768 px), and skipping NR in
draft renders while a slider moves.

**Draft NR drag, task 4.5 (2026-11-24):** a draft with luminance NR now runs the colour NR first and keeps that image
(`Plan::draft_col_key`, `local::denoise_lum_draft`); a Luminance / Detail drag reruns only the luminance NR on it (it
only scales each pixel's rgb, so the swapped order is a close stand-in). Full-quality renders and exports keep the
original order and are bit-identical (`tests_draft_nr.rs`). Windows, loaded machine, paired alternating runs, median of
4: 63.8 → **33.4 ms** wall (single thread 280 → 200 ms). `cargo xtask bench` now carries wall budgets (`BUDGETS`,
incl. this row at 16 ms) that `--strict` fails on, and compares wall time where there is no process CPU time.

**U4 — export 24 MP JPEG at 1.25 s** (under load; idle: 0.43 s GPU / 0.55 s CPU, budget met — keep as a nice-to-have) (GPU render 865 ms of which most is the full-size source develop, plus 386 ms
encode). Overlapping the JPEG encode of strip *n* with the render of strip *n+1*, or a faster demosaic for export
previews, would get under 1 s.
