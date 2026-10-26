#!/usr/bin/env python3
"""Every user flow we can think of, run through the real UI in the iPad layout, one screenshot per step.
Usage: flows.py FILE1 FILE2 FILE3 FILE4 [--out DIR] [--size WxH] [--only flow,flow]
Output: DIR/<flow>/<NN>_<step>.png and DIR/summary.json (which steps replied ok)."""
import json, subprocess, os, shutil, argparse, sys
ap = argparse.ArgumentParser(); ap.add_argument("files", nargs=4); ap.add_argument("--out", default="/tmp/lc_review/L5")
ap.add_argument("--cli", default="target/debug/lightcraft-cli"); ap.add_argument("--size", default="1180x820"); ap.add_argument("--only", default="")
a = ap.parse_args()
F = a.files
FLOWS = {}
def flow(fn): FLOWS[fn.__name__] = fn; return fn

class B:  # builds one script
    def __init__(s, name): s.name, s.lines, s.n, s.k, s.meta = name, [], 0, 0, {}
    def _r(s, m, p, tag=None):
        s.n += 1; s.lines.append(json.dumps({"id": s.n, "method": m, "params": p})); s.meta[s.n] = tag or m; return s.n
    def ui(s, **p): s._r("ui.set", p, "ui.set " + ",".join(f"{k}={v}" for k, v in p.items()))
    def cmd(s, c, **p): s._r("engine.execute", {"command": c, "params": p}, c)
    def click(s, w): s._r("ui.clickWidget", {"id": w}, "click " + w)
    def shot(s, name):
        s._r("ui.settle", {"timeoutMs": 15000}, "settle"); s.k += 1
        s._r("ui.screenshot", {"path": f"{a.out}/{s.name}/{s.k:02d}_{name}.png", "headless": True}, "SHOT " + name)

@flow
def first_launch_and_import(b):
    b.ui(view="photoGrid", right="none"); b.shot("empty_library")
    b.cmd("library.import", paths=F); b.shot("after_import_grid")
    b.cmd("library.select", ids=[1]); b.ui(view="detail", right="none"); b.shot("first_photo_open")
    b.ui(view="squareGrid", right="none"); b.shot("square_grid")
@flow
def cull_rate_pick_reject_filter(b):
    b.cmd("library.import", paths=F); b.ui(view="photoGrid", right="none")
    for i, (r, f) in enumerate([(5, "pick"), (4, "pick"), (2, "reject"), (3, "none")], 1):
        b.cmd("library.select", ids=[i]); b.cmd("photo.rate", rating=r)
        if f != "none": b.cmd("photo.flag", flag=f)
    b.ui(view="photoGrid", right="none"); b.shot("rated_grid")
    b.click("icon:filter"); b.shot("filter_bar_open")
    b.cmd("view.multiSelect"); b.cmd("library.selectAll"); b.shot("select_mode_all")
    b.cmd("dialog.export"); b.shot("bulk_export_dialog")
@flow
def edit_basic_and_undo(b):
    b.cmd("library.import", paths=F); b.cmd("library.select", ids=[1]); b.ui(view="detail", right="edit"); b.shot("detail_edit")
    b.click("button:auto"); b.shot("after_auto")
    b.cmd("develop.set", values={"light.exposure": 0.8, "light.contrast": 30, "color.vibrance": 25}); b.shot("manual")
    b.click("icon:undo"); b.shot("one_undo"); b.click("icon:redo"); b.shot("one_redo")
    b.click("button:reset"); b.shot("reset")
@flow
def masking_flows(b):
    b.cmd("library.import", paths=F); b.cmd("library.select", ids=[1]); b.ui(view="detail", right="masking"); b.shot("masking_panel")
    b.click("maskNew:sky"); b.cmd("mask.adjust", values={"exposure": -0.6}); b.shot("sky_mask")
    b.click("maskNew:radial"); b.cmd("mask.adjust", values={"exposure": 0.5}); b.shot("radial_mask")
    b.click("maskNew:linear"); b.cmd("mask.adjust", values={"exposure": -0.4}); b.shot("linear_mask")
    b.click("maskNew:luminanceRange"); b.shot("luminance_mask")
    b.cmd("mask.addComponent", kind="radial", op="subtract", center=[0.5, 0.5], rx=0.15, ry=0.15); b.shot("subtract_component")
