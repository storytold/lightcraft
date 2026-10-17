# lcc-filters

Sandboxed WebAssembly pixel filters: a host for **PhotoCraft plug-in ABI v1** filter modules,
so filters written for PhotoCraft run unchanged in the app's develop pipeline.

Layer L1 (depends on no app crate). Builds for native and `wasm32`.

## What a filter is

A WebAssembly module with **no imports** that exports:

| export | signature | meaning |
| --- | --- | --- |
| `memory` | memory | its linear memory |
| `pc_abi_version` | `() -> i32` | must return `1` |
| `pc_manifest` | `() -> i64` | `len << 32 \| ptr` of the UTF-8 JSON manifest |
| `pc_alloc` | `(i32) -> i32` | a block of `n` bytes (0 = out of memory) |
| `pc_filter` | `(buf, len, w, h, channels, format, params_ptr, params_len) -> i32` | filters `buf` in place; 0 = success |

The manifest: `{"id", "name", "version", "kind": "filter", "description", "author", "params":
{name: spec}, "overlap", "area"}`, where a spec is `number {min,max,default}`, `int`, `bool` or
`choice {options, default}`. The full ABI is in PhotoCraft's `docs/plugins.md`.

The host hands over interleaved, linear **RGB `f32`** (3 channels, no alpha; `format` = depth 32
\| mode 3 << 16) and the resolved parameters as JSON with an `_image` object: `x`, `y` (the
band's top-left in the full image, overlap included), `width`, `height`, `overlap`,
`canvasWidth`, `canvasHeight`, `mode: "rgb"`, `depth: 32`, `alpha: false`, `channels: 3`.

## API

- `Filter::load(bytes, Limits) -> Result<Filter>`: validates the module and reads its manifest;
  `module_hash()` is the SHA-256 of the bytes.
- `Manifest::resolve_params(&Value) -> Result<Map>`: defaults, clamping, type errors.
- `Filter::apply_rgb(&mut [f32], w, h, FrameInfo, &Value, &AtomicBool) -> Result<()>`.
- `is_filter_module(bytes) -> bool`: a cheap export-section sniff, so the general plug-in
  manager can route PhotoCraft-ABI modules here.
- `Registry`: install from bytes or a path, remove, list, get by id, load a folder of `*.wasm`.

## Sandbox

wasmi (pure-Rust interpreter), no host imports, fuel per call (base + per sample) metered in
slices with a wall-clock deadline (native) and the caller's cancel flag checked between slices
and bands, a linear-memory cap, a recursion-depth cap, a fresh instance per band. Bands are
about `Limits::band_bytes` (4 MiB), padded by the manifest's `overlap` with the buffer's edge
pixels repeated, run with rayon on native and serially on wasm32. Non-finite output samples keep
the original value. Every failure is an `Error`; on error the image is left unchanged.

## Provenance

Ported from the PhotoCraft plug-in host (`crates/plugins`: `runtime.rs`, `manifest.rs`,
`apply.rs`, `registry.rs`; same authors, MIT/Apache-2.0), adapted from tiled surfaces to an RGB
buffer, with cancellation, a module hash, an export sniff and an instance-based registry added.
