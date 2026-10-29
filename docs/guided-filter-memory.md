# CPU fast guided-filter memory

`local::guided_fast` computes blurred coefficients on the same reduced grid as before, then
samples them while applying `a * p + b`. It keeps the raster resizer's weights, boundary
normalization and horizontal-then-vertical accumulation order. Odd dimensions therefore use
their actual reduced/full dimension ratios. Four coefficient row pairs and one bias row are cached
per active 32-row band; adjacent output rows reuse their horizontal samples.

The two full-resolution coefficient planes and their resize intermediates are gone. The returned
full-resolution base remains: downstream CPU tone mapping and the spatial-plane cache need it.
The step-1 filter, Box reduction, Gaussian coefficients, V2's median input, process selection and
Draft sigma cap are unchanged. The GPU implementation is unchanged; this reduces CPU memory only.

## Reproduce

The dev-only example includes the previous materialized algorithm and uses the workspace's
existing `dhat` allocator. No allocator or profiling dependency is added to product builds.

```sh
cargo build --release -p lightcraft-pipeline --example guided_memory
RAYON_NUM_THREADS=1 target/release/examples/guided_memory old 6000 4000 120 15
RAYON_NUM_THREADS=1 target/release/examples/guided_memory new 6000 4000 120 15
RAYON_NUM_THREADS=1 target/release/examples/guided_memory old 6000 4000 120 1 --heap
RAYON_NUM_THREADS=1 target/release/examples/guided_memory new 6000 4000 120 1 --heap
```

If `CARGO_TARGET_DIR` is set, use that directory instead of `target` for the executable.
Arguments are mode, width, height, integer sigma and timing iterations. The example warms up
before measurement; `--heap` counts one filter call with the source allocated beforehand.
Time the runs without `--heap`: allocation profiling significantly changes timings. It reports
the minimum wall time; on a shared machine repeat when other builds/renders have stopped.
Use `LIGHTCRAFT_PROFILE=1` and `cargo xtask bench` for broader pipeline measurements.

## Observed allocation and time

Measured 2026-10-16 UTC, macOS aarch64, Rust 1.96.0, release build, one Rayon thread. Bytes are
requested heap bytes counted by dhat, not process RSS. Timings are minimums of 15 unprofiled
calls on a shared machine with concurrent builds; they are indicative, not a speed guarantee.

| Dimensions / sigma / step | Old peak bytes | New peak bytes | Old total allocated bytes | New total allocated bytes | Old / new wall ms |
|---|---:|---:|---:|---:|---:|
| 6000 × 4000 / 120 / 16 | 289,125,000 | 97,780,856 | 313,083,488 | 135,643,856 | 42.135 / 45.131 |
| 1601 × 1067 / 24 / 8 | 20,822,412 | 7,331,172 | 25,102,604 | 11,562,560 | 3.359 / 3.121 |

The 24 MP result saves about 191 MB of peak allocation (66%). Its **96,000,000-byte output is
included**; its **96,000,000-byte input is excluded**. Application RGB images, log luminance,
V2 median input and its construction temporaries, other spatial planes and caches also remain
outside this isolated measurement. More Rayon threads can hold more band scratch concurrently.
This is not a bound on total pipeline memory.

Regression tests compare the sampled result exactly against materialized bilinear resizes for
empty/singleton/one-dimensional, odd/even and multiple-band images; flat fields, signed texture,
edges and impulses; small/full and reduced filters; and median tone bases.