@flow
def crop_and_rotate(b):
    b.cmd("library.import", paths=F); b.cmd("library.select", ids=[1]); b.ui(view="detail", right="crop"); b.shot("crop_tool")
    b.click("dropdown:cropAspect"); b.shot("aspect_menu")
    b.click("button:cropRotateRight"); b.shot("rotate_right"); b.click("button:cropFlipH"); b.shot("flip_h"); b.click("button:cropReset"); b.shot("reset")
@flow
def library_switch_and_offline(b):
    b.cmd("library.import", paths=F); b.cmd("library.select", ids=[1]); b.ui(view="detail", right="edit"); b.shot("library_a_photo")
    b.cmd("app.newLibrary", name="Second"); b.ui(view="photoGrid", right="none"); b.shot("new_empty_library")
    b.cmd("app.settings"); b.shot("settings_general")
@flow
def compare_and_survey(b):
    b.cmd("library.import", paths=F); b.cmd("library.select", ids=[1, 2, 3]); b.ui(view="compare", right="none"); b.shot("compare")
    b.ui(view="survey", right="none"); b.shot("survey"); b.ui(view="reference", right="none"); b.shot("reference")
@flow
def search_and_filter(b):
    b.cmd("library.import", paths=F); b.ui(view="photoGrid", right="none", search="DSC"); b.shot("search_DSC")
    b.ui(search="zzzz"); b.shot("search_no_match"); b.ui(search=""); b.click("icon:filter"); b.shot("filter_open")
@flow
def settings_tabs(b):
    b.cmd("app.settings"); b.shot("settings_general")
    for tab in ("import", "performance", "display", "interface", "faces", "denoise"):
        b.click(f"button:settingsTab-{tab}"); b.shot("tab_" + tab)
@flow
def info_keywords_versions(b):
    b.cmd("library.import", paths=F); b.cmd("library.select", ids=[1]); b.ui(view="detail", right="info"); b.shot("info")
    b.ui(right="keywords"); b.shot("keywords"); b.ui(right="versions"); b.shot("versions"); b.ui(right="activity"); b.shot("activity")

only = [x for x in a.only.split(",") if x]
os.makedirs(a.out, exist_ok=True); summary = {}
for name, fn in FLOWS.items():
    if only and name not in only: continue
    shutil.rmtree(f"{a.out}/{name}", ignore_errors=True); os.makedirs(f"{a.out}/{name}"); b = B(name); fn(b)
    lib = f"{a.out}/{name}/_lib"; script = f"{a.out}/{name}/_flow.jsonl"
    open(script, "w").write("\n".join(b.lines) + "\n")
    p = subprocess.run([a.cli, "snapshot", "--touch", "--library", lib, "--script", script, "-o", f"{a.out}/{name}/_final.png", "--size", a.size], capture_output=True, text=True)
    R = {}
    for l in p.stdout.splitlines():
        if l.startswith("{"):
            try: x = json.loads(l); R[x["id"]] = x
            except Exception: pass
    bad = [(b.meta[i], str(R.get(i, {}).get("error"))[:80]) for i in b.meta if b.meta[i] != "settle" and not (R.get(i) or {}).get("ok")]
    shots = len([m for m in b.meta.values() if m.startswith("SHOT")])
    summary[name] = {"shots": shots, "failed_steps": bad}
    print(f"{name:34s} {shots:2d} screenshots  failed steps: {len(bad)}", ("  " + str(bad[:3])) if bad else "")
json.dump(summary, open(f"{a.out}/summary.json", "w"), indent=1)
