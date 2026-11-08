#!/usr/bin/env python3
"""lcb.py FILE.jsonl — run control requests from a JSON-lines file ({"method":..., "params":...} or {"sleep": s})."""
import json, socket, sys, time
from pathlib import Path


# Largest control reply we accept (a screenshot response can carry a base64 image), after twice
# that the connection is closed instead of buffering further.
MAX_RESPONSE = 128 << 20

def _jsonl_path(arg):
    """Resolve the CLI file argument; require an existing readable regular file."""
    path = Path(arg).resolve()
    if not path.is_file():
        sys.exit(f"not a readable file: {arg}")
    try:
        path.open("rb").close()
    except OSError:
        sys.exit(f"not a readable file: {arg}")
    return path

def call(method, params):
    s = socket.create_connection(("127.0.0.1", 7980), timeout=180)
    s.sendall((json.dumps({"id": 1, "method": method, "params": params}) + "\n").encode())
    buf = bytearray()
    while not (len(buf) and buf[-1:] == b"\n"):
        c = s.recv(1 << 20)
        if not c: break
        # a hostile server must not make a client buffer or pay quadratic copies forever (CWE-407)
        if len(buf) + len(c) > MAX_RESPONSE:
            s.close()
            return {"ok": False, "error": f"response exceeds {MAX_RESPONSE} bytes"}
        buf.extend(c)
    return json.loads(buf)
for line in open(_jsonl_path(sys.argv[1])):
    line = line.strip()
    if not line or line.startswith("#"): continue
    r = json.loads(line)
    if "sleep" in r:
        time.sleep(r["sleep"]); continue
    out = call(r["method"], r.get("params", {}))
    print(r["method"], json.dumps(out)[:160])
