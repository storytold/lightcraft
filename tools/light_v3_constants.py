#!/usr/bin/env python3
"""Process V3's fitted Light constants: the committed fits and the Rust file generated from them (docs/light-v3.md).

    light_v3_constants.py rust [-o crates/pipeline/src/light_v3_params.rs] [--check]
        Write the Rust constants from the committed fits (tools/light_v3_fits/{contrast,blacks}.json): each
        fit's parameter vector verbatim (shortest round-trip literals; crates/pipeline/src/light_v3.rs unpacks it as
        Family.unpack does), where it came from, and, for the Rust tests only, Family's own curve and apply values at
        fixed points (rounded to 12 significant digits, so the file is the same on every machine). Running it on a
        checkout must reproduce the committed file byte for byte: --check says whether it does, writing nothing.
    light_v3_constants.py sanitize FIT.json SLIDER [-o tools/light_v3_fits/SLIDER.json]
        Turn a result of the fitting tool (fit_pointwise.py, kept with the private fitting data) into the committed
        form: the coefficients, the counts and aggregate errors only (median per amount, reference, size and split).
        Publish an error group or band only when at least three distinct photos contribute,
        with no photo names, paths or per-photo values. Swapping a fit is sanitizing the new one over the slider's
        file, then `rust`.

The fitting data are private: black-box Lightroom exports of the owner's photos. A public render-fidelity tool for
measuring your own exports is proposed separately in #1181; it is not included in this checkout.

Needs numpy only: tools/requirements-light-v3.txt.
"""

import argparse
import hashlib
import json
import sys
from collections import defaultdict
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
from light_v3_family import KNOTS as FAMILY_KNOTS, Family  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
FITS = ROOT / "tools" / "light_v3_fits"
OUT = ROOT / "crates" / "pipeline" / "src" / "light_v3_params.rs"
SLIDERS = {"contrast": "light.contrast", "blacks": "light.blacks"}
MODES = {"pc": "PerChannel", "lum": "Luminance", "mix": "Mix"}
KNOTS = [float(k) for k in FAMILY_KNOTS]
BANDS = ["<.03", ".03-.10", ".10-.25", ".25-.50", ".50-.90", ">.90"]
MIN_PHOTOS = 3
# where the reference values are taken (encoded ProPhoto values, amounts, pixels)
REF_V = [0.0, 0.004, 0.012, 0.03, 0.07, 0.11, 0.17, 0.25, 0.33, 0.45, 0.58, 0.7, 0.81, 0.9, 0.97, 1.0]
REF_A = [-100.0, -63.0, -25.0, -4.0, -1.0, 1.0, 4.0, 25.0, 63.0, 100.0]
REF_PIX = [[0.02, 0.05, 0.01], [0.3, 0.25, 0.2], [0.55, 0.5, 0.62], [0.9, 0.85, 0.7], [0.15, 0.4, 0.35], [0.75, 0.97, 0.99]]


def layout(name, fit, path):
    """Whether a fit is signed, after checking it against the family."""
    if fit.get("slider") != SLIDERS[name]:
        sys.exit(f"{path}: a fit of {fit.get('slider')}, not {SLIDERS[name]}")
    if fit.get("knots") != KNOTS:
        sys.exit(f"{path}: knots {fit.get('knots')} are not the family's {KNOTS}")
    if fit.get("mode") not in MODES:
        sys.exit(f"{path}: unknown mode {fit.get('mode')}")
    base = Family(fit["mode"]).size
    n = len(fit["params"])
    if n != base:
        sys.exit(f"{path}: {n} parameters, the family needs {base}")
    return name == "contrast"



