"""Reference tensors for the Rust BiRefNet port (crates/segment/tests/birefnet_reference.rs).

Runs the MIT reference implementation that ships with the checkpoint (`birefnet.py` and
`BiRefNet_config.py` of the Hugging Face repo `ZhengPeng7/BiRefNet_lite`, fp32, CPU) on one
image and writes the pixel input, the backbone's stage outputs and the final logits.
Usage (needs torch, torchvision, transformers, einops, kornia, timm, safetensors, pillow):
    python tools/birefnet_reference.py <model dir with birefnet.py> [photo.jpg] ref.safetensors
Without a photo, a procedural test image is used. The input side is 1024 (set BIREFNET_SIDE
to change it; a multiple of 32).
"""
import importlib.util
import os
import sys
import types

import numpy as np
import torch
from safetensors.torch import load_file, save_file

torch.set_grad_enabled(False)
model_dir = sys.argv[1]
out_path = sys.argv[-1]
photo = sys.argv[2] if len(sys.argv) > 3 else None
side = int(os.environ.get("BIREFNET_SIDE", "1024"))

# the checkpoint's own code, imported as a package (it uses a relative import of its config)
pkg = types.ModuleType("birefnet_ref")
pkg.__path__ = [model_dir]
sys.modules["birefnet_ref"] = pkg
for name in ("BiRefNet_config", "birefnet"):
    spec = importlib.util.spec_from_file_location(f"birefnet_ref.{name}", os.path.join(model_dir, f"{name}.py"))
    mod = importlib.util.module_from_spec(spec)
    sys.modules[f"birefnet_ref.{name}"] = mod
    spec.loader.exec_module(mod)
BiRefNet = sys.modules["birefnet_ref.birefnet"].BiRefNet
BiRefNetConfig = sys.modules["birefnet_ref.BiRefNet_config"].BiRefNetConfig

model = BiRefNet(config=BiRefNetConfig(bb_pretrained=False))
state = load_file(os.path.join(model_dir, "model.safetensors"))
missing, unexpected = model.load_state_dict(state, strict=False)
assert not missing, missing
model.eval()

if photo:
    from PIL import Image

    img = np.asarray(Image.open(photo).convert("RGB").resize((side, side), Image.BILINEAR), dtype=np.float32) / 255.0
else:
    y, x = np.mgrid[0:side, 0:side].astype(np.float32) / side
    disc = ((x - 0.55) ** 2 + (y - 0.5) ** 2 < 0.07).astype(np.float32)
    img = np.stack([0.2 + 0.6 * disc + 0.1 * x, 0.3 + 0.2 * y, 0.6 - 0.4 * disc + 0.1 * np.sin(20 * x)], -1)
mean = np.array([0.485, 0.456, 0.406], dtype=np.float32)
std = np.array([0.229, 0.224, 0.225], dtype=np.float32)
px = torch.from_numpy(((img - mean) / std).transpose(2, 0, 1)[None].copy())

out = {"pixel_values": px.contiguous()}
for i, f in enumerate(model.bb(px)):
    out[f"bb{i}"] = f.contiguous()
out["logits"] = model(px)[-1].contiguous()
save_file(out, out_path)
print({k: tuple(v.shape) for k, v in out.items()})
