#!/usr/bin/env python3
"""Drive a running LightCraft over its control channel and time the cull-before-import flow.

usage: tools/cull-bench.py PORT FOLDER [--steps N] [--hold-ms 33] [--label NAME] [--size WxH] [--skip-browse] [--overwrite]

Start the app with its own library and settings first, e.g.
  LIGHTCRAFT_CONFIG_DIR=/tmp/lc-cfg lightcraft --control 18520 --library /tmp/lc-lib --no-demo
It browses FOLDER into the Local view (with XMP auto-write off, the default, nothing is written
next to the photos), opens the first photo, steps, holds the arrow key, zooms to 100 % and rates,
and writes LABEL.json (an existing LABEL.json is kept unless --overwrite is given). The headless counterpart is `lightcraft-cli bench-cull FOLDER`.

Browse times are wall clock from this client (polling; each control call costs ~85 ms on this
machine, so they are good to ~0.1 s). Loupe times come from the app itself (`perf.loupeSteps`,
measured on the frames it draws), so they exclude the control channel's round trip.
"""
import json, os, socket, statistics, sys, threading, time

USAGE = __doc__.strip().splitlines()[2]
# (isdecimal, not isdigit: "²" is a digit int() refuses; a decimal string too long for int() too)
try:
    PORT = int(sys.argv[1]) if len(sys.argv) >= 3 and sys.argv[1].isdecimal() else 0
except ValueError:
    PORT = 0
if not 0 < PORT < 65536 or sys.argv[2].startswith("--"):
    sys.exit(USAGE)
FOLDER = sys.argv[2]
args = sys.argv[3:]
# a run takes minutes: a misspelt option, or an option where a value belongs, stops it now
VALUED = {"--steps", "--hold-ms", "--label", "--size"}
FLAGS = {"--skip-browse", "--overwrite"}
_i = 0
while _i < len(args):
    a = args[_i]
    if a in VALUED:
        if _i + 1 >= len(args) or args[_i + 1].startswith("--"):
            sys.exit(f"{a} needs a value; {USAGE}")
        _i += 2
    elif a in FLAGS:
        _i += 1
    else:
        sys.exit(f"unknown option {a!r}; {USAGE}")
def opt(name, default):
    """The value after option `name` (as `default`'s type), `default` when it isn't given; a
    missing or unreadable value stops with the usage line."""
    if name not in args:
        return default
    i = args.index(name) + 1
    if i >= len(args):
        sys.exit(f"{name} needs a value; {USAGE}")
    try:
        return type(default)(args[i])
    except ValueError:
        sys.exit(f"{name} {args[i]!r}: not a {type(default).__name__}; {USAGE}")
STEPS = opt("--steps", 40)
HOLD_MS = opt("--hold-ms", 33)
if STEPS < 1 or HOLD_MS < 1:
    sys.exit(f"--steps and --hold-ms must be 1 or more; {USAGE}")
# the app keeps its last 256 loupe steps; each phase reads only its own, and the hold needs STEPS + 1
# of them with one to spare (a full history is how a phase tells that steps were dropped)
KEPT_STEPS = 256
if STEPS + 1 >= KEPT_STEPS:
    sys.exit(f"--steps {STEPS}: the app keeps {KEPT_STEPS} loupe steps, so at most {KEPT_STEPS - 2}; {USAGE}")
LABEL = opt("--label", "run")
OVERWRITE = "--overwrite" in args
# a run takes minutes: refuse an existing result (or a link there, even a dangling one) before it
# starts, not after
if not OVERWRITE and os.path.lexists(LABEL + ".json"):
    sys.exit(f"{LABEL}.json exists: pass --label NAME for a new one, or --overwrite to replace it")

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
    if new > len(s["steps"]):
        # the app dropped some of this phase's steps: its numbers would be for part of it
        sys.exit(f"{new} loupe steps since the phase began but the app kept {len(s['steps'])}: use fewer --steps")
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
    empty_since = None  # since when the folder has stood loaded with no photo in it
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
        idle = st.get("import") is None and st.get("scan") is None
        if first is not None and idle and allp is None:
            allp = el
        # a folder with no supported photo: nothing will show, so stop a few seconds after loading
        if not shown and idle:
            empty_since = el if empty_since is None else empty_since
            if el - empty_since > 3:
                break
        else:
            empty_since = None
        # twelve thumbnails, or every photo's in a smaller folder: known once the folder has loaded
        if allp is not None and thumbs12 is None:
            thumbs12 = thumb_times.get(min(12, shown))
        if allp is not None and (thumbs12 is not None or not shown):
            break
        # every thumbnail was in memory already (a folder browsed before in this session): no new
        # ones will come, so this run can't time them; said in the output instead of waiting
        if allp is not None and tt == 0 and el - allp > 1:
            break
        # a layout that never holds twelve thumbnails: give up on that number a while after loading
        if allp is not None and el - allp > 30:
            break
    out["browse_first_photo_s"] = first
    out["browse_first_thumb_s"] = thumb1
    out["browse_12_thumbs_s"] = thumbs12
    out["browse_all_s"] = allp
    # the thumbnail times are a cold browse's: none when every one was already in memory
    out["browse_thumbs_were_in_memory"] = thumb1 is None and allp is not None
    out["photos"] = shown