def sanitize(args):
    path = Path(args.fit)
    raw = path.read_bytes()
    fit = json.loads(raw)
    signed = layout(args.slider, fit, path)
    errors = {}
    for split in ("tune", "check"):
        by = defaultdict(list)
        for r in fit.get(split, []):
            by[(r["a_tgt"], r["a_ref"], r["long_edge"])].append(r)
        rows = []
        for (a_tgt, a_ref, edge), g in sorted(by.items()):
            photos = len({r["photo"] for r in g})
            if photos < MIN_PHOTOS:
                continue
            band_photos = {b: len({r["photo"] for r in g if b in r["cells"]}) for b in BANDS}
            band_photos = {b: n for b, n in band_photos.items() if n >= MIN_PHOTOS}
            bands = {b: float(np.median([r["cells"][b]["error"] for r in g if b in r["cells"]]))
                     for b in band_photos}
            rows.append({"amount": a_tgt, "reference": a_ref, "long_edge": edge, "pairs": len(g),
                         "photos": photos, "encoded_error": float(np.median([r["encoded_error"] for r in g])),
                         "band_error": bands, "band_photos": band_photos})
        errors[split] = rows
    out = {
        "slider": fit["slider"], "mode": fit["mode"], "signed": signed,
        "knots": fit["knots"], "params": fit["params"], "amounts": fit.get("amounts"),
        "tune_pairs": fit.get("tune_pairs"), "tune_photos": fit.get("tune_photos"),
        "check_pairs": fit.get("check_pairs"), "check_photos": len({r["photo"] for r in fit.get("check", [])}),
        "cost": fit.get("cost") if len({r["photo"] for r in fit.get("tune", [])}) >= MIN_PHOTOS else None,
        "status": fit.get("status"),
        "fitted_as": {"file": path.name, "sha256": hashlib.sha256(raw).hexdigest()},
        "median_errors": errors,
    }
    text = json.dumps(out, indent=1) + "\n"
    for bad in ("DSC", "/Users", "photo\"", ".ARW", ".arw", ".tif"):
        if bad in text:
            sys.exit(f"{path}: the sanitized fit still contains {bad!r}")
    dest = Path(args.o) if args.o else FITS / f"{args.slider}.json"
    dest.write_text(text)
    print(f"{args.slider}: {path.name} -> {dest}")


def wrap(tokens, indent, width=140):
    """Comma-separated tokens packed into lines of at most `width` characters, each indented."""
    lines, line = [], ""
    for t in tokens:
        if line and len(indent) + len(line) + len(t) + 2 > width:
            lines.append(indent + line.rstrip())
            line = ""
        line += t + ", "
    if line:
        lines.append(indent + line.rstrip())
    return lines


def exact(v):
    return repr(float(v))


def ref12(v):
    """A reference value at 12 significant digits (numpy's last bit differs between machines and libraries)."""
    s = f"{float(v):.12g}"
    return s if any(c in s for c in ".en") else s + ".0"


def load(name):
    path = FITS / f"{name}.json"
    raw = path.read_bytes()
    fit = json.loads(raw)
    signed = layout(name, fit, path)
    fam = Family(fit["mode"], signed=signed)
    p = np.array(fit["params"], dtype=np.float64)
    grid = np.linspace(0, 1, 20001)
    ref = []
    for a in REF_A:
        curve = fam.curve(p, a, np.array(REF_V))
        # Apply where the curve increases everywhere (the Rust tables flatten a fold).
        increasing = bool(np.all(np.diff(fam.curve(p, a, grid)) >= 0))
        apply = fam.apply(p, a, np.array(REF_PIX)) if increasing else None
        ref.append((a, list(curve), None if apply is None else [list(r) for r in apply]))
    return {"name": name, "path": path.relative_to(ROOT).as_posix(), "sha256": hashlib.sha256(raw).hexdigest(),
            "fit": fit, "signed": signed, "reference": ref}


