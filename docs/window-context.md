# Spatial context for zoom windows

A native window render prepares the surrounding pixels needed by its active local filters,
then returns the requested rectangle. The CPU and GPU use the same calculation in
`pipeline::plan` and `crates/pipeline/src/window.rs` (issue #1146).

The web build keeps main's earlier margin policy, automatic spot-source pick,
and 384 px Dehaze proxy. The #1146 difference between zoom windows and full
renders remains on web. Exact context expansion can exceed wasm32 memory;
web full renders and exports keep main's buffers and cache retention too.
Fallible large allocations are a possible follow-up before bringing exact
windows to web. The design and measurements below describe native rendering.

Noise reduction uses main's original-pixel detail scale, including when a
preview source is smaller than the original photo. Exact full-frame airlight
preparation, its memory estimate and window support use the same scale as
the full render (`Plan::detail_scale`, `SourceInfo::native_long`).

## Show the bounded window before the exact result

Native Detail, Before/After, Compare and Reference views first request a bounded
window for a new photo or location. It uses the caller's margin, main's local
filter grid and main's cached 384 px Dehaze proxy. Once that texture arrives,
the view waits 100 ms before requesting the exact window. A newer pan or edit
supersedes the waiting replacement without occupying the slot's worker. The
bounded texture stays visible until the exact result replaces it. Running
exact work is not cancelled. A settled edit of the same location requests
exact output directly, except a released drag with Dehaze active: that release
also requests a bounded window first when its matching draft is already shown.

Draft windows during slider drags use the caller's margin and local filter
grid. They reuse an exact airlight scalar only when its upstream key matches;
otherwise they use the proxy. The end of the interaction requests full quality
even when no additional edit follows. Temporary windows have distinct result
keys and geometry keys. Bounded windows never write stage entries, exact
preparation or exact airlight scalars. Draft windows retain their own stages
but leave exact preparation and scalars alone.

Full frames, deep output and exports do not select the bounded path. Web
keeps its earlier window and cache behavior. The exact accuracy bounds below
apply after the final replacement. Bounded and draft output can differ more,
especially near the window's edge or with strong edits.

This sequence was proposed and prototyped by dev-null-undefined in the review
of [PR #1390](https://github.com/storytold/lightcraft/pull/1390#issuecomment-6169337261).

## Why the old margin was insufficient

`region::margin_for` requests 4.5% of the output long edge, rounds up to 256 pixels,
and caps that margin at 768 pixels. `window_for` can reduce it further to fit the
display texture. These are useful bounds for the displayed window and pan reuse,
but they do not describe the filters' support.

The current loupe renders windows at no more than the source's native resolution.
Above 100%, the display magnifies those pixels. The issue's 4:1 and 8:1 examples
use larger virtual frames through the pipeline API. Both paths need the same
context rule, independent of how the caller chooses its window.

For an uncropped 2400-pixel long edge at full quality:

| Plane | Sigma | Context in each axis |
|---|---|---|
| Dehaze dark channel / transmission | 48 px | 142 px |
| Clarity | 28.8 px | 162 px, including resampling support |
| Highlights and Shadows, V1 | 36 px | 192 px, including resampling support |
| Highlights and Shadows, V2 | 36 px | 196 px, including the 4 px median |
| Texture / sharpening | 4.32 px | 11 px |

A Gaussian here is three finite box passes, so its support is the sum of their
radii (`raster::blur::box_radii`). A guided filter applies that blur twice. Its
fast version downsamples and upsamples too; the calculation includes those taps.
The V2 tone base also reads the median's radius. The maximum support of parallel
planes is enough, while sequential noise reduction adds its support. Local Noise
and Moire use the same fine-detail and chromaticity planes. Mask feathering,
heuristic blurs, and Refine Edges have their own support in the same calculation.
Supplied segmentation mattes skip the heuristic support that their evaluation
bypasses. Background uses the Subject matte before inversion. Refine Edges still
adds support when a matte is supplied. Hidden masks and masks without components
add no unused support, and hidden Dehaze does not trigger the full-frame airlight calculation.
Rendering still selects the same planes as main, including those selected by
hidden and empty masks. This preserves existing V1/V2 pixels. Clarity consumes
the tone base even with Highlights and Shadows at zero, so an inactive tone
mask's base support remains in the context when global or visible local Clarity
is active. CPU and GPU use the same render selector and context plan.

Auto Mask dab reads retain the existing spot/brush context calculation. Global
Defringe adds its shared min/max radius after spot source expansion, because
spots copy already-defringed pixels. Defringe runs before noise reduction and
local planes, so its support adds to theirs, including around spot source reads.

Dehaze windows use the full-resolution airlight statistic when preparation fits its budget.
The statistic and transmission formula for `window=None` are unchanged.

## Design and cost

The display window and its texture limits stay unchanged. The render plan expands
its private work rectangle, clipped only at the physical frame edges. It includes
spot source reads for that expanded rectangle. If the spot-read limit cannot
hold them, the plan uses the full frame. The GPU checks its storage-buffer limit
against the expanded size and uses the existing CPU fallback when necessary.
Only the requested pixels leave the renderer, including float and 16-bit output.

This uses the existing filters at their existing scale. A shared, reduced-size
analysis image would change full-render pixels unless it reproduced those filters
exactly. The work rectangle and its cached stages preserve V1 and V2 full renders byte
for byte. Window airlight retains a scalar and a bounded full preparation, and mask caches retain the alphas
whose inputs still match. No new process version is introduced.

The cost is extra sampled pixels and filter work, and larger stage caches. A warm
Exposure edit finishes only the requested rectangle on both CPU and GPU. Grain,
vignette and plane reads keep their coordinates in the work image. GPU output
allocation and readback also use the requested extent. Deep CPU output follows
the same rule. Diagnostic overlays retain the full work-image finish because
some read neighbouring finished pixels. At very large virtual scales or tight
crops, the context can approach the full frame. It is not
silently capped to a wrong result. Finishing fewer pixels does not shrink the
spatial planes or stage caches. The worst case with every spatial filter and
three refined masks still needs a full-frame work rectangle.

## Full-frame sampling for reduced planes

The fast guided filter uses a nominal step from the full-frame sigma. Its actual
sample spacing is `full / ceil(full / step)` on each axis. For example, the
2397-pixel frame at step 11 has 218 samples, spaced about 10.9954 pixels apart.
Snapping the full-resolution work rectangle to multiples of 11 would still use
the wrong positions.

`local::GuidedGrid` snaps the reduced coefficient rectangle outwards to that
full-frame grid. Both renderers use the same subsets of the full-frame Box and
Bilinear taps. The downsample has the original scale, origin and edge weights;
the upsample has the original phase. Only taps for the work rectangle are
allocated. Missing taps at its outer boundary clamp there, and the existing
context excludes their support from the requested pixels. The full-resolution
work rectangle and cache geometry stay unchanged.

Full renders, whole-frame windows and filters with step 1 use the original path.
The V2 median still runs before the tone base. Resampling alone is bit-identical
to the corresponding full-frame samples. Final rendering can differ by a code
value because the blur's floating-point running sums start at different places.

| Path checked | Sampling behavior |
|---|---|
| Fast guided filter, Highlights/Shadows base, Clarity | Shared full-frame reduced grid on CPU and GPU |
| Dehaze dark channel / transmission | Full-resolution Gaussian; no reduced spatial grid |
| Luminance and color noise reduction | Full-resolution guided filter and Gaussian |
| Refine Edges and Auto Mask refinement | Full-resolution cross-guided filter |
| Supplied mask mattes and segmentation grids | Existing frame-to-source coordinates |
| Dehaze airlight | Full-output-resolution statistic on each renderer, including upstream processing and every-seventh-value selection |

## Full-frame airlight and mask reuse

Planning describes whether a window needs airlight and does no airlight pixel
work. Rendering resolves a separate `window=None` plan at the requested full
output dimensions. Its source sampling, camera local tone, white balance,
Defringe, spots, red-eye corrections, noise reduction and dark-channel blur are
the same as a full render. CPU and Metal compute their own scalar with their
existing every-seventh-value selection and 99.5th percentile. A whole-frame work
buffer can supply the statistic directly. A Metal full frame that exceeds its
buffer limit uses the bounded CPU statistic, matching the full-render fallback.
That device-limit path keeps separate GPU window buffers; it does not share
the full CPU preparation. The Metal measurements below fit the full buffers.

Each renderer keeps two scalar entries keyed by source identity and the full
linear-stage key. It retains one exact preparation up to 1 GiB. Frames whose
sampled RGB, processed RGB and dark channel together fit retain all three.
Larger frames retain only processed RGB and the dark channel (16 bytes per
pixel), and sample the work rectangle again. This keeps exact pans warm through
67,108,864 pixels. White balance and NR edits on those larger frames still need
another full preparation. The stage byte counters include retained buffers.

GPU memory trimming releases image buffers but retains the two weak-source
scalar entries. Exact windows can reuse a matching scalar after their work
entries and full preparation are gone. A matching retained preparation still
supplies the work rectangle when available. Source, geometry, white balance,
NR and other upstream changes invalidate the statistic through the full linear
key. Exposure and the Dehaze amount do not. A full cache reset still drops it.

Before allocating the full preparation, both renderers check a 4 GiB budget.
The CPU image-storage peak is 32 bytes per pixel without NR, 44 with luminance
NR, and 48 with colour NR. The GPU peak is 32 without NR, 48 with colour NR
alone, and 52 when luminance NR is active. The
estimator takes the maximum of sequential filtering, source orientation and
prefilter sampling, and copying the work rectangle. It includes row scratch,
Defringe's 52-byte peak and retouch scratch sized to the largest spot patch.
It excludes the caller's source, existing stage entries and the GPU buffer
pool, so it is not a process memory limit. Native NR consumes temporary buffers
and reuses coefficient storage; wasm32 retains main's existing allocations.

If the full exact preparation or its source exceeds these bounds, native CPU
and GPU windows use main's cached 384-pixel airlight proxy. The proxy cache
keeps four source and frame entries before white balance, as in e7cd00ef.
Exposure, Dehaze and NR changes reuse that sampled proxy; white balance still
runs on it. These windows retain full filter context, but approximate airlight
can differ from a full-render crop by more than one code value. Exact frames
keep the accuracy bounds below. A window work image itself remains limited to
67,108,864 pixels. If complete filter support exceeds that bound, planning keeps
main's bounded window margin. Such windows also approximate the missing filter
context and do not carry the exact path's one-code-value bound. Requested
windows that themselves exceed the work limit still report an error.
Full renders (`window=None`) keep their existing operators and pixels.

On wasm32, compile-time gates keep main's requested window, bounded spot reads,
whole-frame automatic source pick and white-balanced Dehaze proxy. Guided
filters use main's window-local reduced grids. Full renders and exports also
use main's sampling copies, scalar mask-refinement buffers and mask lifetimes;
workers retain sources and stage entries as main does. There is no render
admission estimator, cache-release retry or exact/approximate selection. Web
windows can still differ from full renders, and allocations that exceed the
wasm32 address space can still trap as on main. Fallible large allocations
would let a later change return recoverable errors without estimating every
allocation.

Detail keeps the preview when Region or RegionBefore fails and shows a small
status message suggesting a lower zoom. The message disappears at Fit or when
the failed render is replaced successfully.

Native `Session::region_job_of` first calls `render_job` with the full frame's
size and then adds the window. Source selection therefore matches the equivalent
full render, including a smaller preview source and crop-driven source selection.
`RenderJob::draft` changes quality, not the source. The web `WireJob` keeps the
source level, dimensions and window; `WorkerCore::render` passes the decoded
source and its metadata to the CPU pipeline with the wasm32 policy. The tests include a
601 by 403 source rendered into an 1800 by 1207 frame. On native this covers a source
smaller than the requested virtual frame without a second airlight proxy.

The CPU and GPU alpha caches incorporate #1147's implementation and tests
(`2d85c4a3` and `99b89cd9`). Each mask's key includes its components, component
operations and inversion, mask inversion, Amount, Refine Edges, and work-frame
geometry. Image-dependent masks also include the linear-stage key. Source
identity and supplied matte identity are retained by the containing caches.
Renaming, reordering and changing a mask's adjustment sliders other than Amount
do not change its alpha. Hidden and removed masks release their cached alphas.

| Alpha input | Image dependency | Exposure dependency |
|---|---|---|
| Linear, radial and ordinary brush | None | None |
| Auto Mask brush | Pre-exposure RGB and log luminance | None |
| Subject, Background and People heuristics | Pre-exposure log luminance | None |
| Sky heuristic | RGB and log luminance | Yes |
| Luminance and Color Range | Log luminance or RGB | Yes |
| Stored Object / Prompt segmentation | Stored segmentation and geometry | None |
| Refine Edges | Pre-exposure log luminance, in addition to the shape | No additional dependency |

The imported dependency classifier conservatively treats supplied mattes like
heuristics. A supplied Sky matte can therefore be rebuilt unnecessarily after
Exposure changes. This does not affect the three-mask probe, which has no mattes.
Refine Edges blurs independent channels together, preserving each channel's
scalar pass order and band origins. Tests compare finite values and signed
zero by their bits, and require matching finite, infinite and NaN classes. NaN
payloads may differ. It reuses the guide's means
when the image and radius are unchanged, and a geometric mask's blurred mean
across WB/NR edits. Refinement intermediates have a 512 MiB limit per stage
entry, at most two entries per view. They are counted, invalidated by their
inputs and released with the stage cache. `gaussian_owned` consumes disposable inputs instead of
cloning them. Its arithmetic and blur order are unchanged. Native sampling with
no orientation, prefilter or transform shares the source Arc instead of caching
a duplicate RGB buffer. `StageCache::bytes` counts owned chroma and alpha buffers
and excludes this shared source, which belongs to the caller.

The V2 median updates its sorted window in place instead of removing and
reinserting an element, and copies transpose tiles in blocks of 32. Both changes
preserve the selected float bits, including signed zero and NaN payloads. Tests
compare each median with an independently sorted neighborhood. These operations
were a large part of the expanded tone-base cost in the refined-mask case.

## Known limits

Final window pixels can differ by a code value because floating-point blur sums
start at different places. The measured bounds apply to the regression matrix;
there is no universal bit-identical final-pixel guarantee. Full renders retain
their existing V1/V2 output. Automatic spot sources still use the existing window
source-selection policy, which can differ from a full render's selection.

The all-filters case still needs a full-frame work rectangle. Exposure changes
must recompute the Sky heuristic and its refinement because its alpha changes.
Radial and Subject refined alphas remain reusable. Measurements below separate
that remaining work from the resolved airlight mismatch.

## Current validation (2026-10-12)

The allocation regression executes a 400 by 300 window in a 48,000 by 32,000
virtual frame under a 1 GiB allocation guard. On `8170b9dd`, it requested
18,432,000,000 bytes and exited 77. It now returns a recoverable error on CPU
and Metal before that allocation. Native and serialized worker jobs also test
an error followed by a smaller successful request. A separate regression saw
two upstream preparations per edit on `8170b9dd`; it now sees one.

All 222 full-render hashes match fresh main `25ef65c8` and the preserved main
references. The expanded matrix passes 490 partial windows and 98 whole-frame
windows per renderer. The original matrix passes 288 partial windows and 32
whole-frame windows per renderer. Every partial window is within 1 code value;
every whole-frame window is exact. CPU/Metal parity retains max 2 in the expanded
matrix and max 1 in the original matrix. Cache probes cover upstream edits,
framing, source identity, supplied mattes and enabling/disabling Dehaze.

The focused release run passed 261 tests, with three existing ignored tests.
All 11 worker wire tests passed, including the oversized-request retry. Workspace
tests reproduced exactly the 12 known failures. Formatting, clippy, HEIF, parity,
layers, assets and all 28 WASM checks passed with the temporary #990/#997 patches,
then those patches were restored byte for byte.

The following timings are indicative shared-machine results on an Apple M3 Max
(14 CPU cores, 36 GiB RAM), with two Rayon threads and nice level 15. The 24 MP
procedural scene, viewport and snapped UI margin are the review probes'. Three
serial interleaved process runs use fresh release builds. Values are medians of
the run medians. Each cell lists main `25ef65c8`, `8170b9dd`, then this change.
Times are milliseconds. Decode, display presentation and Draft feedback are excluded.

| Case | Renderer | Zoom | Cold | Exposure | White balance |
|---|---|---|---:|---:|---:|
| Dehaze 15 | CPU | 1:1 | 453.2, 596.4, 492.1 | 334.3, 309.0, 307.4 | 412.9, 531.6, 457.0 |
| Dehaze 15 | CPU | 2:1 | 226.8, 357.7, 310.8 | 171.5, 141.3, 141.8 | 205.5, 304.0, 267.2 |
| Dehaze 15 | Metal | 1:1 | 113.8, 167.3, 121.4 | 40.9, 8.7, 8.9 | 57.5, 106.7, 63.3 |
| Dehaze 15 | Metal | 2:1 | 96.1, 143.2, 106.2 | 37.0, 5.1, 5.2 | 45.7, 86.0, 48.9 |
| One refined mask | CPU | 1:1 | 1539.4, 2130.1, 1728.2 | 769.3, 491.5, 485.4 | 1428.7, 2056.2, 1568.0 |
| One refined mask | CPU | 2:1 | 689.7, 1171.0, 944.6 | 366.1, 229.6, 227.8 | 670.6, 1083.4, 841.2 |

Median starting 1, 5 and 15 minute load averages across the typical runs were:

| Revision | Load averages |
|---|---|
| main | 6.56 7.39 7.83 |
| before | 6.57 7.39 7.82 |
| head | 6.61 7.39 7.82 |

The 1.2x-main target is not met in every case. CPU Dehaze and the refined mask
still exceed it at 2:1. Full-frame NR and crop edits remain slower, even after
sharing removes repeated preparation. In the added single-run 2:1 sequence,
CPU NR is 324.5, 858.7 and 625.9 ms; Metal NR is 60.8, 171.7 and 100.4 ms.
The exact statistic needs the full frame's upstream NR, including outside the
requested window. Warm Exposure remains faster than main in the typical cases.

The single-run 61 MP all-filters CPU case is 3,776, 13,910 and 15,272 ms cold,
with sequence peaks of 2.09, 6.62 and 8.18 GiB. Grouped temporary buffers and
retained refinement guidance add memory. This case still expands to the full
frame and recomputes Sky refinement after Exposure changes. Direct Metal mask
timings omit general Refine Edges and are diagnostic only; the native engine
routes visible refined masks to CPU. Full tables include the peak footprint,
pan, NR/crop, 61 MP cases and each run's before/after load averages in the
implementation evidence. No counted performance run was paused by the disk guard.

## Earlier checks

Round 4 adds 168 windows per renderer at 2397 by 1598, including odd origins,
frame edges, repeating detail, hidden/empty masks, local Clarity, isolated tone
and Clarity, and NR with mask refinement. The tests assert max 1 and mean below
0.01 on both CPU and Metal. Measured maxima are 1 on both; worst means are
0.000896 CPU and 0.002456 Metal. CPU results are not universally bit-identical.
The resampling-only test is bit-identical at steps 2, 7, 11 and 16.

These regressions failed on `a4b0bbca` before the fix. The reported V2 edge window
had max 12 / mean about 0.582; it now has max 0 on CPU and max 1 / mean 0.000266
on Metal. The reported V1 stripe center had max 9 / mean 3.798898; it is now exact
on both. The wider regression matrix previously reached max 13 and mean 4.589532.

The unchanged independent round 3 probe also passes all 288 windows per renderer
within max 1. Worst means are 0.000100 CPU, 0.001593 Metal and 0.002257 CPU/Metal
parity. All 32 whole-frame windows per renderer are exact. All 26 full-render
reference hashes still match main `76b53778`; round 4 evidence is retained in
`scratch/dehaze-round4/`.

The following coverage and CI record describe round 3.

`tests_window` covers the original 2400 by 1600 blocks experiment, tight windows
at each corner and at a block boundary, both process versions, combined global
and local edits, upstream noise reduction, refined masks, and deep output sizes.
Round 2 adds supplied Subject, Background, Sky and People mattes, hidden mask
context, the Defringe boundary repro, and context around spot source reads. All four new regressions failed on `a9d23dc0`: matte context
was 654 by 554 instead of 400 by 300; hidden context was 562 by 462; Defringe had
max 75 and mean 0.879; the spot source had no extra Defringe support.

All 33 window tests and both new compatibility integration tests pass. The
pipeline library has 188 passing tests and two existing ignored tests. All 19
Metal equivalence tests pass without skips. Ten original V1/V2 full CPU renders
and eight new CPU renders with hidden or empty masks match main `76b53778` by
exact hashes. Eight new Metal full-render hashes also match that main revision
on the same GPU. The finish-stage test compares the requested pixels byte for
byte across 8-bit, 16-bit and float output.

Round 3 restores main's plane selection after the hidden-mask optimization
changed full-render Clarity. Eight new 1200 by 800 CPU reference hashes cover V1
and V2, hidden and empty tone masks, and global and local Clarity. Separate
window regressions check the base's full support at a scale without guided-filter
subsampling. The HDR finish case enables HDR and requires samples above 1 before
comparing requested-pixel finishing with full finishing and cropping.

The GPU equivalence suite covers the same local operators on Metal, comparing
both against the CPU window and against the corresponding full-render region.

CI was run with the temporary PR #990 and #997 patches, then those patches were
removed. Formatting, clippy and HEIF passed. Workspace tests reproduced exactly
the twelve known failures in #989, with no additional failing test names. Parity
references, layer rules, asset attribution and all 28 WASM package checks passed.

Reproduce window timings with:

```sh
CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2 RAYON_NUM_THREADS=2 N=5 \
  CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=../target-dehaze \
  nice -n 15 cargo run --release -p lightcraft-gpu --example window_context
```

The benchmark uses 6000 by 4000 and 9504 by 6336 procedural sources, a 2800 by
1700 physical-pixel canvas, and the current UI's native-resolution windows at
1:1 and 2:1. It reports a cold render and the median of warm Exposure edits,
separately for CPU and GPU, with Dehaze 100 alone and combined spatial edits.

## Performance measurements

These are historical round 2 measurements at `f9f64711`. Round 3 restores
main's render plane selection, so the hidden-mask timing and cache-size results
below do not describe the corrected renderer. No new performance claim is made.

Round 2 measurements compare `a9d23dc0` with these fixes on the Apple M3 Max,
36 GB RAM. All timings are indicative on this shared machine. Builds and runs
use `nice -n 15`, `CARGO_BUILD_JOBS=2`, `RUST_TEST_THREADS=2` and
`RAYON_NUM_THREADS=2`. Sources are procedural RGB images already in memory.
Cold means an empty stage cache. Warm means the median of five Exposure edits;
the reported value is the median of three runs. Peak footprint comes from
`/usr/bin/time -l`. Load averages are recorded with `uptime` around each run.

The 61 MP class source is 9504 by 6336 (60,217,344 pixels); its requested window
is 4096 by 2816 at (2560, 1792). The 24 MP source is 6000 by 4000; its requested
window is 4096 by 3072 at (1024, 512). The review's 246 to 387 ms Dehaze figures
came from its 61 MP probe. Both source sizes are measured here explicitly.

Reproduce the CPU cases with `crates/pipeline/examples/window_context_cost.rs`:

```sh
export CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2 RAYON_NUM_THREADS=2
export CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=../target-dehaze
uptime
nice -n 15 cargo build --release -p lightcraft-pipeline --example window_context_cost
nice -n 15 /usr/bin/time -l ../target-dehaze/release/examples/window_context_cost matte
uptime
```

Use `hidden`, `dehaze24`, `dehaze`, or `all` for the other cases. The `all` case
has Dehaze 80, Clarity 80, Texture 60, Highlights -80, Shadows 80, luminance NR 70,
colour NR 60, sharpening 60, Purple Defringe 10, and radial, Subject and Sky masks
with Refine Edges 100. Its timing and footprint are one run, with five warm edits.
The masks also have local Exposure, Clarity, Texture, Noise, Moire and Defringe.

| Case | Work Mpx, old / new | Cold ms, old / new | Warm ms, old / new |
|---|---:|---:|---:|
| 61 MP, Subject matte + Exposure | 35.78 / 11.53 | 1216.0 / 435.3 | 978.0 / 338.9 |
| 61 MP, hidden spatial mask | 26.09 / 11.53 | 1858.9 / 290.5 | 567.5 / 219.4 |
| 24 MP, Dehaze 100 | 18.23 / 18.23 | 803.9 / 568.8 | 581.5 / 394.3 |
| 61 MP, Dehaze 100 | 20.70 / 20.70 | 941.6 / 675.7 | 709.0 / 410.7 |
| 61 MP, all spatial filters + 3 refined masks | 60.22 / 60.22 | 19638.5 / 14779.4 | 12128.0 / 8002.9 |

| Case | Peak footprint GiB, old / new | Historical `StageCache::bytes` GiB, old / new |
|---|---:|---:|
| 61 MP, Subject matte + Exposure | 1.92 / 1.06 | 0.93 / 0.30 |
| 61 MP, hidden spatial mask | 2.87 / 1.02 | 0.97 / 0.30 |
| 24 MP, Dehaze 100 | 0.97 / 0.97 | 0.54 / 0.54 |
| 61 MP, Dehaze 100 | 1.49 / 1.49 | 0.62 / 0.62 |
| 61 MP, all spatial filters + 3 refined masks | 7.97 / 7.97 | 2.47 / 2.47 |

Old is `a9d23dc0`; new includes all three review fixes and requested-pixel
finishing. Baseline runs precede fixed runs, so changing machine load is a
confounder. These are indicative times, not isolated speedup measurements.

The CPU batches recorded these `uptime` load averages (1, 5 and 15 minutes):

| Batch | Start | End |
|---|---|---|
| baseline | 7.00 16.46 22.61 | 9.79 13.76 20.44 |
| fixed | 4.59 8.41 15.36 | 8.56 8.02 14.40 |

These historical revisions used a full 60.22 Mpx work image and reached 7.97 GiB
peak footprint. Their `StageCache::bytes` counter reported 2.47 GiB but omitted
the 0.673 GiB RGB chroma plane, so the cached pixel buffers occupied about
3.14 GiB. The process footprint measurement is unaffected. Round 5 fixes this
counter and also counts cached mask alphas; its current results are separate.

### Historical round 2 GPU cost

One Metal run of the existing `lightcraft-gpu` example, `N=5`, with the same
two-thread limits. Warm values are medians of five Exposure edits. Combined
means Dehaze 100, Clarity 80, Texture 60, Highlights -80 and Shadows 80. It does
not include the three-mask worst case above. These are round 2 absolute costs;
no fresh GPU baseline comparison was made.

| Source | Zoom | Edits | Cold ms | Warm ms |
|---|---|---|---:|---:|
| 24 MP | 1:1 | Dehaze | 126.6 | 47.6 |
| 24 MP | 1:1 | Combined | 148.0 | 41.5 |
| 24 MP | 2:1 | Dehaze | 64.7 | 37.6 |
| 24 MP | 2:1 | Combined | 105.9 | 37.5 |
| 61 MP | 1:1 | Dehaze | 245.7 | 91.9 |
| 61 MP | 1:1 | Combined | 917.5 | 100.3 |
| 61 MP | 2:1 | Dehaze | 130.8 | 90.8 |
| 61 MP | 2:1 | Combined | 214.3 | 87.7 |

GPU benchmark `uptime` load averages were 8.51 8.02 14.36 at the start and
12.61 9.15 14.52 at the end.

## Round 5 measurements

Fresh release builds compare main `25ef65c8`, round 4 `824c2017`, and round 5.
Main's change from the original `76b53778` baseline is only the version bump.
The unchanged independent `review_probe all` ran serially on the Apple M3 Max
with 36 GiB RAM, two build jobs, two test threads, two Rayon threads and
`nice -n 15`. These timings are indicative on a shared machine. Each row is one
cold render and the median of five warm Exposure edits. Peak process footprint
comes from `/usr/bin/time -l`, not maximum resident size.

| Revision | Cold ms | Warm ms | Peak footprint GiB | Cache counter GiB |
|---|---:|---:|---:|---:|
| 25ef65c8 | 4164.8 | 2427.2 | 2.092 | 0.473 |
| 824c2017 | 15741.9 | 8269.1 | 7.977 | 2.468 |
| round5 | 15243.5 | 3142.1 | 6.609 | 3.141 |

The main and round 4 cache counters omit chroma. Their values are historical
`StageCache::bytes`, not total retained memory. Round 5 counts every owned pixel
buffer, including chroma and cached alphas. It excludes the shared source.

| Revision | Load averages before (1, 5, 15 min) | Load averages after |
|---|---|---|
| 25ef65c8 | 9.21 9.64 6.84 | 8.15 9.26 6.83 |
| 824c2017 | 8.39 9.24 6.89 | 6.43 8.30 6.77 |
| round5 | 5.86 6.76 6.69 | 6.33 6.77 6.70 |

Warm edits are 1.29x main, within the requested approximate 1.5x target. Peak
footprint falls by 1.37 GiB from round 4, but remains 3.16x main. Cold time is
3.66x main. The expanded work rectangle still has 60.22 Mpx, against main's
11.53 Mpx. All spatial stages must prepare that context initially. On an
Exposure edit the radial and Subject alphas remain cached; the Sky alpha and
its full-resolution refinement change and must run again. A concrete memory
follow-up is to share scratch buffers between these sequential mask blur passes;
round 6 retains the refinement guide's means with an explicit cache budget.
Neither change requires truncating support.

The separate 61 MP Dehaze-only run measured 962.8 ms cold, 340.3 ms warm and
2.836 GiB peak footprint, with loads 7.64 7.03 6.68 before and 7.64 7.03 6.68
after. It includes the first exact full-resolution statistic; subsequent Exposure
edits reuse it. This run preceded the owned-buffer optimization, which changes
only mask blur inputs and identity source sharing; its partial Dehaze window
uses neither optimization.

Round 5 regressions fail on `824c2017`: whole-frame Dehaze differs by max 48
and the reported partial window by max 47 on CPU and Metal, in both V1 and V2.
An unchanged refined alpha has a new allocation on every Exposure edit. The
chroma counter test reports 144 bytes where its two buffers require 288 bytes.

After the fix, the reported whole-frame window is exact on CPU and Metal.
The partial window is exact on CPU and has max 1 / mean 0.000597 on Metal.
The independent expanded probe checks 490 windows and 98 whole-frame cases per
renderer. All whole-frame cases are exact. All partial cases are within 1 code
value on each renderer, including all 70 Dehaze cases. Dehaze worst means are
0.000100 CPU and 0.002722 Metal; CPU/Metal Dehaze parity is max 1. The unchanged
non-Dehaze parity bound remains max 2, with worst mean 0.176707.

All 26 reference full-render hashes match the preserved main references in
`scratch/dehaze-round3/hash-comparison.json`. All 196 additional full-render
hashes match the round 4 review's main capture. No process version changes.
Evidence and exact commands are in `scratch/dehaze-round5/`.

## Round 5 validation

With the temporary #990/#997 patches applied, `cargo xtask ci` passed formatting,
workspace clippy and HEIF, then stopped at the 11 known engine failures.
`cargo test --workspace --no-fail-fast` completed with exactly the 12 known
failures, matching the round 4 failure names with no additions or omissions.
The final run passed 207 pipeline tests and 51 GPU tests, including all 24 Metal
equivalence tests with no skips. The workspace feature set includes five more
GPU unit tests than the focused package run. Existing ignored tests remain ignored.

Parity, dependency layers, asset attribution and all 28 WASM package checks
passed separately. Parity emitted its existing non-failing stale-summary notice.
The WASM engine check retained the existing `unused_mut` warning in `devices.rs`.
The three temporary patch files were restored byte for byte to the starting
commit. The disk guard paused parity once below 40 GiB and resumed at 54.81 GiB
on its first two-minute poll. Evidence is in `scratch/dehaze-round5/`.
