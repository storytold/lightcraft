# AI Denoise training tools

Offline tooling for training LightCraft's own AI Denoise model. Nothing here is part of the product: Python and
PyTorch are used only to train, and the result is an ONNX file that LightCraft's pure-Rust runners read
(`crates/denoise`). Weights and data are never committed; keep them in a scratch folder outside the repository.

## Data and licences

- **Pictures:** clean, low-ISO (≤ 400) Bayer raws from [raw.pixls.us](https://raw.pixls.us/) whose licence on the site
  is **CC0** (public domain). `fetch_rpu.py` checks every file's SHA-256 and records it in `sources.csv`
  (URL, camera, ISO, licence), which is the model's data credit.
- **Noise:** synthetic Poisson-Gaussian sensor noise. Its parameters can be calibrated from your own photos with
  `calibrate_noise.py`, which stores numbers only (per ISO: `variance = a · mean + b`), never pictures.
- Not used: RawNIND (the dataset is CC BY-SA 4.0 and its published weights GPL-3.0), SIDD, FiveK, DIV2K and other
  research-only sets.

## Steps

```text
python -m venv <scratch>/venv
<scratch>/venv/Scripts/pip install torch --index-url https://download.pytorch.org/whl/cu128
<scratch>/venv/Scripts/pip install numpy onnx onnxscript

# LightCraft's own raw decoder, as a small helper (not a workspace member)
CARGO_TARGET_DIR=<target> cargo build --release --manifest-path tools/denoise-train/rawdump/Cargo.toml

python fetch_rpu.py --out <scratch>/data --rawdump <target>/release/lc-denoise-rawdump
python calibrate_noise.py --rawdump <...> --out <scratch>/noise/camera.json --sample 200 <photo folders>   # optional
python train.py --data <scratch>/data --out <scratch>/run1 --steps 160000 --duty 0.75
python export.py --ckpt <scratch>/run1/final.pt --out <scratch>/model --rawdump <...> --data <scratch>/data
```

`export.py` writes `model.onnx` and `denoise-model.json` (with its SHA-256, size, licence and data credits) and runs
LightCraft's installation self-test on them (`rawdump selftest`). Install the folder with Settings ▸ AI Denoise ▸
**Install from file…**.

## The model

`model.py`: a U-Net (widths 32-64-128-256-512, about 7.8 M weights) for the `bayerToRgb` contract: RGGB planes
`[1, 4, t, t]` → camera RGB `[1, 3, 2t, 2t]`, on the input's scale (gain `none`). It uses only the operators
LightCraft reads: 3 × 3 and 1 × 1 convolutions with LeakyReLU, 2 × 2 max-pooling, 2 × 2 transposed convolutions,
channel concatenation into a convolution, and a final depth-to-space.

The training target is the clean raw with every 2 × 2 cell binned into one RGB pixel, so the reference needs no
demosaicing and has half the noise of the source again. Patches are darkened by a random exposure and re-mosaicked,
then noise is added for a shot-noise coefficient spread from about ISO 50 to beyond ISO 100 000.
