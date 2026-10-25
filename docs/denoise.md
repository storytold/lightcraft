# AI Denoise

Edit ▸ Detail ▸ **AI Denoise** is a per-photo switch above **Amount** (0–100).
Turning it on starts at 50, or preserves an already positive Amount. Turning it off
preserves Amount and mixes none of the cleaned picture. It affects only the requesting
photo, independently of Auto Sync, and is undoable. Manual luminance and colour noise
reduction remain separate controls.

If no model is selected, turning it on opens **Settings ▸ AI Denoise** without changing
the photo. After installation, the pending request enables the original photo even if
the selection changed. Cancel the pending action in Settings. Cancelled downloads,
deleted or relinked photos, switching libraries and quitting cancel the request.
An active slider gesture finishes before the pending action resumes.

## Models and terms

**No model weights are bundled.** Models live in the per-user `denoise-models` folder
(override with `LIGHTCRAFT_DENOISE_MODELS`). Settings provides **Install from file…**
for a user-supplied ONNX file with a `denoise-model.json` beside it. The default build
offers no model downloads. The separate `rawnind-model` Cargo feature enables the
pinned RawNIND offer only when a release's model policy permits it.
Terms must be acknowledged before installation or
download. Downloads use the shared pure-Rust `lightcraft-fetch` client, with resume,
progress, cancellation, size limits and SHA-256 verification.

RawNIND's optional ONNX weights are **GPL-3.0**, as recorded in the pinned model catalog.
They are downloaded separately after consent and read by our own Rust executor.
Whether releases should offer this download remains a maintainer policy decision;
permissively licensed or independently trained weights remain a gap. This is not a
claim that optional downloading settles licensing questions. No GPL decoder or neural
runtime code is included. Model metadata is in `crates/denoise-core/src/known.rs`.

Installation checks the manifest, model hash and size, then runs a synthetic self-test:
finite output of the promised shape, repeatability, the declared scale and reduced
noise. Unsupported operators, malformed files and a failed self-test return errors.
The UI performs copying, extraction, hashing, parsing and CPU/GPU self-tests on one
background installation worker. `denoise.models.downloads` and `denoise.pump` report
`installing`, `installed` or `failed`; an existing model is preserved on a failed replacement.
Command clients can request the same asynchronous path with `background: true`.
The synchronous command remains available for scripts that require the result immediately.

The current contract is `bayerToRgb`: normalized, black-subtracted, non-white-balanced
RGGB planes `[1, 4, tile, tile]` produce camera RGB `[1, 3, 2*tile, 2*tile]`.
RawNIND uses a 512-cell tile, 64-cell overlap and a mean-matched nominal gain of 1e6.
See `DenoiserManifest` for the validated JSON schema.

## Photos, previews and exports

Original RAW files stay untouched. A cleaned picture is cached under
`<library>/denoise/<key>.lcdn`, keyed by source content, model and algorithm version.
The compressed half-float cache has checked headers, bounded windows and checksums.
It can be cleared or recreated; it is not a new catalog photo or a duplicate DNG.

Plain and cleaned camera RGB go through the same highlight reconstruction, white
balance, colour matrix, DNG profile tables, camera hue/saturation look, crop and
orientation. Amount blends the two before the remaining develop pipeline, so edits
and exports use the same data. At zero Amount or with Detail disabled, the ordinary
render path remains in use.

The queue prioritizes the open and selected photos, pauses after input, and adjusts
its worker count while the user works. Progress, cancellation and retry are available
in Settings and through commands. The default cache limit is 20 GB; unused products
are evicted oldest first. A failed file is not continuously retried.

An export with a nonzero effective Amount creates a missing product synchronously.
It returns a clear error if it cannot produce the denoised result; it never silently
exports the plain image instead. Unsupported sensor layouts retain manual noise
reduction and report their limitation.

## Pure-Rust CPU and GPU execution

The CPU executor reads a bounded subset of ONNX into checked network data. Convolutions
use bounded im2col blocks and `faer-core 0.17.1` with its pure-Rust GEMM backend,
without BLAS, C dependencies or an inner thread pool. That backend includes
architecture-specific assembly in its dependencies. Tiles share the outer worker
pool. A scalar implementation checks the optimized operators on synthetic networks.
Tract is not a workspace dependency or a product feature.
CPU inference and the GPU model module are feature-gated; ordinary engine/web builds
do not compile the faer/GEMM backend. The engine depends on the inference crate only through `denoise = ["dep:lightcraft-denoise", …]`;
lightweight `lightcraft-denoise-core` metadata and cache formats remain available without inference. Pictures are capped at 110 megapixels; at most
four tile calls run together, chosen within the process-wide working-memory budget.
One idle CPU workspace is retained, and cache costs reserve room for the blended picture.

The GPU executor uses our WGSL kernels through wgpu. It checks a real model's output
against the CPU and falls back on errors, unsupported networks or a slower device.
Setup has a time limit and a crash marker. Automatic selection estimates throughput
from a check tile; this estimate can choose imperfectly on unmeasured hardware.
Settings offers Automatic, Graphics card and Processor. GPU tests cover supported
synthetic networks against the scalar executor; a real-model test compares CPU/GPU.

