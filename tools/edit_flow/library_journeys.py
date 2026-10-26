#!/usr/bin/env python3
"""Library x import x edit x export journeys on real on-disk libraries (headless, iPad touch layout).
Usage: library_journeys.py FILE1 FILE2 FILE3 [--cli PATH]   (three real photos: any RAW/JPEG)
Each journey is a list of control requests; named checks read the replies. Exit 1 if any check fails."""
import json, subprocess, sys, os, shutil, argparse
ap = argparse.ArgumentParser(); ap.add_argument("files", nargs=3); ap.add_argument("--cli", default="target/debug/lightcraft-cli")
a = ap.parse_args()
W = "/tmp/lc_journeys"; shutil.rmtree(W, ignore_errors=True); os.makedirs(W)
LIB0, LIBA, LIBB, EXP = f"{W}/start", f"{W}/Wedding", f"{W}/Travel", f"{W}/exports"
os.makedirs(LIB0)
F1, F2, F3 = a.files
steps, rid = [], 0
def S(name, method, params=None):
    global rid; rid += 1
    steps.append((rid, name, method, params or {})); return rid
def X(name, command, params=None): return S(name, "engine.execute", {"command": command, "params": params or {}})

X("new library A", "app.newLibrary", {"path": W, "name": "Wedding"})
imp = X("import 3 into A", "library.import", {"paths": [F1, F2, F3]})
X("select 1", "library.select", {"ids": [1]})
X("auto edit", "develop.auto")
X("set exposure 1.25", "develop.set", {"values": {"light.exposure": 1.25}})
X("sky mask", "mask.add", {"kind": "sky"}); X("mask adjust", "mask.adjust", {"values": {"exposure": -0.5}})
X("rate 4", "photo.rate", {"rating": 4}); X("pick flag", "photo.flag", {"flag": "pick"})
X("export A", "app.export", {"dir": EXP, "format": "jpeg", "quality": 85, "longEdge": 1600})
ctrlA = X("read controls A", "develop.controls")
cntA = S("inspect A", "ui.inspect")
X("new library B (while A open)", "app.newLibrary", {"path": W, "name": "Travel"})
X("import 1 into B", "library.import", {"paths": [F2]})
X("select B1", "library.select", {"ids": [1]})
X("edit in B", "develop.set", {"values": {"light.contrast": 33}})
ctrlB = X("read controls B", "develop.controls")
X("reopen A", "app.openLibrary", {"path": LIBA})
X("select A1", "library.select", {"ids": [1]})
ctrlA2 = X("read controls A after reopen", "develop.controls")
insA2 = S("inspect A after reopen", "ui.inspect")
X("export after reopen", "app.export", {"dir": EXP + "2", "format": "jpeg", "quality": 80, "longEdge": 1200})
bad_exists = X("EXPECT FAIL: new library on existing library", "app.newLibrary", {"path": W, "name": "Wedding"})
X("still usable after the refused New Library", "library.select", {"ids": [1]})
ctrlA3 = X("read controls after the refused command", "develop.controls")
X("open a missing folder = creates an empty library (by design)", "app.openLibrary", {"path": f"{W}/Fresh"})
emptyIns = S("inspect empty library", "ui.inspect")
X("reopen A from the empty one", "app.openLibrary", {"path": LIBA})
X("select A1 once more", "library.select", {"ids": [1]})
ctrlA4 = X("read controls after round trip", "develop.controls")
X("reopen B", "app.openLibrary", {"path": LIBB})
X("select B1 again", "library.select", {"ids": [1]})
ctrlB2 = X("read controls B after reopen", "develop.controls")

script = f"{W}/j.jsonl"
open(script, "w").write("\n".join(json.dumps({"id": i, "method": m, "params": p}) for i, _, m, p in steps) + "\n")
p = subprocess.run([a.cli, "snapshot", "--touch", "--library", LIB0, "--script", script, "-o", f"{W}/f.png", "--size", "1180x820"], capture_output=True, text=True)
R = {}
for l in p.stdout.splitlines():
    if l.startswith("{"):
        try: x = json.loads(l); R[x["id"]] = x
        except Exception: pass
def val(rid_, ctl):
    for c in (R.get(rid_, {}).get("result") or []):
        if isinstance(c, dict) and c.get("id") == ctl: return c.get("value")
fails = []
def check(name, cond, info=""):
    print(("PASS " if cond else "FAIL ") + name + (f"  [{info}]" if info and not cond else ""))
    if not cond: fails.append(name)
for i, name, m, pr in steps:
    r = R.get(i); expect_fail = name.startswith("EXPECT FAIL")
    ok = bool(r and r.get("ok"))
    check(("error shown, nothing broken: " if expect_fail else "step ok: ") + name, (not ok) if expect_fail else ok, str((r or {}).get("error"))[:120] if r else "no reply (crash?)")
check("A: exposure kept (1.25) in library A", abs((val(ctrlA, "light.exposure") or 0) - 1.25) < 1e-6, val(ctrlA, "light.exposure"))
check("A: exposure survived closing and reopening", abs((val(ctrlA2, "light.exposure") or 0) - 1.25) < 1e-6, val(ctrlA2, "light.exposure"))
check("A: still intact after a refused command", abs((val(ctrlA3, "light.exposure") or 0) - 1.25) < 1e-6, val(ctrlA3, "light.exposure"))
check("A: intact after visiting an empty library and back", abs((val(ctrlA4, "light.exposure") or 0) - 1.25) < 1e-6, val(ctrlA4, "light.exposure"))
check("B: contrast edit survived reopen", abs((val(ctrlB2, "light.contrast") or 0) - 33) < 1e-6, val(ctrlB2, "light.contrast"))
check("B: library B did not inherit A's exposure", abs(val(ctrlB, "light.exposure") or 0) < 1e-6, val(ctrlB, "light.exposure"))
n1 = len([f for f in os.listdir(EXP)]) if os.path.isdir(EXP) else 0
n2 = len([f for f in os.listdir(EXP + "2")]) if os.path.isdir(EXP + "2") else 0
check("exports written (before and after reopen)", n1 >= 1 and n2 >= 1, f"{n1},{n2}")
print(f"\n{len(steps)} steps, {len(fails)} failed checks; cli exit {p.returncode}")
sys.exit(1 if fails else 0)
