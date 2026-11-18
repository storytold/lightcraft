## Fixes

- **Search indexing failures:** indexing pauses itself after 25 photos in a row fail, instead of decoding the whole library at full CPU on every start, and logs why photos fail. A task row's "failed" count shows the first reason on hover, and a GPU that returns non-finite CLIP embeddings hands the photo to the CPU instead of failing it.
