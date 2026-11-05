# HDR merge memory measurement

Task: `LR-MERGE-HDR`. Checkpoint A, 2026-11-05. The merge algorithm is unchanged.

## Reproduce the baseline

`crates/merge/tests/hdr_heap.rs` installs the workspace's existing `dhat` allocator
in a separate integration-test binary. It counts live requested heap bytes across
all threads, including short-lived allocations between progress callbacks. It
starts counting **before** creating the full-size input frames.

It records the high-water mark after `merge_hdr` returns, while its result is
still alive.

```sh
CARGO_TARGET_DIR=target/agent-hdr-half \
LIGHTCRAFT_HDR_HEAP_WIDTH=6000 \
LIGHTCRAFT_HDR_HEAP_HEIGHT=4000 \
LIGHTCRAFT_HDR_HEAP_FRAMES=3 \
cargo test -p lightcraft-merge --test hdr_heap -- --nocapture
```

Repeat with `LIGHTCRAFT_HDR_HEAP_FRAMES=5`. Run each command twice in separate
processes. No private files, downloaded photos, GPU, or display are needed.
Without the variables, CI runs the same harness at 480 x 320 with three frames.
The harness checks dimensions and caps the workload at 70 MP and five frames.

The scene is OceanSunset from `lightcraft-scenes`, with a moving dark disk,
14-bit quantisation, clipping, and non-integral handheld translations.
The three-frame bracket uses -2, 0 and +2 EV. The five-frame bracket uses
-4, -2, 0, +2 and +4 EV.

The real alignment, Lanczos-3 warp, exposure
estimation, saturation weighting, High deghosting, and full-size overlay run.
Assertions require every non-reference frame to be warped, a non-empty ghost
mask, and finite radiance. No full-size truth image or cloned brackets are kept.

A fixed 1232 x 832 scene (12,300,288 RGB bytes), the four-thread Rayon pool, and
a 96 x 64 warm-up merge are created before profiling. They are excluded from the
heap count. Small phase-label allocations during profiling are included.

For an optional RSS measurement on macOS, build the test with `--no-run`, then
run its printed executable directly under `/usr/bin/time -l` with the same
variables and `--exact hdr_peak_live_bytes --nocapture`. Timing `cargo` would
also measure compilation. RSS includes the fixture, allocator retention, stacks,
code, and profiler overhead; it is a secondary measurement, not the objective.

## Baseline on main

Main and the branch started at `2e66a00d554508341a83f0942b9224f2161f2ba8`.
Only the test, its dev dependency, and this document were added. Measurements
used Rust 1.99.0 and the optimized dev/test profile on macOS arm64 with 36 GiB physical RAM,
four Rayon workers, 6000 x 4000 pixels, alignment on, and High deghosting.
GB below means 1,000,000,000 bytes.

| Frames | Run | Peak live heap bytes | Bytes per image pixel | Peak heap / frame | Maximum RSS bytes |
|---|---|---:|---:|---:|---:|
| 3 | 1 | 1,616,049,506 | 67.335396 | 0.539 GB | 2,156,118,016 |
| 3 | 2 | 1,808,049,139 | 75.335381 | 0.603 GB | 1,676,197,888 |
| 5 | 1 | 2,741,400,023 | 114.225001 | 0.548 GB | 2,661,662,720 |
| 5 | 2 | 2,741,400,023 | 114.225001 | 0.548 GB | 3,554,803,712 |

The retained RGB payload is exactly **12 bytes per pixel per frame**:
288,000,000 bytes per 24 MP frame (804,000,000 bytes per 67 MP frame).
Peak heap divided by frame count includes output and scratch shared by the
merge; it must not be described as the RGB payload alone.

These heap counts are not directly comparable to the 67 MP raw-process
measurements in `merge.md`. The size limits on work images and deghost maps
also prevent an exact linear extrapolation of the total peak to 67 MP.

Even with a fixed thread count, Rayon scheduling changes how many saturation
scratch planes overlap. The three-frame peak varied by 192 MB between these
runs. Compare repeated runs and report their range, rather than claiming one
exact peak is independent of scheduling. RSS can be lower than requested live
heap bytes because it measures resident pages rather than live allocation sizes.

Returned live heap is 384,025,126 bytes for three frames and 384,025,366 for
five, almost entirely the f32 radiance (288 MB) and f32 ghost overlay (96 MB).

### Where the full-size buffers live

Let `P` be the pixel count of one frame, `N` the number of frames, and `S` the
pixel count of the downsampled deghost image. For 6000 x 4000, the downsample
factor is three, so `S = 2000 x 1334`.

