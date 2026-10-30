# Conventional noise reduction: shadow and parameter corrections

Reviewed 2026-10-28 against upstream `59a5d00c608dd4dc4b3c0d9cd4e914e81aa86ce5`.
Contribution for @schattie93.

## Pipeline and existing coverage

`raw::decode` produces sensor samples. `RawImage::normalized` subtracts black,
normalizes white and applies the active crop and sensor opcodes. `develop` demosaics
into camera RGB before the colour model supplies linear Rec.2020 to the rendering
pipeline. Canon CR2, including EOS 80D, takes this path. Conventional NR is later:
geometry/resampling, white balance and retouching, then `local::denoise`, before
local tone processing and the display stages. Thus conventional NR cannot undo
all demosaic colour artifacts. Optional AI Bayer denoise is a separate earlier
path with its own model/cache contract; it is unchanged here.

The current luminance filter is self-guided smoothing of log luminance, applied
as a multiplicative gain. The colour filter blurs RGB/Y and reapplies luminance.
Both CPU and GPU consume `local::nr_params`. Radii use original-image pixels;
preview epsilon scales with the squared resolution ratio. Existing tests cover
resolution scaling, window/whole-image agreement, means and CPU/GPU agreement.
`nrContrast` is exposed and imported but unused by this implementation.
The gitignored `plan/` instructions and `docs/pipeline.md` referenced by the
repository are absent from this checkout; this account follows the actual code.

## Methods considered

These are engineering assessments of the published methods, not benchmarks of
new implementations. Only papers/descriptions were consulted; no external source
implementation, model or image was incorporated.