The convolution kernel stores its shared input tile as scalar elements, each with
one writer, and gathers vectors after a workgroup barrier. Writing separate lanes
of the same shared vector from different invocations is not safe under WGSL
([component references](https://www.w3.org/TR/WGSL/#vector-component-reference));
this caused incorrect Metal results (issue #479). Synthetic equivalence tests include
repeated partial tiles and concurrent calls, and reject non-finite outputs.

### Historical performance, measured 2026-10-07

These are the contributor's measurements **before the current upstream integration**,
on a Ryzen 9 7945HX and RTX 4090 Laptop GPU (DX12), release build, model SHA-256
`da27509dab6a2915da67e988acd86cf71f9d5bbc8d1aa0ed32933578a887b901`.
The 24.34 MP Sony a7 III photo is 35 tiles. Numbers exclude model loading, source
decode, GPU setup, cache encoding and export, but include packing and blending.

| Work | Pure-Rust CPU | Prior Tract reference | GPU |
|---|---:|---:|---:|
| One warm 512-cell tile, one caller | 1148 ms | 1463 ms | 15.1 ms |
| Whole photo, 16 CPU / 4 GPU feeding workers | 5.185 s | 9.534 s | 0.582 s |

The largest relative differences were 1.68e-6 CPU/reference and 1.57e-6 CPU/GPU for
the check tile, below the required 1e-3 tolerance. Reference comparisons were made
on the older contributor branch; that reference runtime is intentionally excluded
from the current workspace. These numbers are not a fresh benchmark of this rebase.

Fresh checks can be run using an already installed model and CC0 raw samples:

```text
LC_DENOISE_MODEL=<model.onnx> cargo test --release -p lightcraft-gpu --lib real_model_on_the_gpu_matches_cpu -- --ignored --nocapture
LC_DENOISE_MODEL=<model.onnx> LC_DENOISE_RAW=<corpus/raw> cargo test --release -p lightcraft-engine --features rawnind-model --test it denoise_cpu:: -- --ignored --nocapture
LC_DENOISE_MODEL=<model.onnx> LC_DENOISE_RAW=<corpus/raw> cargo test --release -p lightcraft-engine --features rawnind-model --test it denoise_real:: -- --ignored --nocapture
```

## Another computer's graphics card

A laptop can hand tiles to a desktop with a bigger graphics card over a LAN or a VPN. The server runs the installed,
selected model the way the app does (the card when it can run it, checked against the CPU first; else the CPU):

```text
export LIGHTCRAFT_DENOISE_TOKEN='a long random string, at least 16 characters'
lightcraft-cli denoise-serve --listen 100.101.102.103:7990          # e.g. the machine's Tailscale address
```

On the laptop, before LightCraft starts:

```text
export LIGHTCRAFT_DENOISE_URL=100.101.102.103:7990
export LIGHTCRAFT_DENOISE_TOKEN='…the same token…'
```

When the laptop first uses the model it greets the server and times a check tile there and back. If that is faster
than a tile on the laptop (its graphics card, or its processor with the tiles it runs at once), the server's slots
(four tiles in flight) are added to the laptop's own: a tile goes to the server when one of its slots is free and runs
locally otherwise. If it is slower, nothing is sent, so a photo never waits for the slower machine:
`denoise.status` → `device.remote.state` is `slower`, with both times (`remoteMs`, `localMs`). Measured with
RawNIND: an Apple M3 laptop does a tile in 116 ms, an RTX 3060 Ti reached over Tailscale 345 ms there and back,
so that laptop keeps its tiles; one without a fast card hands them over. A tile the server fails runs locally; after
three failures the server is no longer used for that model. Both ends must
run the same model file: the client compares the manifest's SHA-256 (or the file's) when it first greets the server,
and the server checks it again for every tile. `denoise.status` → `device.remote` reports the server's address, its
device and whether it is used, or why not.

**The protocol is not encrypted.** Listen on a VPN address (Tailscale, WireGuard) or a LAN, or bind every interface
behind a firewall rule that admits only the VPN; never expose the port to the internet. Requests carry the shared
token (compared in constant time); the server refuses other models, other tile sizes, oversized headers and payloads
(also ones that inflate past their tile) and caps concurrent connections (`--max-connections`, default 8).

Protocol version 1 (`crates/denoise/src/remote.rs`): one JSON line per message, then a payload of `bytes` bytes —
the tile's samples as little-endian half floats, byte-shuffled (low bytes, then high bytes) and deflated:

```text
→ {"v":1,"token":"…","op":"hello"}
← {"ok":true,"v":1,"device":"GPU: …","model":{"id":"…","version":"…","sha256":"…"},"tile":T}
→ {"v":1,"token":"…","op":"run","model":"<sha256>","tile":T,"bytes":N}   + N bytes (4 planes of T × T cells)
← {"ok":true,"tile":T,"bytes":N}                                        + N bytes (3 planes of 2T × 2T pixels)
← {"ok":false,"error":"…"}
```

## Commands and limits

UI, CLI, control channel and MCP dispatch `denoise.toggle`, `denoise.settings`,
`denoise.queue`, `.cancel`, `.status`, `.pump`, `.clear`, and `denoise.models.*`
(`list`, `install`, `download`, `downloads`, `downloadCancel`, `test`, `remove`,
`select`). The develop control is `enhance.denoise`.

Native apps enable inference through their `denoise` feature. A build without it and
the web build report inference unavailable. Manifest/cache primitives still compile
for WASM. Settings and models remain opt-in; nothing is downloaded on launch.

This implementation supports **Bayer RAW only**. X-Trans, Foveon, demosaiced DNG and
non-RAW images remain unsupported by this model contract. No measured quality parity
with Lightroom is claimed. Apple/Metal and wider hardware verification, model policy,
a linear-RGB contract and a side-by-side fidelity suite remain future work.