| Location | Buffers and lifetime |
|---|---|
| `engine::MergeJob::load` | Full-size files decode sequentially, but the returned `Vec<Frame>` retains all N RGB images, `12NP` bytes, before the merge starts. Preview decoding is parallel. |
| `frame::load_frame_with`, `raw_frame` | Standard decoders return `Rgb32f`. Raws retain decoded samples while `RawImage::develop` creates f32 normalised sensor data and f32 RGB. AHD uses tiles. Default crop compacts the RGB vector in place but can retain its larger capacity. Lens opcodes, pixel-aspect stretching, resize, and orientation can add copies or scratch. These decoder costs are not exercised by this direct-frame harness. |
| `Frame::orient` | If input orientations differ, `Image::oriented` clones and then rotates. The original, clone/intermediate, and rotated f32 RGB can coexist, up to `24P` extra bytes while other frames stay live. The fixture uses matching orientations. |
| `align::work_image`, `raster::resample` | A box downsample uses an f32 RGB intermediate, then a small RGB result and f32 luminance. Its intermediate is `source width x work height` for this landscape case. Feature pyramids and detector scratch use f32 planes at at most 1200 px long edge. Frames can prepare features concurrently. |
| `hdr::merge_hdr`, `align::warp`, `flow::warp` | All N f32 frames remain live. Each non-reference frame gets one f32 Lanczos-3 output plus a `Vec<bool>` validity map, then replaces its source. Warps are sequential across frames. Peak here includes `12P` extra RGB bytes and up to `(N-1)P` validity bytes. Rust's `Vec<bool>` stores one byte per element. |
| `hdr::log_ratio` | Sampled f32 log ratios, roughly 400,000 per adjacent pair, not full-size RGB copies. |
| `hdr::deghost_maps` | All N frames stay live. Small f32 RGB plus f32 valid fractions use `16NS` bytes. Predicted RGB uses `12S`; masks, dilation and Gaussian scratch add small f32 planes. Returned masks use `4NS` bytes. |
| `hdr::saturation` | Each concurrent frame creates **three full-size f32 planes**: channel maximum, horizontal maximum, and final neighbourhood maximum, `12P` bytes. The completed final planes stay in `sat`, `4NP` bytes. Several calls can overlap. |
| Final radiance and overlay | f32 RGB output adds `12P`, then the f32 overlay adds `4P`. All input frames, validity maps, saturation maps and small ghost masks are still live while the overlay is allocated. Without deghosting the overlay is empty. |
| `engine::MergeJob::run` | After the merge returns and drops inputs/scratch, the engine orients a copy of the f32 ghost overlay, even for normal orientation. The harness stops at the merge result. |
| `output::write_linear_dng` | Runs after merge inputs and scratch have dropped. It creates a flattened/scaled f32 RGB copy even for half-float DNG output, plus encoding buffers. Outside this harness. |

The three-frame resampling peak was 1,201,728,713 bytes. In its first run the
overall peak came when the overlay was allocated; in its second run concurrent
saturation scratch was larger. In the five-frame first run saturation scratch
was the peak. Optimising only the frame payload would leave this second cost.

## Proposed implementation for review

**Use half-float RGB storage and compute saturation per row or tile.** Keep
sampling, exposure estimation, weighting, and deghost arithmetic in f32, and
preserve the existing f64 exposure chain and weighted-sum accumulators.

1. Add HDR-specific compact input storage, separate from panorama's `Frame`.
   Keep geometry, colour, white balance, metadata, clip and orientation. Preserve
   the original f32 brightness statistics, content tie-break and small alignment
   image, so quantisation does not change the reference or feature registration.
2. Feed full-size HDR decoding into that storage one frame at a time. Packing
   only inside today's `merge_hdr(Vec<Frame>)` cannot lower the caller's `12NP`
   starting allocation. Keep that API as an adapter for existing callers, and
   measure both the adapter and the sequential loading path.
3. Store source and aligned RGB as binary16, **6 bytes per pixel per frame**.
   Widen only Lanczos taps and the current merge row or tile to f32. Write the
   aligned output straight into compact storage, then drop the source before
   warping the next frame. Avoid a full-size f32 warp followed by a conversion.
4. Replace `sat` and its three full-size scratch planes with three-row maximum
   buffers or tile buffers with a one-pixel halo. Preserve the existing 3 x 3
   maximum and edge rules. Without this change the allocator peak can remain
   dominated by f32 saturation scratch.
5. Copy the small result metadata, then release compact inputs, validity maps,
   and saturation scratch before creating the full-size overlay. Keep small
   deghost maps and final radiance as f32.
