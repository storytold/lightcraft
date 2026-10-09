# dac-denoise

L1 pure-Rust AI RAW denoise primitives and CPU inference. No weights are bundled.
See [user workflow and limitations](../../docs/denoise.md).

`dac-denoise-core` provides metadata, Bayer helpers and cache formats,
re-exported here. `manifest` and `known` validate opt-in models; `onnx` reads a bounded operator subset
into `net`; `cpu` executes it and `reference` supplies scalar checks. `bayer`, `tiles`
and `run` pack sensor data and blend overlapping tiles while preserving clipped
highlights. `product` reads/writes the disposable `.lcdn` cache. `runtime` supplies
the installation self-test. Synthetic networks are generated in Rust for tests.

Run `cargo test -p dac-denoise --features runtime`. The CPU backend is optional;
featureless and web builds keep the model metadata and cache format without faer/GEMM.
The `rawnind-model` feature enables the otherwise empty download catalog and requires
an explicit release-policy decision. Real-model tests are ignored by default and
require an explicitly supplied local model. The GPU twin is `dac-gpu::nn`;
model management, background jobs and export integration live in `dac-engine`.
