#!/usr/bin/env python3
"""Drive a running LightCraft over its control channel and time the cull-before-import flow.

usage: tools/cull-bench.py PORT FOLDER [--steps N] [--hold-ms 33] [--label NAME] [--size WxH] [--skip-browse]

Start the app with its own library and settings first, e.g.
  LIGHTCRAFT_CONFIG_DIR=/tmp/lc-cfg lightcraft --control 18520 --library /tmp/lc-lib --no-demo
It browses FOLDER into the Local view (with XMP auto-write off, the default, nothing is written
next to the photos), opens the first photo, steps, holds the arrow key, zooms to 100 % and rates,
and writes LABEL.json. The headless counterpart is `lightcraft-cli bench-cull FOLDER`.

Browse times are wall clock from this client (polling; each control call costs ~85 ms on this
machine, so they are good to ~0.1 s). Loupe times come from the app itself (`perf.loupeSteps`,
measured on the frames it draws), so they exclude the control channel's round trip.
"""
import json, socket, statistics, sys, threading, time

PORT = int(sys.argv[1])
FOLDER = sys.argv[2]
args = sys.argv[3:]
def opt(name, default):
    return type(default)(args[args.index(name) + 1]) if name in args else default
STEPS = opt("--steps", 40)
HOLD_MS = opt("--hold-ms", 33)
LABEL = opt("--label", "run")

class Conn:
    def __init__(self):
        self.s = socket.create_connection(("127.0.0.1", PORT), timeout=600)
        self.s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        self.f = self.s.makefile("rb")
    def call(self, method, params=None):
        self.s.sendall((json.dumps({"id": 1, "method": method, "params": params or {}}) + "\n").encode())
        r = json.loads(self.f.readline())
        if not r.get("ok", True):
            raise RuntimeError(f"{method}: {r.get('error')}")
        return r.get("result", r)

C = Conn()
def ex(command, params=None):
    return C.call("engine.execute", {"command": command, "params": params or {}})
def inspect():
    return C.call("ui.inspect")
def loupe(st):
    return st.get("loupe") or {}

def pct(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, max(0, round(p / 100 * (len(xs) - 1))))]
def stats(xs):
    xs = [x for x in xs if x is not None]
    if not xs:
        return None
    return {"n": len(xs), "p50": round(statistics.median(xs), 1), "p95": round(pct(xs, 95), 1), "max": round(max(xs), 1)}

def wait(pred, timeout=120, every=0.0):
    t0 = time.perf_counter()
    while True:
        st = inspect()
        if pred(st):
            return time.perf_counter() - t0, st
        if time.perf_counter() - t0 > timeout:
            return None, st
        if every:
            time.sleep(every)

def steps_total(s=None):
    """A cursor for the steps finished so far: `total` keeps counting after the app drops its oldest
    steps (it keeps the latest 256); the length of the list is the fallback for builds without it."""
    s = s or inspect()["perf"]["loupeSteps"]
    return s.get("total", len(s["steps"]))

def steps_since(n0):
    s = inspect()["perf"]["loupeSteps"]
    new = steps_total(s) - n0
    return (s["steps"][-new:] if new > 0 else []), s

out = {"label": LABEL, "folder": FOLDER, "holdMs": HOLD_MS}
SIZE = opt("--size", "")
if SIZE:
    w, h = (int(v) for v in SIZE.split("x"))
    C.call("ui.resize", {"width": w, "height": h})
    time.sleep(1.0)
out["window"] = C.call("ui.inspect").get("window")
rtt = []
for _ in range(10):
    t = time.perf_counter(); inspect(); rtt.append((time.perf_counter() - t) * 1000)
out["control_rtt_ms"] = stats(rtt)

# --- browse -------------------------------------------------------------------------------------
if "--skip-browse" not in args:
    # thumbnails already in the cache aren't this browse's: count only the ones it adds. thumbsMade
    # counts photos given a thumbnail (eviction doesn't lower it); thumbTextures is the fallback for
    # builds without it
    def thumbs_made(st):
        p = st.get("perf") or {}
        return p.get("thumbsMade", p.get("thumbTextures", 0))
    pre_thumbs = thumbs_made(inspect())
    t0 = time.perf_counter()
    ex("library.browse", {"path": FOLDER})
    first = thumbs12 = thumb1 = allp = None
    thumb_times = {}  # thumbnails drawn → when that many first were
    shown = 0
    while time.perf_counter() - t0 < 1800:
        st = inspect()
        el = time.perf_counter() - t0
        shown = ex("library.state").get("visibleCount", 0)
        tt = max(0, thumbs_made(st) - pre_thumbs)
        for k in range(1, min(12, tt) + 1):
            thumb_times.setdefault(k, el)
        if shown and first is None:
            first = el
        if tt >= 1 and thumb1 is None:
            thumb1 = el
        if first is not None and st.get("import") is None and st.get("scan") is None and allp is None:
            allp = el
        # twelve thumbnails, or every photo's in a smaller folder: known once the folder has loaded
        if allp is not None and thumbs12 is None:
            thumbs12 = thumb_times.get(min(12, shown))
        if allp is not None and (thumbs12 is not None or not shown):
            break
        # a layout that never holds twelve thumbnails: give up on that number a while after loading
        if allp is not None and el - allp > 30:
            break
    out["browse_first_photo_s"] = first
    out["browse_first_thumb_s"] = thumb1
    out["browse_12_thumbs_s"] = thumbs12
    out["browse_all_s"] = allp
    out["photos"] = shown

