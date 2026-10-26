#!/usr/bin/env python3
"""Click every on-screen button in each UI state (headless, demo library) and report failures.

Pass 1 lists the widgets of every state (ui.widgets); pass 2 clicks each `button:`/`check:`/`combo:`/tab
widget in a fresh state, settles, closes any dialog, and records errors (ok:false) or a crash (the run
stops answering). Output: DIR/buttons.json (the list) and DIR/click_report.json, plus a summary.
Usage: click_all.py [--out DIR] [--cli PATH]
"""
import json, subprocess, os, sys, argparse, collections

ap = argparse.ArgumentParser()
ap.add_argument("--out", default="/tmp/lc_clicks")
ap.add_argument("--cli", default="target/debug/lightcraft-cli")
ap.add_argument("--size", default="1180x820")
a = ap.parse_args()
os.makedirs(a.out, exist_ok=True)

STATES = [{"view": v, "right": r} for v in ("photoGrid", "detail") for r in
          ("none", "edit", "quickDevelop", "crop", "remove", "masking", "redEye", "versions", "activity", "keywords", "info", "profiles")]
STATES += [{"view": v, "right": "none"} for v in ("squareGrid", "compare", "survey", "reference", "people")]
SKIP = ("quit", "deletepermanently", "emptytrash", "signout", "deleteall")  # nothing destructive to the host
KEEP = ("button:", "check:", "combo:", "tab:", "toggle:", "chip:", "icon:")

def run(lines, tag, size):
    sp = os.path.join(a.out, f"{tag}.jsonl")
    open(sp, "w").write("\n".join(lines) + "\n")
    p = subprocess.run([a.cli, "snapshot", "--touch", "--demo", "--script", sp, "-o", os.path.join(a.out, "x.png"), "--size", size],
                       capture_output=True, text=True)
    out = {}
    for l in p.stdout.splitlines():
        try:
            x = json.loads(l)
            out[x["id"]] = x
        except Exception:
            pass
    return out, p

rid = 0
def L(method, params=None, id_holder=None):
    global rid
    rid += 1
    return rid, json.dumps({"id": rid, "method": method, "params": params or {}})

# pass 1
lines, owner = [], {}
for si, st in enumerate(STATES):
    for m, p in (("ui.set", st), ("ui.settle", {"timeoutMs": 4000})):
        i, ln = L(m, p); lines.append(ln)
    i, ln = L("ui.widgets", {}); lines.append(ln); owner[i] = si
res, p = run(lines, "pass1", a.size)
buttons = {}
for i, si in owner.items():
    r = res.get(i)
    ids = [w["id"] for w in (r or {}).get("result", []) if isinstance(w, dict) and w.get("id", "").startswith(KEEP)] if r and r.get("ok") else []
    buttons[si] = sorted(set(i for i in ids if not any(s in i.lower() for s in SKIP)))
json.dump({json.dumps(STATES[si]): v for si, v in buttons.items()}, open(os.path.join(a.out, "buttons.json"), "w"), indent=1)
total = sum(len(v) for v in buttons.values())
print(f"states: {len(STATES)}  buttons (with repeats across states): {total}  unique: {len({b for v in buttons.values() for b in v})}")

# pass 2: chunks of states so a crash only costs one chunk
failures, crashed, clicked = [], [], 0
for si, ids in buttons.items():
    st = STATES[si]
    for chunk_start in range(0, len(ids), 40):
        chunk = ids[chunk_start:chunk_start + 40]
        lines, own = [], {}
        for b in chunk:
            for m, pr in (("ui.dialog.cancel", {}), ("ui.key", {"key": "escape"}), ("ui.set", st), ("ui.settle", {"timeoutMs": 3000})):
                i, ln = L(m, pr); lines.append(ln)
            i, ln = L("ui.clickWidget", {"id": b}); lines.append(ln); own[i] = b
            i, ln = L("ui.settle", {"timeoutMs": 3000}); lines.append(ln)
        res, p = run(lines, "pass2", a.size)
        for i, b in own.items():
            clicked += 1
            r = res.get(i)
            if r is None:
                crashed.append({"state": st, "widget": b, "stderr": p.stderr[-300:]})
            elif not r.get("ok"):
                failures.append({"state": st, "widget": b, "error": str(r.get("error") or r.get("result"))[:200]})
        if p.returncode not in (0, 1) or any(res.get(i) is None for i in own):
            print("crash/short run in state", st, "chunk", chunk_start, "rc", p.returncode)
json.dump({"clicked": clicked, "failures": failures, "crashed": crashed}, open(os.path.join(a.out, "click_report.json"), "w"), indent=1)
print(f"clicked {clicked}  failures {len(failures)}  crashes {len(crashed)}")
by = collections.Counter(f["error"][:80] for f in failures)
for e, n in by.most_common(15):
    print(f"  {n:3d} x {e}")
for c in crashed[:10]:
    print("CRASH", c["state"], c["widget"])
