#!/usr/bin/env python3
"""Make 4 recipes per real photo: auto, tone+colour, masks, export. Usage: gen_raw_recipes.py inputs.txt OUT.jsonl EXPORT_DIR"""
import json, sys, os
files = [l.strip() for l in open(sys.argv[1]) if l.strip()]
out, exp = sys.argv[2], sys.argv[3]
rows = []
for i, f in enumerate(files, 1):
    base = os.path.splitext(os.path.basename(f))[0].replace("-", "_")
    ext = os.path.splitext(f)[1].lstrip(".").lower()
    k = f"{ext}_{base}"
    rows.append({"name": f"{k}_auto", "category": f"{ext}:auto", "photo": i, "steps": [{"command": "develop.auto", "params": {}}]})
    rows.append({"name": f"{k}_tone", "category": f"{ext}:global", "photo": i, "steps": [
        {"command": "develop.set", "params": {"values": {"light.exposure": 0.4, "light.contrast": 18, "light.highlights": -40, "light.shadows": 35,
            "light.whites": 10, "light.blacks": -12, "color.vibrance": 25, "color.saturation": 6, "effects.clarity": 15, "detail.sharpenAmount": 45}}},
        {"command": "develop.set", "params": {"values": {"wb.temp": 6000, "wb.tint": 4}}}]})
    rows.append({"name": f"{k}_masks", "category": f"{ext}:masking", "photo": i, "steps": [
        {"command": "mask.add", "params": {"kind": "sky"}}, {"command": "mask.adjust", "params": {"values": {"exposure": -0.6, "saturation": 15}}},
        {"command": "mask.add", "params": {"kind": "radial", "center": [0.5, 0.5], "rx": 0.35, "ry": 0.3, "feather": 70}},
        {"command": "mask.adjust", "params": {"values": {"exposure": 0.5, "clarity": 20}}},
        {"command": "mask.add", "params": {"kind": "linear", "start": [0.5, 0.0], "end": [0.5, 0.45]}},
        {"command": "mask.adjust", "params": {"values": {"exposure": -0.4}}},
        {"command": "mask.add", "params": {"kind": "luminanceRange", "lo": 0.65, "hi": 1.0}},
        {"command": "mask.adjust", "params": {"values": {"highlights": -50}}}]})
    rows.append({"name": f"{k}_export", "category": f"{ext}:export", "photo": i, "steps": [
        {"command": "develop.auto", "params": {}},
        {"command": "app.export", "params": {"dir": exp, "format": "jpeg", "quality": 85, "longEdge": 2048}}]})
open(out, "w").write("\n".join(json.dumps(r) for r in rows) + "\n")
print(len(rows), "recipes for", len(files), "photos")
