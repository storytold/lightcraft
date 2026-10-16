# lightcraft-vision

Content models for LightCraft's content search and auto-tags: image and text embeddings in one
space, in pure Rust on [candle](https://github.com/huggingface/candle) (`candle-core` /
`candle-nn` 0.9.2; no `candle-transformers`, no `tokenizers` crate, no C code). L1, no UI.

- `ContentModel` (`embed_images`, `embed_texts`, `score`, `manifest`): the model-agnostic API.
  `lightcraft_vision::load(dir, &manifest, towers)` loads a model from its folder; `Towers` picks
  the image tower (indexing), the text tower (search) or both.
- `manifest`: what a model is (id, family, licence, files pinned by size + SHA-256, input
  preprocessing, tokenizer and context, embedding size, score kind, default tag threshold),
  validated with bounds; a family must come with its tokenizer and score kind. Built in: `siglip2-b16-256`
  (default), `siglip2-b16-384`, `siglip2-so400m-16-384`, `clip-vit-b32-openai`; `manifest::hugging_face(id)` is each
  one's pinned Hugging Face revision. Each has a `hint` (quality tier, indexing time relative to the default, English
  only) and its measured cut-offs.
- `siglip`: the SigLIP / SigLIP 2 (fixed resolution) towers — ViT with attention-pooling head,
  text transformer with last-token head; sigmoid scoring with the checkpoint's scale and bias.
- `clip`: the CLIP towers — ViT with class token, `pre_layrnorm`, quick-GELU blocks and `visual_projection`; causal
  text transformer pooled at the end-of-text token with `text_projection` (each batch runs only as long as its longest
  text). No learned bias: the score is `sigmoid(exp(logit_scale) · cos + offset)` with the manifest's calibrated offset.
- `nn`: the encoder blocks both families share (pre-norm attention + MLP, optional causal mask).
- `tokenizer::ClipBpe`: CLIP's byte-level BPE from `vocab.json` + `merges.txt` (lower-cased, `</w>` word ends, the
  markers as added tokens, padded/truncated to any context); also SAM 3's tokenizer (`lightcraft-segment`, 32 tokens).
- `tokenizer::SentencePiece`: Gemma/SentencePiece BPE from `tokenizer.json` (added tokens, `▁`
  normalisation, byte fallback, fused `<unk>`, `<eos>` + padding to the 64-token context).
- `preprocess`: Pillow-exact resampling (bilinear/bicubic with antialiasing, 8-bit fixed point),
  squash or centre crop, `(x/255 − mean)/std`.
- `config`: the model's `config.json`, bounded before anything is allocated from it.

Native only; the wasm build compiles a stub whose `load` returns `Error::Unsupported`.

## Model files

`openai/clip-vit-base-patch32` (MIT, OpenAI): `model.safetensors` (605,157,884 bytes, from the repository's
safetensors conversion, revision `c237dc49…`), `vocab.json`, `merges.txt`, `config.json`, `preprocessor_config.json`
(`manifest::builtin_clip_vit_b32_openai`).

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
- CLIP golden tests (`tests/reference_clip.rs`), the same way: `LIGHTCRAFT_CLIP_DIR=<model dir>` (and
  `LIGHTCRAFT_CLIP_REF=<full json>`), fixture and reference from `tools/clip_reference.py`: token ids exact, cosines
  within 2e-3, embeddings cosine ≥ 0.999.
- Speed: `cargo run --release -p lightcraft-vision --example vision_bench -- <model dir> [image|text|both]`.

## Auto-tag vocabulary

`vocab.rs` reads the auto-tag vocabulary (one hierarchical tag per line, synonyms after `=`, an own cut-off after `@`;
bounded: 5,000 tags, 6 levels, 120 characters, 8 phrases); `vocab/en.txt` is the built-in English list of ~1,560 tags,
original work written for LightCraft (not derived from any published label set), with the prompt template it is
embedded in (`TEMPLATES`). `similarities` is the matrix product the engine scores a library's photos against it with.
How the cut-off was chosen: `docs/content-search.md` ▸ Auto-tags.

## Licence

Apache-2.0 only (the towers and the CLIP tokenizer are ported from Hugging Face Transformers' Apache-2.0 SigLIP and
CLIP code; see `NOTICE`). The vocabulary (`vocab.rs`, `vocab/en.txt`) is LightCraft's own work and also available under MIT, like
the rest of the repository.
