"""Reference outputs for the Rust CLIP ViT-B/32 port (crates/vision/tests/reference_clip.rs).

Runs the Hugging Face transformers implementation (fp32, CPU, the Pillow image processor) on the golden test's
procedural images and texts (the same as tools/siglip_reference.py), and writes:
  - a small fixture (token ids of the tokenizer cases and the embedded texts, image-text cosines, the logit scale),
    committed as crates/vision/tests/fixtures/clip_b32_reference.json;
  - the full reference (the above plus every embedding), kept out of the repo; the Rust test reads it through
    LIGHTCRAFT_CLIP_REF.

Run by hand (not in CI), with the venv of tools/siglip_reference.py:
    cargo run -p lightcraft-vision --example siglip_inputs -- <scratch>/inputs
    <scratch>/venv/bin/python -I tools/clip_reference.py <model dir> <scratch>/inputs \
        crates/vision/tests/fixtures/clip_b32_reference.json <scratch>/clip_b32_full.json
<model dir> holds the openai/clip-vit-base-patch32 files (model.safetensors, config.json, vocab.json, merges.txt,
preprocessor_config.json, tokenizer.json, tokenizer_config.json, special_tokens_map.json).
"""
import json
import os
import sys

import torch
from PIL import Image
from transformers import CLIPModel, CLIPTokenizerFast

try:  # transformers 5: the Pillow processor by name (the default one wants torchvision)
    from transformers import CLIPImageProcessorPil as ImageProcessor
except ImportError:  # transformers 4
    from transformers import CLIPImageProcessor as ImageProcessor

CONTEXT = 77
IMAGES = ["beach", "forest", "flower", "red_square", "gradient"]

model_dir, inputs, fixture_path, full_path = sys.argv[1:5]
torch.set_grad_enabled(False)
tok = CLIPTokenizerFast.from_pretrained(model_dir)
proc = ImageProcessor.from_pretrained(model_dir)
model = CLIPModel.from_pretrained(model_dir, torch_dtype=torch.float32).eval()

texts = json.load(open(os.path.join(inputs, "texts.json"), encoding="utf-8"))
cases = json.load(open(os.path.join(inputs, "clip_token_cases.json"), encoding="utf-8"))


def ids(text):
    return tok(text, padding="max_length", max_length=CONTEXT, truncation=True)["input_ids"]


def pooled(out):
    # transformers 4.x returns a tensor, 5.x an output object with pooler_output
    return out if torch.is_tensor(out) else out.pooler_output


images = [Image.open(os.path.join(inputs, f"{n}.png")).convert("RGB") for n in IMAGES]
pixels = proc(images=images, return_tensors="pt")["pixel_values"]
batch = tok(texts, padding="max_length", max_length=CONTEXT, truncation=True, return_tensors="pt")
img = pooled(model.get_image_features(pixel_values=pixels))
txt = pooled(model.get_text_features(input_ids=batch["input_ids"], attention_mask=batch["attention_mask"]))
img = img / img.norm(dim=-1, keepdim=True)
txt = txt / txt.norm(dim=-1, keepdim=True)
scale = model.logit_scale.exp().item()
cos = img @ txt.T
# cross-check against the model's own forward
check = model(input_ids=batch["input_ids"], attention_mask=batch["attention_mask"], pixel_values=pixels).logits_per_image
assert torch.allclose(cos * scale, check, atol=1e-3), (cos * scale, check)

fixture = {
    "model": "openai/clip-vit-base-patch32",
    "context": CONTEXT,
    "logitScaleExp": scale,
    "images": IMAGES,
    "texts": texts,
    "textIds": batch["input_ids"].tolist(),
    "tokenCases": [{"text": c, "ids": ids(c)} for c in cases],
    "cosines": cos.tolist(),
}
with open(fixture_path, "w", encoding="utf-8") as f:
    json.dump(fixture, f, ensure_ascii=False, separators=(",", ":"))
full = dict(fixture, imageEmbeds=img.tolist(), textEmbeds=txt.tolist(), pixelValues0=pixels[0, :, :4, :4].tolist())
with open(full_path, "w", encoding="utf-8") as f:
    json.dump(full, f, ensure_ascii=False)
print(f"scale {scale:.4f}", file=sys.stderr)
for i, n in enumerate(IMAGES):
    best = max(range(len(texts)), key=lambda j: cos[i, j])
    print(f"{n:>11}: best '{texts[best]}' cosine {cos[i, best]:.4f}", file=sys.stderr)
