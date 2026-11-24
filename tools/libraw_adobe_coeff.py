#!/usr/bin/env python3
"""Convert LibRaw's `adobe_coeff` colour-matrix table into camera database entries.

Usage: libraw_adobe_coeff.py COLORDATA_CPP DCRAW_TOML BODIES_TSV > crates/camdb/data/cameras/libraw.toml

COLORDATA_CPP is LibRaw's src/tables/colordata.cpp (LGPL-2.1 or CDDL-1.0; licence text in
crates/camdb/data/cameras/LICENSE-LibRaw-LGPL-2.1.txt, credit in NOTICE). LibRaw's table descends from dcraw's
(tools/dcraw_adobe_coeff.py) and carries the bodies released since dcraw 9.28 (2018). Its rows are XYZ (D65) ->
camera matrices x 10000. Only the 3 x 3 matrices are taken; LibRaw's black / maximum levels stay out. Rows whose
camera dcraw.toml (DCRAW_TOML) already has are skipped, so this file only adds bodies.

Names: LibRaw matches a row when its model is a case-insensitive prefix of the camera's model (first match in table
order wins); the database matches exact names. So each row gets as aliases (a) the models its own "same CMs:"
comment lists and (b) every model in BODIES_TSV (maker, model, extension: the bodies with a pinned raw sample,
crates/camdb/tests/supported_bodies.tsv) that LibRaw's prefix rule gives that row, and (c) the North American and
Japanese names of Canon bodies and a few regional names, which LibRaw maps to the EOS number through Canon's model id rather than the name.
"""
import hashlib
import re
import sys

MAKES = {
    "PhaseOne": "Phase One",
    "Minolta": "Konica Minolta",
    "Agfa": "AgfaPhoto",
    "OmniVison": "OmniVision",
}
# Exif makes that differ from LibRaw's maker: (LibRaw maker, model prefix) -> [(Exif make, model prefix to add)]
EXTRA_MAKES = {
    "Pentax": [("RICOH IMAGING COMPANY, LTD.", "PENTAX ")],
    "Kodak": [("EASTMAN KODAK COMPANY", "")],
}
# Canon regional names (Exif model without "Canon") -> the EOS model LibRaw's table names. Public product facts.
CANON_REGIONAL = {
    "EOS Rebel T3": "EOS 1100D", "EOS Kiss X50": "EOS 1100D",
    "EOS Rebel T4i": "EOS 650D", "EOS Kiss X6i": "EOS 650D",
    "EOS Rebel T5i": "EOS 700D", "EOS Kiss X7i": "EOS 700D",
    "EOS Rebel T5": "EOS 1200D", "EOS Kiss X70": "EOS 1200D",
    "EOS Rebel T6": "EOS 1300D", "EOS Kiss X80": "EOS 1300D",
    "EOS Rebel T7": "EOS 2000D", "EOS Kiss X90": "EOS 2000D",
    "EOS Rebel T7i": "EOS 800D", "EOS Kiss X9i": "EOS 800D",
    "EOS Rebel T8i": "EOS 850D", "EOS Kiss X10i": "EOS 850D",
    "EOS Rebel SL2": "EOS 200D", "EOS Kiss X9": "EOS 200D",
    "EOS Rebel SL3": "EOS 250D", "EOS Kiss X10": "EOS 250D",
    "EOS Rebel XT": "EOS 350D", "EOS Kiss Digital N": "EOS 350D",
    "EOS Rebel T100": "EOS 4000D",
}
# Other regional Exif names -> the model LibRaw's table names (LibRaw maps them by model id).
REGIONAL = {("PANASONIC", "DC-FZ1000M2"): ["DC-FZ10002"]}

ROW = re.compile(
    r"\{\s*LIBRAW_CAMERAMAKER_(\w+)\s*,\s*\"([^\"]*)\"\s*,\s*[-\w]+\s*,\s*[-\w]+\s*,\s*((?://[^\n]*|/\*.*?\*/)\s*)?\{([^}]*)\}\s*\}",
    re.S,
)


def table(src: str) -> str:
    start = src.index("} table[] = {")
    body = src[start:]
    return body[: body.index("};")]


def toml_str(s: str) -> str:
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def key(brand: str, model: str) -> str:
    """lcc_camdb's name key: upper case, without a leading brand word, a leading EOS or a trailing DIGITAL."""
    w = model.upper().split()
    if len(w) > 1 and w[0] == brand:
        w = w[1:]
    if len(w) > 1 and w[0] == "EOS":
        w = w[1:]
    if len(w) > 1 and w[-1] == "DIGITAL":
        w = w[:-1]
    return " ".join(w)