# --- loupe on the first photo ----------------------------------------------------------------------
ex("library.selectAll")
ids = inspect().get("selection") or []
ex("library.select", {"ids": [ids[0]]})
n0 = steps_total()
ex("view.detail")
wait(lambda s: loupe(s).get("source") in ("render", "cached") and not loupe(s).get("pending"), 120)
first_cur = inspect()["perf"]["loupeSteps"]["current"]
out["loupe_first"] = first_cur

# --- deliberate stepping: a step, then rest until it is sharp ---------------------------------------
n0 = steps_total()
for i in range(10):
    ex("library.next")
    wait(lambda s: loupe(s).get("source") in ("render", "cached") and not loupe(s).get("pending"), 60)
    time.sleep(0.4)
ex("library.next")
time.sleep(0.3)
done, _ = steps_since(n0)
done = done[:10]
out["step_rest"] = {
    "any": stats([d["anyMs"] for d in done]),
    "full": stats([d["fullMs"] for d in done]),
    "sharp": stats([d["sharpMs"] for d in done]),
    "fullSources": sorted({d["fullSource"] for d in done}),
}

# --- held arrow: a key every HOLD_MS ---------------------------------------------------------------
wait(lambda s: loupe(s).get("source") in ("render", "cached") and not loupe(s).get("pending"), 60)
n0 = steps_total()
conns = [Conn() for _ in range(4)]
t0 = time.perf_counter() + 0.05
def sender(k):
    for i in range(k, STEPS, len(conns)):
        delay = t0 + i * HOLD_MS / 1000 - time.perf_counter()
        if delay > 0:
            time.sleep(delay)
        conns[k].call("ui.key", {"key": "ArrowRight"})
threads = [threading.Thread(target=sender, args=(k,)) for k in range(len(conns))]
[t.start() for t in threads]
[t.join() for t in threads]
held_for = time.perf_counter() - t0
rel_t, _ = wait(lambda s: loupe(s).get("source") in ("render", "cached") and not loupe(s).get("pending"), 60)
time.sleep(0.2)
ex("library.next")
time.sleep(0.2)
done, _ = steps_since(n0)
held = done[:-1]  # the last one is the landing photo (rested on)
landing = done[-1] if done else None
out["hold"] = {
    "steps_sent": STEPS,
    "photos_drawn": len(held),
    "seconds": round(held_for, 2),
    "any": stats([d["anyMs"] for d in held]),
    "shownFull": sum(1 for d in held if d["fullMs"] is not None),
    "full": stats([d["fullMs"] for d in held]),
    "anySources": {k: sum(1 for d in held if d["anySource"] == k) for k in {d["anySource"] for d in held}},
    "dwell": stats([d["dwellMs"] for d in held]),
    "landing": landing,
    "release_to_sharp_s_client": rel_t,
}

# --- zoom to 100 % ------------------------------------------------------------------------------
wait(lambda s: loupe(s).get("source") in ("render", "cached") and not loupe(s).get("pending"), 60)
t0 = time.perf_counter()
ex("view.zoom100")
cam = None
while time.perf_counter() - t0 < 60:
    st = inspect()
    if cam is None and loupe(st).get("source") == "embedded":
        cam = time.perf_counter() - t0
    if (loupe(st).get("region") or {}).get("pending") is False and not loupe(st).get("pending") and loupe(st).get("source") != "embedded":
        break
t = time.perf_counter() - t0
out["zoom100_camera_s_client"] = cam
out["zoom100_s_client"] = t
out["zoom100_region"] = loupe(st).get("region")
ex("view.zoomFit")

# --- rating / flagging latency (control round trip; the app's own time is in persistence) --------
rate = []
for i in range(20):
    t0 = time.perf_counter()
    ex("photo.rate", {"rating": (i % 5) + 1})
    rate.append((time.perf_counter() - t0) * 1000)
out["rate_cmd_ms_client"] = stats(rate)
info = ex("library.info")
out["persistence"] = {k: v for k, v in (info.get("persistence") or {}).items() if k in ("appends", "lastAppendMs", "maxAppendMs")}
p = inspect()["perf"]
out["perf"] = {k: p.get(k) for k in ("fps", "maxUpdateMs", "gpu", "rendersDone", "thumbTextures")}
print(json.dumps(out, indent=1, default=str))
open(LABEL + ".json", "w", encoding="utf-8").write(json.dumps(out, indent=1, default=str))





