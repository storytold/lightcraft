# AI Defringe

Optics ▸ Defringe ▸ **AI Defringe** is a per-photo switch with an **Amount** (0 to 100). It removes colour fringes of
every hue (purple, blue-violet, green) with a trained network, and leaves real colours alone. Turning it on the first
time starts at 100; turning it off keeps the Amount. It acts on the requesting photo only, independently of Auto Sync,
and is undoable. The manual Defringe sliders below it (Purple and Green Amount and their hue ranges) still work, on
their own or together with it.

AI Defringe needs a model, and **LightCraft ships none**. Settings ▸ AI Defringe ▸ **Install from file…** takes an ONNX
file with a `defringe-model.json` beside it (see [The model contract](#the-model-contract)). Before anything is
installed, Settings shows the model's name, licence and notice, and the user accepts them. Without a model the section
says where to install one. Builds without the model runtime (the web build, or a native build without the `denoise`
feature) say so and never offer the switch.

## How it works

For each photo with AI Defringe on, the model reads the picture at the Defringe stage: the full-size source developed
with the default settings up to that point (white balance, exposure, lateral chromatic aberration removed; no
geometry, so it lines up with the source pixel for pixel). Its correction is kept as a gain per channel for each pixel
it changes, applied to the source picture before the develop pipeline. So:

- The edit never makes the correction again. Moving any slider, Amount included, uses the one already made.
- A render smaller than the source (the loupe, thumbnails) takes the mean gain over each of its pixels, so thin
  fringes aren't missed between samples.
- An export with AI Defringe on and no correction yet makes it before it renders, and fails with a message when no
  model is installed. It never comes out without the correction it was asked for.

The correction of the photo being edited is made in the background (Activity shows it and can cancel it) and kept in
`<library>/ai-defringe/<key>.lcadf`, named by a hash of the photo's content, the model file's SHA-256 and the algorithm
version (`crates/engine/src/aidefringe.rs`, `VERSION`). It is cached data: Clear (`aiDefringe.clear`) deletes it and it
is made again when wanted. A file is compressed, checksummed and read as hostile input. Up to 24 corrections stay in
memory; each holds only the pixels it changes.

Inference runs in pure Rust on the processor, with the same ONNX reader and convolution runner as AI Denoise
(`crates/denoise/src/defringe.rs`). The network runs on 256 px tiles overlapping by 32 px, blended with linear ramps,
and tiles without an edge are skipped. The picture is worked through in bands of 768 rows, so memory stays bounded on
large photos; the result is identical to working on it whole (`bands_and_tiles_leave_no_seams`). On an Apple M-series
processor a 24 MP photo's tiles take about 1.5 s with ten threads. There is no graphics card path yet.

### What the host does with the network's output

The network only proposes a change; the host decides how much of it to keep. These guards are fixed, not learned:

1. **Lightness is never changed.** Only OkLab a and b move.
2. **Only near edges.** The change is scaled by `smoothstep(0.05, 0.20, r)`, where `r` is the range of OkLab L within
   7 × 7 px, so flat areas are never touched.
3. **Bounded.** The change is at most `0.15 · tanh(Δ)` per channel, scaled by the gate's sigmoid, and no larger than
   the pixel's own chroma plus 0.01.
4. **To grey, not past it.** The part of the change against the pixel's colour stops at grey.
5. **Not green unless it was.** A pixel that wasn't green (chroma above 0.01, hue 100° to 170°) keeps at most 0.01 of
   green chroma.
6. **No more colourful than its neighbourhood:** at most 1.2 × the highest chroma within 7 × 7 px.
7. **Toward the surroundings.** Of each change, only the part that moves the pixel toward the mean colour of the
   untouched pixels within 7 px is kept, at most up to it. Without this, a correction that stopped at grey could move
   a purple fringe sideways into a brown-orange rim.
8. **Exact elsewhere.** A pixel whose colour moves by 1e-5 or less keeps its exact value.

## The model contract

`defringe-model.json` (checked, unknown fields refused):

```json
{
  "id": "my-defringe",
  "name": "My defringe model",
  "version": "1",
  "licence": "the licence the weights are under",
  "file": "my-defringe.onnx",
  "sha256": "64 hexadecimal digits of the ONNX file",
  "contract": "lightcraftDefringe1",
  "notice": "optional: anything the user should know (training data, terms)",
  "url": "optional"
}
```

The ONNX file is read by LightCraft's own bounded reader: operators Conv, LeakyRelu, MaxPool (2 × 2), ConvTranspose
(2 × 2, stride 2), Concat and DepthToSpace (CRD), as for AI Denoise.

- **Input** `[1, 20, 128, 128]`: a 256 px tile of five planes folded space-to-depth 2 × 2 (channel `c·4 + i·2 + j` is
  plane `c` at row offset `i`, column offset `j`). The planes are OkLab L, a and b, `log2(Y + 2⁻¹²) / 12` with Y the
  Rec.2020 luminance, and the clip mask (1 where the 5 × 5 mean of the brightest channel is within 7 % of the
  picture's top). The picture is linear Rec.2020 divided by its top (the 99.9th percentile of the brightest channel),
  and converted to OkLab by way of linear Rec.709 with negative values clamped.
- **Output** `[1, 3, 256, 256]`: Δa and Δb before `tanh`, and the gate before its sigmoid.

Installation checks the manifest, the file's size (at most 256 MB) and SHA-256, the network's shape, and runs it twice
on a synthetic fringe: it must give finite colours, the same answer both times, and keep lightness. A model that fails
is not installed and the one already there stays.

## Commands

`aiDefringe.toggle {id?, enabled?}`, `aiDefringe.status {id?}`, `aiDefringe.pump`, `aiDefringe.make {id?}` (makes a
correction now and waits, for scripts), `aiDefringe.retry {id?}`, `aiDefringe.clear`, and `aiDefringe.models.inspect
{path}`, `.install {path, acknowledged: true}`, `.remove`. The develop control is `optics.aiDefringe` (with
`optics.ai_defringe_on` in the settings, as AI Denoise has `enhance.denoise_on`). The model folder is
`<config>/defringe-models`, or `$LIGHTCRAFT_DEFRINGE_MODELS`.

## Limits

- No model is distributed with LightCraft or offered for download.
- The correction is made from the plain picture, not the AI-denoised one.
- Processor only; a graphics card path and a background queue for photos other than the one being edited remain to
  do.
- The Amount scales the gain linearly; at Amount 100 the result is the model's.
