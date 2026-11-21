# HDR merge memory measurement

Task: `LR-MERGE-HDR`. Stage 1 reduces allocations without changing pixel precision. Half-float frame storage is deferred.

## Reproduce the original and exact paths

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
LIGHTCRAFT_HDR_HEAP_MODE=stream \
cargo test -p lightcraft-merge --test hdr_heap -- --nocapture
```

Use `LIGHTCRAFT_HDR_HEAP_MODE=original` for the frozen pre-change f32 merge,
`stream` for sequential input storage, or `exact` for the validated `Vec<Frame>`
adapter. Repeat with five or seven frames. Run comparisons in separate processes. No private files, downloaded photos, GPU, or display are needed.
Without the variables, CI runs the same harness at 480 x 320 with three frames.
The harness checks dimensions and caps the workload at 70 MP and seven frames.
Large cases run only when dimensions are set explicitly, outside fast CI.
The 67.28 MP case uses width 10048 and height 6696.

The scene is OceanSunset from `lightcraft-scenes`, with a moving dark disk,
14-bit quantisation, clipping, and non-integral handheld translations.
The three-frame bracket uses -2, 0 and +2 EV. The five-frame bracket uses
-4, -2, 0, +2 and +4 EV. Seven frames use -3 through +3 EV in one-stop steps.

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

## Checkpoint A baseline on main

Main and the branch started at `2e66a00d554508341a83f0942b9224f2161f2ba8`.
At checkpoint A only the test, its dev dependency, and this document were added. Measurements
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
288,000,000 bytes per 24-MP frame (807,376,896 bytes per 67.28-MP frame).
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

### Where the original path holds full-size buffers

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

## Stage 1: exact reductions

**All retained samples are still f32.** The new path has no input quantisation.
The frozen test-only oracle is the original `hdr.rs` at
`2e66a00d554508341a83f0942b9224f2161f2ba8`, with helper visibility changed for tests.

- Saturation uses three horizontal rows per frame in each 64-row merge band.
  Vertical maxima keep the original top-to-bottom fold, zero initial value,
  and clipped edge intervals. No full-size saturation plane is retained.
- `merge_hdr_iter` validates and retains one decoded frame before requesting
  the next. A buffer with no excess capacity moves without copying. A cropped
  buffer uses a fallible allocation at its logical size; the oversized buffer
  is freed before the next decode. The engine's HDR path uses this iterator.
  The `Vec<Frame>` adapter cannot prevent allocations its caller already made.
- A warp writes straight into the retained f32 image, then releases its source.
  It uses the existing Lanczos taps, 4 x 4 neighbour clamp, coordinates and
  validity edges. An identity transform keeps the source without cloning it.
- Inputs, validity maps and all row scratch are freed before the full-size
  ghost overlay is allocated. Small result metadata is copied first.
- Both entry points reject empty or malformed image layouts, NaN/inf samples,
  invalid clip thresholds, non-finite exposures and unrepresentable exposure
  arithmetic with `MergeError`. Limits are 64 frames, 65,000 pixels per edge,
  512 MP and a representable allocation size. Valid finite f32 values are kept.

### Measured Stage 1 saving

These paired runs use the frozen original and the exact streaming path on the
same generated inputs, four workers, alignment on and High deghosting. Counts
include retained inputs and every transient heap allocation, but exclude the
fixed fixture and warm-up described above. Each row is one pair of runs;
Rayon scheduling still affects feature and deghost scratch.

| Pixels per frame | Frames | Original peak bytes | Stage 1 peak bytes | Saving |
|---|---:|---:|---:|---:|
| 6000 x 4000 (24 MP) | 3 | 1,808,049,127 | 1,232,914,083 | 31.8% |
| 6000 x 4000 (24 MP) | 5 | 2,741,400,011 | 1,962,955,376 | 28.4% |
| 6000 x 4000 (24 MP) | 7 | 3,482,746,715 | 2,672,332,024 | 23.3% |
| 10048 x 6696 (67.28 MP) | 3 | 5,011,178,119 | 3,397,872,195 | 32.2% |

Amortised peak bytes per pixel per frame are 17.124, 16.358, 15.907 and
16.834 respectively after Stage 1. RGB payload itself remains 12 bytes per
pixel per frame. At 67.28 MP that is 807,376,896 RGB bytes per frame.

Maximum RSS for the same pairs was 2,559,066,112 to 1,790,705,664 bytes;
3,718,823,936 to 2,636,070,912; 4,504,600,576 to 3,329,277,952; and
5,858,770,944 to 4,173,119,488. RSS includes allocator retention and the profiler.
Profiled wall times were 3.227 to 3.521 s, 5.900 to 6.164 s, 8.533 to
9.051 s, and 8.011 to 8.015 s. These include generation and allocation
profiling, so they are not decoder or export benchmarks.

A final 24-MP, five-frame run through the `Vec<Frame>` adapter peaked at
1,952,283,376 bytes, below its 2,100,000,000-byte budget. The streaming and
adapter paths share the same storage and merge code; callers of the adapter
have already allocated all inputs before the call.
A final streaming run repeated the 67.28 MP peak exactly. The seven-frame,
24 MP repeat used 2,672,324,024 bytes, also below its budget.

### Allocation regression budgets

The large generated profiles have upper limits with a scheduling allowance
smaller than one full RGB frame. These are budgets for this fixed scene and
worker count, not bounds for arbitrary image content.

| Generated profile | Observed Stage 1 peak | Upper budget | Allowance | One RGB frame |
|---|---:|---:|---:|---:|
| 24 MP, 3 frames | 1,232,914,083 | 1,350,000,000 | 117,085,917 | 288,000,000 |
| 24 MP, 5 frames | 1,962,955,376 | 2,100,000,000 | 137,044,624 | 288,000,000 |
| 24 MP, 7 frames | 2,672,332,024 | 2,850,000,000 | 177,667,976 | 288,000,000 |
| 67.28 MP, 3 frames | 3,397,872,195 | 3,650,000,000 | 252,127,805 | 807,376,896 |

The fast 480 x 320 functional case has a 40 MB limit. It is below the 2048 px
deghost cap, and feature work dominates, so it is not a scaled proxy for the
large profiles. It also creates excess decoded capacity and checks, before
each subsequent input, that only previous inputs' logical RGB bytes plus
65,536 metadata bytes remain live.

A second fast control uses one worker, no alignment and no deghosting. Its
budget is input RGB + one RGB output + three saturation rows per frame +
128,000 metadata/worker bytes: **7,518,080 bytes**. The observed peak is
7,399,377 bytes. There is no feature or small-map peak to hide a retained copy.
Setting `LIGHTCRAFT_HDR_HEAP_EXTRA_COPY=1` deliberately holds an additional
1,843,200-byte RGB image through the merge. It raises the control's peak to
9,242,577 bytes and the test fails its upper budget. That diagnostic was run
and rejected as expected. The old oracle is measured without a saving budget.

### Precision and rejected half-float candidates

Stage 1 is checked **bit for bit** against the frozen original for radiance,
overlay, exposure values, alignment, reference and metadata. Tests include
all deghost settings, alignment on and off, band boundaries, narrow images,
all-black, all-clipped and moving-highlight cases.

Saturation scratch is compared with exact float equality. Its `f32::max`
reductions can select different signs when positive and negative zero tie, as
[Rust documents](https://doc.rust-lang.org/std/primitive.f32.html#method.max).

The original materialised reduction returns `-0.0` where the streamed path
returns `+0.0` for the `7x1` fixture on x86. This was reproduced with the
unchanged failing test under Rosetta using `x86_64-apple-darwin`.

For finite maxima, only zero has two numerically equal bit patterns; no
tolerance is used. Saturation is never a divisor or inspected for its sign.
Both uses subtract a positive smoothstep threshold, which removes the zero
sign before it can affect a weight.

Production arithmetic is unchanged.
Signed-zero, negative-sample and subnormal fixtures retain strict bit checks
for radiance and overlay, byte checks for half and float DNGs, and bit checks
after DNG decoding and scale restoration. The Lanczos equality fixture also
contains mixed signed zeros and negative samples.

A second fixture has fixed non-empty shadow-ramp, coloured clipping-boundary
and moving-highlight regions. Every channel is checked before encoding and
after decoding the half-float DNG and undoing its maximum-dependent scale via
`BaselineExposure` (subtracting the source's nonzero baseline first). DNG bytes
are identical too. The CPU developer checks the decoded results with shadow
lifting and different white balances; tests also require those edits to change
the result. Relative error has no fixed denominator floor: nonzero against zero
is infinite relative error. Stage 1 has zero error in every region and channel.

A future half-float mode must keep p99 relative error at most 0.2% and maximum
relative error at most 1%, with a separately computed exposure-aware absolute
rounding budget. A single subnormal rounding contributes at most `2^-25` in
input-frame units. With normalised weights `a_i` and exposures `k_i`, its
reference-unit contribution is bounded by `sum(a_i * 2^-25 / k_i)`.

Source rounding through Lanczos adds its absolute tap-weight sums on both
axes; the neighbour clamp also needs a bound on its original endpoints.
Aligned-pixel rounding adds another `2^-25` before exposure division. DNG
encoding must be accounted for separately after its maximum-dependent scale:
subnormal quantisation of both compared encodings contributes at most
`2^-25 * (1/scale_original + 1/scale_candidate)` in reference units. Normal
rounding and developer gains also need their own error checks. No fixed 0.001
brightness floor is accepted. A generated test proves that deleting `1e-6`
exceeds the subnormal budget even after Lanczos and two input roundings.

**Half-float frame storage is deferred.** Diagnostics show why the two cheap
approaches do not establish a useful, decision-preserving representation:

- The reviewer's scalar bracket gives about 0.80001 in f32 and 0.679834 after
  naive packing, over 15% relative error. Binary16 switches off the reference
  highlight guard. Tests retain this counterexample.
- Packing can change exposure eligibility at the raw 0.01 and rendered 0.04
  floors and the 0.8 upper cutoff. Admitting the 500th sample switches off the
  EXIF fallback. Deghost prediction at 0.8 and 0.001 can change a mask by over 0.9.
- Protecting only source samples near a cutoff is insufficient for alignment.
  A generated step with source values 0.7004006 and 0.89962, both far from 0.8,
  crosses that cutoff after a fractional Lanczos warp when its source is packed.
- A conservative whole-frame fallback whenever the envelope of original and
  rounded samples can straddle a cutoff retains all 3, all 5 and all 7 f32
  frames in the generated decision fixtures. It saves no additional storage.
- Delaying packing until the original exposure and deghost analysis finishes
  preserves those decisions but cannot reduce an allocation peak already
  reached during that analysis (the five- and seven-frame measurements).

A useful early half-float representation therefore still needs compact original
analysis/decision data that remains correct through alignment, or a different
loading strategy. No production half-float mode or non-finite fallback was
introduced. `half` 2.7.1 is a dev dependency with default features disabled,
used to reproduce the rejected candidates. The HDR parity row stays partial
and gap 29 remains open.

## Final review follow-up

`merge_hdr_iter` checks cancellation before every request to its input iterator,
including the first. The engine maps those checks to the number of completed
reads, keeping loading in 0..30% and merging in 30..90%. An input decode error
propagates unchanged and stops all further requests.

Generated regression tests cover cancellation before loading and after 1, 3
or 6 successful inputs of a seven-frame bracket. Without the new check, both
cancellation tests requested all seven frames and failed. With it, later
frames are never requested. Additional frozen-oracle coverage includes two-frame
merges and mixed orientations; another test fails the third decode after two
successful inputs and requires the original error and exactly three requests.
An engine test checks cancellation before reads and monotonic progress through
the original loading and merge ranges.

### Verification at the reviewed base

The reviewed tree merges `origin/main` at
`bf96a5fa79fb351bd328cac3614e6b5bb137e365`. All 64 merge tests passed
(42 unit tests, 21 integration tests and the allocation harness), including the
new cancellation and orientation coverage. The engine progress regression passed
in isolation and in the workspace run. Doctests, parity, layers, assets and WASM
passed separately; parity still reports the existing stale summary.

Quick CI passed fmt, Clippy and HEIF, then stopped at two GPU equivalence tests.
A complete workspace run with `--no-fail-fast` found seven failures. Each was
reproduced on an unchanged, clean worktree at the merged base, with the same
resolved `CARGO_TARGET_DIR` and workspace features:

| Test | Failure on branch and base | Base verification |
|---|---|---|
| `equivalence::v3_luminance_cutoff_chroma_2` | uniform-image assertion at `crates/gpu/tests/it/equivalence.rs:56` | three exact runs |
| `equivalence::v3_luminance_cutoff_chroma_4` | the same uniform-image assertion | three exact runs |
| `masks_stay_on_the_content_under_every_geometry_change_on_the_gpu` | straighten mask centroid at `(73.44033722438391, 39.81999258847508)` instead of `(67.46466224131198, 35.23096446700507)` | three exact runs |
| `tests_light::v1_and_v2_render_as_before_v3` | identical actual hashes `[5201756129139503724, 10832193741061795827]`, expected `[6624979530767351604, 9773754074516659789]` | three exact runs |
| `dialog_role::settings_is_a_dialog_too` | missing Georgian glyph U+10E5 | three exact runs |
| `wire::tests::wire_job_renders_like_the_engine` | identical pixel assertion arrays on branch and base | exact case passed three times; complete web binary failed three times |
| `tests::denoise::model_installation_keeps_the_frame_pump_responsive_and_reuses_the_loaded_model` | three-second receive timeout at `crates/engine/src/tests/denoise.rs:107` | exact case passed three times; complete engine binary passed once and failed twice |

The current-main GPU, pipeline, web, UI and denoiser failures were left unchanged.
Their proof logs stay outside the disposable build directory.

The final integration base is `272c255f298ae155028724d1bf4e2e3c685b091d`.
It includes upstream GPU dither-test corrections. The pull request records
full CI on the final commit and failure confirmations on that unchanged base.

## Stage 1 verification

Quick CI passed fmt, Clippy, HEIF and all 59 merge tests (37 unit, 21 existing
integration and the heap harness). Its only failure was the already confirmed
`dialog_role::settings_is_a_dialog_too` Georgian glyph failure described below.
Find Similar passed in this run. CI reported 179 skipped checks for absent
corpus files, craft-fonts, models or GPU capability. The generated HDR checks ran.

Full `cargo xtask ci` passed fmt, Clippy and HEIF, then failed only the same
Georgian glyph assertion in its workspace test step. All 59 merge tests passed
again, including the generated allocation harness. Find Similar passed.
The command exited nonzero and marked parity, layers, assets and WASM as not
run after that failure. Each of those gates and workspace doctests passed in
separate commands. The full report listed 179 skipped checks and detected
Apple M3 Max through Metal. These are the approved pre-existing CI exceptions,
not a claim that the full command was green.

A workspace run with `--no-fail-fast` checked the targets after that UI failure.
It found one additional pre-existing assertion:
`wire::tests::wire_job_renders_like_the_engine`, at
`apps/lightcraft-web/src/wire.rs:456`, requires exact pixel equality.
All Stage 1 changes were stashed and the clean base
`2e66a00d554508341a83f0942b9224f2161f2ba8` was checked with the same target directory.
The exact case passed three times on base. Its complete 38-test web binary,
built with workspace features, failed the assertion three times. Base and
Stage 1 had identical left/right arrays for all 10,880 pixels; the maximum
channel difference was one 8-bit value. The base tree stayed clean, and the
Stage 1 index was restored exactly. No web or UI code was changed.

The repository's `cargo xtask bench` also ran on the clean original base and
the restored Stage 1 tree, with the same target directory and its generated
24 MP source. No files were downloaded. Each run recorded 78 render metrics;
this benchmark does not call HDR merge. CPU export process time was
5,595.4 to 5,964.3 ms, a 6.6% increase. No CPU pipeline metric exceeded the
benchmark's 20% warning threshold.

Six GPU process-CPU metrics did exceed that threshold. These are observed
warnings, not proof of an HDR regression or a clean GPU timing comparison:

| Render metric, GPU process CPU time | Base ms | Stage 1 ms |
|---|---:|---:|
| 24 MP export | 420.5 | 520.8 |
| Finish with vibrance/saturation | 3.2 | 3.9 |
| Finish without highlights/shadows | 3.7 | 4.6 |
| Loupe draft clarity drag | 1.6 | 2.4 |
| Loupe draft highlights drag | 1.4 | 2.5 |
| Window draft exposure drag | 6.6 | 12.3 |

The benchmark's reported load averages were 36.44 and 33.51 on this shared
14-CPU machine. GPU HDR merge does not exist, and the render code was unchanged.
GPU/CPU image comparisons in both runs stayed at a maximum difference of one
8-bit code value. The base was clean before and after its run; Stage 1 was
stashed and its index restored exactly afterward.

Doctests, parity, layers, assets and the WASM check passed separately. Parity
reports the existing stale checklist summary; no feature count or status changed.

## Checkpoint A verification and pre-existing CI failure

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

## Release performance review (2026-11-07)

`tests/hdr_timing.rs` shares its generated fixture with `hdr_heap` through
`tests/support/hdr_fixture.rs`. It uses the standard allocator, not `dhat`.
Only `merge_hdr` is timed: the inputs are generated and allocated beforehand,
a 96 x 64 merge warms the pool, and output hashing runs after the timer stops.
The comparable Vec adapter is used on both main and the PR. This excludes raw
decoding, streaming input generation, DNG writing, and app caches.

The comparison uses release builds, Rust 1.99.0, the same Apple M3 Max with
36 GiB RAM, 6000 x 4000 pixels, four Rayon workers, alignment and High
deghosting. Main is `8a0b91a95c30618b690374dd2db9d0684ebc9b23`; the reviewed
PR head is `400703b7096534a62552f138de2a20dc1505fff3`. Identical test-only
fixture and timing files were added to the main checkout; its production code
was unchanged. Five separate processes per case alternate implementation
order while holding the machine's CI lock. Each merge exercises every
non-reference Lanczos warp. Input hashes and full radiance/overlay hashes
match across all runs.

| Frames | Main median (min to max), s | Reviewed PR median (min to max), s | Revised median (min to max), s |
|---|---:|---:|---:|
| 3 | 1.092 (1.087 to 1.191) | 1.219 (1.216 to 1.230) | 1.072 (1.062 to 1.079) |
| 5 | 2.033 (1.998 to 2.177) | 2.222 (2.216 to 2.272) | 1.982 (1.962 to 2.020) |

The reviewed head was 11.7% and 9.3% slower than main in this series. Revised
medians are 12.1% and 10.8% below the reviewed head, and 1.8% and 2.5% below
main. Five-frame ranges overlap, so this is evidence that the measured
regression is removed, not a general speedup claim for real files or 67 MP.
An earlier independent five-run main/PR comparison also found a slowdown
(1.082 to 1.210 s for three frames; 1.990 to 2.194 s for five).

### Changes and A/B checks

- Image and mask sizes are checked before blending. Row views resolve image,
  validity and saturation offsets once, then checked tile slices feed zipped
  iterators. A 256-pixel stack tile retains f64 accumulators and the original
  brightness order for every pixel. No additional heap image or row is made.
  A separate row-only A/B reduced blend-phase medians from 0.244 to 0.176 s
  and 0.416 to 0.292 s (28% and 30%); whole merges improved 4.8% and 6.2%.
  The reviewed blend phase was about 19% to 20% of total time, not the largest
  phase. Warping was larger.
- Full-buffer finite-value checks now scan chunks across workers. The input
  validation median fell from 0.040 to 0.010 s and 0.068 to 0.017 s. The
  alignment gain overflow check stays in place, as do checks after warping,
  blending and overlay creation. A regression checks non-finite samples in a
  final partial chunk, stops later decoding, and checks gain overflow there.
- The initializer remains fallible and safe. An experiment used fallibly
  reserved vectors and Rayon's safe `unzip_into_vecs` for fully assigned warp
  pixels, explicitly producing black/false outside the source. Removing the
  zero pass from that collecting warp changed warp medians by 9 and 15 ms,
  but the complete alternative was 3.0% and 2.8% slower than the row fill.
  Its total medians were 1.178 and 2.144 s versus 1.144 and 2.086 s for the
  row fill in the same series. It was rejected. No `unsafe`, `set_len` or
  unchecked input-derived indexing was added.

Three paired release heap profiles per large case compare the reviewed and
revised code, including generated inputs. Three-frame peaks are exactly
1,232,914,139 bytes in both. Five-frame maxima are 1,962,955,460 bytes in both
(the revised range was 1,952,283,460 to 1,962,955,460 bytes). The fast control
is exactly 7,399,433 bytes in both versions. The small aligned case varies
with feature scheduling, about 20.6 to 24.8 MB across both implementations;
all upper budgets still pass. The changes add bounded stack accumulators,
not heap scratch. Existing exactness checks include a new 257-pixel row that
crosses a tile boundary. The 67 MP and seven-frame profiles were not repeated
for this CPU review.

Reproduce one unprofiled timing process, then repeat with five frames:

```sh
CARGO_TARGET_DIR=target/agent-hdr-half \
LIGHTCRAFT_HDR_TIMING_FRAMES=3 \
cargo test --release -p lightcraft-merge --test hdr_timing \
  -- --ignored --exact hdr_release_wall_time --nocapture --test-threads=1
