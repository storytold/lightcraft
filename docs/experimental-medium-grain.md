# Medium particle grain

This change replaces grain rendering with **signed particle grain**, an artistic
approximation, not a port of the Boolean disk reference, calibrated color film, or
commercial-renderer parity. The old CPU grid-noise
and GPU grain kernels are removed. Existing grain edits intentionally change appearance.

## Try it

Edit → Effects → Grain. Start with **Amount 40, Size 25, Roughness 0**, seed 2026 for
repeatable comparisons. There is no process selector and no CLI/JSON way to select the old
renderer. New imports and reset use Amount 0, Size 25, Roughness 0; the Roughness control's
reset default is also 0. Amount 0 is an exact no-op. The Effects eye disables grain.
Numeric sliders remain the existing develop controls, using normal undo/history, sync,
cache invalidation and save paths. No alternate settings store is introduced.

Partial settings JSON (also accepted by CLI `render --settings medium.json`):

```json
{"grain":{"process":"mediumParticlesV1","amount":40,"size":25,"roughness":0,"seed":2026}}
```

`grain.process` absent, `legacy`, and `mediumParticlesV1` all load as the one current
`mediumParticlesV1` process. Unknown strings are rejected. Saving always writes the canonical
name, changing cache keys relative to old settings so stale old-grain pixels are not reused.
Existing Amount, Size, Roughness and seed values load unchanged (including Roughness 50);
their old appearance is deliberately not preserved. Numeric-only partials retain untouched
settings. Old and experimental presets interpolate numerics at every amount, including
unrelated Light settings; integer seeds retain their categorical 50% threshold. No special
process-transfer representation or old-preset switching logic remains. This grain process is
independent of the unchanged global V1/V2 tone process. Older released applications ignore
this grain-process field and render their own old grain; do not round-trip experimental edits
through them. Back up experimental settings before switching builds.

## Design and differences from the reference

Original clean-room Rust, no third-party algorithm implementation or new runtime dependency.
The reference is a tone-dependent Poisson **union of disks**, Gaussian-integrated by Monte
Carlo, with shared encoded RGB finishing residual. This experiment instead sums **signed
Gaussian particle footprints**: one independently jittered continuous position per 2×2
material cell, equiprobable signs, coordinate-keyed hashes and per-photo seed. The lattice
only indexes particles; this is not interpolated grid-value noise. No Poisson counts, disk
union, void-probability inversion or Monte Carlo occupancy evaluations are performed.

At Size 25, native material scale is 1 original pixel and the Gaussian per-axis variance is
`.8² + .65²/4`: the selected reference aperture plus the disk's second spatial moment.
This matches a footprint moment, **not the reference's occupancy covariance, spectrum or
morphology**. Normalized Gaussian footprints at density 1/4 and Amount 40 (gain .040)
produce a similar midtone residual strength. The residual is shared by encoded RGB channels,
modulated by `sqrt(4u(1-u))` using encoded luminance, before profile LUT application. That
modulation is an artistic approximation to tone-dependent occupancy variance, not a physical
exposure/density model. Clipping, output profiles and LUTs may change the observed residual.

Size maps to material scale `(.25 + .016*size)/.65`. Roughness jitters particle footprint
width by ±half its normalized value; 0 keeps the constant-width Medium target. There is no
claim that the legacy Size or Roughness settings have equivalent meaning in this new process.
The same seed is repeatable within this backend, but intentionally does not reproduce the
reference's realization (different random process). No cross-platform bitwise guarantee.

Absolute oriented full-image coordinates anchor the material across crops/windows/tiles.
`SourceInfo.native_long` retains original-pixel scale when the app supplies a smaller source.
Reduction adds Gaussian variance `max(pitch²-1,0)/12`, an approximate pixel-aperture filter;
it does not renormalize visible noise back to native strength. Gaussian tails truncate at
3.5 sigma. At material pitches 24–32 grain smoothly fades out, and at ≥32 it is omitted,
bounding work for arbitrarily large native sources at tiny preview sizes. This is an explicit
extreme-reduction approximation, not exact area integration. Upsampling retains the native
Gaussian aperture. Grain follows the existing oriented film plane, not optical distortion.

CPU only: `lightcraft_gpu::try_render` explicitly declines every active nonzero grain
request, and existing callers use the CPU renderer. The old WGSL kernel, hash constants and
grain parameter plumbing are removed. Zero amount and disabled Effects can still use the GPU
normally. 8-bit, 16-bit and float exports share the CPU finishing path.
The GPU capability heuristic used to size drag drafts is unchanged, so drafts may be slower
than a GPU-supported edit; this is not an interactive frame-rate guarantee.

