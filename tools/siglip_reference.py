"""Reference outputs for the Rust SigLIP 2 port (crates/vision/tests/reference.rs).

Runs the Hugging Face transformers implementation (fp32, CPU) on the golden test's procedural
images and texts, and writes:
  - a small fixture (token ids of the tokenizer cases and the embedded texts, image-text sigmoid
    scores, logit scale/bias), committed as crates/vision/tests/fixtures/siglip2_reference.json;
  - the full reference (the above plus every embedding), kept out of the repo; the Rust test
    reads it through LIGHTCRAFT_VISION_REF.

Run by hand (not in CI). Set up once, in a scratch directory (not the repo):
    python3 -m venv <scratch>/venv
    <scratch>/venv/bin/pip install torch --index-url https://download.pytorch.org/whl/cpu
    <scratch>/venv/bin/pip install transformers pillow sentencepiece
Then:
    cargo run -p lightcraft-vision --example siglip_inputs -- <scratch>/inputs
    <scratch>/venv/bin/python -I tools/siglip_reference.py <model dir> <scratch>/inputs \
        crates/vision/tests/fixtures/siglip2_reference.json <scratch>/siglip2_full.json
<model dir> holds the google/siglip2-base-patch16-256 files (model.safetensors, config.json,
tokenizer.json, preprocessor_config.json, tokenizer_config.json, special_tokens_map.json).
Texts are embedded lower-cased, as SigLIP 2 expects (the Rust side lower-cases per manifest).
"""
import json
import os
import sys

import torch
from PIL import Image
from transformers import AutoModel, AutoProcessor

CONTEXT = 64
IMAGES = ["beach", "forest", "flower", "red_square", "gradient"]

model_dir, inputs, fixture_path, full_path = sys.argv[1:5]
torch.set_grad_enabled(False)
processor = AutoProcessor.from_pretrained(model_dir)
model = AutoModel.from_pretrained(model_dir, torch_dtype=torch.float32).eval()
tok = processor.tokenizer

texts = json.load(open(os.path.join(inputs, "texts.json"), encoding="utf-8"))
cases = json.load(open(os.path.join(inputs, "token_cases.json"), encoding="utf-8"))


def ids(text):
    return tok(text, padding="max_length", max_length=CONTEXT, truncation=True)["input_ids"]


def pooled(out):
    # transformers 4.x returns a tensor, 5.x an output object with pooler_output
    return out if torch.is_tensor(out) else out.pooler_output


images = [Image.open(os.path.join(inputs, f"{n}.png")).convert("RGB") for n in IMAGES]
lower = [t.lower() for t in texts]
batch = processor(text=lower, images=images, padding="max_length", max_length=CONTEXT, return_tensors="pt")
assert set(batch.keys()) == {"input_ids", "pixel_values"}, batch.keys()  # no attention mask
img = pooled(model.get_image_features(pixel_values=batch["pixel_values"]))
txt = pooled(model.get_text_features(input_ids=batch["input_ids"]))
img = img / img.norm(dim=-1, keepdim=True)
txt = txt / txt.norm(dim=-1, keepdim=True)
scale, bias = model.logit_scale.exp().item(), model.logit_bias.item()
probs = torch.sigmoid(img @ txt.T * scale + bias)
# cross-check against the model's own forward
check = torch.sigmoid(model(**batch).logits_per_image)
assert torch.allclose(probs, check, atol=1e-5), (probs, check)

fixture = {
    "model": "google/siglip2-base-patch16-256",
    "context": CONTEXT,
    "logitScaleExp": scale,
    "logitBias": bias,
    "images": IMAGES,
    "texts": texts,
    "textIds": batch["input_ids"].tolist(),
    "tokenCases": [{"text": c, "ids": ids(c)} for c in cases],
    "probs": probs.tolist(),
}
with open(fixture_path, "w", encoding="utf-8") as f:
    json.dump(fixture, f, ensure_ascii=False, separators=(",", ":"))
full = dict(fixture, imageEmbeds=img.tolist(), textEmbeds=txt.tolist(), pixelValues0=batch["pixel_values"][0, :, :4, :4].tolist())
with open(full_path, "w", encoding="utf-8") as f:
    json.dump(full, f, ensure_ascii=False)
print(f"scale {scale:.4f} bias {bias:.4f}", file=sys.stderr)
for i, n in enumerate(IMAGES):
    best = max(range(len(texts)), key=lambda j: probs[i, j])
    print(f"{n:>11}: best '{texts[best]}' {probs[i, best]:.4f}", file=sys.stderr)
