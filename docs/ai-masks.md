# AI masks (Object and Describe)

> Names in angle brackets (`<app>`, `<binary>`, `<cli>`, `<PREFIX>`, `<settings_dir>`, …) are the values set in
> [`brand.toml`](../brand.toml); see the README.

The app selects objects with **SAM 3** (Segment Anything with Concepts, Meta 2025), run in
pure Rust by `crates/segment` on [candle](https://github.com/huggingface/candle): on the GPU
through Metal on macOS, on the CPU elsewhere (for now).

**The model is optional.** It is not part of the app, and nothing requires it: without it,
Object and Describe offer to download it, every other feature (and every existing mask, see
below) works, and no render, export or command ever waits for it.

## Using them

In the Masking panel (M):

- **Object**: click the thing you want; every click refines the selection. ⌥-click (Alt) a part
  to leave it out. Clicks show as green (include) and red (leave out) dots.
- **Describe**: type what to select — `sky`, `trees`, `the red car` — and press Return. Every
  instance the model finds (score above 0.5) joins the mask; if nothing matches, nothing is
  created and you are told so.
- Several things at once: `car, road` (comma-separated) selects each and merges them.
- **+ / −** under the selected mask (and the Add / Subtract / Intersect menus) combine
  selections: Describe `car`, click the mask, + ▸ Describe… `road`; − ▸ Describe… `people` takes
  the people out of a sky mask; − ▸ Object then clicks remove one object.
- **Edge** (per selection, −100…100): below 0 a crisper border, above 0 a feathered one (up to 2 %
  of the long edge, so previews and exports match).
- **Detail:** about a second after the last click (or right after a description), the photo
  around the selection is analyzed again, zoomed in, from a sharper source (~5 s on an M4 Pro,
  in the background); the result is kept as a high-resolution patch (at most 4 per selection),
  so small objects get 5–10× finer edges than one pass over the whole photo gives.

The first Object or Describe on a photo analyzes it (the image encoder runs once per photo and
look: ~3 s on an Apple M4 Pro, plus a one-time ~6 s for compiling GPU kernels and loading the
model after launch). That starts in the background as soon as you pick Object; after it, each
click takes ~35 ms and each description ~0.6 s. Everything the model does runs on its own worker
thread: the window stays responsive, a click shows its dot at once and the selection follows
when it's computed ("Selecting…" / "Analyzing the photo…" under the mask tiles). The model is
unloaded after 10 minutes without use (it takes several GB of memory) and loaded again when
needed.

The result is stored with the mask: the model's 288 × 288 mask logits over the uncropped photo
(quantized, compressed, ~10–30 KB). Renders and exports sample them at any resolution and never
need the model, so masks survive moving the library to a machine without it, and the web build
renders them. Editing the photo's look later does not recompute a mask (click again to update).

## Sky, Subject, Background and Landscape without a model

These masks don't use SAM 3. A photo that carries its own mattes (DNG semantic masks, e.g.
iPhone ProRAW) uses them. Otherwise the app analyses the whole photo once per look
(`crates/pipeline/src/auto_mask.rs`). It works on a 256 px proxy of the uncropped, oriented,
white-balanced photo, keeps the result cached, and samples it at any size, crop or zoom window,
then snaps it to the photo's edges with a guided filter:

- **Sky:** a colour model seeded from smooth, sky-coloured pixels near the top and refitted twice.
  Each column ends the sky at the row that best separates sky-like pixels above from the rest
  below, with smooth gradients carrying the sky on and a bonus for ending at an edge.
- **Subject** (Background is its inverse): colours unlike the frame's border, sharper than
  their surroundings and near the centre, thresholded with Otsu and kept to the main regions.
- **Landscape:** `sky`, `water` (below the sky line, smooth, sky-hued or blue-green) and
  `vegetation` (green). Mountains, architecture and the ground classes select nothing.

