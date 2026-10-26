#!/usr/bin/env python3
"""Geometric UI lint over every screen (iPad touch layout, headless): renders each state, reads the registered
widget rectangles (`ui.widgets`) and flags what a screenshot review misses by eye:
  small   interactive widget under 32pt on a side (Apple asks for 44pt; 32 is the floor we accept)
  overlap two interactive widgets whose rectangles cross (neither inside the other)
  offscreen widget outside the window
  clipped  zero or negative size
Writes <out>/<orient>_<state>.png and <out>/lint.txt, <out>/lint.json.
Usage: ui_lint.py [--out DIR] [--cli PATH]"""
import json, subprocess, os, argparse, itertools

ap = argparse.ArgumentParser()
ap.add_argument("--out", default="/tmp/lc_lint")
ap.add_argument("--cli", default="target/debug/lightcraft-cli")
a = ap.parse_args()
os.makedirs(a.out, exist_ok=True)

SIZES = {"landscape": (1180, 820), "portrait": (820, 1180)}
INTERACTIVE = ("button:", "icon:", "menu:", "slider:", "check:", "toggle:", "combo:", "tab:", "chip:", "field:", "star", "module:", "dropdown")
# (name, ui.set params, extra commands run first)
STATES = [
    ("grid", {"view": "photoGrid", "right": "none"}, []),
    ("grid_select", {"view": "photoGrid", "right": "none"}, []),
    ("grid_left", {"view": "photoGrid", "right": "none", "leftPanel": True}, []),
    ("square_grid", {"view": "squareGrid", "right": "none"}, []),
    ("detail_edit", {"view": "detail", "right": "edit"}, []),
    ("detail_quick", {"view": "detail", "right": "quickDevelop"}, []),
    ("detail_crop", {"view": "detail", "right": "crop"}, []),
    ("detail_masking", {"view": "detail", "right": "masking"}, []),
    ("detail_info", {"view": "detail", "right": "info"}, []),
    ("detail_keywords", {"view": "detail", "right": "keywords"}, []),
    ("detail_versions", {"view": "detail", "right": "versions"}, []),
    ("detail_profiles", {"view": "detail", "right": "profiles"}, []),
    ("detail_redeye", {"view": "detail", "right": "redEye"}, []),
    ("detail_remove", {"view": "detail", "right": "remove"}, []),
    ("detail_activity", {"view": "detail", "right": "activity"}, []),
    ("detail_left", {"view": "detail", "right": "edit", "leftPanel": True}, []),
    ("compare", {"view": "compare", "right": "none"}, []),
    ("survey", {"view": "survey", "right": "none"}, []),
]
DIALOGS = ["app.settings", "dialog.export", "app.about", "app.shortcuts", "dialog.rename", "dialog.newAlbum", "dialog.newFolder",
           "dialog.contactSheet", "dialog.cull", "dialog.copySettings", "dialog.captureTime", "dialog.autoStack", "app.systemInfo", "app.whatsNew"]

rid = 0
def L(m, p=None):
    global rid
    rid += 1
    return rid, json.dumps({"id": rid, "method": m, "params": p or {}})

def run(orient, W, H):
    lines, marks = [], []
    def state(name, setp, cmds, dialog=None):
        for m, p in (("ui.dialog.cancel", {}), ("ui.key", {"key": "escape"}), ("ui.set", {"view": "photoGrid", "right": "none", "leftPanel": False})):
            lines.append(L(m, p)[1])
        # one selected, rated photo so the rater and overlays show
        lines.append(L("engine.execute", {"command": "library.select", "params": {"ids": [3]}})[1])
        lines.append(L("ui.set", setp)[1])
        lines.append(L("ui.set", {"multiSelect": name == "grid_select"})[1])
        for c, p in cmds:
            lines.append(L("engine.execute", {"command": c, "params": p})[1])
        if dialog:
            lines.append(L("engine.execute", {"command": dialog, "params": {}})[1])
        lines.append(L("ui.settle", {"timeoutMs": 3000})[1])
        wid, wl = L("ui.widgets")
        lines.append(wl)
        lines.append(L("ui.screenshot", {"path": f"{a.out}/{orient}_{name}.png", "headless": True})[1])
        marks.append((name, wid))
    for n, s, c in STATES:
        state(n, s, c)
    for d in DIALOGS:
        state("dlg_" + d.split(".")[-1], {"view": "photoGrid", "right": "none"}, [], dialog=d)
    sp = f"{a.out}/{orient}.jsonl"
    open(sp, "w").write("\n".join(lines) + "\n")
    p = subprocess.run([a.cli, "snapshot", "--touch", "--demo", "--script", sp, "-o", f"{a.out}/{orient}_end.png", "--size", f"{W}x{H}"], capture_output=True, text=True)
    res = {}
    for l in p.stdout.splitlines():
        if l.startswith("{"):
            try:
                x = json.loads(l)
                res[x["id"]] = x
            except Exception:
                pass
    return marks, res

def inter(r1, r2):
    x0, y0 = max(r1[0], r2[0]), max(r1[1], r2[1])
    x1, y1 = min(r1[0] + r1[2], r2[0] + r2[2]), min(r1[1] + r1[3], r2[1] + r2[3])
    return max(0.0, x1 - x0) * max(0.0, y1 - y0)

def contains(o, i, tol=1.5):
    return o[0] - tol <= i[0] and o[1] - tol <= i[1] and o[0] + o[2] + tol >= i[0] + i[2] and o[1] + o[3] + tol >= i[1] + i[3]

report, issues = [], {}
for orient, (W, H) in SIZES.items():
    marks, res = run(orient, W, H)
    for name, wid in marks:
        r = res.get(wid)
        key = f"{orient}/{name}"
        found = []
        if not r or not r.get("ok"):
            found.append(("error", "ui.widgets failed"))
        else:
            ws = [w for w in r["result"] if w["id"].startswith(INTERACTIVE) and len(w["rect"]) == 4]
            for w in ws:
                x, y, ww, hh = w["rect"]
                if ww <= 0 or hh <= 0:
                    found.append(("clipped", w["id"]))
                    continue
                if min(ww, hh) < 32 and not w["id"].startswith(("slider:", "field:", "menu:")):
                    found.append(("small", f'{w["id"]} {ww:.0f}x{hh:.0f}'))
                if x < -2 or y < -2 or x + ww > W + 2 or y + hh > H + 2:
                    found.append(("offscreen", f'{w["id"]} at {x:.0f},{y:.0f} {ww:.0f}x{hh:.0f}'))
            for p, q in itertools.combinations(ws, 2):
                if p["id"] == q["id"]:
                    continue
                ov = inter(p["rect"], q["rect"])
                small = min(p["rect"][2] * p["rect"][3], q["rect"][2] * q["rect"][3])
                if small > 0 and ov / small > 0.2 and not contains(p["rect"], q["rect"]) and not contains(q["rect"], p["rect"]):
                    found.append(("overlap", f'{p["id"]} x {q["id"]} ({ov / small:.0%})'))
        issues[key] = found
        report.append(f"## {key}: {len(found)} issue(s)")
        report.extend(f"  {k:9} {v}" for k, v in found)
open(f"{a.out}/lint.txt", "w").write("\n".join(report) + "\n")
json.dump(issues, open(f"{a.out}/lint.json", "w"), indent=1)
tot = sum(len(v) for v in issues.values())
print(f"{len(issues)} screens, {tot} issues -> {a.out}/lint.txt")
