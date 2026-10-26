# lightcraft-vision

Content models for LightCraft's content search and auto-tags: image and text embeddings in one
space, in pure Rust on [candle](https://github.com/huggingface/candle) (`candle-core` /
`candle-nn` 0.9.2; no `candle-transformers`, no `tokenizers` crate, no C code). L1, no UI.

- `ContentModel` (`embed_images`, `embed_texts`, `score`, `manifest`): the model-agnostic API.
  `lightcraft_vision::load(dir, &manifest, towers)` loads a model from its folder; `Towers` picks
  the image tower (indexing), the text tower (search) or both.
- `manifest`: what a model is (id, family, licence, files pinned by size + SHA-256, input
  preprocessing, tokenizer and context, embedding size, score kind, default tag threshold),
  validated with bounds. Built in: `siglip2-b16-256` (default), `siglip2-b16-384`,
  `siglip2-so400m-16-384`; `manifest::hugging_face(id)` is each one's pinned Hugging Face revision.
- `siglip`: the SigLIP / SigLIP 2 (fixed resolution) towers — ViT with attention-pooling head,
  text transformer with last-token head; sigmoid scoring with the checkpoint's scale and bias.
- `tokenizer::SentencePiece`: Gemma/SentencePiece BPE from `tokenizer.json` (added tokens, `▁`
  normalisation, byte fallback, fused `<unk>`, `<eos>` + padding to the 64-token context).
- `preprocess`: Pillow-exact resampling (bilinear/bicubic with antialiasing, 8-bit fixed point),
  squash or centre crop, `(x/255 − mean)/std`.
- `config`: the model's `config.json`, bounded before anything is allocated from it.

Native only; the wasm build compiles a stub whose `load` returns `Error::Unsupported`.

## Model files

`google/siglip2-base-patch16-256` from Hugging Face: `model.safetensors` (1,500,985,224 bytes,
f32, both towers), `tokenizer.json`, `config.json`, `preprocessor_config.json` (sizes and hashes
in `manifest::builtin_siglip2_b16_256`). The weights are **Apache-2.0, by Google**; they are not
part of LightCraft and are never bundled. The text tower reads token embeddings row by row from
the file, so the 256k × 768 table (≈790 MB) is not held in memory.

## Tests

- Unit tests need no model: hostile `config.json`, truncated safetensors, missing tensors, wrong
  shapes, empty/huge text, empty/huge images, a tiny synthetic model end to end.
- Golden tests against Hugging Face `transformers` (`tests/reference.rs`):
  `LIGHTCRAFT_VISION_DIR=<model dir> cargo test -p lightcraft-vision --release --test reference -- --nocapture`
  (skipped without it). Token ids must match exactly and image–text sigmoid scores within 0.01;
  with `LIGHTCRAFT_VISION_REF=<full reference json>` every embedding must have cosine ≥ 0.999.
  The fixture and full reference come from `tools/siglip_reference.py` (run by hand; see its header)
  on images written by `cargo run -p lightcraft-vision --example siglip_inputs -- <dir>`.
- Speed: `cargo run --release -p lightcraft-vision --example vision_bench -- <model dir> [image|text|both]`.

## Licence

Apache-2.0 only (the towers are ported from Hugging Face Transformers' Apache-2.0 SigLIP code;
see `NOTICE`).