**Evaluation.** The procedural demo scenes know what each pixel shows (`lcc_scenes::Label`). That
made every demo photo a labelled reference; the soft IoU per scene below was measured with an
evaluation harness that has since been removed from the test suite (2026-10-13 test audit).

| Mask | Scenes | Before (per-pixel heuristic) | After |
|---|---|---|---|
| Sky | 21 | 0.649 | 0.787 |
| Subject | 3 (macro) | 0.396 | 0.881 |
| Landscape water | 12 | 0.000 | 0.321 |

The demo scenes are synthetic, so these numbers can't show quality on real photos. Weak cases:
a calm sea under a sunset sky (0.43–0.64), aurora rays (0.50–0.55, textured sky) and a sea the
same blue as the sky (0.60–0.67). Real photos need a CC0 set with hand-made reference masks in
`corpus/`, which doesn't exist yet.

## Subject and Background with BiRefNet

**Maintainer decision 2026-10-13: BiRefNet adopted** for Subject (and Background, its inverse);
the plug-in store, signing and Lua plug-ins (gap 19) are on hold.

`crates/segment/src/birefnet.rs` runs **BiRefNet_lite** (Swin-T backbone, MIT code and weights,
`ZhengPeng7/BiRefNet_lite`) in pure Rust on candle. Like SAM 3 it is optional and downloaded only
on request: `segment.subject.download {"acknowledged": true}` fetches `model.safetensors`
(177,634,392 bytes, SHA-256 `4417d897…0815`, from the author's Hugging Face repository at a pinned
revision; `<PREFIX>_BIREFNET_MIRRORS` adds mirrors) into `<settings_dir>/models/birefnet`
(`<PREFIX>_BIREFNET_DIR` overrides it); `segment.subject.status` reports it and puts a finished
download to use. The weights are never committed or shipped.

Once installed, the engine registers it as the pipeline's subject model
(`lcc_pipeline::auto_mask::set_subject_model`). Subject and Background masks then use its matte:
it sees the uncropped, oriented photo at 1024 px (exposure-normalized, sRGB), stretched to the
1024² input, and its 1024² matte is sampled over the frame like the analysis grid and snapped to
the photo's edges. A run is cached per photo (white balance left out, so a WB drag doesn't re-run
it) and holds an activity-stack row ("Finding the subject", kind `segment`). If the model can't
load or run, the photo analysis is used. Without the model nothing changes. The CLI and web builds
don't enable the `sam` feature, so they always use the analysis.

