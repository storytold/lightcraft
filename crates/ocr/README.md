# lightcraft-ocr

Text in photos (L1, pure Rust, no C dependencies): PaddleOCR 3's PP-OCRv6 text detector, text-line orientation classifier and recogniser, run on candle from PaddlePaddle's own ONNX exports by a small bounded ONNX executor. The wasm32 build has the model list, images, box and CTC code only; the models run natively.

Nothing in the app uses it yet (the engine command and the UI come in later PRs). The weights are never part of LightCraft: `known::download` fetches a set from its pinned Hugging Face addresses (repository, 40-character commit, size and SHA-256 for every file) and refuses to start until the caller passes `acknowledged = true`, meaning the user was shown the licence and accepted it.

## Model sets

| id | download | detector + recogniser |
|---|---|---|
| `pp-ocrv6-tiny` | 7.3 MB | PP-OCRv6 tiny |
| `pp-ocrv6-small` (recommended) | 32.3 MB | PP-OCRv6 small |
| `pp-ocrv6-medium` | 139.8 MB | PP-OCRv6 medium |

All three share the 1 MB PP-LCNet x0.25 orientation classifier. Licence: Apache-2.0 for every file (each Hugging Face model card says `license: apache-2.0`; PaddleOCR is Apache-2.0, https://github.com/PaddlePaddle/PaddleOCR/blob/main/LICENSE). Commercial use is allowed. The publisher does not itemise the training data.

## Measured: HierText validation, word spotting, CPU

500 photos of the 1,724-photo validation set (`--limit 500 --seed 55`, 53,317 legible words). A word is right when its text equals the annotation's and its box overlaps it by IoU >= 0.5. The reader gives lines, so a line is cut into words at its spaces, each word's box its share of the line's width.

| set | precision | recall | word F1 | seconds per photo (CPU) |
|---|---|---|---|---|
| tiny | 43.25 | 33.27 | 37.61 | 4.26 |
| small | 45.98 | 38.74 | 42.05 | 8.75 |
| medium | 47.42 | 42.88 | 45.04 | 28.34 |

Seconds per photo: 12 photos of the same subset, one process, nothing else running (the 500-photo accuracy runs were sharded across processes, so their own timings are not used). Reading only; the JPEG decode is not counted.

`small` is the recommendation: +4.4 F1 over tiny for 2x the time, while medium adds 3.0 more for 3.2x the time and 4.3x the download.

## Run the benchmark

HierText (Long et al., Google; https://github.com/google-research-datasets/hiertext): annotations CC BY-SA 4.0, photos CC BY 2.0 per image; used here for evaluation only and never shipped. Put `validation.jsonl.gz` (gt/validation.jsonl.gz) and the unpacked photos (`validation/<image_id>.jpg`, from validation.tgz) in one folder.

```text
cargo run --release -p lightcraft-ocr --example hiertext -- \
    --data DIR --models MODELS_DIR --model pp-ocrv6-small --limit 500 --seed 55 \
    [--download --accept-licence]
```

`--shard K/N` splits a run over N processes; add up the printed word counts to combine them. It moves to `cargo xtask ai-bench` once that harness is in main.