def rust(args):
    fits = [load(name) for name in SLIDERS]
    out = [
        "//! Fitted constants of process V3's Contrast and Blacks ([`crate::light_v3`]).",
        "//!",
        "//! GENERATED by `tools/light_v3_constants.py rust` from the committed fits named below: do not edit by hand,",
        "//! regenerate (the script must reproduce this file byte for byte).",
        "//!",
        "//! The values are fitted to black-box Lightroom exports of one person's Sony ILCE-7CR photos (the exports'",
        "//! pixels only: no Lightroom code, tables or files), each slider against the same photo's Neutral or another",
        "//! amount exported in the same batch, on the tune photos; the check photos were held out. The exports are",
        "//! private; errors are published only with at least three contributing photos. These are our operator family's",
        "//! parameters (`docs/light-v3.md`), not Lightroom's.",
        "//!",
    ]
    for f in fits:
        fit = f["fit"]
        out += [f"//! - {f['name'].capitalize()}: `{f['path']}` (fitted as `{fit['fitted_as']['file']}`), mode "
                f"`{fit['mode']}`; {fit['tune_pairs']} tune pairs on "
                f"{fit['tune_photos']} photos, {fit['check_pairs']} check pairs on {fit['check_photos']} photos;",
                f"//!   sha256 `{f['sha256']}`."]
    out += ["", "use crate::light_v3::{Fit, Mode};", "#[cfg(test)]", "use crate::light_v3::Reference;", "",
            "/// The family's knots: the displacement shapes are piecewise linear on them.",
            f"pub const KNOTS: [f64; {len(KNOTS)}] = [{', '.join(exact(k) for k in KNOTS)}];", ""]
    for f in fits:
        fit = f["fit"]
        doc = [f"/// `{f['path']}`."]
        out += [*doc,
                f"pub const {f['name'].upper()}: Fit = Fit {{",
                f"    mode: Mode::{MODES[fit['mode']]},",
                f"    signed: {'true' if f['signed'] else 'false'},",
                "    params: &[",
                *wrap([exact(v) for v in fit["params"]], "        "),
                "    ],",
                "};",
                ""]
    out += ["/// Where the reference values are taken: encoded values and pixels (encoded ProPhoto).",
            "#[cfg(test)]", f"pub const REF_V: [f64; {len(REF_V)}] = [", *wrap(map(exact, REF_V), "    "), "];",
            "#[cfg(test)]", f"pub const REF_PIX: [[f64; 3]; {len(REF_PIX)}] = [",
            *wrap(("[" + ", ".join(map(exact, r)) + "]" for r in REF_PIX), "    "), "];",
            ""]
    for f in fits:
        rows = []
        for a, curve, apply in f["reference"]:
            rows += ["    Reference {", f"        amount: {exact(a)},", "        curve: &[",
                     *wrap(map(ref12, curve), "            "), "        ],"]
            if apply is None:
                rows.append("        apply: None,")
            else:
                rows += ["        apply: Some(&[",
                         *wrap(("[" + ", ".join(map(ref12, r)) + "]" for r in apply), "            "), "        ]),"]
            rows.append("    },")
        out += [f"/// The family's own `curve` at [`REF_V`] and `apply` on [`REF_PIX`] for `{f['path']}` (12 significant",
                "/// digits).", "#[cfg(test)]", f"pub const {f['name'].upper()}_REFERENCE: &[Reference] = &[", *rows,
                "];", ""]
    dest = Path(args.o) if args.o else OUT
    text = "\n".join(out)
    if args.check:
        same = dest.exists() and dest.read_text() == text
        print(f"{dest}: {'reproduced byte for byte' if same else 'DIFFERS from what the committed fits give'}")
        return 0 if same else 1
    dest.write_text(text)
    for f in fits:
        print(f"{f['name']}: {f['path']} ({f['fit']['fitted_as']['file']}) {f['fit']['mode']}"
              f", {len(f['fit']['params'])} parameters")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("rust")
    r.add_argument("-o"), r.add_argument("--check", action="store_true")
    s = sub.add_parser("sanitize")
    s.add_argument("fit"), s.add_argument("slider", choices=list(SLIDERS)), s.add_argument("-o")
    args = ap.parse_args()
    return rust(args) if args.cmd == "rust" else sanitize(args)


if __name__ == "__main__":
    sys.exit(main())
