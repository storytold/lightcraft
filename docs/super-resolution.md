# Super Resolution and AI Sharpen

Photo ▸ Enhance ▸ **Super Resolution** makes a copy of the selected photo with twice its pixels across (four times
the pixels), filled in by a neural upscaler. **AI Sharpen** runs the same upscaler and averages its answer back
down to the photo's own size: finer detail and slightly sharper edges at the same pixel dimensions. Both write a new
16-bit TIFF next to the photo, `<name>-Enhanced-SR.tif` (then `-2`, `-3` …; nothing is ever replaced), import it,
stack it with the photo, on top, and select it. The photo and its edits are untouched.

The work runs in the background with a row in the activity stack that shows its progress and cancels it.

## What goes into the model

The photo is rendered with its **import defaults** — none of its edits — to 16-bit sRGB at full size, uncropped and
upright. Raw photos are demosaiced and get their as-shot white balance and camera look this way; finished pictures
(JPEG, PNG, TIFF …) come out as they are. The upscaler works on that picture, because models of this kind are
trained on finished sRGB pictures. Edit the new TIFF like any other photo.

This differs from Lightroom, whose Super Resolution keeps a raw a raw (an enhanced DNG that the raw's edits carry
over to). Doing the same here needs the model to run before the colour pipeline, on data it was not trained on;
that is a later step (gap 11 in [gaps.md](gaps.md)).

What an upscaler does is limited to what it learned. Real-ESRGAN General v3 was trained on pictures blurred, made
noisy and compressed on purpose: it restores slight softness, noise and compression artefacts well. It does not
undo motion blur or heavy misfocus.

## Models

**No upscaler is bundled.** Upscalers are kept with the AI Denoise models ([denoise.md](denoise.md#models-and-terms))
and installed, chosen and removed the same way, with their terms accepted first and their installation self-test.
Their manifest says `domain: "rgbToRgb4x"`: three planes of gamma-encoded sRGB in 0 to 1 in, four times the size
across out. The upscaler in use is `upscaleModel` in the models folder's `settings.json`; installing one fills that
slot, and `denoise.models.select {kind: "upscale", id: …}` changes it (`id: null` empties it). The denoise queue never
uses an upscaler.

An upscaler's self-test checks that it gives the promised size and the same answer twice, keeps the picture's
brightness, and keeps to the picture: on a black-to-white test tile, its answer averaged back down strays from the
input by at most half as much as the picture itself varies (a flat grey strays by about all of it).

The `realesrgan-model` Cargo feature offers **Real-ESRGAN General v3** (`realesr-general-x4v3`, Xintao Wang et al.,
Tencent ARC Lab) for download: about 5 MB, code and weights BSD-3-Clause, trained on the DF2K and OST picture sets,
which are published for research. The file is fetched from a pinned revision of its Hugging Face re-host and checked
against its SHA-256. Whether releases offer it is a maintainer decision, like `rawnind-model`. Without the feature,
the same file installs from disk and is recognised by its hash.

## Tiles, memory and time

The picture is cut into overlapping tiles (256 pixels with an overlap of 80 for General v3), each run through the
model; the outer quarter of the overlap counts for nothing, because a model gets the edge of its tile wrong. For 2×
and AI Sharpen each tile's 4× answer is averaged down before it is blended in, so no picture larger than the result
is held. A result may have at most 110 megapixels: 2× works on photos up to about 27 MP, 4× up to about 6.8 MP.

On an Apple M2 (Metal) a 256-pixel tile takes 0.28 s on the graphics card and about 3 s on the CPU; a 1200 × 900
picture to 2400 × 1800 takes 11.5 s with 322 MB at its peak, so a 24 MP photo takes about four minutes.

## Commands

| Command | Parameters | Result |
|---|---|---|
| `enhance.superResolution` | `{id?, scale=2\|4}` | `{id, path, bytes, width, height, factor}` |
| `enhance.aiSharpen` | `{id?}` (no `scale`) | the same, `factor: 1` |

Both act on one photo (`id`, else the selection) and run to the end before they answer (CLI, MCP, control channel).
In the desktop app, Photo ▸ Enhance runs them in the background (`enhance.superResolutionStart`,
`enhance.aiSharpenStart`).

`LIGHTCRAFT_DENOISE_MODELS` names the folder AI models are installed into (the desktop app's own by default, see
[denoise.md](denoise.md)); here a new folder `models` next to a new library `library`, with the downloaded
`realesr-general-x4v3.onnx` and a `photo.jpg` in the current folder:

```sh
LIGHTCRAFT_DENOISE_MODELS=models lightcraft-cli run --library library --import photo.jpg denoise.models.install '{"path": "realesr-general-x4v3.onnx", "acknowledged": true}' enhance.superResolution
```

Tests with the real model: `LC_SR_MODEL=<realesr-general-x4v3.onnx> cargo test --release -p lightcraft-denoise
--all-features upscale -- --include-ignored`, and `cargo test --release -p lightcraft-gpu --features denoise
real_upscaler -- --ignored --nocapture` for the graphics card.
