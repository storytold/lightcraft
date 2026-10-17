#!/usr/bin/env python3
"""lcb.py FILE.jsonl — run control requests from a JSON-lines file ({"method":..., "params":...} or {"sleep": s}).

Authenticates like the CLI: $<PREFIX>_CONTROL_TOKEN, else the token the app wrote to
control-token-7980 in its settings folder (docs/control-protocol.md → Authentication); the prefix
and the settings folder's name come from brand.toml."""
import json, os, socket, sys, time, tomllib
from pathlib import Path
PORT = 7980
BRAND = tomllib.loads((Path(__file__).resolve().parents[2] / "brand.toml").read_text(encoding="utf-8"))
PREFIX = BRAND["product"]["env_prefix"]
SETTINGS = BRAND["identity"]["settings_dir"]
def token():
    if os.environ.get(f"{PREFIX}_CONTROL_TOKEN", "").strip():
        return os.environ[f"{PREFIX}_CONTROL_TOKEN"].strip()
    if f"{PREFIX}_CONFIG_DIR" in os.environ:
        folder = Path(os.environ[f"{PREFIX}_CONFIG_DIR"])
    elif sys.platform == "win32":
        folder = Path(os.environ.get("APPDATA", "")) / SETTINGS
    elif sys.platform == "darwin":
        folder = Path.home() / "Library/Application Support" / SETTINGS
    else:
        folder = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config") / SETTINGS.lower()
    return (folder / f"control-token-{PORT}").read_text().strip()
def line(s, msg):
    s.sendall((json.dumps(msg) + "\n").encode())
    buf = b""
    while not buf.endswith(b"\n"):
        c = s.recv(1 << 20)
        if not c: break
        buf += c
    return json.loads(buf)
def call(method, params):
    s = socket.create_connection(("127.0.0.1", PORT), timeout=180)
    auth = line(s, {"id": 0, "method": "auth", "params": {"token": token()}})
    if not auth.get("ok"):
        return auth
    return line(s, {"id": 1, "method": method, "params": params})
for line in open(sys.argv[1]):
    line = line.strip()
    if not line or line.startswith("#"): continue
    r = json.loads(line)
    if "sleep" in r:
        time.sleep(r["sleep"]); continue
    out = call(r["method"], r.get("params", {}))
    print(r["method"], json.dumps(out)[:160])
