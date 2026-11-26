# AI Denoise

Edit ▸ Detail ▸ **AI Denoise** is a per-photo switch above **Amount** (0–100).
Turning it on starts at 50, or preserves an already positive Amount. Turning it off
preserves Amount and mixes none of the cleaned picture. It affects only the requesting
photo, independently of Auto Sync, and is undoable. Manual luminance and colour noise
reduction remain separate controls.

## The built-in denoiser

AI Denoise needs no set-up. Unless you choose a model of your own (below), it runs the
**built-in denoiser** (`crates/denoise-core/src/builtin.rs`, id `builtin`): it measures
the photo's noise level once, from blocks of all four colour planes spread over the whole mosaic (variance =
a · signal + b; see [How the noise is measured](#how-the-noise-is-measured)), turns the noise into unit variance with a variance-stabilising transform,
and cleans the four Bayer planes together with block-matching collaborative filtering
(`crates/denoise-core/src/bm3d.rs`, after the published BM3D / CBM3D method). The four planes are rotated
into a brightness plane and three colour-difference planes; 4 × 4-cell blocks of the brightness plane that look alike
are stacked into groups (the colour planes follow the same matches, so colour noise is cleaned with the brightness).
Each stack is transformed (a 2-D DCT of every block, a Haar transform across the stack), small coefficients are
removed, and the blocks are put back with weights; a second step matches blocks again on that cleaner estimate and
shrinks by a Wiener filter built from it. How hard it thresholds follows the photo's noise: gently at low noise, to
keep fine detail, and harder as the noise grows. It then demosaics with
edge-directed green and colour-difference red and blue. It is written for
LightCraft, pure Rust, MIT OR Apache-2.0, has no weights and no download, and uses the
same tiling, cache, queue, preview and export path as a model. Its tile is 256 cells with a
16-cell overlap. The filter (the grouping, the transforms and the putting back, which are nearly all of its work) runs on
the graphics card when there is one that agrees with the processor and is faster (`crates/gpu/src/bm3d.rs`, WGSL kernels
in `crates/gpu/src/wgsl/bm3d_*.wgsl`); measuring the noise, the variance-stabilising transform and the demosaic stay on the
processor. The card is set up for the first photo that may use it, like a model's: its kernels are built, a check tile is
run on both devices and the card's filter must match the processor's to within a third of a noise deviation at the
worst place and a thousandth on average (they differ by the rounding of the arithmetic: a few blocks whose match or
threshold falls either side of a tie change by about a tenth), and both are timed. Settings ▸ AI Denoise ▸ Run on chooses
Automatic (the card where it is faster), Graphics card or Processor, and says where it runs; a set-up that crashed or ran out
of time is not tried again until you choose (Try the graphics card again). A tile the card gets wrong is run on the
processor, and three stop the card for the session. On an RTX 4080 SUPER a tile takes 22 ms on the card against 408 ms on one
processor thread, and the 1536 × 1024 benchmark crop 0.13 to 0.24 s against 1.0 to 2.1 s on four threads, with the very
same scores. A change
to the algorithm bumps `builtin::VERSION`, so cached pictures are made again. The id
`builtin` is reserved: a model with that id is refused. It is a classical filter, not a
trained network, so a good model can still do better on very noisy files. The earlier
non-local-means filter (`crates/denoise-core/src/nlm.rs`) is still there as a fallback
(`Tuning::bm3d = None`). Why block matching and not something else: see [Why block matching](#why-block-matching).

To measure a change to it, run the opt-in benchmark on a public CC0 raw (see `cargo xtask corpus`):
`LC_DENOISE_RAW="$(git rev-parse --show-toplevel)/corpus/raw/arw-sony-a7m3-compressed.arw" LC_DENOISE_OUT=/tmp/dn cargo test --release -p lightcraft-engine --test denoise_builtin_quality -- --ignored --nocapture`
(the path must be absolute: tests run from the crate's folder).
It adds noise of four known levels to the raw, runs the denoiser and prints the PSNR against the
original, plus a texture figure (how much of the true picture's fine structure the result keeps; noise
can't inflate it); `LC_DENOISE_OUT` also writes truth | noisy | denoised PNGs. Use public files only.
`LC_B_N`, `LC_B_STEP`, `LC_B_S`, `LC_B_K`, `LC_B_T1`, `LC_B_T2`, `LC_B_LOFF` (added to every point of the threshold curve),
`LC_B_SIG` and `LC_B_WIENER`
override the matching `bm3d::Params` field for a sweep; `LC_NLM=1` runs the non-local-means filter instead (then
`LC_S1`, `LC_S2`, `LC_HL`, `LC_HH`, `LC_H2`, `LC_LIGHT`, `LC_HEAVY`, `LC_SC`, `LC_HC`, `LC_P1`, `LC_P2`,
`LC_LOOSE` and `LC_FEW` override its `nlm::Tuning` fields).
PSNR and texture trade off (smoothing more raises one and lowers the other), so judge a change on both and on the
crops. On the Sony A7 III crop (PSNR gain over the noisy picture in dB / texture, light, medium, heavy, extreme noise):
non-local means +3.70 / 0.62, +7.22 / 0.42, +9.69 / 0.22, +10.37 / 0.09; block matching +4.28 / 0.58, +7.60 / 0.44,
+10.04 / 0.22, +10.62 / 0.12, in about the same time (1 to 2 s for the 1536 × 1024 crop on 4 cores). `LC_GPU=1` (with
`--features denoise`) runs the block-matching filter on the graphics card instead: the same scores in 0.13 to 0.24 s on an
RTX 4080 SUPER. The tests `lightcraft-gpu` `bm3d::tests` (the card against `bm3d::filter` on synthetic tiles, shared between
threads) and the engine's `gpu_tests` (set-up check, fallback after wrong tiles, the processor chosen, a marker left behind)
need a graphics card and record a skip without one.

### Why block matching

The built-in filter was chosen by measuring, on the benchmark above (the public Sony A7 III crop with noise of four known
levels; PSNR gain over the noisy picture in dB / texture), not by taste. Candidates, all written from the published
papers (no GPL code), in the order tried:

| Filter | light | medium | heavy | extreme | Time per crop (4 CPU threads) |
|---|---|---|---|---|---|
| Non-local means (the first built-in) | +3.70 / 0.62 | +7.22 / 0.42 | +9.69 / 0.22 | +10.37 / 0.09 | 1.1–1.7 s |
| **Block matching, hard threshold then Wiener (BM3D / CBM3D)** | **+4.28 / 0.58** | **+7.60 / 0.44** | **+10.04 / 0.22** | **+10.62 / 0.12** | 1.1–2.1 s |
| Weighted nuclear norm minimisation (WNNM), best setting per level | +4.68 / 0.56 | +8.14 / 0.39 | +10.04 / 0.22 | +10.35 / 0.15 | 15–20 s |

- **Block matching beats non-local means** on PSNR at every level and on texture at medium, heavy and extreme noise
  (only light noise keeps slightly less: 0.58 against 0.62, for 0.6 dB more). The crops show more needle and branch detail
  and no colour blotches or isolated specks. It costs about the same on the processor.
- **WNNM**, the strongest classical method we know of (singular values of each group of blocks shrunk with weights),
  gains about 0.4 dB at light noise and 0.03 of texture at medium and extreme, and nothing at heavy noise, for about ten
  times the processing time (a 24 MP photo would take about five minutes on the processor, against about a quarter of a
  minute). Its singular values came from Jacobi rotations of each group's Gram matrix; more iterations, a smaller feedback
  and 6 × 6 blocks (115 s a crop) did not help. Not kept. Untried: patches over all four planes at once (about fourteen times
  the work per group).
- **What the tuning showed** (for whoever changes it): the hard threshold has to rise with the noise (`Params::lambda` is a curve
  of the noise's deviation at mid-grey against the threshold: 1.2 at 0.0028, 1.9 at 0.0062, 2.3 at 0.0094, 2.65 at 0.0152, 3.2 at
  0.038, 3.5 at 0.14; a fixed value wins at one level and loses at another, see
  [Fitted on real cameras](#fitted-on-real-cameras)); a higher
  threshold on the colour planes than on the brightness plane hurts both PSNR and texture, because the colour differences
  carry real signal; 4 × 4 blocks beat 3, 5, 6 and 8 on quality (3 × 3 is a little faster, 6 × 6 and 8 × 8 much slower); the
  match tolerances and the group size (16 against 8 or 32) barely matter; a search of 8 cells and a step of 3 cost 0.1 dB
  against 12 and 2 and keep the
  processor as fast as non-local means. The Wiener step adds a little over the hard threshold alone.
- **The first benchmark's limit:** its noise is synthetic and white, and its reference (a public raw's own mosaic) has noise of
  its own, which hides what happens at the low noise levels of real cameras. Real high-ISO raws also have correlated,
  low-frequency colour noise and banding, which none of this measures; judge a change to the real thing by eye on a
  high-ISO raw too.

### How the noise is measured

`builtin::estimate_noise` reads `(a, b)` of a photo (variance = `a · signal + b`, signal in 0..1 after black and white
levels) from the picture itself, so no camera table is needed. Each of the four colour planes is read separately (they sit at
different levels in a raw), cut into 12 × 12-cell blocks, and a second-difference operator (`[1 -2 1; -2 4 -2; 1 -2 1]`, which
cancels flat and smoothly sloping signal; white noise has `36 σ²` of variance through it) gives each block's variance; texture
and edges only add to it. At each brightness (about 300 blocks of each) the quietest 5 % of blocks are the noise, divided by
the bias of that quantile of pure noise (0.67, measured by
`denoise_noise_estimate::a_quantile_of_pure_noise_blocks_is_a_known_fraction_of_the_true_variance`), and a line is fitted
through the points. The first version mixed the colour planes in each block and took the lower quarter of a plain
neighbour difference: it read the picture's texture as noise, never below about 0.003 at mid-grey whatever the
true noise (`estimate_noise_v1`, kept to compare). On 30 different public pictures with known noise added, the ratio of
estimated to true deviation at mid-grey, at the ISO 6400 level of a Canon R-series body (0.0094), had a 10th to 90th percentile of
0.97 to 1.29 against 0.94 to 1.96 for the first version, and at the ISO 1600 level (0.0062) 0.99 to 1.45 against 0.97 to 2.77
(`LC_CORPUS=<corpus/raw> cargo test --release -p lightcraft-engine --test denoise_noise_estimate -- --ignored --nocapture`).
A picture with no flat area at all still reads somewhat high, never low.

### Fitted on real cameras

A Canon EOS R, EOS R6 Mark III, M50 Mark II and some EF-era bodies were measured on a private library of about 8,400 raws
(only aggregates were kept; no photo is in the repository). The deviation of the noise at mid-grey (the measure the filter
judges a photo by) was about 0.0024 to 0.0028 at ISO 400 and below, 0.005 to 0.006 at ISO 500 to 1600, 0.009 to 0.010 at ISO 2000
to 6400, 0.015 to 0.018 at ISO 8000 to 25600 and 0.038 above that: far lower than the first benchmark's levels (0.024 to
0.14), so a filter tuned on those was too gentle exactly where these photos live. The curve above was fitted on clean low-ISO
photos of those cameras with noise of their measured levels added (`LC_REAL=1`; `LC_FULL=1` measures the noise on the whole
frame, as the app does). Against the first estimator with a constant threshold of 1.9, averaged over 10 EOS R and 5 R6 Mark III
photos, the output reached (PSNR in dB, EOS R / R6 Mark III): ISO 400 level 43.76 / 44.93 (from 43.27 / 44.66), 1600 41.03 / 41.86
(41.05 / 41.82), 6400 39.93 / 40.69 (39.30 / 39.74), 25600 38.36 / 38.86 (36.18 / 36.21) and 51200 level 33.83 / 34.45
(28.42 / 28.78), with the noise left in the result ("detail" above 1) falling from 1.3 to 3.1 down to 0.73 to 0.99. The threshold
at each level is the lowest that stays within about a quarter of a decibel of the best, so texture is kept (on the EOS R at the
25600 level the texture figure is 0.28, against 0.25 at the highest-PSNR threshold). The first estimator also cost PSNR by itself: told the true
noise instead of its estimate, the same filter did the right thing at every level; the new estimator is as good as that from
ISO 6400 up.

## Models and terms (optional)

**No model weights are bundled.** Choosing a model replaces the built-in denoiser; **Stop using**
(or removing the model) goes back to it. Models live in the per-user `denoise-models` folder
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
