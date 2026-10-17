"""Reference tensors for the Rust SAM 3 port (crates/segment/tests/reference.rs).

Runs the Hugging Face transformers implementation (fp32, CPU) on one photo and writes the
shared pixel input plus intermediate and final outputs of the click (tracker) and text
(detector) paths. Usage (needs torch, torchvision, transformers, pillow, safetensors):
    <PREFIX>_SAM3_DIR=<model dir> (PREFIX = env_prefix in brand.toml) python tools/sam3_reference.py photo.jpg ref.safetensors
The text prompts below suit a photo of the moon in a blue sky; change them for other photos.
"""
import os, sys, time
import numpy as np
import torch
from PIL import Image
from safetensors.torch import save_file
from transformers import Sam3TrackerModel, Sam3Model, CLIPTokenizer

def _brand(key):
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "brand.toml")
    with open(path, encoding="utf-8") as f:
        for line in f:
            k, _, v = line.partition("=")
            if k.strip() == key:
                return v.split("#")[0].strip().strip('"')
    raise KeyError(key)


MODEL = os.environ.get(
    _brand("env_prefix") + "_SAM3_DIR",
    os.path.expanduser("~/Library/Application Support/" + _brand("settings_dir") + "/models/sam3"),
)
img_path, out_path = sys.argv[1], sys.argv[2]
torch.set_grad_enabled(False)

img = Image.open(img_path).convert("RGB")
W, H = img.size
# Same preprocessing as the Rust side: bilinear resize to 1008², /255, (x-0.5)/0.5.
r = img.resize((1008, 1008), Image.BILINEAR)
px = torch.from_numpy(np.asarray(r, dtype=np.float32) / 255.0).permute(2, 0, 1)[None]
px = (px - 0.5) / 0.5
out = {"pixel_values": px.contiguous()}

t = time.time()
trk = Sam3TrackerModel.from_pretrained(MODEL).eval()
vis = trk.vision_encoder(px)
out["backbone"] = vis.last_hidden_state.contiguous()
for i, f in enumerate(vis.fpn_hidden_states):
    out[f"trk_fpn{i}"] = f.contiguous()
# points in 1008 space: one positive, one negative
pts = torch.tensor([[[[0.5 * 1008, 0.55 * 1008], [0.2 * 1008, 0.2 * 1008]]]], dtype=torch.float32)
lbl = torch.tensor([[[1, 0]]], dtype=torch.int32)
o = trk(pixel_values=px, input_points=pts, input_labels=lbl, multimask_output=False)
out["trk_points"] = pts.contiguous()
out["trk_masks"] = o.pred_masks.contiguous()
out["trk_iou"] = o.iou_scores.contiguous()
o3 = trk(pixel_values=px, input_points=pts[:, :, :1], input_labels=lbl[:, :, :1], multimask_output=True)
out["trk_multi_masks"] = o3.pred_masks.contiguous()
out["trk_multi_iou"] = o3.iou_scores.contiguous()
print("tracker", time.time() - t, o.pred_masks.shape, o.iou_scores, file=sys.stderr)
del trk

t = time.time()
det = Sam3Model.from_pretrained(MODEL).eval()
tok = CLIPTokenizer.from_pretrained(MODEL)
for i, text in enumerate(["moon", "sky", "bird"]):
    enc = tok(text, padding="max_length", max_length=32, return_tensors="pt")
    out[f"det{i}_ids"] = enc.input_ids.to(torch.int64).contiguous()
    out[f"det{i}_mask"] = enc.attention_mask.to(torch.int64).contiguous()
    tf = det.get_text_features(input_ids=enc.input_ids, attention_mask=enc.attention_mask)
    out[f"det{i}_text"] = tf.pooler_output.contiguous()
    o = det(pixel_values=px, input_ids=enc.input_ids, attention_mask=enc.attention_mask)
    out[f"det{i}_logits"] = o.pred_logits.contiguous()
    out[f"det{i}_presence"] = o.presence_logits.contiguous()
    out[f"det{i}_boxes"] = o.pred_boxes.contiguous()
    out[f"det{i}_masks"] = o.pred_masks.contiguous()
    sc = (o.pred_logits.sigmoid() * o.presence_logits.sigmoid())[0]
    print(text, "presence", o.presence_logits.sigmoid().item(), "top", sc.topk(3).values.tolist(), file=sys.stderr)
for i, f in enumerate(det.vision_encoder(px).fpn_hidden_states):
    out[f"det_fpn{i}"] = f.contiguous()
print("detector", time.time() - t, file=sys.stderr)
# tokenizer cases for the Rust BPE
cases = ["person", "Red  Car", "a dog's toy", "sky, clouds!", "café 2 people", "tree-line"]
for i, c in enumerate(cases):
    out[f"tok{i}"] = tok(c, padding="max_length", max_length=32, return_tensors="pt").input_ids.to(torch.int64).contiguous()
save_file(out, out_path, metadata={"image": img_path, "W": str(W), "H": str(H), "cases": "|".join(cases)})
print("saved", out_path, file=sys.stderr)