**Port check.** `tools/birefnet_reference.py` runs the MIT reference implementation (the
checkpoint's own `birefnet.py`, fp32 CPU) and `crates/segment/tests/birefnet_reference.rs`
compares: backbone stages within 2.5·10⁻⁴ relative, logits within 2·10⁻⁵ relative, no pixel
on the other side of 0.5. Shape and plumbing tests use tiny synthetic weights
(`birefnet::tiny_checkpoint`). Sources: the BiRefNet paper (Zheng et al., arXiv 2401.03407) and the
MIT reference implementation (github.com/ZhengPeng7/BiRefNet); Swin Transformer (Liu et al.,
ICCV 2021); DCNv2 (Zhu et al., CVPR 2019).

**Measured (2026-10-13, Linux, 32 cores shared with other jobs, load average 10–20):** about
5.5–5.8 s per photo on the CPU (backbone at 1024 and 512 px ≈ 2.6 s, decoder ≈ 2.8 s). On the
three labelled demo macro scenes its soft IoU is **0.44** (0.55 / 0.28 / 0.49), against 0.88 for
the analysis: the procedural bokeh discs are drawn sharp, so the model reads them as foreground
objects along with the flower (`cargo test --release -p lcc-engine --features sam --test
subject_eval`, `MASK_EVAL_DUMP` writes the mattes). The flower itself, stem included, is cut out
cleanly. The synthetic scenes favour the hand-tuned analysis; a real-photo reference set is still
needed to compare them fairly. Not measured yet: Metal on macOS.

Left: real-photo evaluation, GPU (Metal) timing, the full BiRefNet (Swin-L) as an option, a
smaller input for drafts, and an in-app offer of the download (today a command).

### Model options

Weight licences were checked as published by their authors. Verify them again before shipping
anything:

- **BiRefNet** (MIT code and weights): high-quality salient-object / subject matting. It is the
  strongest candidate for Subject and Background. It needs a candle port (Swin backbone + decoder,
  ~200 MB of f32 weights, ~1–2 s per photo on CPU), downloaded on request like SAM 3. Its weights
  can be mirrored without a gated licence.
- **U²-Net** (Apache-2.0, weights included): a smaller (4.7 MB / 176 MB) salient-object model.
  It is weaker than BiRefNet and quick to port.
- **MODNet** (Apache-2.0): portrait matting, a candidate for People (whole person only, no
  parts).
- **Mask2Former / OneFormer** (MIT code and checkpoints) trained on ADE20K: this would cover Sky
  and every Landscape class. Check the ADE20K terms that apply to derived weights first. It is a
  large port.
- **Rejected:** SegFormer (NVIDIA licence, non-commercial), and anything trained only on
  non-commercial data or released under GPL / CC-BY-NC.

BiRefNet was adopted for Subject and Background (above). For Sky, People and Landscape the choice is
still open, and the analysis above runs.

### SAM 3 mirror

The mirror mechanism is finished in code: `<PREFIX>_SAM3_MIRRORS`, the `sam3-mirrors.txt`
file, and the built-in `DEFAULT_MIRRORS`, with size + SHA-256 pinning of all three files (checked
against the official checkpoint on 2026-10-13). What's missing isn't code but a
host. Someone has to redistribute Meta's SAM Materials under the SAM License (with the licence
next to the files) and add that URL.
Only the maintainer can decide that. Hugging Face's `facebook/sam3` is gated and can't be a
default.

## Remote Metal inference

Native builds with `sam` can keep the editor, RAW files, library, and exports local while
a Mac runs SAM 3. Build both ends from the same revision. On the Mac, install the model
as described below, then run:

```sh
cargo build --release --locked -p lcc-segment --bin sam3-worker
target/release/sam3-worker "$HOME/Library/Application Support/<settings_dir>/models/sam3" 127.0.0.1:8793
```

On the editing computer, start an SSH tunnel in one terminal and the app in another:

```sh
ssh -N -T -o ExitOnForwardFailure=yes -L 127.0.0.1:8793:127.0.0.1:8793 user@your-mac
<PREFIX>_SAM3_REMOTE=127.0.0.1:8793 <binary>
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
`<PREFIX>_SAM3_REMOTE` to use the local model again. This offloads segmentation only,
not RAW development, denoising, or upscaling. The model licence below still applies.

## Getting the model

### In the app (recommended)

The first time you pick Object or Describe without the model, the app asks:

> **Download the SAM 3 model?** Object and Describe masks use SAM 3, Meta's segmentation model.
> It isn't part of the app, and everything else works without it. Download it once (about
> 3.4 GB) · *folder*. Licence: SAM License (Meta) — Meta's terms, not the app's. Downloading
> it means accepting them. [Read the SAM License] — **Not Now** / **Download**

Nothing is downloaded until you choose **Download**. The download runs in the background (close
the dialog and keep working; the Masking panel shows its progress), can be cancelled, resumes
where it stopped if interrupted (also after quitting), and tries the next download location when
one fails or stalls. Each file is written as `<name>.part` and moved into the model folder only
when complete and verified — `model.safetensors` must have the exact size and SHA-256 of Meta's
official checkpoint; a file that doesn't match is deleted, never used. If no location works, the
dialog says why and the app carries on without the model.

Agents and scripts use the same commands (control channel, `<cli> mcp --connect`):
`segment.model.status`, `segment.model.download {"acknowledged": true}` (refused without the
explicit `acknowledged`: pass it only after the user agreed to the download and the licence),
`segment.model.cancel`.

### Download locations

The model is fetched from an ordered list of mirrors: base URLs where `<base>/model.safetensors`,
`<base>/vocab.json` and `<base>/merges.txt` live. The list is, in order:

1. `<PREFIX>_SAM3_MIRRORS` (environment variable; URLs separated by commas or spaces);
2. the file `models/sam3-mirrors.txt` in the app's settings folder (one URL per line, `#`
   comments) — `~/Library/Application Support/<settings_dir>/` (macOS), `%APPDATA%\<settings_dir>\`
   (Windows), `~/.config/<settings_dir>/` (Linux);
3. the built-in list, `DEFAULT_MIRRORS` in `crates/segment/src/fetch/mod.rs`.

A mirror can also be a **local folder** holding the three files: an absolute path or a
`file://` URL (`<PREFIX>_SAM3_MIRRORS=/data/sam3` or `file:///data/sam3`; on Windows
`file:///C:/data/sam3`). The files are copied into the model folder with the same checks as a
download (exact size and SHA-256, `.part` file, progress in the activity stack, cancellable); a
copy that doesn't match is deleted. Use this when you already have the checkpoint (for example
from Hugging Face with your own access) and want the app to verify and install it.

> **Maintainers:** the built-in list is **empty** until the app's own CDN locations exist
> (see the `TODO(maintainer)` there): add them in order of preference, host the three files
> unchanged (they must match the pins in `SAM3_FILES`). Until then the in-app download needs a user-configured mirror, and the dialog says so:
> it names the three files to put in the model folder by hand, with a button that shows that folder
> (created if needed) and a link to this guide.

Hugging Face's `facebook/sam3` can't be a default: it is **gated** (each person must accept the
licence there, wait for approval and download with their own token). Someone with access can
use it as their mirror:

```sh
<PREFIX>_SAM3_MIRRORS=https://huggingface.co/facebook/sam3/resolve/main \
<PREFIX>_SAM3_TOKEN=hf_... <binary>
```

(`<PREFIX>_SAM3_TOKEN` is sent as a bearer token only over https, and only to the mirror's own
host — not to the storage host it redirects to.)

The download is plain HTTP/1.1 over TLS in pure Rust (rustls with the RustCrypto provider and
the Mozilla root certificates; no OpenSSL, `ring` or `aws-lc`, nothing compiled from C), with a
15 s connect timeout and a 30 s stall timeout; it follows redirects but doesn't use HTTP proxies.

### By hand

Put `model.safetensors`, `vocab.json` and `merges.txt` from `facebook/sam3` in the model folder:
`models/sam3/` in the settings folder above, or the folder `<PREFIX>_SAM3_DIR` names. Developers
can use the installer scripts, which download from Hugging Face with your token and verify the
weights:

```sh
HF_TOKEN=hf_... tools/install-sam3.sh            # macOS, Linux
tools/install-sam3.sh --check                     # verify an installation
```

```powershell
$env:HF_TOKEN = "hf_..."; .\tools\install-sam3.ps1   # Windows
```

## Licence of the model

The app's code is MIT; `crates/segment` (a port of Apache-2.0 code) is
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
| `segment.prepare` | `{}` | load the model and analyze the active photo → `{busy}` |
| `segment.model.status` | `{}` | `{available, installed, remote, dir, loaded, busy, analyzing, sizeBytes, license, licenseUrl, mirrors, download}` |
| `segment.model.download` | `{acknowledged: true}` | start the download in the background → `{started}` |
| `segment.model.cancel` | `{}` | stop it (it resumes next time) → `{cancelled}` |
| `segment.subject.status` | `{}` | the Subject / Background model (BiRefNet_lite): installed, in use, download progress → `{available, installed, active, dir, sizeBytes, license, licenseUrl, download}` |
| `segment.subject.download` | `{acknowledged: true}` | download it (178 MB, MIT) in the background → `{started, installed, downloading}` |
| `segment.subject.cancel` | `{}` | stop that download → `{cancelled}` |