6. Use an f32 fallback for any frame that cannot be represented safely as half
   (non-finite samples or values outside finite binary16 range); never silently
   clamp or turn a finite value into infinity. Validate dimensions, lengths,
   clip and arithmetic before allocating or reading rows. Reject malformed
   inputs with `MergeError`.

`half` 2.7.1 is already in `Cargo.lock` as a transitive dependency. Making it a
direct workspace dependency would add no new package; inspect its selected
features before use. The raw crate also has original binary16 converters for
DNG reading and writing. The merge has no GPU path, so both preview and full
merges would use the same CPU implementation.

Halving RGB storage saves exactly 144 MB per 24 MP frame or 402 MB per 67 MP
frame, before scratch and decoder costs. This is **not a promise of halving
total process memory**.

Removing saturation planes and compacting during loading are needed to improve
the measured peak and its growth per frame.
No after numbers have been measured yet.

### Proposed precision gate

For each output radiance channel, compare to the unchanged f32 implementation
using `e = abs(new - old) / max(abs(old), 0.001)`, in reference-frame units.
Require **p99(e) <= 0.002 and max(e) <= 0.01**, separately for the whole image,
near-clip highlights, deep shadows, and moving-object regions. Thus for a
channel below 0.001, the maximum absolute error is 0.00001; above that floor,
the maximum relative error is 1%. Also require exposure changes <= 0.002 EV,
ghost-overlay maximum absolute change <= 0.02, and unchanged reference and
alignment. These are proposed acceptance limits, not results.

Normal binary16 samples round by at most about 0.049% relative error, but
exposure ratios, clipping weights and deghost thresholds can amplify it.
Subnormal samples need the absolute floor above. Preserve unclipped values,
including negative Lanczos intermediates, rather than adding a new clamp.

At checkpoint B, retain an f32 test oracle and compare identical generated
brackets with 3 and 5 exposures, all deghost strengths, alignment on and off,
samples around 0.8/0.96 clip, subnormal shadows, coloured clipped edges, and
moving highlights and dark objects. Existing merge tests must pass. Add checks
for one frame, mismatched or malformed sizes, all-black, and all-clipped inputs.
If the precision gate fails, preserve more f32 analysis data or revise the
design for review; do not widen the tolerance silently.

## Verification and pre-existing CI failure

**Quick CI is blocked by an existing UI assertion.** Three runs of
`cargo xtask ci --quick` passed fmt, Clippy, HEIF, and all 47 merge tests
(25 unit tests, 21 existing integration tests, and the new heap test).
The workspace test step failed in
`tests::find_similar::stopping_leaves_the_view_alone_and_the_next_search_reads_only_the_rest`
at `crates/ui-egui/src/tests/find_similar.rs:164`.

The first two runs expected 12 worker reads and counted 11. The third used
`RUST_TEST_THREADS=1` and counted 10. The exact case passed when run alone
with the harness present.

To check the base, all four checkpoint files were stashed. HEAD stayed at
`2e66a00d554508341a83f0942b9224f2161f2ba8`; the working tree and index were
clean before and after every base run. All runs used the same
`CARGO_TARGET_DIR=target/agent-hdr-half`.

- The exact case passed three times on base with `RUST_TEST_THREADS=4`,
  using `cargo test --workspace --lib --bins --tests <exact-test-name> -- --exact --nocapture`.
- The base's full workspace test suite then failed at the identical assertion:
  12 reads expected, 10 counted, matching the harness run with serial tests.
- A repeat of the base suite passed that case. It later failed the existing
  `dialog_role::settings_is_a_dialog_too` UI test because a Georgian glyph
  (U+10E5) was missing. The read-count failure varies with suite execution;
  these runs do not show that the harness causes the failure.

UI code and tests were left untouched. The checkpoint commit records the
pre-existing failure. The failed quick runs also reported 178 checks skipped
for missing corpus files, fonts, models, or a GPU capability; the generated HDR
measurement and merge tests did run.

Separate runs of `cargo xtask parity`, `cargo xtask layers`, `cargo xtask assets`
and `cargo xtask wasm` passed. Parity also reported that the existing checklist
summary is stale; this measurement commit does not refresh feature counts.

## Limits of this checkpoint

This measures the merge core with generated decoded frames, not raw decode,
catalog caches, DNG output, GPU rendering, or an end-to-end app merge. It does
not validate camera noise or colour on real files. The fixture is a 24 MP
upsample of a smaller procedural scene, sufficient to exercise full-size
allocations and actual warps; it is not a 24 MP detail or speed benchmark.
No 67 MP merge, half-float conversion, precision comparison, or implementation
change has run at checkpoint A.
