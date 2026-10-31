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
Settings offers Automatic, Graphics card, Processor and (Apple silicon) Neural Engine. GPU tests cover
supported synthetic networks against the scalar executor; a real-model test compares CPU/GPU.

### The Neural Engine (Apple silicon)

Core ML is the only way to reach Apple's Neural Engine; wgpu and Metal never do, so the GPU path above leaves it
idle (0 mW in `powermetrics`). `crates/coreml` (`lightcraft-coreml`, the second crate allowed `unsafe`, in one
file) compiles and runs a Core ML model; Core ML decides per layer where it really runs, and only allows the
Neural Engine (`CPUAndNeuralEngine`), never the graphics card.

Core ML takes no ONNX file and the Neural Engine computes in 16-bit floats (largest value 65504), so the installed
model is rewritten on this computer (`lightcraft_denoise::coreml`, pure Rust, no Python):

1. The activations' reach is measured on the check tile on the CPU. RawNIND's reach 10⁶ and its answer is a million
   times the input, which came back as NaN from 16-bit arithmetic.
2. Each layer's weights and bias absorb a power-of-two factor that keeps the value before its activation at 4096 or
   below (a factor 16 below the limit, for brighter or noisier tiles than the check tile, which has the whole
   range from black to clipped white). The peaks are measured after the activation, and before a leaky ReLU a value
   can be `1 / alpha` times larger, which is allowed for. Convolutions are linear, and a leaky ReLU or a maximum
   commutes with a positive factor, so in real arithmetic this changes nothing but the scale of the answer, which
   the runner multiplies back (RawNIND's comes out 2⁻⁹ smaller): `Gain::MatchMean` and the rest of the pipeline
   never see it. A test checks this against the CPU reference. The 16-bit arithmetic itself is not exact: the
   check tile's answer differs from the CPU's by 3.6×10⁻³ of its largest value. Smaller weights lose precision in
   16 bits, so `coreml::fp16_report` counts the ones below its normal range: on RawNIND 9,849 of 7.76 million
   before the rescale, 16,217 after (9 round to zero).
3. The network is written as a Core ML `NeuralNetwork` model, compiled by Core ML and kept in
   `<model folder>/coreml/<model hash>-t<tile>-v<writer>/` (`net.mlmodelc` and `scale`). A kept model that does not
   load is made again.

Setting it up works like the graphics card's (first photo that may use it, own thread, 60 s limit that includes
the first compile (0.3 s on an A18 Pro; the compiled model is kept, so later set-ups skip it), `ane-setup.marker`
beside the model while it runs, a check tile compared with the CPU's answer and timed): the answer may differ from the
CPU's by up to 2 % of its largest value (measured: 3.6×10⁻³ on RawNIND's model), a tile it gets wrong or cannot run goes to
the CPU, and three wrong tiles stop it. **Automatic** tries the Neural Engine first and uses it where its check tile
is faster than the processor's at the pace the photo runs at, and than the card's when that was measured; **Neural
Engine** uses it whenever it can (and otherwise behaves as Automatic); choosing where denoise runs again lets a
failed set-up or a crash be tried again. `LIGHTCRAFT_GPU=0` and `LIGHTCRAFT_GPU_BACKEND=off` keep it off too, like the
graphics card. `denoise.status` → `device` says `ane` with `aneMs` and `cpuMs`; when the Neural Engine was chosen (or Automatic tried it) and its set-up failed or it got too many tiles wrong, `neuralEngineReason` says why and `retry` is offered where the processor runs the photo; kept models of an older model file, tile or writer are removed when a new one is made;
`denoise.models.list` → `neuralEngine` says whether this computer can have one.

Measured on an Apple A18 Pro (8 GB, macOS 26.7), RawNIND UtNet2 (Bayer), one 512-cell tile, warm
(`LC_DENOISE_MODEL=<model.onnx>`):

| Where | One tile |
|---|---:|
| CPU, one core (our faer executor) | 1621 ms |
| Graphics card (our WGSL kernels, fp32) | 238 ms |
| Core ML, CPU + GPU (fp16) | 66 ms |
| Core ML, **Neural Engine** (fp16) | **7.8 ms** |

`powermetrics --samplers cpu_power,gpu_power,ane_power` while tiles ran in a loop: ANE 3.4–4.4 W, GPU about 10 mW,
CPU about 0.7 W (before: ANE 0 mW, and the graphics card at up to 7.5 W for the longer run). The Neural Engine draws
power too; energy per photo is that times the (much shorter) time, to be measured per photo, not assumed.

Whole photo, in the app (a Canon CR3, 5208 × 3908 after the model's Bayer packing, `powermetrics` at 1 s, CPU + GPU +
ANE power summed over the busy seconds, one run each, so approximate):

| Speed | Time | Energy |
|---|---:|---:|
| Neural Engine | 4.0 s | about 17 J |
| Graphics card | 10.4 s | about 85 J |
| Processor | 28.2 s | about 126 J |

With the Neural Engine the CPU draws about 5 W for 3 s (decode, packing, tiling); the Neural Engine itself 0.25–0.9 W
and the graphics card almost nothing. The picture it makes differs from the processor's by 0.016 of an 8-bit
display level on average (largest 4 levels in single pixels, fewer than 0.0001 % of samples differ by more than 1 level; white taken at the
99.5th percentile, gamma 2.2).

```text
LC_DENOISE_MODEL=<model.onnx> cargo test --release -p lightcraft-gpu --features denoise --lib real_model -- --ignored --nocapture
LC_DENOISE_MODEL=<model.onnx> LC_COREML_LOOP_SECS=22 cargo test --release -p lightcraft-denoise --features runtime --lib coreml::core_ml_tests::the_real_model -- --nocapture
```

The second one prints the tile time and the difference from the CPU, and with `LC_COREML_LOOP_SECS` keeps the
Neural Engine busy that long for `powermetrics`. The Settings text for the new choice is English until translated.

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
