# Local reflection removal (experimental)

Open a photo, choose Remove, and expand Reflections. Select DSRNet-S, DSRNet-L, or DSRNet-L (Nature), choose Preview, Standard, or High, and enable Apply. The desktop app performs inference locally; photos are not uploaded. Recalculate after changing model or quality. Amount and protection controls update without repeating inference.

Start at the default 35% Amount, with sky protection and 60% Detail Protection. Single-image reflection separation is ambiguous: clouds, glass, and real scene details can be mistaken for reflections. A larger model does not guarantee better results. Compare against the original before exporting.

Sky protection uses an embedded sky matte when available, otherwise a heuristic estimate. For reliable exclusion, enable Limit to area and set its four boundaries. Coordinates are percentages of the original oriented image, before user crop and rotation. Pixels outside the area are unchanged at the reflection stage; later filters can still affect them. Detail Protection limits the correction relative to source brightness; 100% disables correction.

## Getting the model

Weights are not distributed with LightCraft. Use the author's Apache-2.0 [DSRNet repository](https://github.com/mingcv/DSRNet) and its trained-weight links. All three variants have pinned source hashes in the conversion script. Conversion requires Python once; the installed app uses Rust/Candle, with Metal on macOS, and requires no Python runtime.

From this repository, on macOS or Linux:

```sh
python3 -m venv .venv-reflections
.venv-reflections/bin/pip install -r tools/requirements-reflection-convert.txt
.venv-reflections/bin/python tools/convert-reflection-model.py --model dsrnet-l-nature --download --output ./dsrnet-l-nature
```

On Windows use `py -m venv .venv-reflections`, then `.venv-reflections\Scripts\pip.exe` and `.venv-reflections\Scripts\python.exe` with the same arguments. Substitute `dsrnet-s` for the smaller model or `dsrnet-l` for the standard large model. Downloading the Nature checkpoint and VGG features requires roughly 1 GB of temporary storage, in addition to the converted model. The installed Nature model is approximately 542 MB.

Use Install Reflection Model… and select the resulting directory containing `manifest.json` and `model.safetensors`. The installer verifies the manifest and SHA-256 before copying. It does not replace an existing installation. Alternatively supply the author checkpoint and VGG file manually; see the script's `--help`.

Models live under the application's configuration directory in `reflection-models/<model-id>`. `LIGHTCRAFT_REFLECTION_MODELS` can override that directory. The Model download instructions link opens the author's trained-weight page; the conversion above creates the format needed by this app.

## Implementation and verification

The native port implements the DSRNet transmission branch and VGG19 feature extractor; the training-only LRM is omitted. Source and predicted transmission are converted to linear Rec.2020 and stored as a signed correction grid. Preview, loupe, and export use the same correction stage, before normal development. Inference is limited to a proxy of 384, 768, or 1024 pixels; the correction is upscaled for full-resolution exports.

DSRNet-S, DSRNet-L, and DSRNet-L Nature were compared against their upstream PyTorch implementations on deterministic 64×64 input, with maximum absolute transmission differences below 0.000001 on Metal and 0.000002 on CPU. This checks implementation agreement, not photographic quality.

Corrections are bound to the source content identity. They are stored in `reflection-cache`; keep that directory with your catalog backup or recalculate after moving machines. Missing corrections produce an error rather than silently changing the result. Reset is undoable. Original source files remain untouched.

The feature is currently desktop-only. Model weights are external assets; see NOTICE for code and model provenance. No Adobe code, assets, or model weights are included.