No additional image-sized buffers or cached particle arrays: each output pixel queries only
the bounded neighborhood of deterministic cells. Native source decode and existing pipeline
buffers still dominate export memory. Very large exports remain subject to existing pipeline
resource limits; this does not harden all pre-existing render allocation paths.

## Validation

Run the repository gates from the checkout root:

```sh
cargo xtask ci
cargo test --release -p lightcraft-engine medium_grain -- --nocapture
cargo test --release -p lightcraft-pipeline -p lightcraft-gpu --test medium_grain -- --nocapture
```

`cargo xtask ci` passed all eight gates on macOS arm64 with Rust 1.97.1: formatting,
strict workspace/all-targets Clippy, optional HEIF build/tests, workspace tests, parity
references, layering, assets and all 29 WASM package checks. Workspace summaries counted
4,186 passed, zero failed and 35 ignored tests across 115 test/doc-test groups. The CI
report additionally records 165 availability skips (137 corpus, 20 fonts and eight model);
these tests return successfully without performing their unavailable checks. Metal GPU
was available; the grain GPU-eligibility tests did not availability-skip. WASM validation
is a compile check, not a browser runtime or performance test. Windows/Linux execution
and optional corpus/font/model coverage were not established by this run.

`cargo xtask ci` writes `target/ci/report.json` and `target/ci/summary.md` with
step results, hardware capabilities and recorded availability skips. A successful test
summary does not imply every optional GPU, corpus, font or model path ran. Optional
`craft-fonts` must be supplied separately for their glyph coverage and dependent tests.

`crates/pipeline/tests/medium_grain.rs` exercises real render/export output and saved
settings. Its seven tests cover missing/old/experimental process migration to identical
particle pixels, numeric preservation, settings round trip, old/experimental presets at
0/25/49/50/100/200% with unrelated exposure, partial numeric merges, canonical cache keys,
determinism, seed changes, zero/disabled identity, exact pixel-aligned crops/windows,
native-scale preview sources, filtered reduction variance and 16-bit/float output.
GPU integration tests check refusal for active migrated grain and actual GPU rendering
for disabled/zero grain, or explicitly record GPU unavailability. Engine tests cover
copy/paste, manual sync, presets and numeric Auto Sync with undo/redo. Other GPU effects
retain their equivalence coverage; active grain no longer claims GPU equivalence.

These are regression checks, not proof of chemical accuracy, universal texture fidelity,
exact downsampling equivalence or interactive performance. No private photographs,
reference implementation or comparison images are required by these tests or included
in this contribution.

## Performance and reproduction

Active grain forces the entire request onto the existing CPU fallback, even when a GPU
is available. This may regress interactive drafts and full-resolution exports relative
to the previous GPU path. No additional image-sized particle buffers are allocated,
but the normal decode and pipeline buffers remain.

Earlier single-run measurements on an Apple M1 Max, using a private 7728×5152 source,
forced CPU rendering and concurrent test compilation, were:

| render | output | finish ms | total wall s | max RSS bytes |
|---|---|---:|---:|---:|
| Particle preview | 2000×1333 | 153.7 | 2.05 | 1,888,501,760 |
| Particle native | 7728×5152 | 5225.5 | 10.96 | 2,564,030,464 |
| No-grain native | 7728×5152 | 1194.2 | 6.51 | 2,559,803,392 |

These are observations, not controlled latency distributions or a public benchmark
fixture. Both native runs miss the repository's ≤1s/24MP export aspiration. They do not
establish GPU speed, cross-platform performance or a reference-renderer speedup.

To measure your own source, build the CLI, save the settings JSON above as `medium.json`,
and render to a disposable output directory. The following uses the default target directory;
adjust `BIN` if you set `CARGO_TARGET_DIR`. `/usr/bin/time -l` is specific to macOS.

```sh
cargo build --release -p lightcraft-cli
BIN=target/release/lightcraft-cli
INPUT=/path/to/input.dng
OUT=target/grain-comparison
mkdir -p "$OUT"
LIGHTCRAFT_CONFIG_DIR="$OUT/config" LIGHTCRAFT_GPU=0 LIGHTCRAFT_PROFILE=1 \
  /usr/bin/time -l "$BIN" render "$INPUT" \
  --settings medium.json --size 2000 -o "$OUT/particle-preview.png"
# Native comparison: omit --size; no-grain baseline: replace --settings with --set grain.amount=0.
```

Keep source, dimensions, build, CPU/GPU choice and machine load fixed when comparing runs.
The private source and outputs used for the observations above are not distributed.
