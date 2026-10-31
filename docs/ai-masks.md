# AI masks

LightCraft's AI masks run in pure Rust in `crates/segment`: the larger models on
[candle](https://github.com/huggingface/candle) plus plain-Rust kernels, Google's MediaPipe models on
a small built-in TFLite interpreter that reads the files Google publishes, unchanged. Everything runs
on the CPU (an experimental GPU path for the largest matrix products is opt-in, see Speed), except
the optional SAM 3, which runs on the GPU through Metal on macOS and falls back to the CPU when the
GPU can't run it (see the worker below). Each mask
type uses a model whose weights are openly licensed, which LightCraft can host and download without
accounts:

| Mask | Model | Weights licence | Training-data tier | Download | Without it |
|---|---|---|---|---|---|
| **People**: entire person, face / body skin, hair, clothes | MediaPipe Selfie Multiclass (Google) | Apache-2.0 | A (Google's own) | 16 MB | the entire person from EoMT; parts: not available |
| **People**: eyebrows, eye sclera, iris and pupil, lips, teeth; telling people apart | MediaPipe Face Landmarker (BlazeFace + FaceMesh-V2, Google) | Apache-2.0 | A (Google's own) | 4 MB | not available |
| **Subject**, **Background** | BiRefNet-portrait (Swin-L) | MIT | pending (maintainer decision) | 885 MB | the person from MediaPipe, else EoMT's most confident object, else a heuristic |
| **Sky**, **Landscape**; People without MediaPipe | EoMT-S, COCO panoptic (DINOv2 ViT-S) | MIT (backbone Apache-2.0) | pending (maintainer decision) | 96 MB | Sky: a heuristic; Landscape: approximate (empty) |
| **Object** (clicks) | SAM 2.1 Hiera-Tiny (Meta) | Apache-2.0 | A (Meta's own) | 156 MB | SAM 3 if installed |
| **Describe** (text) | SAM 3 (optional) | SAM License (Meta, gated) | — | 3.4 GB | none: the dialog explains how to install SAM 3 |

Depth Anything V2 Small is ported (`crates/segment/src/depth.rs`) but **not offered**: its training
data includes SA-1B (whose licence restricts derivative works) and Virtual KITTI 2 (non-commercial).
Tiers: **A** the rights holder released the weights permissively and the training data is theirs or
cleared; **pending** the weights are permissive but a training dataset is research-only without an
explicit derived-data clause — a maintainer decision, shown in the app as "licence pending a
maintainer decision"; models whose weights or data explicitly forbid commercial use (tier B) are
never offered.

**No model is required.** None is part of LightCraft, its repository or its installers. Each is
downloaded once, into its own folder of the models folder, only after the user agreed (the dialog
names the model, its size and its licence). Without one, Object says what to download, and Subject,
Sky and Background are made by a heuristic — the Masking panel says the mask is approximate and
offers the model. Renders, exports and every other feature never touch a model: the model's result
is stored with the mask.

## Using them

In the Masking panel (M):

- **Subject**, **Sky**, **Background**: one click. With the model installed the mask appears as
  the heuristic at once and is replaced by the model's selection a moment later ("Analyzing the
  photo for AI masks…" under the tiles; one undo step). Background is the subject inverted.
- **People**: pick everyone or Person 1–4 (counted from the left, by their face) and the parts —
  Entire Person, Face Skin, Body Skin, Hair, Clothes, Eyebrows, Eye Sclera, Iris and Pupil, Lips,
  Teeth — then **Create Mask** (`mask.add {"kind": "people", "person": n, "parts": [...]}`). Face
  parts come from landmarks (polygons; teeth are the bright part of the mouth opening) and need a
  face that isn't in profile; with several people, body parts go to the person whose face is nearest. **Face Skin**
  and **Body Skin** are skin only: the eyes (with the lash line), eyebrows, lips and mouth
  (teeth, tongue) are cut out, and so is facial hair — the model's hair class, plus, in the lower
  face, pixels clearly darker and more textured than the cheeks (beards, moustaches, stubble; skin
  in shadow is dark but smooth and stays). Each eye is cut out whole, lids and lashes, however far it
  is open (a smiling or blinking eye's opening is only a sliver), and each brow at least a fifth of
  its length thick. Face Skin reaches the hairline: above the brows, inside the face's outline
  stretched up to where the hair starts, what has the forehead's own colour and isn't hair is skin
  (the class map alone fades out on a high forehead). Nostrils stay in the skin. On a face seen in profile the
  features can't be located: only hair is left out there, and the app says so. Pick Lips, Eyebrows
  and so on alongside a skin part to add them back.
  **Clothes** on its own leaves the skin and hair the class map's blur spills onto (a neckline,
  arms, hands) out, and its edge is pulled onto the photo's.
- **Landscape**: pick a class under the tiles — Sky, Water, Vegetation, Mountains, Architecture,
  Natural Ground, Artificial Ground.
- **Update**: a mask made before its model was installed (or an older document) keeps its
  heuristic; once the model is there, the panel offers **Update** (`mask.recompute`).
- **Object**: click the thing you want; every click refines the selection. ⌥-click (Alt) a part
  to leave it out. Clicks show as green (include) and red (leave out) dots.
- **Describe** (SAM 3 only): type what to select — `sky`, `trees`, `the red car` — and press Return. Every
  instance the model finds (score above 0.5) joins the mask; if nothing matches, nothing is
  created and you are told so. Around what it found, the mask follows the model's semantic mask
  for the phrase, which is sure inside and runs on where two instances touch (the instances'
  own masks alone left a sky or a net about 70–90 % selected and a thin line between two people
  side by side).
- Several things at once: `car, road` (comma-separated) selects each and merges them.
- **+ / −** under the selected mask (and the Add / Subtract / Intersect menus) combine
  selections: Describe `car`, click the mask, + ▸ Describe… `road`; − ▸ Describe… `people` takes
  the people out of a sky mask; − ▸ Object then clicks remove one object.
- **Edge** (Object and Describe, −100…100): below 0 a crisper border, above 0 a feathered one (up to 2 %
  of the long edge, so previews and exports match). **Refine Edges** (every mask) snaps the edge to the photo's.
- **Detail** (Object and Describe): about a second after the last click (or right after a description), the photo
  around the selection is analyzed again, zoomed in, from a sharper source; the result is kept as a
  high-resolution patch (at most 4 per selection), so small objects get 5–10× finer edges than one
  pass over the whole photo gives.

Everything a model does runs on one worker thread: the window stays responsive. Models are loaded
when first needed and unloaded after 10 minutes without use.

What is stored with the mask: the model's logits over the uncropped photo, quantized and compressed
— 1024 × 1024 for Subject / Background (BiRefNet's own resolution: hair strands survive), 256 × 256
for Sky / People / Landscape (EoMT predicts at 160 px on the long edge), 256 × 256 for an SAM 2.1
Object selection, 288 × 288 for SAM 3. Renders and exports sample them at any resolution and never
need the model, so masks survive moving the library to a machine without it, and the web build
renders them. Editing the photo's look later does not recompute a mask (Update does).

### Speed

Measured on a Ryzen 9 5950X (16 cores), Windows 11, release build, on the CPU, per photo (the
model input is rendered at 1024 px on the long edge):

| Model | Load (cold) | Per photo (warm) | Notes |
|---|---|---|---|
| MediaPipe Selfie Multiclass | 8–18 ms | ~45 ms network, ~130 ms with resampling | TFLite interpreter (matches LiteRT to 2e-5 relative) |
| MediaPipe Face Landmarker | 7 ms | ~4 ms per BlazeFace window, ~13 ms per FaceMesh face; ~330 ms for a photo (about ten windows) | faces in profile are not landmarked |
| EoMT-S | ~75 ms | ~0.9–1 s | Sky, Landscape and People share one pass per photo |
| SAM 2.1 Tiny | ~0.15 s | encode ~1.2 s, then ~45 ms per click | |
| BiRefNet-portrait | ~1.5 s | ~9–12 s | Subject and Background share one pass; the slow one |

The first mask of each kind on a photo pays the "per photo" time; further masks on the same photo
reuse the result. GPU: `LIGHTCRAFT_AI_GPU=1` sends BiRefNet's large matrix products to the GPU
(wgpu, DX12 / Vulkan / Metal). On an RTX 5070 it saved ~20 % of a warm pass but its first pass costs
~20 s more (kernel compilation, uploading 885 MB of weights), so it is off by default. Measured
alternatives (scratch evaluation, not shipped): burn on wgpu ran EoMT in 76 ms and SAM 2.1's encoder
in 200 ms on the same GPU, but compiles each model into the app, pulls in C code through its
dependencies and failed to import BiRefNet and SAM 2.1's decoder; tract (pure-Rust ONNX) can't read
MediaPipe's files, needs an assembler at build time and was 2–7× slower here.

## The openly licensed models: download and licences

The first time a mask needs a model that isn't installed, LightCraft asks: **Download an AI
Model?** — what the model does, its size, its licence (MIT or Apache-2.0: open-source licences that
allow any use, including commercial work) and a link to it — **Not Now** / **Download**. The
download runs in the background (a row in the activity stack with ✕ to cancel; the Masking panel
shows its progress), resumes where it stopped if interrupted, and tries the next location when one
fails. Each file is written as `<name>.part` and moved into place only when it has the exact size
and SHA-256 pinned in `crates/segment/src/models.rs`; a file that doesn't match is deleted, never
used.

Folders: `models/<name>/` in LightCraft's settings folder — `%APPDATA%\LightCraft\models\` (Windows),
`~/Library/Application Support/LightCraft/models/` (macOS), `~/.config/lightcraft/models/` (Linux) —
or the folder `LIGHTCRAFT_AI_MODELS_DIR` names: `birefnet-portrait/`, `eomt-s-coco/`,
`sam2.1-hiera-tiny/`, `depth-anything-v2-small/`. To install one by hand, put its files there
(`model.safetensors`, plus `config.json` for EoMT and Depth Anything).

Download locations, in order: `LIGHTCRAFT_<MODEL>_MIRRORS` (`LIGHTCRAFT_SUBJECT_MIRRORS`,
`LIGHTCRAFT_SCENE_MIRRORS`, `LIGHTCRAFT_SAM2_MIRRORS`, `LIGHTCRAFT_DEPTH_MIRRORS`; base URLs
separated by commas or spaces), the file `models/<folder>-mirrors.txt`, then the built-in list: the
publisher's own Hugging Face repository, pinned to a commit (their licences allow redistribution
and the repositories are not gated). **Maintainers:** put LightCraft's own mirror first in each
list (`TODO(maintainer)` in `models.rs`), with the files unchanged and each model's licence next
to them.

Agents and scripts: `ai.models.status`, `ai.models.download {"id": "subject"|"scene"|"sam2"|"depth",
"acknowledged": true}` (refused without `acknowledged`: pass it only after the user agreed),
`ai.models.cancel {"id"}`.

### Licences, checked 2026-10-14

| Model | Code | Weights | Gated | Training data (task) | Sources checked |
|---|---|---|---|---|---|
| BiRefNet-portrait (pending) | MIT | MIT | no | P3M-10k (released under the MIT license) + TR-humans (Apache-2.0, synthetic); the starting checkpoint isn't stated | <https://huggingface.co/ZhengPeng7/BiRefNet-portrait>, <https://github.com/ZhengPeng7/BiRefNet>, P3M-10k dataset agreement |
| EoMT-S COCO panoptic (pending) | MIT | MIT; DINOv2 backbone Apache-2.0 | no | COCO panoptic (annotations CC BY 4.0; images under Flickr's terms) | <https://huggingface.co/tue-mps/coco_panoptic_eomt_small_640_2x>, <https://github.com/tue-mps/eomt> |
| SAM 2.1 Hiera-Tiny | Apache-2.0 | Apache-2.0 | no | SA-1B (Meta's) + SA-V (CC BY 4.0) | <https://github.com/facebookresearch/sam2>, <https://huggingface.co/facebook/sam2.1-hiera-tiny> |
| Depth Anything V2 Small (not offered) | Apache-2.0 | Apache-2.0 (Base / Large are CC-BY-NC) | no | teacher on Virtual KITTI 2 (non-commercial) among others; student on SA-1B pseudo-labels | <https://github.com/DepthAnything/Depth-Anything-V2>, <https://huggingface.co/depth-anything/Depth-Anything-V2-Small-hf> |
| MediaPipe Selfie Multiclass | Apache-2.0 | Apache-2.0 (model card) | no | Google's own (card) | <https://storage.googleapis.com/mediapipe-assets/Model%20Card%20Multiclass%20Segmentation.pdf> |
| MediaPipe Face Landmarker (BlazeFace, FaceMesh-V2) | Apache-2.0 | Apache-2.0 (model cards) | no | "consented images of people" captured by Google (cards) | <https://storage.googleapis.com/mediapipe-assets/MediaPipe%20BlazeFace%20Model%20Card%20(Short%20Range).pdf>, <https://storage.googleapis.com/mediapipe-assets/Model%20Card%20MediaPipe%20Face%20Mesh%20V2.pdf> |
| SAM 3 | SAM License | SAM License | yes | — | see [Licence of SAM 3](#licence-of-sam-3) |

Rejected, and why: YuNet (its training set WIDER FACE is CC BY-NC-ND), the general BiRefNet / BiRefNet_lite / IS-Net checkpoints (trained on DIS5K,
whose terms forbid commercial use), BiRefNet matting / HR (PPM-100, CC BY-NC-SA), RMBG 1.4 / 2.0
(non-commercial, gated), ViTMatte (Adobe's Composition-1k), SegFormer (NVIDIA non-commercial
licence), ADE20K-trained Mask2Former / OneFormer (ADE20K images are for non-commercial research),
Depth Anything V2 Base / Large (CC-BY-NC), every face-parsing network found (CelebAMask-HQ, LaPa,
Helen, iBugMask, LIP, FaceSynthetics, Sapiens: non-commercial). Backbones pre-trained on ImageNet
are industry practice and not counted against a model.

Not yet covered: **Describe** without SAM 3 (Florence-2-base, MIT, and OWLv2 / Grounding DINO are
candidates, all with research-only data in their training sets: a maintainer decision) and
**Depth Range**. No openly licensed face-parsing network exists (all are trained on
non-commercial datasets), so face parts come from MediaPipe's landmarks.

## SAM 3 (optional)

SAM 3 (Segment Anything with Concepts, Meta 2025) is the only model for Describe, and when it is
installed Object uses it instead of SAM 2.1. Its weights are gated under Meta's SAM License, so
LightCraft can't host or download them by default. On macOS it runs on the GPU through Metal.

### Remote Metal inference

Native builds with `sam` can keep the editor, RAW files, library, and exports local while
a Mac runs SAM 3. Build both ends from the same revision. On the Mac, install the model
as described below, then run:

```sh
cargo build --release --locked -p lightcraft-segment --bin lightcraft-sam3-worker
target/release/lightcraft-sam3-worker "$HOME/Library/Application Support/LightCraft/models/sam3" 127.0.0.1:8793
```

On the editing computer, start an SSH tunnel in one terminal and LightCraft in another:

```sh
ssh -N -T -o ExitOnForwardFailure=yes -L 127.0.0.1:8793:127.0.0.1:8793 user@your-mac
LIGHTCRAFT_SAM3_REMOTE=127.0.0.1:8793 lightcraft
```

Object clicks, Describe prompts, and detail refinement use the remote worker. Only the
rendered model input (long edge at most 1008 pixels) and prompts cross the tunnel. The
worker caches the current photo; detail crops do not replace that cache. Returned masks
stay in the local library, so later adjustments and exports work without the connection.

The worker requires Metal and accepts one editing connection at a time. It loads weights
on demand and releases them when the client disconnects or remains idle for ten minutes.
Both endpoints require loopback addresses: SSH provides authentication and encryption.
The listener has no independent authentication; do not expose it through a public proxy. Any
process of any user on the worker Mac can connect to it, and on the editing computer a local
process listening on the port while the tunnel is down receives the rendered model input, so use
this only on machines whose local users you trust.

No local weights are required in remote mode. An unavailable worker produces an explicit
error, not a silent CPU fallback. Restore the tunnel and retry. Unset
`LIGHTCRAFT_SAM3_REMOTE` to use the local model again. This offloads segmentation only,
not RAW development, denoising, or upscaling. The model licence below still applies.

### Getting SAM 3

#### In the app (recommended)

Object uses SAM 3 when it is installed or a remote worker is set (`LIGHTCRAFT_SAM3_REMOTE`, above),
else SAM 2.1; with none of them, it offers SAM 2.1 (the open model above). With a remote worker,
Object and Describe need no local SAM 3 and nothing is offered for download (an unreachable worker
is an error). Otherwise, the first time you pick Describe without SAM 3 (or install SAM 3 for
Object), LightCraft asks:

> **Download the SAM 3 model?** Object and Describe masks use SAM 3, Meta's segmentation model.
> It isn't part of LightCraft, and everything else works without it. Download it once (about
> 3.4 GB) · *folder*. Licence: SAM License (Meta) — Meta's terms, not LightCraft's. Downloading
> it means accepting them. [Read the SAM License] — **Not Now** / **Download**

Nothing is downloaded until you choose **Download**. The download runs in the background (close
the dialog and keep working; the Masking panel shows its progress), can be cancelled, resumes
where it stopped if interrupted (also after quitting), and tries the next download location when
one fails or stalls. Each file is written as `<name>.part` and moved into the model folder only
when complete and verified — `model.safetensors` must have the exact size and SHA-256 of Meta's
official checkpoint; a file that doesn't match is deleted, never used. If no location works, the
dialog says why and LightCraft carries on without the model.

Agents and scripts use the same commands (control channel, `lightcraft-cli mcp --connect`):
`segment.model.status`, `segment.model.download {"acknowledged": true}` (refused without the
explicit `acknowledged`: pass it only after the user agreed to the download and the licence),
`segment.model.cancel`.

#### Download locations

The model is fetched from an ordered list of mirrors: base URLs where `<base>/model.safetensors`,
`<base>/vocab.json` and `<base>/merges.txt` live. The list is, in order:

1. `LIGHTCRAFT_SAM3_MIRRORS` (environment variable; URLs separated by commas or spaces);
2. the file `models/sam3-mirrors.txt` in LightCraft's settings folder (one URL per line, `#`
   comments) — `~/Library/Application Support/LightCraft/` (macOS), `%APPDATA%\LightCraft\`
   (Windows), `~/.config/lightcraft/` (Linux);
3. the built-in list, `DEFAULT_MIRRORS` in `crates/segment/src/fetch.rs`.

> **Maintainers:** the built-in list is **empty** until LightCraft's own CDN locations exist
> (see the `TODO(maintainer)` there): add them in order of preference, host the three files
> unchanged, and pin `vocab.json` / `merges.txt` (size + SHA-256) in `SAM3_FILES` at the same
> time. Until then the in-app download needs a user-configured mirror, and the dialog says so:
> it names the three files to put in the model folder by hand, with a button that shows that folder
> (created if needed) and a link to this guide.

Hugging Face's `facebook/sam3` can't be a default: it is **gated** (each person must accept the
licence there, wait for approval and download with their own token). Someone with access can
use it as their mirror:

```sh
LIGHTCRAFT_SAM3_MIRRORS=https://huggingface.co/facebook/sam3/resolve/main \
LIGHTCRAFT_SAM3_TOKEN=hf_... lightcraft
```

(`LIGHTCRAFT_SAM3_TOKEN` is sent as a bearer token only over https, and only to the mirror's own
host — not to the storage host it redirects to.)

The download is plain HTTP/1.1 over TLS in pure Rust (rustls with the RustCrypto provider and
the Mozilla root certificates; no OpenSSL, `ring` or `aws-lc`, nothing compiled from C), with a
15 s connect timeout and a 30 s stall timeout; it follows redirects but doesn't use HTTP proxies.

#### By hand

Put `model.safetensors`, `vocab.json` and `merges.txt` from `facebook/sam3` in the model folder:
`models/sam3/` in the settings folder above, or the folder `LIGHTCRAFT_SAM3_DIR` names. Developers
can use the installer scripts, which download from Hugging Face with your token and verify the
weights:

```sh
HF_TOKEN=hf_... tools/install-sam3.sh            # macOS, Linux
tools/install-sam3.sh --check                     # verify an installation
```

```powershell
$env:HF_TOKEN = "hf_..."; .\tools\install-sam3.ps1   # Windows
```

### Licence of SAM 3

LightCraft's code is MIT OR Apache-2.0; `crates/segment` (a port of Apache-2.0 code) is
Apache-2.0. **The SAM 3 weights are neither**: they are Meta's, released under the **SAM
License** (<https://github.com/facebookresearch/sam3/blob/main/LICENSE>), a custom licence that
is not an OSI open-source licence. It grants a non-exclusive, worldwide, non-transferable,
royalty-free licence to use, reproduce, distribute, copy, modify and create derivative works of
the "SAM Materials", subject to its terms, including: anyone redistributing them must provide a
copy of the licence with them, and derivative works are distributed under its terms; uses
subject to ITAR, military or warfare purposes, nuclear industries, espionage, guns or illegal
weapons, and reverse engineering are prohibited, and trade controls (sanctions, export rules)
apply; the materials come "as is", without warranty; Meta may terminate the licence on a breach
(then the materials must be deleted); California law governs. Read the licence itself — this
summary is not legal advice.

Therefore:

- **The weights are never part of this repository, its releases or installers**, and must not be
  committed.
- They reach a user's computer only when that user asks for them, after the dialog names the
  licence (or when they install them by hand).
- Whoever hosts a mirror redistributes the SAM Materials and must follow the licence (ship it
  next to the files).

## Commands (control channel, MCP)

| Command | Params | |
|---|---|---|
| `mask.add` | `{kind: "object", points?: [[x,y],…], exclude?: [[x,y],…], seg?}` | an Object mask (empty until clicked) |
| `mask.add` | `{kind: "prompt", text, seg?}` | a Describe mask |
| `mask.addComponent` | `{op: add\|subtract\|intersect, kind: "object"\|"prompt", …}` | the same as a component |
| `mask.objectPoint` | `{x, y, exclude?: bool, id?}` | one click on the selected mask's Object selection (≤ 64 clicks) |
| `mask.refineDetail` | `{id?, component?}` | the zoomed-in detail pass → `{started}` |
| `mask.add` | `{kind: "subject"\|"background"\|"sky", seg?}` | an automatic mask; `{pending}` in the app, `{approximate, model, note}` without its model |
| `mask.add` | `{kind: "people", person?: 0..n, parts?: ["Hair", "Lips", …]}` | a People mask (none or "Entire Person": the whole person) |
| `mask.add` | `{kind: "landscape", class: sky\|water\|vegetation\|mountains\|architecture\|naturalGround\|artificialGround}` | a Landscape mask |
| `mask.recompute` | `{id?, component?}` | compute a mask's automatic selections again with their models (Update) → `{updated, pending}` |
| `ai.models.status` | `{}` | every model: `{id, title, purpose, installed, dir, sizeBytes, license, licenseUrl, pageUrl, open, licencePending, mirrors, download}` |
| `ai.models.download` | `{id, acknowledged: true}` | download one in the background (an activity row) → `{started, installed, downloading}` |
| `ai.models.cancel` | `{id}` | stop it (it resumes next time) → `{cancelled}` |
| `segment.prepare` | `{}` | load the model and analyze the active photo → `{busy}` |
| `segment.model.status` | `{}` | `{available, installed, remote, dir, loaded, busy, analyzing, sizeBytes, license, licenseUrl, mirrors, download}` |
| `segment.model.download` | `{acknowledged: true}` | start the download in the background → `{started}` |
| `segment.model.cancel` | `{}` | stop it (it resumes next time) → `{cancelled}` |

`remote` reports the configured endpoint, not its health. `installed` describes local weights only.
`download` is `{running, done, total, file, error, cancelled, finished}`: `cancelled` is true when the last
download was cancelled (its `error` says so in words; decide on the flag). An AI mask command refused because
the model is not installed (or still downloading) fails with an error that starts with "The SAM 3 model is
not installed"; inside LightCraft it is `ErrorKind::ModelMissing(Sam3)`, which is what the app offers the
download on.

Coordinates are normalized to the uncropped, oriented photo, like every mask shape. `seg` passes
a stored segmentation (as `mask.list` / the develop settings hold it): the mask is made without
the model. Without SAM 3 (local, or a remote worker set by `LIGHTCRAFT_SAM3_REMOTE`), Describe
(`prompt`) fails with an error starting "The SAM 3 model is not installed" that says how to get it;
Object fails only when there is no SAM 3 and SAM 2.1 isn't installed either, with an error offering
SAM 2.1. With a remote worker, an unreachable worker is reported as such, not as a missing model. Subject, Background and Sky never fail for a missing model: they
return an approximate mask (`{approximate, model, note}`).

In the desktop app the commands don't wait for the model: `mask.objectPoint` returns
`{pending: true}` and the selection is applied when it's computed (one undo step per settled
selection); `mask.add {kind: "prompt"}` returns `{pending: true}` and the mask appears when
something is found (or a message says nothing was). `lightcraft-cli` and the headless MCP server
wait and return the result (when built with the `sam` feature; the default CLI has no AI masks).

## Implementation notes

- `crates/segment` ports the Hugging Face `transformers` implementation (Apache-2.0; see
  `NOTICE` — the ported files say so and that they were modified): the 32-layer ViT backbone
  with 2-D RoPE and windowed attention, the feature pyramids, the SAM 2-style prompt encoder and
  two-way mask decoder for clicks, and the CLIP text encoder, DETR encoder/decoder (box
  relative-position bias, presence token), pixel decoder and semantic head for text. The CLIP tokenizer is a
  small BPE in `tokenizer.rs`. The downloader (original code) is the `lightcraft-fetch` crate, shared by any model that is downloaded; `fetch/` here says what SAM 3 needs.
- candle is pinned at 0.9.2: later releases make `candle-core` depend on `tokenizers` with the
  Oniguruma C library, and the product is pure Rust. Weights are read with positional reads (no
  memory map: `unsafe` stays in `lightcraft-sysmem`), only the tensors a path needs, and every
  tensor's byte range is checked against the file's length when it is opened (a truncated or
  damaged file is an error, never a crash).
- The engine (`crates/engine/src/segment/`) owns one worker thread with the model and the last
  encoded photo; the session sends it jobs and receives results over channels, never holding a
  lock the model holds. Each job runs under `catch_unwind`: a panic inside candle becomes an
  error and the model is reloaded on the next request; busy counters are reset by drop guards.
- GPU fallback (issue #1974): a job that fails on the GPU (a panic or a candle error, e.g. an
  AMD GPU in an Intel Mac whose Metal compiler crashes on a matmul kernel) frees the model and runs
  once more on the CPU. If that works, the model stays on the CPU until LightCraft restarts and
  the user is told once ("The GPU couldn't run the AI mask model…"); if the CPU fails too, the
  error is reported and the next job tries the GPU again. Missing or damaged model files and
  remote-worker errors are not retried. `LIGHTCRAFT_GPU=0` or `LIGHTCRAFT_GPU_BACKEND=off` keep
  the model on the CPU from the start.
- Hostile documents: a selection keeps at most 4 detail patches (when read and when rendered),
  each at most 1024² logits; more than 64 clicks are never sent to the model.
- Accuracy: `crates/segment/tests/it/reference.rs` compares every stage against the Python
  reference (`tools/sam3_reference.py`); on Metal the backbone matches to 7e-5 relative error
  and masks to ~5e-5. f16 was tried and rejected (overflow, ~7 % faster).
- The engine feature `sam` (on in the desktop app, off for web and CLI) gates the model; without
  it the commands report that the build has no AI masks.

**Developers:** the accuracy test against the Python reference needs `torch` and
`transformers` (`tools/requirements-sam3-reference.txt`):

```sh
python3 -m venv .venv-sam3 && .venv-sam3/bin/pip install -r tools/requirements-sam3-reference.txt
.venv-sam3/bin/python tools/sam3_reference.py photo.jpg ref.safetensors
LIGHTCRAFT_SAM3_DIR=<model dir> LIGHTCRAFT_SAM3_REF=ref.safetensors cargo test -p lightcraft-segment --release -- --nocapture
```