| Method | Detail and colour behavior | Artifact risks | Cost / suitability |
|---|---|---|---|
| Guided filtering ([He, Sun, Tang](https://people.csail.mit.edu/kaiming/eccv10/index.html)) | Edge-aware, local linear model; already used for luminance | Fine low-contrast texture can be treated as noise; self-guidance can retain noise | Linear-time implementations; modest memory, good interactive baseline |
| Joint bilateral colour filtering ([Tomasi, Manduchi](https://users.cs.duke.edu/~tomasi/papers/tomasi/tomasiIccv98.pdf)) | Shared colour-distance weights can protect chroma edges absent in luminance | Noisy guides retain specks; large range tolerances bleed colours | Direct form scales with neighborhood area; requires CPU/GPU and halo work |
| Non-local means ([Buades, Coll, Morel](https://www.ipol.im/pub/art/2011/bcm_nlm/)) | Similar patches help retain recurring texture while averaging noise | Rare structures and mismatched patches may smear; needs noise calibration | Search and patch comparisons materially exceed the current local filter cost |
| BM3D ([Lebrun's analysis](https://www.ipol.im/pub/art/2012/l-bm3d/)) | Grouped patch transforms provide a stronger texture/noise separation | Grouping/threshold errors can produce structured artifacts and lose fine detail | Block matching and two-stage processing are too large for this bounded correction |

Open #1348 implements joint chroma, #1349 wavelet luminance, #1714 an alternative
conventional NR process, and #2598 colour-mean/shadow corrections. Replacing the
algorithm here would duplicate active work and require process-version
coordination. Those changes have not been imported. The selected corrections are
parameter-domain enforcement in the shared CPU/GPU parameter builder and removal of a luminance-denominator floor in global colour NR.
The latter overlaps a small part of #2598, without its weighted colour blur,
white-balance clamp changes or signed-input blending.

## Correction and compatibility

Global colour NR used `RGB / max(Y, 1e-6)` and multiplied the result by `Y`.
On a flat shadow below the floor this produces `RGB * Y / 1e-6`, even when
there is no colour noise at all. A flat grey of 1e-8 becomes 1e-10: 99% darker.
Use actual positive Y instead, substituting 1 only for non-positive Y to handle
black without division by zero. The render path already clamps negative channels
before NR, so RGB/Y is bounded by the positive Rec.2020 luminance coefficients;
small positive Y alone cannot create an unbounded chromaticity. This is the same
on CPU and in a dedicated `nr_chroma_k` GPU kernel. Local Moire/Noise keeps its
existing kernel and behavior. The separate kernel does not add another GPU pass.

All five consumed percentage settings are bounded to [0, 100] **before** casting,
radius calculation, squaring or square roots. Non-finite settings use zero.
As a result, luminance blend and colour blend remain within [0, 1], epsilon is
positive, and full-size sigma is at most 3.5 px (luminance) / 15 px (colour).
Negative Detail cannot amplify smoothing past the intended range; excessive
Detail cannot collapse epsilon to its numerical floor or make colour blending
negative. Amounts above 100 cannot extrapolate past the smoothed image and invert fine colour structure.

Valid slider settings retain exactly the old parameter arithmetic. Images with
positive luminance at or above 1e-6 throughout keep their old chromaticity arithmetic;
deeper shadows now avoid erroneous darkening. This correctness fix ships in place
under the process-version bug-fix rule. No serialization, slider range/default,
process, camera/decoder, warp or model changes. Saved settings are preserved.
`RENDER_CACHE_VERSION` takes 71 (was 70) to refresh existing thumbnails. Other open
PRs also claim 71; renumber against main if one lands first.

## Reproduction and measured scope

Run:

```sh
cargo test -p lightcraft-pipeline tests_nr_scale -- --nocapture
cargo test -p lightcraft-pipeline
cargo test -p lightcraft-raw --test it corpus:: -- --nocapture --skip _corpus::
cargo xtask ci
```

`excessive_nr_quality_comparison` creates a deterministic 192×128 scene with
fine sinusoidal texture, a luminance step, a colour boundary and independent
Gaussian channel noise. The pre-change parameters are explicitly evaluated by
`apply_nr`; the corrected result uses `denoise` at strength 400, keeping
Detail/Smoothness at 50. Both are compared against the known clean scene:

| Error (lower is better) | Before | Corrected |
|---|---:|---:|
| RMS log luminance (EV) | 0.280748 | 0.061043 |
| RMS chromaticity (R/sum, B/sum) | 0.209997 | 0.028527 |

These numbers demonstrate prevention of out-of-range oversmoothing/extrapolation,
**not a new normal-range denoising algorithm**. The separate shadow fix restores
luminance even at legal settings. Tests also require exact
endpoint equivalence on pixel-scale texture and two edge types, bounded finite
parameters at four preview scales, and unchanged legacy mapping at multiple
valid strengths/details. Existing resolution/window tests remain relevant.

`colour_nr_preserves_luminance_of_noisy_deep_shadows` uses independent Gaussian
channel noise around 1e-8. Chromaticity RMS against neutral drops from 0.078563
(noisy input) to 0.020608, while every pixel retains luminance within 2e-5 relative.
Flat neutral/coloured patches from 1e-12 through 0.18 and exact black are checked
at colour strengths 0.1, 25 and 100. Before this correction, the 1e-8 flat neutral
patch loses 99% of its luminance. The test can write before/after PNGs using the
real display pipeline with a common +20 EV diagnostic exposure (beyond the normal
UI exposure range, deliberately making the tiny values visible):

```sh
LIGHTCRAFT_NR_REPORT=/tmp/lightcraft-nr-report cargo test -p lightcraft-pipeline \
  colour_nr_preserves_luminance_of_noisy_deep_shadows -- --nocapture
```

The generated PNGs were inspected; the old result is nearly black, the corrected
one retains the shadow signal. These are procedural diagnostic images, not
real-camera quality reference renders. No media is committed.

The EOS 80D CC0 source is the existing `xtask/src/raw_samples.rs` entry:
`cr2-canon-80d.cr2`, raw.pixls.us file 1294, SHA-256
`8f28465cee09844ffad95dbd3ec91923717a329c5ec4f3f405f5b4fce83aaea3`.
A second existing CC0 reference, EOS 5D Mark III (raw.pixls.us file 771), uses
`cr2-canon-5d3.cr2`, SHA-256
`ec069b178b9383d80c72f8cdc1b79bd54ca451e7c20793b109d8ad73bef6bdf6`.
Both files are downloaded only to the gitignored corpus. Decoder tests check
sensor recognition, CFA and white balance on both; they are not high-ISO clean/noisy pairs.
No claim of camera-specific or Lightroom quality parity follows from those tests.

## Remaining limitations and overlap

Normal-range chroma-edge bleeding, noise estimation, high-ISO shadow bias,
Luminance Contrast, and demosaic-correlated noise remain algorithm work. No
high-ISO EOS 80D clean/noisy reference pair or measured native GPU performance is
supplied here. Bounds add five scalar checks per parameter calculation; the
shadow fix replaces the denominator calculation in the existing chroma pass.
There is no new per-pixel pass, image allocation or dependency.

The open NR PR file lists were checked: #1348, #1349 and #2598 overlap both
`local.rs` and `tests_nr_scale.rs`; #1714 overlaps `local.rs` and shared docs.
Their parameter builders must retain equivalent bounds if they land first.
#2598 already removes the denominator floor as part of its wider change; keep
that behavior and do not restore this narrower RGB/Y path over its weighted blur.
The GPU kernel registration/render call and cache version also overlap those PRs.
This small helper/call-site change does not claim textual merge independence.
RAW decoders and WarpFisheye are untouched. Shared documentation may conflict
with unrelated PRs; retain each PR's own entries when rebasing.

## Final validation on this branch

- CPU pipeline: 211 unit tests passed, two ignored diagnostics; two integration
  tests passed, one ignored. The final compiled test binaries were run directly
  while the unrelated CLI build held Cargo's artifact lock. The NR subset was
  also run through Cargo (12 passed).
- Regression proof: temporarily restoring upstream `local.rs`, while retaining
  the new tests, makes five NR regressions fail. Restoring the correction makes
  all 12 NR tests pass. No baseline code is retained in the branch.
- GPU crate: test command passed (52 non-ignored test functions, six ignored).
  Hardware-dependent cases, including the new equivalence case, explicitly skip
  because no adapter is available. The assembled global-NR WGSL module passes
  Naga parsing and validation without an adapter. Native execution remains unverified.
- Scoped all-target Clippy for pipeline/GPU passes with Rust 1.95.0; formatting,
  parity (including regenerated summary), layers and assets pass.
- `cargo xtask ci` was run again on the final implementation: formatting passes,
  then workspace Clippy fails on unchanged `crates/engine/src/cmd/export.rs:41`
  (`clippy::nonminimal_bool`). That file is byte-identical to upstream. The gate
  stops there, so its HEIF, workspace-test and WASM steps are not claimed as passed.
  No lint suppression or unrelated cleanup from #3198 is included.
- Both pinned Canon files decode; the existing CFA and as-shot white-balance
  tests pass on both. Other missing corpus entries skip explicitly.
- Ran the actual CLI on both files and inspected its 1600×1067 PNG exports with
  Luminance 40 / Colour 50, CPU forced. Tree bark, fine twigs, brick edges (80D)
  and wall/metal texture (5D III) remain visible. These are smoke checks, not
  clean-reference measurements. Development-build wall times on this 2-CPU host
  were 2.235 s (80D) and 3.421 s (5D III), including decode/export; these are not
  before/after performance comparisons.

Reproduce the camera smoke checks with the pinned files downloaded above:

```sh
LIGHTCRAFT_GPU=0 cargo run -p lightcraft-cli -- render corpus/raw/cr2-canon-80d.cr2 \
  -o /tmp/eos-80d.png --size 1600 --set detail.nrLuminance=40 --set detail.nrColor=50
LIGHTCRAFT_GPU=0 cargo run -p lightcraft-cli -- render corpus/raw/cr2-canon-5d3.cr2 \
  -o /tmp/eos-5d3.png --size 1600 --set detail.nrLuminance=40 --set detail.nrColor=50
```

## Publication result

The implementation is committed and pushed to
[schattie93/lightcraft, fix/nr-shadow-luminance-bounds](https://github.com/schattie93/lightcraft/tree/fix/nr-shadow-luminance-bounds).
Only @schattie93 is credited as the commit author.

Creating the intended draft PR against `storytold/lightcraft:main` was attempted
through the connected GitHub tool, `gh pr create` (GraphQL), and the REST
`POST /repos/storytold/lightcraft/pulls` endpoint. All three return HTTP 403 /
`Resource not accessible by integration`. The environment's authenticated
`GET /user` confirms `schattie93`, and pushing to the fork succeeds. No PR was
created; the remaining blocker is the integration's permission on the upstream
PR-creation operation. No credentials or repository permissions were changed.

[Review the branch / open the PR comparison](https://github.com/storytold/lightcraft/compare/main...schattie93:fix/nr-shadow-luminance-bounds?expand=1).
The intended PR should remain a draft while the unchanged upstream Clippy failure
prevents a green full gate. Its prepared title is
`LR-EDIT-DETAIL-CNR: preserve deep shadows and bound noise reduction`.

Merge-tree checks against the fetched open PR heads found these conflicts:
#1348 / #1349: parity and What's New docs only; #1714: `local.rs`, gaps and parity
among this branch's files; #2598: `local.rs`, GPU registration/render/map shader,
and parity. These are conflict checks, not successful integration tests.
