# dac-denoise-core

L1 model metadata, bounded archive/cache formats and Bayer packing/tiling helpers.
No weights or inference backend. See [the user workflow](../../docs/denoise.md).
`dac-denoise` re-exports these types for CPU/GPU users; the engine uses this
crate directly when inference is disabled. The default download catalog is empty.
Run `cargo test -p dac-denoise-core`; optional catalog checks use `--features rawnind-model`.