def brand(make: str) -> str:
    return make.split()[0].upper() if make.split() else ""


def norm(model: str) -> str:
    """A model name for LibRaw's prefix rule: upper case, LibRaw's "FinePix" dropped, spaces and hyphens ignored
    (LibRaw normalises "GFX100 II" / "GFX 100 II" and "C5060WZ" / "C-5060WZ" before matching)."""
    m = model.upper()
    if m.startswith("FINEPIX "):
        m = m[len("FINEPIX ") :]
    return m.replace(" ", "").replace("-", "")


def same_cms(comment: str):
    m = re.search(r"same CMs:(.*)", comment or "", re.S)
    if not m:
        return []
    text = re.sub(r"\*/|\(and |\)", ",", m.group(1))
    out = []
    for part in re.split(r",|/| and ", text):
        part = part.strip().strip('"').strip()
        if part:
            out.append(part)
    return out


def dcraw_keys(path: str):
    keys = set()
    make = None
    for line in open(path, encoding="utf-8"):
        m = re.match(r'make = "(.*)"', line)
        if m:
            make = m.group(1)
        m = re.match(r'model = "(.*)"', line)
        if m and make:
            keys.add((brand(make), key(brand(make), m.group(1))))
    return keys


def main() -> int:
    if len(sys.argv) < 4:
        print(__doc__, file=sys.stderr)
        return 2
    raw = open(sys.argv[1], "rb").read()
    src = raw.decode("latin-1")
    have = dcraw_keys(sys.argv[2])
    bodies = []
    for line in open(sys.argv[3], encoding="utf-8"):
        if line.startswith("#") or not line.strip():
            continue
        f = line.rstrip("\n").split("\t")
        if len(f) >= 2:
            bodies.append((f[0], f[1]))
    rows = []
    for maker, model, comment, values in ROW.findall(table(src)):
        nums = [int(v) for v in re.findall(r"-?\d+", values)]
        if len(nums) != 9 or nums[0] == 0:
            continue
        rows.append((MAKES.get(maker, maker), model, comment, nums))
    # LibRaw's prefix rule for each body: the first row of the same maker whose model prefixes the body's model
    prefix_alias = {}
    for bmake, bmodel in bodies:
        for i, (make, model, _c, _n) in enumerate(rows):
            if brand(make) == brand(bmake) and model and norm(bmodel).startswith(norm(model)):
                if model.upper() != bmodel.upper():
                    prefix_alias.setdefault(i, []).append(bmodel)
                break
    regional = {}
    for exif, eos in CANON_REGIONAL.items():
        regional.setdefault(eos, []).append(exif)
    out = [
        "# Generated by tools/libraw_adobe_coeff.py from LibRaw src/tables/colordata.cpp "
        f"(commit 2ab632347ea003c96593eff2dc3105c91f5328e2, sha256 {hashlib.sha256(raw).hexdigest()}).",
        "# Do not edit by hand: re-run the tool. LGPL-2.1 or CDDL-1.0 (see NOTICE). XYZ (D65) -> camera matrices, same rank",
        "# as dcraw.toml (Source::Dcraw); measured and spectral entries win. Only bodies dcraw.toml lacks.",
        "",
    ]
    seen = set(have)
    kept = 0
    for i, (make, model, comment, nums) in enumerate(rows):
        b = brand(make)
        names = [model] + same_cms(comment) + prefix_alias.get(i, [])
        if b == "CANON":
            names += regional.get(model, [])
        names += REGIONAL.get((b, model), [])
        uniq = []
        for n in names:
            k = (b, key(b, n))
            if k[1] and k not in seen:
                seen.add(k)
                uniq.append(n)
        if not uniq:
            continue
        m = [[nums[r * 3 + c] / 10000.0 for c in range(3)] for r in range(3)]
        rows_txt = ", ".join("[" + ", ".join(f"{v:.4f}" for v in r) + "]" for r in m)
        variants = [(make, uniq)] + [(exif, [p + n for n in uniq]) for exif, p in EXTRA_MAKES.get(make, [])]
        for mk, ns in variants:
            out += ["[[camera]]", f"make = {toml_str(mk)}", f"model = {toml_str(ns[0])}"]
            if len(ns) > 1:
                out.append("aliases = [" + ", ".join(toml_str(n) for n in ns[1:]) + "]")
            out += [
                "illuminants = [21, 0]",
                f"color_matrix_1 = [{rows_txt}]",
                'provenance = { source = "dcraw", detail = "LibRaw adobe_coeff (src/tables/colordata.cpp, commit 2ab6323)" }',
                "",
            ]
            kept += 1
    sys.stdout.write("\n".join(out))
    print(f"libraw_adobe_coeff: {kept} entries written", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
