# lightcraft-denoise-core

L1 model metadata, bounded archive/cache formats and Bayer packing/tiling helpers.
No weights or inference backend. See [the user workflow](../../docs/denoise.md).
`lightcraft-denoise` re-exports these types for CPU/GPU users; the engine uses this
crate directly when inference is disabled. The default download catalog is empty.
Run `cargo test -p lightcraft-denoise-core`; optional catalog checks use `--features rawnind-model`.

The Bayer runner rejects non-finite source samples before inference and non-finite
results after gain matching, highlight protection and tile blending. Finite negative
and HDR source samples remain supported. These guards do not implement Raw Details
or Super Resolution.