# --- loupe on the first photo ----------------------------------------------------------------------
ex("library.selectAll")
ids = inspect().get("selection") or []
if not ids:
    sys.exit(f"{FOLDER}: no photos visible to open in the loupe; check the filters")
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
sender_errors = []
def sender(k):
    try:
        for i in range(k, STEPS, len(conns)):
            delay = t0 + i * HOLD_MS / 1000 - time.perf_counter()
            if delay > 0:
                time.sleep(delay)
            conns[k].call("ui.key", {"key": "ArrowRight"})
    except Exception as e:  # a failed key: the hold's numbers would be for fewer steps than sent
        sender_errors.append(f"sender {k}: {e}")
threads = [threading.Thread(target=sender, args=(k,)) for k in range(len(conns))]
[t.start() for t in threads]
[t.join() for t in threads]
if sender_errors:
    sys.exit("the held arrow failed: " + "; ".join(sender_errors))
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
settled_whole = None  # since when the whole-frame render has stood settled with no region asked for
zoom_end = None  # when the zoom was done: the region's arrival, or the settled whole frame
while time.perf_counter() - t0 < 60:
    st = inspect()
    now = time.perf_counter() - t0
    if cam is None and loupe(st).get("source") == "embedded":
        cam = now
    region = loupe(st).get("region")
    settled = not loupe(st).get("pending") and loupe(st).get("source") in ("render", "cached")
    if settled and region is not None and region.get("pending") is False:
        zoom_end = now
        break
    # the whole-frame render already holds 100 %: no region is asked for (the app asks within a
    # frame of the zoom, so half a second without one means none is coming)
    if settled and region is None:
        settled_whole = now if settled_whole is None else settled_whole
        if now - settled_whole >= 0.5:
            zoom_end = settled_whole
            break
    else:
        settled_whole = None
# (with no region, the zoom was done when the whole-frame render stood settled, not half a second on;
# a region that came after a settled whole frame ends it when the region is ready)
t = zoom_end  # None: the zoom did not finish within 60 s (missing data, not a 60 s zoom)
out["zoom100_camera_s_client"] = cam
out["zoom100_s_client"] = t
out["zoom100_region"] = loupe(st).get("region")
ex("view.zoomFit")

# --- rating / flagging latency (control round trip; the app's own time is in persistence) --------
# one photo, named on every call: the folder's filter may hide a new rating, and an id-less rating
# would then move the selection and rate the next photo; its own rating is put back afterwards
target = ex("photo.inspect")
target_id, target_rating = target["id"], target.get("rating") or 0
rate = []
try:
    for i in range(20):
        t0 = time.perf_counter()
        ex("photo.rate", {"rating": (i % 5) + 1, "ids": [target_id]})
        rate.append((time.perf_counter() - t0) * 1000)
finally:
    ex("photo.rate", {"rating": target_rating, "ids": [target_id]})
out["rate_cmd_ms_client"] = stats(rate)
info = ex("library.info")
out["persistence"] = {k: v for k, v in (info.get("persistence") or {}).items() if k in ("appends", "lastAppendMs", "maxAppendMs")}
p = inspect()["perf"]
out["perf"] = {k: p.get(k) for k in ("fps", "maxUpdateMs", "gpu", "rendersDone", "thumbTextures")}
print(json.dumps(out, indent=1, default=str))
open(LABEL + ".json", "w" if OVERWRITE else "x", encoding="utf-8").write(json.dumps(out, indent=1, default=str))





