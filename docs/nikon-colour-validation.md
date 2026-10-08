# Nikon colour pipeline validation — 2026-10-08

**The framework is implemented. No Nikon camera has been measured/calibrated by this change.**
No Z 8 profile is generated or bundled. There are no real warm/daylight chart captures, measured patch
references or independent real-camera holdouts in this session. Consequently no measured Nikon ΔE,
absolute Kelvin accuracy, spectral accuracy or Lightroom parity is claimed.

## Private sample, read-only

The user supplied `_DSC2360.NEF` and `_DSC2360.JPG`. Testing used local copies; neither originals nor
media/screen captures were committed or uploaded. Inspection of the NEF through `chart inspect`:

| Item | Observed result |
|---|---|
| Camera / decode | NIKON CORPORATION / NIKON Z 8; 14-bit NEF decoded successfully |
| Full sensor | 8280 × 5520 |
| Nikon black levels | 1008 at all four CFA sites |
| Stable white | 16383, code-range estimate; no measured per-body saturation claim |
| Nikon CropArea | left 12, top 8, width 8256, height 5504 |
| Camera WB gains | R 1.998046875, G 1, B 1.419921875 |
| Usable standard file calibration | None |

The camera's WB coefficients alone do not identify capture Kelvin. Base/Match Camera therefore
report `status: estimated`, `relativeWB: true`, `model: null`, and preserve the old source interpretation.
The fallback deliberately retains legacy geometry and levels too; its full-size export is **8280 × 5520**.
The stable **8256 × 5504** crop belongs to the stable decoder/chart path and calibrated rendering.
This sample has not been used with an invented Z 8 profile to manufacture a “calibrated” comparison.

## Actual rendered checks

The baseline CLI was rebuilt from the exact requested **d614dc7** checkout, with the same Rust toolchain,
and the original checkout remained clean. PNGs were compared as decoded pixels (metadata excluded).

| Check | Result |
|---|---|
| Legacy NEF As Shot, 1200 × 800 | New and d614dc7: max/mean pixel difference **0** |
| Legacy NEF custom 8000 / −40, Exposure +0.4, 1200 × 800 | New and d614dc7: max/mean difference **0** |
| Supplied JPEG, 1200 × 800 | New and d614dc7: max/mean difference **0** |
| Base without profile vs Legacy | Max/mean difference **0** |
| Match Camera without profile vs Legacy | Max/mean difference **0**; explicitly labelled fallback |
| Real NEF CPU/GPU, 8000 / Tint −40, 0, +40 | All three pairs: max **1 LSB**, mean < **0.00004 LSB** in 8-bit RGB |
| Tint direction in native UI/exports | Negative adds green; positive adds magenta |
| Exposure −1 | Output changes; mean absolute difference from As Shot ≈36.77 LSB in RGB |
| Full-size fallback JPEG export | 8280 × 5520, 6,763,099 bytes; completed |

The preserved legacy sample has pale/bright highlights and is not proof of accurate colour. This
change does not relabel that rendering as calibrated or claim it improves the supplied photo's fidelity.
“Match camera JPEG” on a valid calibrated source fits a guarded tone/chroma curve; it is approximate and
may be rejected. No real calibrated Z 8 rendering was available to evaluate that mode's visual accuracy.

The native desktop ran on **Apple M3 / Metal**, isolated in-memory session, control port 18763.
Control-channel screenshots were inspected for the mode selector, readable estimate notice and WB
behaviour. Every recorded settled view reported `loupe.source: render`, no pending slots and no GPU
fallback. The ordinary embedded-JPEG stand-in was not counted as a completed render. Under concurrent
build/test load, changing interpretation required roughly 1.3–1.8 s to decode; edited renders were around
30–34 ms. These are observations, **not a controlled performance benchmark or a 16 ms slider claim**.

## Regression evidence

The procedural tests cover:

- Nikon 12-/14-bit black levels, constant white across bright-scene plateaus, bounds-checked/odd crop
  coordinates, preserved CFA phase and Nikon WB coefficients.
- Two-light least-squares fitting, reciprocal-temperature interpolation, independent synthetic
  exposures, neutral grey ramps, model-based WB, positive/negative Tint and EV gain.
- Standard reference-camera / analog balance / forward-matrix semantics and calibration-signature
  matching; file calibration priority;
  camera-mismatch rejection; singular/ill-conditioned/hostile input rejection.
- Training/validation SHA-256 separation, duplicate-capture rejection, missing lighting coverage in
  reports, chart sampling before WB/demosaic, and the real `chart fit` / `chart validate` binaries.
- Missing-field legacy defaults, undo restoring old settings/pixels, stale render-job rejection,
  preset strength not interpolating sensor calibration, and calibrated smart-preview metadata.
- Calibrated CPU/GPU comparisons at three WB/EV settings and two sizes, plus shared preview/U16-export
  conversion. Procedural held-out chart errors <0.05 ΔE76 verify the implementation's known analytical
  model; they are not measurements of any real camera.

Final gate: **`cargo xtask ci` passed all seven stages** (fmt, Clippy with warnings denied, workspace
tests, parity, layers, asset audit and WASM). The final log totals **1088 passed, 0 failed, 10 ignored**.
Toolchain: rustc 1.99.0 (`b940084d7`), cargo 1.99.0, aarch64 macOS; `wasm32-unknown-unknown` installed.
The optional craft-fonts checkout was unavailable; no CJK-font test coverage is claimed. Private logs,
pixel comparisons and UI captures remain under the worktree's gitignored `plan/nikon-color/` directory.

An intermediate complete CI run passed all seven stages. A subsequent run hit the pre-existing SAM
`waiting_requests_with_a_damaged_model_are_errors` busy-state assertion while a full-resolution photo
export also ran. The SAM code/test was not changed; its isolated recheck and the final complete run both
passed. Stale workspace build artifacts from the baseline comparison were cleaned before the final
run. Intermediate build/Clippy errors were repaired before delivery; no checks were disabled.
