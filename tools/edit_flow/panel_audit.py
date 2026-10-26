#!/usr/bin/env python3
"""Invoke every enabled menu command (iPad touch layout, demo library) and audit what opens:
- the command must not error or crash;
- a dialog that opens must fit inside the screen and offer a close button (button:dialogClose or Cancel/OK).
Usage: panel_audit.py [--out DIR] [--cli PATH] [--size WxH]"""
import json, subprocess, os, argparse, collections
ap = argparse.ArgumentParser(); ap.add_argument("--out", default="/tmp/lc_panels"); ap.add_argument("--cli", default="target/debug/lightcraft-cli"); ap.add_argument("--size", default="1180x820")
a = ap.parse_args(); os.makedirs(a.out, exist_ok=True)
SW, SH = [float(x) for x in a.size.split("x")]
SKIP = ("quit", "exit", "delete", "empty", "permanent", "signout", "remove", "clearall", "reset", "restart")
def run(lines, tag, shots=False):
    sp = f"{a.out}/{tag}.jsonl"; open(sp, "w").write("\n".join(lines) + "\n")
    p = subprocess.run([a.cli, "snapshot", "--touch", "--demo", "--script", sp, "-o", f"{a.out}/{tag}.png", "--size", a.size], capture_output=True, text=True)
    R = {}
    for l in p.stdout.splitlines():
        if l.startswith("{"):
            try: x = json.loads(l); R[x["id"]] = x
            except Exception: pass
    return R, p
R, _ = run([json.dumps({"id": 1, "method": "ui.menu.list", "params": {}})], "list")
items = [i for i in R[1]["result"] if i.get("enabled") and not any(s in i["id"].lower() for s in SKIP)]
print(f"menu commands: {len(R[1]['result'])}  enabled & safe to run: {len(items)}")
rid = 0
def L(m, p=None):
    global rid; rid += 1; return rid, json.dumps({"id": rid, "method": m, "params": p or {}})
problems, opened, crashes = [], [], []
for c0 in range(0, len(items), 25):
    chunk = items[c0:c0 + 25]; lines, own = [], {}
    for it in chunk:
        for m, p in (("ui.dialog.cancel", {}), ("ui.key", {"key": "escape"}), ("ui.set", {"view": "detail", "right": "edit"}), ("ui.settle", {"timeoutMs": 2500})):
            lines.append(L(m, p)[1])
        i1, l1 = L("engine.execute", {"command": it["id"], "params": {}}); lines.append(l1)
        i2, l2 = L("ui.settle", {"timeoutMs": 3000}); lines.append(l2)
        i3, l3 = L("ui.widgets", {}); lines.append(l3)
        own[it["id"]] = (i1, i3, it)
    Rr, p = run(lines, "chunk")
    for cid, (i1, i3, it) in own.items():
        r1, r3 = Rr.get(i1), Rr.get(i3)
        if r1 is None or r3 is None:
            crashes.append(cid); continue
        ws = {w["id"]: w["rect"] for w in (r3.get("result") or []) if isinstance(w, dict) and "rect" in w}
        win = ws.get("dialog:window")
        err = None if r1.get("ok") else str(r1.get("error"))[:100]
        if err and not any(k in err.lower() for k in ("no active", "nothing", "select", "not available", "no photo", "needs", "no selection", "requires", "not a")):
            problems.append((cid, "command error", err))
        if win:
            opened.append(cid); x, y, w, h = win
            if x < -0.5 or y < -0.5 or x + w > SW + 0.5 or y + h > SH + 0.5:
                problems.append((cid, "dialog outside the screen", f"rect {win}"))
            has_close = any(k in ws for k in ("button:dialogClose", "button:dialogCancel", "button:dialogOk", "button:dialogDone"))
            if not has_close:
                problems.append((cid, "dialog without a close/cancel/ok button", ",".join(k for k in ws if k.startswith("button:dialog"))))
            cb = ws.get("button:dialogClose")
            if cb and (cb[2] < 32 or cb[3] < 32):
                problems.append((cid, "close button under 32pt", str(cb)))
print(f"dialogs/panels opened: {len(opened)}  crashes: {len(crashes)}  problems: {len(problems)}")
for c in crashes: print("CRASH", c)
by = collections.defaultdict(list)
for cid, kind, info in problems: by[kind].append((cid, info))
for k, v in by.items():
    print(f"\n{k} ({len(v)}):")
    for cid, info in v[:25]: print(f"  {cid}  {info}")
json.dump({"opened": opened, "crashes": crashes, "problems": problems}, open(f"{a.out}/panel_report.json", "w"), indent=1)