```

Build first and run the emitted test executable directly for repeated runs;
compilation is outside its internal merge timer too. The large test is ignored
in routine CI. Raw runs, A/B variants and stage medians are retained in the
local `scratch/hdr-3797` report beside the draft review reply.

## Limits of this checkpoint

The memory measurement covers the merge core with generated decoded frames, not raw decode,
catalog caches, DNG output, GPU rendering, or an end-to-end app merge. It does
not validate camera noise or colour on real files. The fixture is a 24 MP
upsample of a smaller procedural scene, sufficient to exercise full-size
allocations and actual warps; it does not measure camera detail or real-file merge speed.
The large runs cover 67.28 MP with three frames and 24 MP with seven, not
67 MP with seven. Small fixtures verify DNG and developer exactness, but their
allocations are outside the merge-memory measurement. No production half-float
frame storage has been shipped.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-07 | patch | LR-MERGE-HDR: reproduce x86 signed-zero max ties under Rosetta; compare saturation maxima exactly without imposing a zero sign, and extend strict output and Lanczos regression coverage |
| 2026-11-07 | patch | LR-MERGE-HDR: release comparisons and A/B checks; checked row/tile views and parallel finite scans remove the measured slowdown, with unchanged large-case heap maxima; safe no-zero warp rejected |
| 2026-11-05 | patch | LR-MERGE-HDR: check cancellation before each lazy input request; regressions cover loading cancellation, decode errors, two-frame merges and mixed orientations; merge current main before PR |
| 2026-11-05 | minor | LR-MERGE-HDR: Stage 1 exact reductions, frozen-oracle and cutoff diagnostics, large generated profiles and upper allocation budgets; half-float storage deferred |
| 2026-11-05 | minor | LR-MERGE-HDR: checkpoint A generated heap harness and original baseline |
