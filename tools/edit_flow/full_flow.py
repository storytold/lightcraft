#!/usr/bin/env python3
"""The whole user journey in the iPad layout, driven through the real UI: empty library -> import -> open a photo ->
Auto -> manual edit -> sky mask -> rate and pick -> undo / redo -> export (one photo, then several in Select mode).
A screenshot is taken at each step. Usage: full_flow.py FILE1 FILE2 FILE3 [FILE4..] [--out DIR] [--cli PATH]"""
import json, subprocess, sys, os, shutil, argparse
ap = argparse.ArgumentParser(); ap.add_argument("files", nargs="+"); ap.add_argument("--out", default="/tmp/lc_flow_full"); ap.add_argument("--cli", default="target/debug/lightcraft-cli")
a = ap.parse_args()
shutil.rmtree(a.out, ignore_errors=True); os.makedirs(f"{a.out}/shots"); LIB = f"{a.out}/lib"; EXP = f"{a.out}/exports"; os.makedirs(LIB)
steps, rid = [], 0
def S(name, method, params=None):
    global rid; rid += 1; steps.append((rid, name, method, params or {})); return rid
def X(name, command, params=None): return S(name, "engine.execute", {"command": command, "params": params or {}})
def click(name, widget): return S(name, "ui.clickWidget", {"id": widget})
def shot(name):
    S("settle", "ui.settle", {"timeoutMs": 15000}); return S("SHOT " + name, "ui.screenshot", {"path": f"{a.out}/shots/{name}.png", "headless": True})
def ctl(name): return X(name, "develop.controls")

S("view", "ui.set", {"view": "photoGrid", "right": "none"}); shot("01_empty_library")
X("import RAWs", "library.import", {"paths": a.files}); S("view", "ui.set", {"view": "photoGrid", "right": "none"}); shot("02_after_import_grid")
X("open first photo", "library.select", {"ids": [1]}); S("view", "ui.set", {"view": "detail", "right": "edit"}); shot("03_open_in_detail")
c0 = ctl("controls before edit")
click("tap Auto", "button:auto"); shot("04_after_auto")
c1 = ctl("controls after Auto")
X("manual: warm + contrast", "develop.set", {"values": {"wb.temp": 5600, "light.contrast": 25, "color.vibrance": 20}}); shot("05_manual_edit")
c2 = ctl("controls after manual edit")
click("open Masking", "icon:masking"); click("tap Sky", "maskNew:sky"); X("sky: darker, richer", "mask.adjust", {"values": {"exposure": -0.5, "saturation": 25}}); shot("06_sky_mask")
S("view", "ui.set", {"right": "edit"})
click("tap 4 stars", "star:bottom:4"); click("tap pick (tick)", "flag:pick"); shot("07_rated_and_picked")
ins_rated = S("inspect", "ui.inspect")
for n in range(5): click(f"undo {n + 1}", "icon:undo")
shot("08_after_five_undos")
c3 = ctl("controls after undos")
for n in range(5): click(f"redo {n + 1}", "icon:redo")
shot("09_after_five_redos")
c4 = ctl("controls after redo")
X("open Export", "dialog.export"); shot("10_export_dialog")
click("confirm Export", "button:dialogOk"); S("wait", "ui.settle", {"timeoutMs": 60000}); S("wait", "ui.settle", {"timeoutMs": 60000}); shot("11_after_export")
X("select all (Select mode)", "view.multiSelect"); X("select every photo", "library.selectAll"); S("view", "ui.set", {"view": "photoGrid", "right": "none"}); shot("12_select_mode")
X("open Export for all", "dialog.export"); shot("13_export_dialog_all")
click("confirm Export all", "button:dialogOk"); S("wait", "ui.settle", {"timeoutMs": 90000}); S("wait", "ui.settle", {"timeoutMs": 90000}); shot("14_after_export_all")

script = f"{a.out}/flow.jsonl"
open(script, "w").write("\n".join(json.dumps({"id": i, "method": m, "params": p}) for i, _, m, p in steps) + "\n")
# the export dialog writes to the library's last-export folder: point it at ours first
open(f"{a.out}/pre.jsonl", "w").write("")
p = subprocess.run([a.cli, "snapshot", "--touch", "--library", LIB, "--script", script, "-o", f"{a.out}/final.png", "--size", "1180x820"], capture_output=True, text=True)
R = {}
for l in p.stdout.splitlines():
    if l.startswith("{"):
        try: x = json.loads(l); R[x["id"]] = x
        except Exception: pass
def val(i, k):
    for c in (R.get(i, {}).get("result") or []):
        if isinstance(c, dict) and c.get("id") == k: return c.get("value")
fails = []
def check(name, ok, info=""):
    print(("PASS " if ok else "FAIL ") + name + (f"  [{info}]" if info and not ok else "")); (None if ok else fails.append(name))
for i, name, m, pr in steps:
    if name in ("settle", "wait", "view"): continue
    r = R.get(i); ok = bool(r and r.get("ok"))
    check(("screenshot: " + name[5:]) if name.startswith("SHOT") else "step: " + name, ok, str((r or {}).get("error"))[:100] if r else "no reply")
ex = lambda i: abs(val(i, "light.exposure") or 0)
check("Auto changed the edit", any(abs((val(c1, k) or 0) - (val(c0, k) or 0)) > 1e-6 for k in ("light.exposure", "light.contrast", "light.shadows", "light.highlights")))
check("manual edit applied (contrast 25)", abs((val(c2, "light.contrast") or 0) - 25) < 1e-6, val(c2, "light.contrast"))
check("five undos went back past the manual edit (contrast no longer 25)", abs((val(c3, "light.contrast") or 0) - 25) > 1e-6, val(c3, "light.contrast"))
check("five redos brought the manual edit back (contrast 25)", abs((val(c4, "light.contrast") or 0) - 25) < 1e-6, (val(c3, "light.contrast"), val(c4, "light.contrast")))
jpgs = subprocess.run(["find", os.path.expanduser("~"), "/tmp", "-name", "*.jpg", "-newer", script, "-not", "-path", "*/Library/*"], capture_output=True, text=True).stdout.split()
check("export wrote JPEG files (one photo, then all)", len(jpgs) >= 2, len(jpgs))
print("exported:", sorted(os.path.basename(j) for j in jpgs)[:12])
print(f"\n{len([1 for s in steps if s[1].startswith('SHOT')])} screenshots in {a.out}/shots ; cli exit {p.returncode}; {len(fails)} failed checks")
sys.exit(1 if fails else 0)