`remote` reports the configured endpoint, not its health. `installed` describes local weights only.
`download` is `{running, done, total, file, error, cancelled, finished}`: `cancelled` is true when the last
download was cancelled (its `error` says so in words; decide on the flag). An AI mask command refused because
the model is not installed (or still downloading) fails with an error that starts with "The SAM 3 model is
not installed"; inside the app it is `ErrorKind::ModelMissing(Sam3)`, which is what the app offers the
download on.

Coordinates are normalized to the uncropped, oriented photo, like every mask shape. `seg` passes
a stored segmentation (as `mask.list` / the develop settings hold it): the mask is made without
the model. Without the model, the AI kinds fail with an error starting "The SAM 3 model is not
installed" that says how to get it.

In the desktop app the commands don't wait for the model: `mask.objectPoint` returns
`{pending: true}` and the selection is applied when it's computed (one undo step per settled
selection); `mask.add {kind: "prompt"}` returns `{pending: true}` and the mask appears when
something is found (or a message says nothing was). `<cli>` and the headless MCP server
wait and return the result (when built with the `sam` feature; the default CLI has no AI masks).

## Implementation notes

- `crates/segment` ports the Hugging Face `transformers` implementation (Apache-2.0; see
  `NOTICE` — the ported files say so and that they were modified): the 32-layer ViT backbone
  with 2-D RoPE and windowed attention, the feature pyramids, the SAM 2-style prompt encoder and
  two-way mask decoder for clicks, and the CLIP text encoder, DETR encoder/decoder (box
  relative-position bias, presence token) and pixel decoder for text. The CLIP tokenizer is a
  small BPE in `tokenizer.rs`. The downloader (original code) is the `lcc-fetch` crate, shared by any model that is downloaded; `fetch/` here says what SAM 3 needs.
- candle is pinned at 0.9.2: later releases make `candle-core` depend on `tokenizers` with the
  Oniguruma C library, and the product is pure Rust. Weights are read with positional reads (no
  memory map: `unsafe` stays in `lcc-sysmem`), only the tensors a path needs, and every
  tensor's byte range is checked against the file's length when it is opened (a truncated or
  damaged file is an error, never a crash).
- The engine (`crates/engine/src/segment/`) owns one worker thread with the model and the last
  encoded photo; the session sends it jobs and receives results over channels, never holding a
  lock the model holds. Each job runs under `catch_unwind`: a panic inside candle becomes an
  error and the model is reloaded on the next request; busy counters are reset by drop guards.
- Hostile documents: a selection keeps at most 4 detail patches (when read and when rendered),
  each at most 1024² logits; more than 64 clicks are never sent to the model.
- Accuracy: `crates/segment/tests/reference.rs` compares every stage against the Python
  reference (`tools/sam3_reference.py`); on Metal the backbone matches to 7e-5 relative error
  and masks to ~5e-5. f16 was tried and rejected (overflow, ~7 % faster).
- The engine feature `sam` (on in the desktop app, off for web and CLI) gates the model; without
  it the commands report that the build has no AI masks.

**Developers:** the accuracy test against the Python reference needs `torch` and
`transformers` (`tools/requirements-sam3-reference.txt`):

```sh
python3 -m venv .venv-sam3 && .venv-sam3/bin/pip install -r tools/requirements-sam3-reference.txt
.venv-sam3/bin/python tools/sam3_reference.py photo.jpg ref.safetensors
<PREFIX>_SAM3_DIR=<model dir> <PREFIX>_SAM3_REF=ref.safetensors cargo test -p lcc-segment --release -- --nocapture
```
