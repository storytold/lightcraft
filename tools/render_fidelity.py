#!/usr/bin/env python3
"""Render fidelity: compare LightCraft's single-slider responses with Lightroom's, on your own exports.

    render_fidelity.py plan OUT_DIR [--recipe SLIDER=A,B,...]...
        Lightroom develop presets (one per recipe, Neutral included) and export instructions.
    render_fidelity.py pair --exports DIR --raws DIR [--raws DIR]... [-o cases.json]
        Check each export's recipe from its embedded XMP and pair every slider export with a reference export of
        the same photo from the same export session.
    render_fidelity.py render cases.json --cli PATH/lightcraft-cli --run RUN_DIR [--split tune|check]
        Render the same recipes with LightCraft, register them on Lightroom's frame and sample both engines.
    render_fidelity.py score cases.json --run RUN_DIR
        Per-band agreement of the two engines' responses: RUN_DIR/scores.json and RUN_DIR/summary.txt.
    render_fidelity.py report --run RUN_DIR [--names] [-o report.md]
        A Markdown summary to paste into an issue: numbers only (file names with --names).
    render_fidelity.py selftest
        Check the parsing, pairing, registration and scoring on synthetic data (no photos needed).

What it measures, its rules and why each exists, how to make the exports and how to read the numbers:
docs/render-fidelity.md.

Clean-room: the tool reads only your own files: raws, and the TIFFs you exported from Lightroom (their pixels, the
develop settings Lightroom embeds in them as XMP, and the description of their colour profile). It never reads
Adobe's application files, camera profiles, presets or tables. Of the profile and look an export used it keeps only
their names, not the look's embedded parameters. The presets `plan` writes are generated here from the recipe values.

Needs numpy and tifffile, nothing else: tools/requirements-render-fidelity.txt pins versions that need Python
3.11+; the code also runs on Python 3.9 with numpy 2.0 and tifffile 2024.8.
"""

import argparse
import contextlib
import csv
import datetime
import hashlib
import io
import json
import os
import re
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import xml.etree.ElementTree as ET
from collections import Counter, defaultdict
from pathlib import Path
from xml.sax.saxutils import escape, quoteattr

try:
    import numpy as np
    import tifffile
except ImportError as e:  # pragma: no cover - environment problem, not a code path
    sys.exit(f"render_fidelity.py needs numpy and tifffile ({e}): pip install -r tools/requirements-render-fidelity.txt")

TOOL = Path(__file__).resolve()
CASES_FORMAT = "lightcraft-render-fidelity-cases/1"
RUN_FORMAT = "lightcraft-render-fidelity-run/1"
# Bump when the meaning of a cached sample changes (grid, registration, colour decoding): runs then refuse reuse.
SAMPLING_VERSION = 2  # 2: samples are .npz files with their provenance
DOC = "https://github.com/storytold/lightcraft/blob/main/docs/render-fidelity.md"

# ---------------------------------------------------------------------------------------------------------------
# Recipes: the Basic panel, as Lightroom's crs fields and LightCraft's develop controls
# ---------------------------------------------------------------------------------------------------------------

# crs field, LightCraft control, Lightroom label, slider range, default amounts for `plan`
BASIC = [
    ("Exposure2012", "light.exposure", "Exposure", (-5.0, 5.0), (-1.0, 1.0)),
    ("Contrast2012", "light.contrast", "Contrast", (-100.0, 100.0), (-50.0, 50.0)),
    ("Highlights2012", "light.highlights", "Highlights", (-100.0, 100.0), (-50.0, 50.0)),
    ("Shadows2012", "light.shadows", "Shadows", (-100.0, 100.0), (-50.0, 50.0)),
    ("Whites2012", "light.whites", "Whites", (-100.0, 100.0), (-50.0, 50.0)),
    ("Blacks2012", "light.blacks", "Blacks", (-100.0, 100.0), (-50.0, 50.0)),
    ("Texture", "effects.texture", "Texture", (-100.0, 100.0), (-50.0, 50.0)),
    ("Clarity2012", "effects.clarity", "Clarity", (-100.0, 100.0), (-50.0, 50.0)),
    ("Dehaze", "effects.dehaze", "Dehaze", (-100.0, 100.0), (-50.0, 50.0)),
    ("Vibrance", "color.vibrance", "Vibrance", (-100.0, 100.0), (-50.0, 50.0)),
    ("Saturation", "color.saturation", "Saturation", (-100.0, 100.0), (-50.0, 50.0)),
]
CRS_TO_LC = {b[0]: b[1] for b in BASIC}
LC_TO_BASIC = {b[1]: b for b in BASIC}
# What LightCraft renders on top of the recipe: sharpening, noise reduction and lens-profile corrections off, as the
# clean recipe requires of Lightroom (`--set`, so the CLI's own defaults can't drift into the measurement).
CLEAN = {"detail.sharpenAmount": 0, "detail.nrColor": 0, "detail.nrLuminance": 0,
         "optics.profileDistortion": 0, "optics.profileVignetting": 0}

# ---------------------------------------------------------------------------------------------------------------
# The clean recipe, checked affirmatively on the export's own XMP
# ---------------------------------------------------------------------------------------------------------------

# Settings an export must state, with the value the clean recipe needs.
REQUIRED = {"WhiteBalance": "As Shot", "Sharpness": "0", "LuminanceSmoothing": "0", "ColorNoiseReduction": "0",
            "LensProfileEnable": "0", "AutoLateralCA": "0", "AlreadyApplied": "True"}
# Settings whose Lightroom default isn't zero; absent means the default.
DEFAULTS = {"ColorGradeBlending": "50", "ParametricShadowSplit": "25", "ParametricMidtoneSplit": "50",
            "ParametricHighlightSplit": "75", "PerspectiveScale": "100", "CropRight": "1", "CropBottom": "1",
            "CurveRefineSaturation": "100", "ConvertToGrayscale": "False", "OverrideLookVignette": "False",
            "HasCrop": "False", "ToneCurveName2012": "Linear"}
# Identifiers, bookkeeping and the as-shot white (WhiteBalance must be As Shot): not compared, not develop settings.
IGNORED = {"RawFileName", "HasSettings", "AutoToneDigest", "AutoToneDigestNoSat", "Temperature", "Tint",
           "AsShotTemperature", "AsShotTint", "CameraProfileDigest", "Version", "CompatibleVersion",
           "CropConstrainToWarp", "CropConstrainToUnitSquare", "ToneCurveName", "ProcessVersion", "CameraProfile",
           "PresetType", "Cluster", "UUID", "SupportsAmount", "SupportsAmount2", "SupportsColor",
           "SupportsMonochrome", "SupportsHighDynamicRange", "SupportsNormalDynamicRange", "SupportsSceneReferred",
           "SupportsOutputReferred", "CameraModelRestriction", "Copyright", "ContactInfo", "RequiresRGBTables",
           "JPEGHandling"}
# Settings that change nothing while their amount (checked separately) is zero: (prefix, amount).
DEPENDENT = [("Sharpen", "Sharpness"), ("LuminanceNoiseReduction", "LuminanceSmoothing"),
             ("ColorNoiseReduction", "ColorNoiseReduction"), ("Grain", "GrainAmount"),
             ("PostCropVignette", "PostCropVignetteAmount"), ("Upright", "PerspectiveUpright"),
             ("VignetteMidpoint", "VignetteAmount"), ("LensProfile", "LensProfileEnable"),
             ("DefringePurpleHue", "DefringePurpleAmount"), ("DefringeGreenHue", "DefringeGreenAmount"),
             ("GrayMixer", "ConvertToGrayscale"), ("HDR", "HDREditMode"), ("SDR", "HDREditMode")]
# The profiles LightCraft's own starting look stands in for (docs/xmp-interop.md); it can select no other.
PROFILES = {("Adobe Standard", None), ("Adobe Standard", "Adobe Color")}
CURVES = ("ToneCurvePV2012", "ToneCurvePV2012Red", "ToneCurvePV2012Green", "ToneCurvePV2012Blue")
# Structures that hold nothing to check here: the 2010 process's curve (unused by 2012+ processes), the point-colour
# range (unused without point colours), and preset names.
IGNORED_ELEMENTS = {"ToneCurve", "ColorVariance", "Name", "ShortName", "SortName", "Group", "Description"}
TIFF_COMPRESSIONS = {1: "none", 8: "deflate", 32946: "deflate"}  # what tifffile decodes without extra packages
RAW_EXTENSIONS = {".arw", ".srf", ".sr2", ".cr2", ".cr3", ".crw", ".nef", ".nrw", ".raf", ".orf", ".rw2", ".pef",
                  ".dng", ".srw", ".3fr", ".iiq", ".erf", ".kdc", ".dcr", ".mos", ".mrw", ".x3f", ".rwl", ".gpr",
                  ".mef", ".raw"}
NS_RDF = "http://www.w3.org/1999/02/22-rdf-syntax-ns#"
NS_CRS = "http://ns.adobe.com/camera-raw-settings/1.0/"
NS_XMP = "http://ns.adobe.com/xap/1.0/"
RDF, CRS, XMPNS = "{%s}" % NS_RDF, "{%s}" % NS_CRS, "{%s}" % NS_XMP


class Refused(Exception):
    """An input the tool won't use: `kind` groups the counts, `detail` says exactly why."""

    def __init__(self, kind, detail=""):
        super().__init__(f"{kind}: {detail}" if detail else kind)
        self.kind, self.detail = kind, detail


def is_zero(v):
    s = str(v).strip()
    if s in ("", "False", "false"):
        return True
    try:
        return float(s) == 0.0
    except ValueError:
        return False


def same_value(a, b):
    try:
        return float(a) == float(b)
    except (TypeError, ValueError):
        return str(a).strip() == str(b).strip()


def normal(v):
    try:
        return repr(float(v))
    except (TypeError, ValueError):
        return str(v).strip()


def parse_xmp(data):
    """(crs simple values, crs structures, xmp simple values) of an XMP packet's top-level descriptions."""
    text = data.decode("utf-8", "replace") if isinstance(data, (bytes, bytearray)) else str(data)
    if len(text) > 16 << 20:
        raise Refused("unreadable XMP", "larger than 16 MB")
    if "<!DOCTYPE" in text or "<!ENTITY" in text:
        raise Refused("unreadable XMP", "has a DOCTYPE (not an XMP packet)")
    for open_tag, close_tag in (("<x:xmpmeta", "</x:xmpmeta>"), ("<x:xapmeta", "</x:xapmeta>"),
                                ("<rdf:RDF", "</rdf:RDF>")):
        start, end = text.find(open_tag), text.rfind(close_tag)
        if start >= 0 and end > start:
            text = text[start:end + len(close_tag)]
            break
    else:
        raise Refused("unreadable XMP", "no xmpmeta or rdf:RDF element")
    try:
        root = ET.fromstring(text)
    except ET.ParseError as e:
        raise Refused("unreadable XMP", str(e)) from None
    rdf = root if root.tag == RDF + "RDF" else root.find(RDF + "RDF")
    if rdf is None:
        raise Refused("unreadable XMP", "no rdf:RDF element")
    crs, elements, xmp = {}, {}, {}
    for desc in rdf.findall(RDF + "Description"):
        for k, v in desc.attrib.items():
            if k.startswith(CRS):
                crs[k[len(CRS):]] = v
            elif k.startswith(XMPNS):
                xmp[k[len(XMPNS):]] = v
        for child in desc:
            if child.tag.startswith(CRS):
                name = child.tag[len(CRS):]
                if len(child) == 0 and not child.attrib:
                    crs[name] = (child.text or "").strip()  # element form of a simple value
                else:
                    elements[name] = child  # a structure (also one written as attributes on an empty element)
            elif child.tag.startswith(XMPNS) and len(child) == 0:
                xmp[child.tag[len(XMPNS):]] = (child.text or "").strip()
    return crs, elements, xmp


def seq_items(element):
    return [(li.text or "").strip() for li in element.iter(RDF + "li")]


def curve_points(element):
    points = []
    for item in seq_items(element):
        try:
            x, y = (float(v) for v in item.split(","))
        except ValueError:
            raise Refused("not the clean recipe", f"unreadable curve point {item!r}") from None
        points.append((x, y))
    return points


def look_of(elements):
    """(name, amount) of the export's look; (None, None) without one. A look whose name can't be read is refused:
    it isn't taken for no look. Reads the rdf:Description form Lightroom writes, rdf:parseType="Resource" and
    attributes on the element itself."""
    look = elements.get("Look")
    if look is None:
        return None, None
    name = amount = None
    for node in (look, look.find(RDF + "Description")):
        if node is None:
            continue
        for prop in ("Name", "Amount"):
            value = node.get(CRS + prop)
            el = node.find(CRS + prop)
            if value is None and el is not None:
                texts = [t.strip() for t in ([el.text or ""] + [li.text or "" for li in el.iter(RDF + "li")]) if t.strip()]
                value = texts[0] if texts else None
            if value is not None and prop == "Name":
                name = name or value
            elif value is not None:
                amount = amount or value
    if not name:
        raise Refused("not the clean recipe", "the look's name can't be read")
    return name, amount or "1"


def clean_recipe(crs, elements):
    """The export's recipe if its settings are the clean recipe with Basic sliders only, else Refused."""
    pv = crs.get("ProcessVersion")
    try:
        if float(pv) < 6.7:
            raise ValueError
    except (TypeError, ValueError):
        raise Refused("not the clean recipe", f"ProcessVersion {pv!r} (needs a 2012-or-later process)") from None
    for k, want in REQUIRED.items():
        if k not in crs:
            raise Refused("not the clean recipe", f"{k} is not stated in the embedded XMP")
        if not same_value(crs[k], want):
            raise Refused("not the clean recipe", f"{k}={crs[k]!r} (needs {want})")
    profile = crs.get("CameraProfile")
    look, look_amount = look_of(elements)
    if (profile, look) not in PROFILES:
        raise Refused("profile LightCraft can't stand in for",
                      f"{profile!r} with look {look!r} (needs Adobe Standard, with the Adobe Color look or none)")
    if look is not None and not same_value(look_amount, "1"):
        raise Refused("not the clean recipe", f"look amount {look_amount!r} (needs 100%)")
    for name in CURVES:
        if name in elements and curve_points(elements[name]) not in ([], [(0.0, 0.0), (255.0, 255.0)]):
            raise Refused("not the clean recipe", f"{name} is not the identity curve")
    if "ToneCurvePV2012" not in elements and not same_value(crs.get("ToneCurveName2012", "Linear"), "Linear"):
        raise Refused("not the clean recipe", f"ToneCurveName2012={crs['ToneCurveName2012']!r} without points")
    for name, element in elements.items():
        if name in CURVES or name in IGNORED_ELEMENTS or name == "Look":
            continue
        items = seq_items(element)
        if name == "PointColors" and all(set(re.findall(r"-?[\d.]+", i)) <= {"-1.000000", "-1", "-1.0"} for i in items):
            continue  # every point colour unset
        own = {k for k in element.attrib if k != RDF + "parseType"}
        if items or own or any(len(c) or c.attrib or (c.text or "").strip() for c in element.iter() if c is not element):
            raise Refused("not the clean recipe", f"has {name} (local corrections, spots or other edits)")
    for k, v in crs.items():
        if k in IGNORED or k in CRS_TO_LC or k in REQUIRED:
            continue
        if k in DEFAULTS:
            if k == "ToneCurveName2012" and "ToneCurvePV2012" in elements:
                continue  # the points (identity, checked above) decide
            if not same_value(v, DEFAULTS[k]):
                raise Refused("not the clean recipe", f"{k}={v!r} (needs {DEFAULTS[k]})")
            continue
        owner = next((a for p, a in DEPENDENT if k.startswith(p) and k != a), None)
        if owner is not None and is_zero(crs.get(owner, "0")):
            continue
        if (k.startswith(("ColorGrade", "SplitToning")) and k.endswith("Hue")
                and is_zero(crs.get(k[:-3] + "Sat", crs.get(k[:-3] + "Saturation", "0")))):
            continue  # a grading hue without saturation changes nothing (calibration hues are checked)
        if not is_zero(v):
            raise Refused("setting the tool can't map", f"{k}={v!r}")
    recipe = {}
    for crs_name, lc, label, (lo, hi), _ in BASIC:
        raw = crs.get(crs_name, "0")
        try:
            value = float(raw)
        except ValueError:
            raise Refused("not the clean recipe", f"{crs_name}={raw!r} is not a number") from None
        if not lo <= value <= hi:
            raise Refused("not the clean recipe", f"{crs_name}={raw!r} outside {lo:g}..{hi:g}")
        recipe[lc] = value
    signature = sorted((k, normal(v)) for k, v in crs.items()
                       if k not in IGNORED and k not in CRS_TO_LC and k != "ToneCurveName2012")
    signature += [("CameraProfile", str(profile)), ("Look", str(look)), ("ProcessVersion", normal(pv))]
    return {"recipe": recipe, "moved": [lc for lc, v in recipe.items() if v != 0.0], "signature": signature,
            "profile": profile, "look": look, "process": str(pv)}


# ---------------------------------------------------------------------------------------------------------------
# Exports and raws
# ---------------------------------------------------------------------------------------------------------------

def icc_description(icc):
    """The 'desc' text of an ICC profile (v2 textDescription or v4 multiLocalizedUnicode), else None."""
    if not isinstance(icc, (bytes, bytearray)) or len(icc) < 132:
        return None
    count = int.from_bytes(icc[128:132], "big")
    for i in range(min(count, 512)):
        at = 132 + 12 * i
        if at + 12 > len(icc):
            break
        sig, off, size = icc[at:at + 4], int.from_bytes(icc[at + 4:at + 8], "big"), int.from_bytes(icc[at + 8:at + 12], "big")
        if sig != b"desc":
            continue
        d = icc[off:off + size]
        if d[:4] == b"desc" and len(d) >= 12:
            n = int.from_bytes(d[8:12], "big")
            return d[12:12 + n].split(b"\0")[0].decode("latin-1")
        if d[:4] == b"mluc" and len(d) >= 28:
            length, start = int.from_bytes(d[20:24], "big"), int.from_bytes(d[24:28], "big")
            return d[start:start + length].decode("utf-16-be", "replace").rstrip("\0")
        return None
    return None


def export_time(xmp, tiff_datetime):
    """Seconds since the epoch at which Lightroom wrote the export: its XMP MetadataDate, else ModifyDate, else the
    TIFF DateTime (local time). In Lightroom's exports all three are the time the file was written."""
    for v in (xmp.get("MetadataDate"), xmp.get("ModifyDate")):
        if v:
            # Python < 3.11 reads only 3- or 6-digit fractions of a second
            v = re.sub(r"\.(\d+)", lambda m: "." + (m.group(1) + "000000")[:6], v.strip().replace("Z", "+00:00"))
            try:
                return datetime.datetime.fromisoformat(v).timestamp()
            except ValueError:
                pass
    if tiff_datetime:
        try:
            return datetime.datetime.strptime(str(tiff_datetime).strip(), "%Y:%m:%d %H:%M:%S").timestamp()
        except ValueError:
            pass
    return None


def read_export(path):
    """The header facts of a Lightroom export (no pixel data is read), or Refused."""
    try:
        with tifffile.TiffFile(path) as t:
            if not t.pages:
                raise Refused("not a usable TIFF", "no image")
            page = t.pages[0]

            def tag(code):
                found = page.tags.get(code)
                return found.value if found is not None else None

            if page.dtype != np.uint16 or page.samplesperpixel != 3 or int(page.photometric) != 2:
                raise Refused("not a 16-bit RGB TIFF", f"{page.dtype}, {page.samplesperpixel} samples, "
                              f"photometric {int(page.photometric)}")
            compression = int(page.compression)
            if compression not in TIFF_COMPRESSIONS:
                raise Refused("TIFF compression", f"{compression} (export with Compression: None)")
            shape = [int(page.imagelength), int(page.imagewidth)]
            xmp_data, icc = tag(700), tag(34675)
            make, model, software, stamp = tag(271), tag(272), tag(305), tag(306)
    except Refused:
        raise
    except Exception as e:  # any unreadable header
        raise Refused("not a usable TIFF", f"{type(e).__name__}: {e}") from None
    if not xmp_data:
        raise Refused("no embedded XMP", "export with Metadata: All Metadata")
    desc = icc_description(icc)
    if desc is None:
        raise Refused("colour space not sRGB", "no readable embedded colour profile")
    if "srgb" not in desc.lower():
        raise Refused("colour space not sRGB", repr(desc))
    crs, elements, xmp = parse_xmp(xmp_data)
    st = os.stat(path)
    return {"path": str(path), "size": st.st_size, "mtime_ns": st.st_mtime_ns, "shape": shape,
            "long_edge": max(shape), "icc_sha256": hashlib.sha256(bytes(icc)).hexdigest(), "icc": desc,
            "crs": crs, "elements": elements, "time": export_time(xmp, stamp),
            "creator": xmp.get("CreatorTool") or (str(software) if software else None),
            "camera": " ".join(str(v).strip() for v in (make, model) if v)}


# Develop settings LightCraft applies from a raw's own XMP when there is no sidecar (crates/engine/src/sidecar.rs):
# its complete settings from `lc:settings` (LightCraft writes them into its DNGs), else the mapped `crs:` fields.
# Two layers keep such a raw out (docs/render-fidelity.md):
# - the authority is LightCraft's own importer: `render` imports each raw (sidecar-free) with the CLI being measured
#   and refuses it when the settings the import gave it differ from a reset (CliRenderer.inspect);
# - this conservative prefilter in `pair` reads only the XMP packets LightCraft's reader takes from a file
#   (crates/meta/src/container.rs: TIFF IFD0 tag 700, the CR3 XMP `uuid` box, a RAF's embedded JPEG, JPEG APP1 XMP
#   and extended XMP, PNG iTXt, WebP `XMP `) and refuses any lc / crs develop packet in them, under any prefix, a
#   `crs:` or `lc:settings` name however it is bound (LightCraft keeps unknown prefixes as written), and anything it
#   can't inspect completely: a packet that isn't well-formed UTF-8 XML, a namespace declaration that is escaped
#   (real XMP never escapes namespace URIs), overlong or unreadable, a container it can't walk, and XMP-like bytes
#   in a container it doesn't know.
NS_DECL = re.compile(rb"""xmlns(?::[A-Za-z_][\w.-]*)?\s*=\s*(?:"([^"<&]{0,1024})"|'([^'<&]{0,1024})')""")
CRS_NAME = re.compile(rb"""(?:^|[<\s/])crs:[A-Za-z_][\w.-]*(?:\s*=|[\s/>]|$)""")
LC_SETTINGS = re.compile(rb"""(?:^|[<\s/])lc:settings(?:\s*=|[\s/>]|$)""")
XMP_UUID = bytes.fromhex("be7acfcb97a942e89c71999491e3afac")
JPEG_XMP, JPEG_XMP_EXT = b"http://ns.adobe.com/xap/1.0/\0", b"http://ns.adobe.com/xmp/extension/\0"
TIFF_TYPE_SIZES = {1: 1, 2: 1, 3: 2, 4: 4, 5: 8, 6: 1, 7: 1, 8: 2, 9: 4, 10: 8, 11: 4, 12: 8, 13: 4, 16: 8, 17: 8, 18: 8}


class Uninspectable(Exception):
    """Something LightCraft's reader might take XMP from that the prefilter can't read completely: refused."""


def _int(data, at, n, order="big"):
    b = data[at:at + n] if at >= 0 else b""
    if len(b) != n:
        raise Uninspectable("truncated container")
    return int.from_bytes(b, order)


def _tiff_packets(data):
    order = "little" if data[:2] == b"II" else "big"
    magic = _int(data, 2, 2, order)
    if magic == 43:
        big, first = True, _int(data, 8, 8, order)
    elif magic in (42, 0x4F52, 0x5352, 0x55):
        big, first = False, _int(data, 4, 4, order)
    else:
        return None
    count = _int(data, first, 8 if big else 2, order)
    if count > 65535:
        raise Uninspectable("implausible IFD0")
    size, inline = (20, 8) if big else (12, 4)
    out = []
    for i in range(count):
        at = first + (8 if big else 2) + i * size
        if _int(data, at, 2, order) != 700:
            continue
        kind, n = _int(data, at + 2, 2, order), _int(data, at + 4, inline, order)
        if kind not in TIFF_TYPE_SIZES:
            raise Uninspectable("tag 700 of an unknown type")
        length = TIFF_TYPE_SIZES[kind] * n
        where = at + 4 + inline if length <= inline else _int(data, at + 4 + inline, inline, order)
        value = data[where:where + length]
        if len(value) != length:
            raise Uninspectable("tag 700 outside the file")
        out.append(value)
    return out


def _jpeg_packets(b):
    out, ext, i = [], [], 2
    while i + 4 <= len(b):
        if b[i] != 0xFF:
            raise Uninspectable("JPEG markers can't be walked")
        marker = b[i + 1]
        if marker == 0xFF:
            i += 1
            continue
        if marker == 0xD8 or 0xD0 <= marker <= 0xD7 or marker == 0x01:
            i += 2
            continue
        if marker in (0xD9, 0xDA):
            break
        length = _int(b, i + 2, 2)
        if length < 2:
            raise Uninspectable("JPEG segment length")
        payload = b[i + 4:i + 2 + length]
        if marker == 0xE1 and payload.startswith(JPEG_XMP):
            out.append(payload[len(JPEG_XMP):])
        elif marker == 0xE1 and payload.startswith(JPEG_XMP_EXT) and len(payload) >= len(JPEG_XMP_EXT) + 40:
            ext.append((_int(payload, len(JPEG_XMP_EXT) + 36, 4), payload[len(JPEG_XMP_EXT) + 40:]))
        i += 2 + length
    if ext:
        out.append(b"".join(chunk for _, chunk in sorted(ext)))
    return out


def _cr3_packets(data):
    out, at = [], 0
    while at + 8 <= len(data):
        size, kind = _int(data, at, 4), data[at + 4:at + 8]
        header = 8
        if size == 1:
            size, header = _int(data, at + 8, 8), 16
        elif size == 0:
            size = len(data) - at
        if size < header or at + size > len(data):
            raise Uninspectable("CR3 box structure")
        if kind == b"uuid" and data[at + header:at + header + 16] == XMP_UUID:
            out.append(data[at + header + 16:at + size])
        at += size
    return out


def _png_packets(data):
    import zlib
    out, at = [], 8
    while at + 8 <= len(data):
        length, kind = _int(data, at, 4), data[at + 4:at + 8]
        body = data[at + 8:at + 8 + length]
        if len(body) != length:
            raise Uninspectable("PNG chunk structure")
        if kind == b"iTXt" and body.startswith(b"XML:com.adobe.xmp\0"):
            rest = body[len(b"XML:com.adobe.xmp\0"):]
            compressed, text = rest[:1] == b"\1", rest[2:].split(b"\0", 2)
            if len(text) < 3:
                raise Uninspectable("PNG iTXt")
            try:
                out.append(zlib.decompress(text[2]) if compressed else text[2])
            except zlib.error:
                raise Uninspectable("PNG iTXt compression") from None
        at += 12 + length
    return out


def _webp_packets(data):
    out, at = [], 12
    while at + 8 <= len(data):
        kind, length = data[at:at + 4], _int(data, at + 4, 4, "little")
        if at + 8 + length > len(data):
            raise Uninspectable("WebP chunk structure")
        if kind == b"XMP ":
            out.append(data[at + 8:at + 8 + length])
        at += 8 + length + (length & 1)
    return out


def xmp_packets(data):
    """The XMP packets LightCraft's reader takes from a file, sniffed in its order (crates/meta/src/container.rs)."""
    if data.startswith(b"FUJIFILMCCD-RAW"):
        offset, length = _int(data, 84, 4), _int(data, 88, 4)
        return _jpeg_packets(data[offset:offset + length]) if offset > 0 else []
    if data[4:12] == b"ftypcrx ":
        return _cr3_packets(data)
    if data.startswith(b"\xff\xd8"):
        return _jpeg_packets(data)
    if data.startswith(b"\x89PNG\r\n\x1a\n"):
        return _png_packets(data)
    if data[:4] == b"RIFF" and data[8:12] == b"WEBP":
        return _webp_packets(data)
    if data[:2] in (b"II", b"MM"):
        packets = _tiff_packets(data)
        if packets is not None:
            return packets
    if any(m in data for m in (b"xmlns", b"<x:xmpmeta", b"rdf:RDF", b"rdf:Description")):
        raise Uninspectable("XMP-like text in a container the guard doesn't know")
    return []


def packet_develop(packet):
    """The develop packets in one XMP packet (see the comment above); Uninspectable when it can't be read fully."""
    body = packet.strip(b"\x00 \t\r\n")
    if not body:
        return set()
    try:
        text = body.decode("utf-8")
    except UnicodeDecodeError:
        raise Uninspectable("an XMP packet that isn't UTF-8") from None
    if "<!DOCTYPE" in text or "<!ENTITY" in text:
        raise Uninspectable("an XMP packet with a DOCTYPE")
    try:
        ET.fromstring(text.lstrip("\ufeff"))
    except ET.ParseError as e:
        raise Uninspectable(f"an XMP packet that isn't well-formed ({e})") from None
    found = set()
    at = body.find(b"xmlns")
    while at != -1:
        m = NS_DECL.match(body, at)
        if not m:
            raise Uninspectable("a namespace declaration it can't read (escaped, overlong or malformed)")
        uri = (m.group(1) if m.group(1) is not None else m.group(2)).decode("latin-1").strip().lower()
        if "ns.lightcraft.app/lc/" in uri:
            found.add("LightCraft lc:settings")
        if "camera-raw-settings" in uri:
            found.add("Camera Raw crs: settings")
        at = body.find(b"xmlns", at + 5)
    if CRS_NAME.search(body):
        found.add("Camera Raw crs: settings")
    if LC_SETTINGS.search(body):
        found.add("LightCraft lc:settings")
    return found


def embedded_develop(data):
    """What keeps a raw out, by the prefilter: its develop packets, or why it can't be inspected; [] when clean."""
    try:
        return sorted(set().union(*(packet_develop(x) for x in xmp_packets(data))))
    except Uninspectable as e:
        return [f"an XMP packet the guard can't inspect ({e})"]


def hash_raw(path):
    """sha256 of a raw file, and what the prefilter finds in its embedded XMP (embedded_develop)."""
    data = Path(path).read_bytes()
    return hashlib.sha256(data).hexdigest(), embedded_develop(data)


class Raws:
    """Raw files under the given folders, found by an export's crs:RawFileName (name, else stem)."""

    def __init__(self, roots):
        self.by_name, self.by_stem, self.hashes = defaultdict(list), defaultdict(list), {}
        for root in roots:
            root = Path(root)
            files = [root] if root.is_file() else (Path(d) / f for d, _, fs in os.walk(root) for f in sorted(fs))
            for f in files:
                if f.suffix.lower() in RAW_EXTENSIONS:
                    self.by_name[f.name.lower()].append(f)
                    self.by_stem[f.stem.lower()].append(f)

    def hash(self, path):
        real = os.path.realpath(path)
        if real not in self.hashes:
            try:
                self.hashes[real] = hash_raw(real)
            except OSError as e:
                raise Refused("raw unreadable", f"{Path(real).name}: {e.strerror or e}") from None
        return self.hashes[real]

    def find(self, raw_name):
        if not raw_name:
            raise Refused("no raw named", "the XMP has no crs:RawFileName")
        for table, key in ((self.by_name, raw_name.lower()), (self.by_stem, Path(raw_name).stem.lower())):
            found = table.get(key, [])
            if found:
                if len({self.hash(f)[0] for f in found}) > 1:
                    raise Refused("raw ambiguous", f"{len(found)} different files named like {raw_name}")
                return found[0]
        raise Refused("raw not found", raw_name)


def session_id(folder, segment):
    """A session's id: its export folder and time segment, JSON-encoded so that no folder name (`s#2`, `s (2)`) can
    collide with another folder's segment."""
    return json.dumps([folder, segment])


def session_label(session):
    folder, segment = json.loads(session)
    return folder if segment == 1 else f"{folder} (part {segment})"


def assign_sessions(exports, gap_minutes):
    """One export folder is one session; a folder whose export times have a gap longer than `gap_minutes` is split
    there (two export operations into one folder). Returns {session id: facts}."""
    by_folder = defaultdict(list)
    for e in exports:
        by_folder[e["folder"]].append(e)
    sessions = {}
    for folder, items in sorted(by_folder.items()):
        timed = all(e["time"] is not None for e in items)
        items.sort(key=lambda e: (e["time"] or 0.0, e["path"]) if timed else (0.0, e["path"]))
        part, last = 1, None
        for e in items:
            if timed and gap_minutes and last is not None and e["time"] - last > gap_minutes * 60:
                part += 1
            last = e["time"]
            e["session"] = session_id(folder, part)
        for e in items:
            s = sessions.setdefault(e["session"], {"folder": folder, "segment": json.loads(e["session"])[1],
                                                   "exports": 0, "first": None, "last": None})
            s["exports"] += 1
            if e["time"] is not None:
                s["first"] = e["time"] if s["first"] is None else min(s["first"], e["time"])
                s["last"] = e["time"] if s["last"] is None else max(s["last"], e["time"])
        if not timed:
            sessions[session_id(folder, 1)]["note"] = "export times missing: not split by time"
    for s in sessions.values():
        for k in ("first", "last"):
            if s[k] is not None:
                s[k] = datetime.datetime.fromtimestamp(s[k], datetime.timezone.utc).isoformat()
    return sessions


def export_ref(e):
    return {"path": e["path"], "size": e["size"], "mtime_ns": e["mtime_ns"]}


def recipe_key(slider, amount):
    return f"{slider}={amount:g}"


def make_pairs(exports):
    """Pair each single-slider export with a reference of the same photo, session, size, colour profile and
    settings: a Neutral, else the same slider's export with the smallest other amount. One pair per photo, recipe
    and size: a Neutral reference first, else the first session. Returns ({photo: [pair]}, [exploratory])."""
    by = defaultdict(lambda: defaultdict(list))
    for e in exports:
        by[e["photo"]][e["session"]].append(e)
    photos, exploratory = {}, []
    for photo in sorted(by):
        chosen = {}
        for session in sorted(by[photo]):
            items = sorted(by[photo][session], key=lambda e: e["path"])
            neutrals = [x for x in items if not x["moved"]]
            for e in items:
                if not e["moved"]:
                    continue
                slider = e["moved"][0]
                amount = e["recipe"][slider]

                def same(x, e=e):
                    return (x["long_edge"] == e["long_edge"] and x["shape"] == e["shape"]
                            and x["icc_sha256"] == e["icc_sha256"] and x["signature"] == e["signature"])

                ref, kind = next((x for x in neutrals if same(x)), None), "neutral"
                if ref is None:
                    others = [x for x in items if x["moved"] == [slider] and x["recipe"][slider] != amount and same(x)]
                    ref = min(others, key=lambda x: (abs(x["recipe"][slider]), x["recipe"][slider]), default=None)
                    kind = "same slider, other amount"
                key = recipe_key(slider, amount)
                if ref is None:
                    exploratory.append({"photo": photo, "recipe": key, "long_edge": e["long_edge"],
                                        "session": session, "export": e["path"]})
                    continue
                pid = f"{key}@{e['long_edge']}"
                old = chosen.get(pid)
                if old is not None and not (old["reference"]["kind"] != "neutral" and kind == "neutral"):
                    continue
                ref_recipe = "neutral" if not ref["moved"] else recipe_key(slider, ref["recipe"][slider])
                chosen[pid] = {
                    "id": pid, "key": key, "slider": slider, "amount": amount, "long_edge": e["long_edge"],
                    "shape": e["shape"], "session": session, "lightroom_state": "not recorded",
                    "export": export_ref(e),
                    "reference": {"kind": kind, "recipe": ref_recipe, "export": export_ref(ref)},
                    "lightcraft": {"recipe": e["recipe"], "reference": ref["recipe"]},
                    "profile": e["profile"], "look": e["look"], "process": e["process"],
                    "lightroom": e["creator"], "camera": e["camera"]}
        if chosen:
            photos[photo] = [chosen[k] for k in sorted(chosen, key=lambda k: (chosen[k]["slider"],
                                                                               chosen[k]["amount"],
                                                                               chosen[k]["long_edge"]))]
    return photos, exploratory


def split_of(raw_sha256):
    """Tune or check, fixed per photo by its raw file's hash (two thirds tune)."""
    return "check" if int(raw_sha256, 16) % 3 == 0 else "tune"


def cmd_pair(args):
    root = Path(args.exports).resolve()
    if not root.is_dir():
        sys.exit(f"{root}: not a folder of exports")
    raws = Raws(args.raws)
    overrides = {}
    if args.splits:
        overrides = {str(k).lower(): v for k, v in json.loads(Path(args.splits).read_text()).items()}
        bad = {v for v in overrides.values() if v not in ("tune", "check")}
        if bad:
            sys.exit(f"{args.splits}: splits must be tune or check, not {sorted(bad)}")
    kept, skipped, photos_meta = [], [], {}
    files = sorted(p for p in root.rglob("*") if p.suffix.lower() in (".tif", ".tiff") and p.is_file())
    for path in files:
        try:
            e = read_export(path)
            crs = e.pop("crs")
            e.update(clean_recipe(crs, e.pop("elements")))
            if len(e["moved"]) > 1:
                raise Refused("several Basic sliders moved", ", ".join(e["moved"]))
            raw = os.path.realpath(raws.find(crs.get("RawFileName")))
            sha, embedded = raws.hash(raw)
            if embedded:
                raise Refused("raw carries develop settings",
                              f"{Path(raw).name} embeds {' and '.join(embedded)}: LightCraft could apply develop "
                              f"settings from it over the recipe (use the original raw)")
        except Refused as r:
            skipped.append({"export": str(path), "kind": r.kind, "detail": r.detail})
            continue
        except Exception as e:  # anything else wrong with one export skips that export, with the reason
            skipped.append({"export": str(path), "kind": "unexpected error", "detail": f"{type(e).__name__}: {e}"})
            continue
        e.update(raw=raw, photo=sha[:12], folder=path.parent.relative_to(root).as_posix())
        photos_meta.setdefault(e["photo"], {"name": Path(raw).stem, "raw": raw, "raw_sha256": sha,
                                            "camera": e["camera"]})
        kept.append(e)
    sessions = assign_sessions(kept, args.session_gap)
    pairs, exploratory = make_pairs(kept)
    photos = {}
    for photo, items in pairs.items():
        meta = photos_meta[photo]
        split = next((overrides[k] for k in (photo, meta["raw_sha256"], Path(meta["raw"]).name.lower(),
                                             meta["name"].lower()) if k in overrides), split_of(meta["raw_sha256"]))
        photos[photo] = {**meta, "split": split, "pairs": items}
    counts = Counter(s["kind"] for s in skipped)
    out = {"format": CASES_FORMAT, "tool_sha256": sha256_file(TOOL), "exports_root": str(root),
           "raw_roots": [str(Path(r).resolve()) for r in args.raws],
           "session_rule": f"one export folder; split where export times are more than {args.session_gap} min apart"
                           if args.session_gap else "one export folder",
           "split_rule": "sha256 of the raw file mod 3 == 0: check, else tune" + (
               f"; overrides from {args.splits}" if args.splits else ""),
           "photos": photos, "sessions": sessions, "exploratory": exploratory,
           "skipped": skipped, "skipped_counts": dict(counts)}
    write_json(Path(args.output), out)
    n_pairs = sum(len(p["pairs"]) for p in photos.values())
    kinds = Counter(p["reference"]["kind"] for c in photos.values() for p in c["pairs"])
    print(f"{len(files)} TIFFs under {root}: {len(kept)} usable exports in {len(sessions)} sessions; "
          f"{len(skipped)} skipped")
    for kind, n in counts.most_common():
        detail = Counter(s["detail"] for s in skipped if s["kind"] == kind).most_common(1)[0][0]
        print(f"  {n:5} {kind} (e.g. {detail})")
    print(f"{n_pairs} pairs on {len(photos)} photos ({sum(c['split'] == 'tune' for c in photos.values())} tune, "
          f"{sum(c['split'] == 'check' for c in photos.values())} check); by reference: {dict(kinds)}; "
          f"{len(exploratory)} slider exports without a same-session reference (not scored)")
    per = defaultdict(Counter)
    for c in photos.values():
        for p in c["pairs"]:
            per[p["slider"]][c["split"]] += 1
    for slider, n in sorted(per.items()):
        print(f"  {slider:18} pairs: tune {n['tune']:4}  check {n['check']:4}")
    print(f"wrote {args.output}")


# ---------------------------------------------------------------------------------------------------------------
# plan: Lightroom presets for the recipes
# ---------------------------------------------------------------------------------------------------------------

def crs_number(lc, v):
    if lc == "light.exposure":
        return "0.00" if v == 0 else f"{v:+.2f}"
    return "0" if v == 0 else f"{v:+.0f}"


def preset_xmp(name, group, recipe):
    """A Lightroom / Camera Raw develop preset, written from scratch: every Basic slider set (so applying it fully
    defines the recipe whatever the photo had before), As Shot white balance, sharpening, noise reduction, lens
    corrections and chromatic aberration removal off, linear point curves. No profile, look or process version (the
    photo keeps Lightroom's defaults)."""
    attrs = {"PresetType": "Normal", "Cluster": "",
             "UUID": hashlib.sha256(f"lightcraft-render-fidelity/{name}".encode()).hexdigest()[:32].upper(),
             "SupportsAmount": "False", "SupportsColor": "True", "SupportsMonochrome": "True",
             "SupportsHighDynamicRange": "True", "SupportsNormalDynamicRange": "True",
             "SupportsSceneReferred": "True", "SupportsOutputReferred": "True", "CameraModelRestriction": "",
             "Copyright": "", "ContactInfo": "", "WhiteBalance": "As Shot"}
    for crs_name, lc, *_ in BASIC:
        attrs[crs_name] = crs_number(lc, recipe.get(lc, 0.0))
    attrs.update({"Sharpness": "0", "LuminanceSmoothing": "0", "ColorNoiseReduction": "0", "LensProfileEnable": "0",
                  "AutoLateralCA": "0", "ToneCurveName2012": "Linear", "HasSettings": "True"})
    lines = ['<x:xmpmeta xmlns:x="adobe:ns:meta/">', f' <rdf:RDF xmlns:rdf="{NS_RDF}">',
             '  <rdf:Description rdf:about=""', f'    xmlns:crs="{NS_CRS}"']
    lines += [f"   crs:{k}={quoteattr(v)}" for k, v in attrs.items()]
    lines[-1] += ">"
    for tag, text in (("Name", name), ("Group", group),
                      ("Description", "LightCraft render-fidelity recipe (docs/render-fidelity.md)")):
        lines += [f"   <crs:{tag}>", "    <rdf:Alt>", f'     <rdf:li xml:lang="x-default">{escape(text)}</rdf:li>',
                  "    </rdf:Alt>", f"   </crs:{tag}>"]
    for curve in CURVES:
        lines += [f"   <crs:{curve}>", "    <rdf:Seq>", "     <rdf:li>0, 0</rdf:li>", "     <rdf:li>255, 255</rdf:li>",
                  "    </rdf:Seq>", f"   </crs:{curve}>"]
    lines += ["  </rdf:Description>", " </rdf:RDF>", "</x:xmpmeta>", ""]
    return "\n".join(lines)


def slider_of(name):
    for crs_name, lc, label, *_ in BASIC:
        if name.lower() in (crs_name.lower(), lc.lower(), label.lower()):
            return lc
    raise ValueError(f"unknown Basic slider {name!r} (one of: {', '.join(b[2] for b in BASIC)})")


def plan_recipes(specs):
    """[(name, recipe)] for the Neutral and each SLIDER=A,B,... spec (default: every Basic slider at two amounts)."""
    amounts = {}
    if specs:
        for spec in specs:
            name, _, values = spec.partition("=")
            lc = slider_of(name.strip())
            lo, hi = LC_TO_BASIC[lc][3]
            for v in values.split(","):
                a = float(v)
                if not lo <= a <= hi or a == 0 or (lc != "light.exposure" and a != round(a)):
                    raise ValueError(f"{spec}: {v} is not a non-zero {LC_TO_BASIC[lc][2]} amount in {lo:g}..{hi:g}")
                amounts.setdefault(lc, []).append(a)
    else:
        amounts = {b[1]: list(b[4]) for b in BASIC}
    recipes = [("Neutral", {b[1]: 0.0 for b in BASIC})]
    for lc in (b[1] for b in BASIC):
        for a in sorted(set(amounts.get(lc, []))):
            label = LC_TO_BASIC[lc][2]
            recipes.append((f"{label} {crs_number(lc, a)}", {**{b[1]: 0.0 for b in BASIC}, lc: a}))
    return [(f"{i:02d} {n}", r) for i, (n, r) in enumerate(recipes)]


INSTRUCTIONS = """LightCraft render fidelity: making the Lightroom exports
==========================================================

{n} develop presets are in {presets}/ (group "{group}"): {names}.
Full instructions and why each step matters: {doc}

1. Use a new catalog (File > New Catalog), so nothing touches your own. Preferences > Presets > Raw Defaults:
   Adobe Default. Import the raws with Add and Develop Settings: None.
2. Develop module > Presets panel > + > Import Presets..., and pick the .xmp files in {presets}/.
3. For each photo, in the Develop module: click "{first}". Then for each other preset: Photo > Create Virtual
   Copy (Cmd/Ctrl+'), and click the preset (it sets every Basic slider, so it doesn't matter what the copy had).
   Leave the profile at Lightroom's default (Adobe Color) and don't crop or retouch.
4. In the Library grid select every photo and virtual copy, and export them in ONE export operation:
   - Export To: a new, empty folder (one folder per export session; never add a second export to it)
   - File Naming: any template that gives unique names (e.g. Filename - Sequence)
   - File Settings: TIFF, Compression None, Color Space sRGB, Bit Depth 16 bits/component
   - Image Sizing: off, or Resize to Fit Long Edge (e.g. 2048 px); Output Sharpening: off
   - Metadata: All Metadata (the tool reads the develop settings Lightroom embeds); no watermark
5. Run:  python tools/render_fidelity.py pair --exports EXPORTS_FOLDER --raws RAWS_FOLDER -o cases.json
"""


def cmd_plan(args):
    out = Path(args.out)
    group = "LightCraft fidelity"
    try:
        recipes = plan_recipes(args.recipe)
    except ValueError as e:
        sys.exit(str(e))
    presets = out / "presets"
    presets.mkdir(parents=True, exist_ok=True)
    for name, recipe in recipes:
        (presets / f"{name}.xmp").write_text(preset_xmp(name, group, recipe), encoding="utf-8")
    write_json(out / "recipes.json", {"group": group, "recipes": [{"name": n, "lightcraft": r} for n, r in recipes]})
    (out / "INSTRUCTIONS.txt").write_text(INSTRUCTIONS.format(
        n=len(recipes), presets=presets, group=group, names=", ".join(n for n, _ in recipes), first=recipes[0][0],
        doc=DOC), encoding="utf-8")
    print(f"wrote {len(recipes)} presets to {presets}, {out / 'recipes.json'} and {out / 'INSTRUCTIONS.txt'}")


# ---------------------------------------------------------------------------------------------------------------
# render: LightCraft renders, registration and samples
# ---------------------------------------------------------------------------------------------------------------

Y = np.array([0.2126, 0.7152, 0.0722])
WINDOWS = ((0.15, 0.15), (0.15, 0.85), (0.5, 0.5), (0.85, 0.15), (0.85, 0.85), (0.3, 0.7), (0.7, 0.3))


def grid(long_edge):
    """(stride, search radius, margin) for a long edge: the margin covers the search radius plus two strides, so
    every allowed shift samples inside the image."""
    s = max(2, long_edge // 750)
    r = max(4, long_edge // 375)
    return s, r, r + 2 * s


def decode(a):
    a = np.asarray(a, np.float64) / 65535
    return np.where(a <= 0.04045, a / 12.92, ((a + 0.055) / 1.055) ** 2.4)


def log_y(a):
    return np.log2(np.maximum(decode(a) @ Y, 1e-4))


def read_rgb16(path):
    a = tifffile.imread(path)
    if a.dtype != np.uint16 or a.ndim != 3 or a.shape[2] < 3:
        raise ValueError(f"{path}: {a.dtype} {a.shape}, expected 16-bit RGB")
    return a[..., :3]


def sample(a, long_edge, dy=0, dx=0):
    s, r, m = grid(long_edge)
    if abs(dy) > r or abs(dx) > r:
        raise ValueError(f"shift {dy},{dx} outside the search radius {r}")
    h, w = a.shape[:2]
    out = a[m + dy:h - m + dy:s, m + dx:w - m + dx:s]
    expected = (len(range(m, h - m, s)), len(range(m, w - m, s)))
    if out.shape[:2] != expected:
        raise ValueError(f"sample shape {out.shape[:2]}, expected {expected}")
    return np.ascontiguousarray(out)


def register(lc, lr, long_edge):
    """Shift d such that LightCraft pixel (p + d) shows Lightroom's p, or (None, reason, windows).

    Seven windows; a window counts when it is textured (log-Y std >= 0.15), its best shift is inside the search
    (not on its boundary) and clear (the lowest loss more than two pixels away is at least 2% higher). The shift is
    the median of the N counted windows, accepted when max(4, ceil(2N/3)) of them agree within one pixel in both
    axes and the agreeing ones cover at least three of the four quadrants and the centre."""
    h, w = lr.shape[:2]
    _, r, _ = grid(long_edge)
    half, step = max(64, max(h, w) // 15), max(1, max(h, w) // 1500)
    found = []
    for fy, fx in WINDOWS:
        y, x = int(fy * h), int(fx * w)
        if y - half - r < 0 or x - half - r < 0 or y + half + r > h or x + half + r > w:
            found.append({"at": [y, x], "used": False, "outside": True})
            continue
        ref = log_y(lr[y - half:y + half:step, x - half:x + half:step])
        texture = float(ref.std())
        if texture < 0.15:
            found.append({"at": [y, x], "texture": texture, "used": False})
            continue
        losses = {}
        for dy in range(-r, r + 1):
            for dx in range(-r, r + 1):
                la = log_y(lc[y - half + dy:y + half + dy:step, x - half + dx:x + half + dx:step])
                losses[(dy, dx)] = float(np.abs((la - la.mean()) - (ref - ref.mean())).mean())
        (by, bx), e = min(losses.items(), key=lambda kv: kv[1])
        far = min(v for (a, b), v in losses.items() if max(abs(a - by), abs(b - bx)) > 2)
        found.append({"at": [y, x], "texture": texture, "used": True, "error": e, "dy": by, "dx": bx,
                      "separation": (far - e) / far if far > 0 else 0.0, "boundary": abs(by) == r or abs(bx) == r})
    used = [f for f in found if f["used"] and not f["boundary"] and f["separation"] >= 0.02]
    if len(used) < 4:
        return None, f"only {len(used)} textured windows with a clear best shift inside the search", found
    dy, dx = int(np.median([f["dy"] for f in used])), int(np.median([f["dx"] for f in used]))
    inliers = [f for f in used if abs(f["dy"] - dy) <= 1 and abs(f["dx"] - dx) <= 1]
    need = max(4, -(-2 * len(used) // 3))
    if len(inliers) < need:
        return None, f"{len(inliers)} of {len(used)} windows agree (need {need})", found
    regions = {("c" if 0.25 < f["at"][0] / h < 0.75 and 0.25 < f["at"][1] / w < 0.75 else
                f"{int(f['at'][0] > h / 2)}{int(f['at'][1] > w / 2)}") for f in inliers}
    if len(regions) < 3:
        return None, f"agreeing windows cover only {len(regions)} regions", found
    for f in inliers:
        f["inlier"] = True
    return (dy, dx), None, found


def sha256_file(path, block=1 << 20):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(block), b""):
            h.update(chunk)
    return h.hexdigest()


def run_state(path, missing=None):
    """A run-state JSON object (manifest, registration, failures). Absent: `missing` (None: an error). Damaged (not
    JSON, not an object): stop with a message, since nothing in that run can be trusted."""
    path = Path(path)
    if not path.exists():
        if missing is None:
            sys.exit(f"{path}: missing (run `render` first)")
        return dict(missing)
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as e:
        sys.exit(f"{path} can't be read ({type(e).__name__}: {e}): use a new --run folder")
    if not isinstance(data, dict):
        sys.exit(f"{path} is damaged (not a JSON object): use a new --run folder")
    return data


def write_atomic(path, data):
    """Write bytes through a temporary file in the same folder, fsync it, then os.replace it into place: a reader
    sees the old file or the new one, never a part."""
    path = Path(path)
    tmp = path.with_name(f"{path.name}.{secrets.token_hex(4)}.tmp")
    try:
        with open(tmp, "wb") as f:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)
    finally:
        remove_quietly(tmp)


def write_json(path, data):
    write_atomic(path, (json.dumps(data, indent=1) + "\n").encode("utf-8"))


def recipe_json(recipe):
    """A LightCraft recipe as rendered (with CLEAN), as canonical JSON."""
    return json.dumps({**CLEAN, **recipe}, sort_keys=True)


def provenance(run_id, raw_sha256, long_edge, frame, export=None, recipe=None, shift=None):
    """What a cached sample is: the run (`run_id`: binding id, CLI and tool hashes, the render's own identity or the
    manifest's when scoring), the raw, the size and registered frame, and either the Lightroom export it samples or
    the LightCraft recipe and shift it was rendered and sampled with."""
    p = {"binding": run_id.get("binding_id"), "cli_sha256": run_id.get("cli_sha256"),
         "tool_sha256": run_id.get("tool_sha256"), "sampling_version": SAMPLING_VERSION, "raw_sha256": raw_sha256,
         "long_edge": long_edge, "frame": [int(x) for x in frame]}
    if export is not None:
        p.update(kind="lightroom", export={"path": export["path"], "size": export["size"],
                                           "mtime_ns": export["mtime_ns"]})
    else:
        p.update(kind="lightcraft", recipe=recipe_json(recipe), shift=[int(shift[0]), int(shift[1])])
    return p


def registration_provenance(run_id, raw_sha256, long_edge, reference, recipe):
    """What a registration entry is: the run, the raw, the size, the reference export and LightCraft's recipe."""
    return {"binding": run_id.get("binding_id"), "cli_sha256": run_id.get("cli_sha256"),
            "tool_sha256": run_id.get("tool_sha256"), "sampling_version": SAMPLING_VERSION,
            "raw_sha256": raw_sha256, "long_edge": long_edge,
            "reference": {"path": reference["path"], "size": reference["size"], "mtime_ns": reference["mtime_ns"]},
            "recipe": recipe_json(recipe)}


def save_sample(path, a, prov):
    """A sample and its provenance in one .npz, written atomically."""
    buf = io.BytesIO()
    np.savez(buf, sample=a, provenance=np.array(json.dumps(prov, sort_keys=True)))
    write_atomic(path, buf.getvalue())


def read_sample(path, expected, data=True):
    """(the sample, None) when the file at `path` holds a sample whose provenance is exactly `expected`; else
    (None, why): "missing", unreadable, without provenance, or provenance of another run, CLI, raw or recipe."""
    try:
        z = np.load(path, allow_pickle=False)
    except FileNotFoundError:
        return None, "missing"
    except Exception as e:  # a damaged file
        return None, f"unreadable ({type(e).__name__})"
    if not hasattr(z, "files"):
        return None, "no provenance (an older tool or another program wrote it)"
    with z:
        if "provenance" not in z.files or "sample" not in z.files:
            return None, "no provenance (an older tool or another program wrote it)"
        try:
            prov = json.loads(str(z["provenance"][()]))
        except Exception:
            return None, "damaged provenance"
        if not isinstance(prov, dict):
            return None, "damaged provenance"
        differ = sorted(k for k in set(expected) | set(prov) if prov.get(k) != expected.get(k))
        if differ:
            return None, f"provenance of another run ({', '.join(differ)} differ)"
        if not data:
            return True, None
        try:
            return np.asarray(z["sample"]), None
        except Exception as e:
            return None, f"unreadable ({type(e).__name__})"


def damaged(path, what):
    sys.exit(f"{path} is damaged ({what}): use a new --run folder")


def _whole(v):
    return isinstance(v, int) and not isinstance(v, bool)


def check_manifest(m, path):
    """A manifest's structure, before anything in it is used (damage stops with a message, never a traceback)."""
    version = m.get("sampling_version")
    if not _whole(version):
        damaged(path, "sampling_version isn't a whole number")
    if version != SAMPLING_VERSION:
        sys.exit(f"{Path(path).parent} was sampled by another version of the tool (sampling {version}, this tool "
                 f"{SAMPLING_VERSION}): render into a new --run folder")
    for k in ("cli_sha256", "cases_sha256", "tool_sha256", "binding_id"):
        if not (isinstance(m.get(k), str) and m[k]):
            damaged(path, f"{k} is missing or not text")
    if "raw_check" in m and not isinstance(m["raw_check"], str):
        damaged(path, "raw_check isn't text")
    checks = m.get("importer_checks", {})
    if not isinstance(checks, dict):
        damaged(path, "importer_checks isn't an object")
    for k, c in checks.items():
        if not (isinstance(c, dict) and isinstance(c.get("clean"), bool) and isinstance(c.get("changed"), list)
                and all(isinstance(x, str) for x in c["changed"])):
            damaged(path, "an importer check isn't {clean: true|false, changed: [field, ...]}")


def check_registration(reg, path):
    for k, e in reg.items():
        if not isinstance(e, dict):
            damaged(path, "a registration entry isn't an object")
        if "provenance" in e and not isinstance(e["provenance"], dict):
            damaged(path, "a registration entry's provenance isn't an object")
        if "rejected" in e:
            if not (isinstance(e["rejected"], str) and e["rejected"].strip()):
                damaged(path, "a rejection reason is empty or not text")
            continue
        if not (_whole(e.get("dy")) and _whole(e.get("dx"))):
            damaged(path, "a registration shift isn't a whole number")
        shape = e.get("shape")
        if not (isinstance(shape, list) and len(shape) == 2 and all(_whole(x) and x > 0 for x in shape)):
            damaged(path, "a registered frame isn't [height, width]")
        rejected = e.get("pairs_rejected")
        if not (isinstance(rejected, dict) and all(isinstance(x, str) and x.strip() for x in rejected.values())):
            damaged(path, "a registration entry's rejected pairs aren't {pair: reason}")


def check_failures(failures, path):
    if not all(isinstance(v, str) and v.strip() for v in failures.values()):
        damaged(path, "a failure reason is empty or not text")


def lc_key(recipe, shift=(0, 0)):
    """A LightCraft sample's name: the rendered settings and the registration shift it was sampled with."""
    h = hashlib.sha256(json.dumps({**CLEAN, **recipe}, sort_keys=True).encode()).hexdigest()[:16]
    return f"{h}@{shift[0]},{shift[1]}"


def lr_key(path):
    return hashlib.sha256(str(path).encode()).hexdigest()[:16]


IMPORTER_STEPS = ("develop.get", "develop.reset", "develop.get")


def importer_results(returncode, stdout):
    """(imported, reset) develop settings from `run --import RAW develop.get develop.reset develop.get`, checked
    structurally: exit status 0, UTF-8, exactly three JSON objects for those three commands with "ok": true, and the
    two develop.get results non-empty objects. ValueError says what is wrong."""
    if returncode != 0:
        raise ValueError(f"exit status {returncode}")
    try:
        text = stdout.decode("utf-8")
    except UnicodeDecodeError:
        raise ValueError("output that isn't UTF-8") from None
    lines = [x for x in text.splitlines() if x.strip()]
    if len(lines) != len(IMPORTER_STEPS):
        raise ValueError(f"{len(lines)} result lines, not {len(IMPORTER_STEPS)}")
    results = []
    for line, step in zip(lines, IMPORTER_STEPS):
        try:
            obj = json.loads(line)
        except ValueError:
            raise ValueError(f"a result line that isn't JSON ({line[:60]!r})") from None
        if not isinstance(obj, dict):
            raise ValueError(f"a result that is a JSON {type(obj).__name__}, not an object")
        if obj.get("command") != step:
            raise ValueError(f"a result for {obj.get('command')!r} where {step} was expected")
        if obj.get("ok") is not True:
            raise ValueError(f"{step} failed ({str(obj.get('error'))[:200]})")
        results.append(obj.get("result"))
    imported, reset = results[0], results[2]
    if not (isinstance(imported, dict) and imported and isinstance(reset, dict) and reset):
        raise ValueError("develop.get didn't return develop settings")
    return imported, reset


def remove_quietly(path):
    """Delete a temporary file. A problem doing so is a warning: it never replaces the error being raised."""
    try:
        Path(path).unlink(missing_ok=True)
    except OSError as e:
        print(f"warning: couldn't remove {path} ({type(e).__name__}: {e})", file=sys.stderr)


def shown(output):
    """A process's output as text to show or record: bytes that aren't UTF-8 replaced, never an error."""
    return output.decode("utf-8", "replace").strip() if isinstance(output, bytes) else str(output or "").strip()


class RenderFailed(Exception):
    """A render or read that may work next time (a LightCraft error, a missing disk): a resumed run retries it."""


# Default limits on one lightcraft-cli call, generous for a 100 MP raw rendered at full size on a slow machine:
# a render (`render --timeout`), and the version query or an importer check, which read no pixels (`--check-timeout`)
RENDER_TIMEOUT, CHECK_TIMEOUT = 900.0, 180.0
MAX_TIMEOUT = 86400.0
VERSION_TIMEOUT = 15.0  # `--version` may be the first run of a freshly copied binary: its own start-up allowance


def seconds(value):
    """A time limit in seconds: finite, positive and at most a day (argparse type; ValueError otherwise)."""
    try:
        s = float(value)
    except (TypeError, ValueError):
        raise ValueError(f"{value!r} is not a number of seconds") from None
    if not (s == s and 0 < s <= MAX_TIMEOUT):  # s == s is false for nan
        raise ValueError(f"{value!r}: a time limit must be more than 0 and at most {MAX_TIMEOUT:g} s (a day)")
    return s


def _seconds_arg(value):
    try:
        return seconds(value)
    except ValueError as e:
        raise argparse.ArgumentTypeError(str(e)) from None


@contextlib.contextmanager
def run_lock(run):
    """Hold RUN/.lock exclusively (fcntl.flock on POSIX, msvcrt.locking on Windows) for as long as a command reads
    or writes the run's state; refuse at once, with a message, when another process holds it."""
    run.mkdir(parents=True, exist_ok=True)
    if os.environ.get("RENDER_FIDELITY_LOCK") == "off":  # testing only: a share whose locks don't exclude
        yield
        return
    path = run / ".lock"
    f = open(path, "a+b")
    try:
        try:
            if os.name == "nt":
                import msvcrt
                if f.seek(0, 2) == 0:
                    f.write(b"\0")
                    f.flush()
                f.seek(0)
                msvcrt.locking(f.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(f.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            sys.exit(f"{run} is in use by another `render` or `score` (it holds {path}): wait for it to finish, or "
                     f"use another --run folder")
        yield
    finally:
        f.close()  # releases the lock


class CliRenderer:
    """Renders with a lightcraft-cli binary: 16-bit sRGB TIFF at a long edge, built-in camera profiles only, from a
    sidecar-free symlink to the raw (sidecars carry crops and other edits that LightCraft would import). Every call
    has a time limit, and every failure to run it (a launch error, a timeout, a non-zero exit, unreadable output) is
    a RenderFailed: recorded for that photo and retried when the run is resumed."""

    def __init__(self, cli, tmp, timeout=RENDER_TIMEOUT, check_timeout=CHECK_TIMEOUT):
        self.cli, self.tmp = str(Path(cli).resolve()), Path(tmp)
        self.timeout, self.check_timeout = seconds(timeout), seconds(check_timeout)
        profiles = self.tmp / "no-local-profiles"
        profiles.mkdir(exist_ok=True)
        self.env = dict(os.environ, LIGHTCRAFT_CAMERA_PROFILES=str(profiles))
        self.links = {}

    def link(self, raw):
        real = os.path.realpath(raw)
        if real not in self.links:
            try:
                d = self.tmp / f"raw{len(self.links)}"
                d.mkdir()
                (d / Path(real).name).symlink_to(real)
            except OSError as e:
                raise RenderFailed(f"can't link the raw for LightCraft ({type(e).__name__}: {e})") from None
            self.links[real] = d / Path(real).name
        return self.links[real]

    def call(self, cmd, timeout, what):
        """The finished process, its output as bytes (decoded by the caller: strictly where it is data, with
        replacement where it is only shown); RenderFailed when it can't be run or doesn't finish in time."""
        try:
            return subprocess.run(cmd, env=self.env, capture_output=True, timeout=timeout)
        except subprocess.TimeoutExpired:
            raise RenderFailed(f"{what}: {' '.join(cmd)}\ntimed out after {timeout:g} s (a slower machine needs a larger "
                               f"`render --timeout` / `--check-timeout`)") from None
        except (OSError, subprocess.SubprocessError) as e:
            raise RenderFailed(f"{what}: {' '.join(cmd)}\ncan't run {self.cli} ({type(e).__name__}: {e})") from None

    def inspect(self, raw):
        """The develop settings LightCraft's importer gave the raw (sidecar-free, as rendered) beyond its defaults:
        the fields that differ between the import and a reset (an import in a fresh session applies only camera
        defaults, so a difference is settings from the file's own XMP). [] when clean."""
        cmd = [self.cli, "run", "--import", str(self.link(raw)), "develop.get", "develop.reset", "develop.get"]
        r = self.call(cmd, self.check_timeout, "importer check")
        try:
            imported, reset = importer_results(r.returncode, r.stdout)
        except ValueError as e:  # the problem goes on the last line: that is what score shows
            detail = (shown(r.stderr) or shown(r.stdout))[-1500:]
            raise RenderFailed(f"importer check: {' '.join(cmd)}\n" + (f"{detail}\n" if detail else "")
                               + f"importer check: {e}") from None

        def flat(d, prefix=""):
            out = {}
            for k, v in d.items():
                out.update(flat(v, f"{prefix}{k}.") if isinstance(v, dict) else {f"{prefix}{k}": v})
            return out
        a, b = flat(imported), flat(reset)
        return sorted(k for k in set(a) | set(b) if a.get(k) != b.get(k))

    def __call__(self, raw, long_edge, recipe):
        # a fresh output path per call, removed whatever happens: a render that wrote its file and then failed or
        # timed out can never be read as a later render's output (its size is checked against the registered frame)
        out = self.tmp / f"render-{secrets.token_hex(8)}.tif"
        sets = [a for k, v in {**CLEAN, **recipe}.items() for a in ("--set", f"{k}={v:g}")]
        cmd = [self.cli, "render", str(self.link(raw)), "-o", str(out), "--size", str(long_edge),
               "--opt", "bitDepth=16", "--opt", "colorSpace=srgb", *sets]
        try:
            r = self.call(cmd, self.timeout, "render")
            if r.returncode != 0:
                said = shown(r.stderr)[-2000:]
                raise RenderFailed(f"{' '.join(cmd)}\n" + (f"{said}\n" if said else "")
                                   + f"render: exit status {r.returncode}"
                                   + (f" ({said.splitlines()[-1][:160]})" if said else ""))
            if not out.is_file():
                raise RenderFailed(f"{' '.join(cmd)}\nrender: exited 0 but " + (
                    "put something that isn't a file at the output path" if os.path.lexists(out) else "wrote no output"))
            try:
                return read_rgb16(out)
            except Exception as e:  # the CLI wrote something that isn't a 16-bit RGB TIFF
                raise RenderFailed(f"{' '.join(cmd)}\nunreadable render: {e}") from None
        finally:
            remove_quietly(out)


def run_binding(cases_path, cli_sha256, cli_version):
    """What a run's cached samples depend on; a run directory is only resumed when this matches."""
    env = {k: v for k, v in sorted(os.environ.items())
           if k.startswith("LIGHTCRAFT_") and k != "LIGHTCRAFT_CAMERA_PROFILES"}
    return {"format": RUN_FORMAT, "sampling_version": SAMPLING_VERSION, "tool_sha256": sha256_file(TOOL),
            "cli_sha256": cli_sha256, "cli_version": cli_version, "cases_sha256": sha256_file(cases_path),
            "clean": CLEAN, "camera_profiles": "built-in only (LIGHTCRAFT_CAMERA_PROFILES is an empty folder)",
            "sidecars": "rendered from a sidecar-free symlink to the raw",
            "output": "16-bit sRGB TIFF at the export's long edge", "environment": env,
            "raw_check": "each raw checked by the CLI's own importer (run --import, develop.get, develop.reset, "
                         "develop.get) before it is rendered",
            "grid": "stride max(2, L//750), search radius max(4, L//375), margin radius + 2 strides"}


def binding_id(binding):
    """The id every cached sample and registration of a run carries (sha256 of its binding)."""
    return hashlib.sha256(json.dumps(binding, sort_keys=True).encode()).hexdigest()


def create_manifest(path, data):
    """Create `path` with `data` only if it doesn't exist (an fsynced temporary file hard-linked into place, else an
    O_EXCL create): True when this call made it, False when it already existed. Never overwrites."""
    path = Path(path)
    tmp = path.with_name(f"{path.name}.{secrets.token_hex(4)}.tmp")
    try:
        with open(tmp, "xb") as f:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
        try:
            os.link(tmp, path)
            return True
        except FileExistsError:
            return False
        except OSError:  # a filesystem without hard links: an exclusive create instead
            try:
                fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o644)
            except FileExistsError:
                return False
            with os.fdopen(fd, "wb") as f:
                f.write(data)
                f.flush()
                os.fsync(f.fileno())
            return True
    finally:
        remove_quietly(tmp)


RUN_KEYS = ("binding_id", "cli_sha256", "tool_sha256")


def run_identity(binding):
    """The identity a render works under, fixed when it starts: every sample, registration and importer check it
    writes carries it, and the run's manifest must keep matching it."""
    return {"binding_id": binding_id(binding), "cli_sha256": binding["cli_sha256"], "tool_sha256": binding["tool_sha256"]}


def changed_under_us(run):
    sys.exit(f"another render changed this run folder ({run}: its manifest is another run's now); use a new --run "
             f"folder")


RUN_STATE = ("registration.json", "failures.json", "lr", "lc", "scores.json", "summary.txt", "report-salt.txt")


def complete_json(path):
    """Whether `path` holds a whole JSON object (not empty, not cut short)."""
    try:
        return isinstance(json.loads(Path(path).read_text(encoding="utf-8")), dict)
    except (OSError, ValueError):
        return False


def open_run(run, binding, extra):
    """Create the run's manifest, or resume a run made with the same binding. Returns the run's identity."""
    run.mkdir(parents=True, exist_ok=True)
    path = run / "manifest.json"
    ident = run_identity(binding)
    data = (json.dumps({**binding, "binding_id": ident["binding_id"], **extra}, indent=1) + "\n").encode()
    if path.exists() and not any((run / n).exists() for n in RUN_STATE) and not complete_json(path):
        # an initial creation that was interrupted (empty or partial, e.g. killed between the O_EXCL create and its
        # write) and that nothing references: replaced atomically. `render` holds the run's lock here.
        write_atomic(path, data)
        return ident
    if not path.exists():
        if any((run / n).exists() for n in ("registration.json", "lr", "lc", "scores.json")):
            sys.exit(f"{run} has samples but no manifest.json, so what made them can't be checked: use a new --run "
                     f"folder")
        if create_manifest(path, data):
            return ident
        old = run_state(path, {})  # another render created it first
        if any(old.get(k) != v for k, v in ident.items()):
            changed_under_us(run)
        return ident
    old = run_state(path, {})
    diff = {k: (old.get(k), v) for k, v in binding.items() if old.get(k) != v}
    if not diff and old.get("binding_id") != ident["binding_id"]:
        diff = {"binding_id": (old.get("binding_id"), ident["binding_id"])}
    if diff:
        sys.exit(f"{run} was made with different inputs, not resuming (use a new --run folder):\n"
                 + json.dumps(diff, indent=1))
    return ident


def selected_photos(cases, split=None, photos=None):
    names = set(photos.split(",")) if photos else None
    out = []
    for pid, c in sorted(cases["photos"].items()):
        if split and c["split"] != split:
            continue
        if names is not None and not (pid in names or c["name"] in names):
            continue
        out.append(pid)
    return out


def read_export_pixels(ref, role):
    """The export's 16-bit RGB pixels, or Refused (it can't be used in this run: changed, damaged), or RenderFailed
    (it may be readable next time: a missing disk). Reasons name the export by role, never by path."""
    try:
        st = os.stat(ref["path"])
        if st.st_size != ref["size"] or st.st_mtime_ns != ref["mtime_ns"]:
            raise Refused("changed", f"the Lightroom {role} export changed since `pair` (run pair again)")
        return read_rgb16(ref["path"])
    except (OSError, MemoryError) as e:
        raise RenderFailed(f"the Lightroom {role} export can't be read now ({type(e).__name__})") from None
    except Refused:
        raise
    except Exception as e:  # a damaged file: any decoder error
        raise Refused("unreadable", f"the Lightroom {role} export can't be decoded ({type(e).__name__})") from None


def registration_key(photo, session, long_edge, reference_path):
    """Registration is kept per photo, export session, size and reference export (JSON: no field can run into
    another)."""
    return json.dumps([photo, session, long_edge, lr_key(reference_path)])


def render_cases(cases, run, identity, renderer, photo_ids, sliders=None, log=print):
    """Render and sample every selected pair into `run` (resumable). LightCraft's render of each reference recipe is
    registered on that Lightroom reference export, and the pair's recipe is sampled with the same shift. Rejections
    (registration, sizes, changed or damaged exports) are recorded in RUN/registration.json; failures that may pass
    next time (LightCraft errors, unreadable files) in RUN/failures.json, retried when the run is resumed."""
    state_path, fail_path = run / "registration.json", run / "failures.json"
    state, failures = run_state(state_path, {}), run_state(fail_path, {})
    manifest_path = run / "manifest.json"
    identity = dict(identity)  # the caller's, fixed: provenance never comes from a reloaded manifest

    def current_manifest():
        """The manifest as it is now, which must still be this render's."""
        m = run_state(manifest_path)
        check_manifest(m, manifest_path)
        if any(m.get(k) != identity[k] for k in RUN_KEYS):
            changed_under_us(run)
        return m

    def record_check(key, entry):
        m = current_manifest()
        m.setdefault("importer_checks", {})[key] = entry
        write_json(manifest_path, m)

    current_manifest()
    check_registration(state, state_path)
    check_failures(failures, fail_path)
    inspect = renderer.inspect  # every renderer checks raws with its importer (the self-test's stand-ins too)
    started, renders = time.time(), 0

    # every record this render writes (samples, registrations, failures, importer checks) is written only while the
    # manifest still names it, and it checks once more before returning. That stop is best-effort (another process
    # can still change the folder between a check and the write); the guarantee is each record's provenance, which
    # `score` checks against the manifest.
    def keep(path, a, prov):
        current_manifest()
        save_sample(path, a, prov)

    def save_state():
        current_manifest()
        write_json(state_path, state)

    def fail(where, why):
        current_manifest()
        failures[where] = str(why)
        write_json(fail_path, failures)

    def clear(where):
        if failures.pop(where, None) is not None:
            current_manifest()
            write_json(fail_path, failures)

    def render_photo(n, photo):
        nonlocal renders
        c = cases["photos"][photo]
        try:
            sha = hash_raw(os.path.realpath(c["raw"]))[0]
        except OSError as e:
            fail(photo, f"the raw can't be read now ({e.strerror or e})")
            log(f"[{n}/{len(photo_ids)}] {photo} {c['name']}: {failures[photo]}; skipped")
            return
        if sha != c["raw_sha256"]:
            fail(photo, "the raw is not the file `pair` hashed (run pair again)")
            log(f"[{n}/{len(photo_ids)}] {photo} {c['name']}: {failures[photo]}; skipped")
            return
        key = f"{c['raw_sha256']}:{identity['cli_sha256']}"
        checks = current_manifest().get("importer_checks", {})
        if key not in checks:
            try:
                changed = inspect(c["raw"])
            except RenderFailed as e:
                fail(photo, e)
                log(f"[{n}/{len(photo_ids)}] {photo} {c['name']}: {str(e).splitlines()[-1][:160]}; skipped (retried "
                    f"on resume)")
                return
            checks[key] = {"clean": not changed, "changed": changed}
            record_check(key, checks[key])
        if not checks[key]["clean"]:
            log(f"[{n}/{len(photo_ids)}] {photo} {c['name']}: refused, its import changes LightCraft's develop "
                f"settings ({', '.join(checks[key]['changed'][:6])})")
            return
        clear(photo)
        groups = defaultdict(list)
        for p in c["pairs"]:
            if sliders is None or p["slider"] in sliders:
                groups[(p["session"], p["long_edge"])].append(p)
        notes = []
        for (session, le), pairs in sorted(groups.items()):
            lr_dir, lc_dir = run / "lr" / photo / str(le), run / "lc" / photo / str(le)
            lr_dir.mkdir(parents=True, exist_ok=True)
            lc_dir.mkdir(parents=True, exist_ok=True)
            by_ref = defaultdict(list)
            for p in pairs:
                by_ref[p["reference"]["export"]["path"]].append(p)
            ok = failed = refused = 0
            shifts, problems = Counter(), Counter()
            # every reference export is registered on LightCraft's render of its own recipe, so no export is
            # sampled in a frame that was registered on another one (a rotated virtual copy keeps its size)
            for ref_path, ref_pairs in sorted(by_ref.items()):
                rid = registration_key(photo, session, le, ref_path)
                on = ref_pairs[0]
                reg_prov = registration_provenance(identity, c["raw_sha256"], le, on["reference"]["export"],
                                                   on["lightcraft"]["reference"])
                if rid not in state or state[rid].get("provenance") != reg_prov:  # absent, or another run's
                    try:
                        lr_full = read_export_pixels(on["reference"]["export"], "reference")
                        renders += 1
                        lc_full = renderer(c["raw"], le, on["lightcraft"]["reference"])
                    except RenderFailed as e:
                        fail(rid, e)
                        failed += len(ref_pairs)
                        problems[str(e).splitlines()[-1][:160]] += 1
                        continue
                    except Refused as r:
                        state[rid] = {"rejected": r.detail, "file": ref_path, "provenance": reg_prov}
                    else:
                        if lc_full.shape[:2] != lr_full.shape[:2] or list(lr_full.shape[:2]) != on["shape"]:
                            state[rid] = {"rejected": f"LightCraft renders {list(lc_full.shape[:2])}, Lightroom "
                                                      f"exported {list(lr_full.shape[:2])}" +
                                                      ("; for a one-pixel resize mismatch, see docs/render-fidelity.md "
                                                       "(LightCraft 0.7.1)" if max(abs(a - b) for a, b in
                                                       zip(lc_full.shape[:2], lr_full.shape[:2])) == 1 else ""),
                                          "provenance": reg_prov}
                        else:
                            shift, why, windows = register(lc_full, lr_full, le)
                            if shift is None:
                                state[rid] = {"rejected": f"reference not registered: {why}", "windows": windows,
                                              "provenance": reg_prov}
                            else:
                                frame = list(lr_full.shape[:2])
                                state[rid] = {"dy": shift[0], "dx": shift[1], "shape": frame,
                                              "lightroom": ref_path, "lightcraft": on["lightcraft"]["reference"],
                                              "windows": windows, "pairs_rejected": {}, "provenance": reg_prov}
                                keep(lc_dir / f"{lc_key(on['lightcraft']['reference'], shift)}.npz",
                                            sample(lc_full, le, *shift),
                                            provenance(identity, c["raw_sha256"], le, frame,
                                                       recipe=on["lightcraft"]["reference"], shift=shift))
                                keep(lr_dir / f"{lr_key(ref_path)}.npz", sample(lr_full, le),
                                            provenance(identity, c["raw_sha256"], le, frame,
                                                       export=on["reference"]["export"]))
                        del lr_full, lc_full
                    clear(rid)
                    save_state()
                reg = state[rid]
                if "rejected" in reg:
                    refused += len(ref_pairs)
                    problems[reg["rejected"]] += 1
                    continue
                shift = (reg["dy"], reg["dx"])
                # the reference's own samples are rebuilt when missing (deleted after damage, as `score` advises)
                # or written by another run, in the recorded frame and with the recorded shift
                try:
                    target = lr_dir / f"{lr_key(ref_path)}.npz"
                    expected = provenance(identity, c["raw_sha256"], le, reg["shape"], export=on["reference"]["export"])
                    if read_sample(target, expected, data=False)[1] is not None:
                        a = read_export_pixels(on["reference"]["export"], "reference")
                        if list(a.shape[:2]) != reg["shape"]:
                            raise Refused("size", f"Lightroom reference export {list(a.shape[:2])}, registered "
                                                  f"frame {reg['shape']}")
                        keep(target, sample(a, le), expected)
                    target = lc_dir / f"{lc_key(on['lightcraft']['reference'], shift)}.npz"
                    expected = provenance(identity, c["raw_sha256"], le, reg["shape"],
                                          recipe=on["lightcraft"]["reference"], shift=shift)
                    if read_sample(target, expected, data=False)[1] is not None:
                        renders += 1
                        a = renderer(c["raw"], le, on["lightcraft"]["reference"])
                        if list(a.shape[:2]) != reg["shape"]:
                            raise Refused("size", f"LightCraft renders {list(a.shape[:2])}, registered frame "
                                                  f"{reg['shape']}")
                        keep(target, sample(a, le, *shift), expected)
                except RenderFailed as e:
                    fail(rid, e)
                    failed += len(ref_pairs)
                    problems[str(e).splitlines()[-1][:160]] += 1
                    continue
                except Refused as r:
                    reg["rejected"] = r.detail
                    save_state()
                    refused += len(ref_pairs)
                    problems[r.detail] += 1
                    continue
                clear(rid)
                shifts[shift] += 1
                for p in ref_pairs:
                    if p["id"] in reg["pairs_rejected"]:
                        refused += 1
                        continue
                    where = f"{photo}/{p['id']}"
                    try:
                        target = lr_dir / f"{lr_key(p['export']['path'])}.npz"
                        expected = provenance(identity, c["raw_sha256"], le, reg["shape"], export=p["export"])
                        if read_sample(target, expected, data=False)[1] is not None:
                            a = read_export_pixels(p["export"], "recipe")
                            if list(a.shape[:2]) != reg["shape"]:
                                raise Refused("size", f"Lightroom recipe export {list(a.shape[:2])}, registered "
                                                      f"frame {reg['shape']}")
                            keep(target, sample(a, le), expected)
                        target = lc_dir / f"{lc_key(p['lightcraft']['recipe'], shift)}.npz"
                        expected = provenance(identity, c["raw_sha256"], le, reg["shape"],
                                              recipe=p["lightcraft"]["recipe"], shift=shift)
                        if read_sample(target, expected, data=False)[1] is not None:
                            renders += 1
                            a = renderer(c["raw"], le, p["lightcraft"]["recipe"])
                            if list(a.shape[:2]) != reg["shape"]:
                                raise Refused("size", f"LightCraft renders {list(a.shape[:2])}, registered frame "
                                                      f"{reg['shape']}")
                            keep(target, sample(a, le, *shift), expected)
                    except RenderFailed as e:
                        fail(where, e)
                        failed += 1
                        continue
                    except Refused as r:
                        reg["pairs_rejected"][p["id"]] = r.detail
                        save_state()
                        clear(where)
                        refused += 1
                        continue
                    clear(where)
                    ok += 1
            note = (f"{le} px {session_label(session)}: {ok} of {len(pairs)} pairs on {len(by_ref)} references, shifts "
                    + (", ".join(f"{dy},{dx}" + (f" x{k}" if k > 1 else "") for (dy, dx), k in sorted(shifts.items()))
                       or "none"))
            if refused or failed:
                note += (f"; {refused} rejected, {failed} failed (retried on resume): "
                         + "; ".join(f"{why} x{k}" if k > 1 else why for why, k in problems.most_common(3)))
            notes.append(note)
        log(f"[{n}/{len(photo_ids)}] {photo} {c['name']} ({c['split']}): {'; '.join(notes)}; {renders} renders, "
            f"{time.time() - started:.0f} s")
    for n, photo in enumerate(photo_ids, 1):
        try:
            render_photo(n, photo)
        except Exception as e:  # anything else that goes wrong is this photo's failure, not the run's
            fail(photo, f"{type(e).__name__}: {e}")
            log(f"[{n}/{len(photo_ids)}] {photo}: {type(e).__name__}: {e}; skipped (retried on resume)")
    current_manifest()  # still this render's at the end
    return state

def cmd_render(args):
    cases = load_cases(args.cases)
    # resolved once: a bare name such as `lightcraft-cli` or `./lightcraft-cli` (which Path shortens to a bare
    # name) would otherwise make subprocess search PATH and could query or run another binary
    cli = Path(args.cli).expanduser().resolve()
    if not cli.is_file():
        sys.exit(f"{args.cli}: no such lightcraft-cli")
    timeout, check_timeout = seconds(args.timeout), seconds(args.check_timeout)
    run = Path(args.run)
    with run_lock(run):  # before the manifest is read or made, for the whole render
        tmp = Path(tempfile.mkdtemp(prefix="render-fidelity-"))
        try:
            # the binary measured is a private copy, hashed, queried and run for the whole render: replacing the
            # original during the run changes nothing. There is no fallback to running the original in place.
            copy = Path(tmp) / "cli" / cli.name
            advice = (f"render measures a private copy of the CLI; copy it to a local folder (for example "
                      f"`cp {cli} ~/lightcraft-cli`) and pass that copy with --cli")
            try:
                copy.parent.mkdir(mode=0o700)
                shutil.copy2(cli, copy)
                os.chmod(copy, 0o700)
            except OSError as e:
                sys.exit(f"{cli}: can't make a private copy of it ({type(e).__name__}: {e}): {advice}")
            version, problem = cli_version(copy, max(VERSION_TIMEOUT, check_timeout))
            if problem is not None:
                sys.exit(f"{cli}: its private copy doesn't run ({problem}): {advice}")
            ident = open_run(run, run_binding(args.cases, sha256_file(copy), version),
                             {"cli": str(cli), "measured": "a private copy", "cases": str(Path(args.cases).resolve())})
            photos = selected_photos(cases, args.split, args.photos)
            sliders = set(args.sliders.split(",")) if args.sliders else None
            render_cases(cases, run, ident, CliRenderer(copy, tmp, timeout, check_timeout), photos, sliders)
        finally:
            try:
                shutil.rmtree(tmp)
            except OSError as e:
                print(f"warning: couldn't remove the temporary folder {tmp} ({e})", file=sys.stderr)


def cli_version(cli, timeout):
    """(its --version line, None), or (None, why it can't be run)."""
    try:
        r = subprocess.run([str(cli), "--version"], capture_output=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return None, f"--version didn't answer within {timeout:g} s (`--check-timeout`)"
    except (OSError, subprocess.SubprocessError) as e:
        return None, f"can't run it ({type(e).__name__}: {e})"
    if r.returncode != 0:
        return None, f"--version failed (exit status {r.returncode}): {shown(r.stderr)[-500:]}"
    return shown(r.stdout), None  # only recorded: bytes that aren't UTF-8 are replaced, never fatal


def load_cases(path):
    data = json.loads(Path(path).read_text())
    if data.get("format") != CASES_FORMAT:
        sys.exit(f"{path}: not a cases file from `render_fidelity.py pair` ({data.get('format')!r})")
    return data


# ---------------------------------------------------------------------------------------------------------------
# score
# ---------------------------------------------------------------------------------------------------------------

CLIP, BLACK, FLOOR = 65400, 0.0005, 0.5 / 65535 / 12.92
BANDS = [(0.0, BLACK, "black"), (BLACK, 0.03, "<.03"), (0.03, 0.10, ".03-.10"), (0.10, 0.25, ".10-.25"),
         (0.25, 0.50, ".25-.50"), (0.50, 0.90, ".50-.90"), (0.90, 1.01, ">.90")]
SHOWN = [b[2] for b in BANDS[1:]]
SRGB_TO_XYZ = np.array([[0.4124564, 0.3575761, 0.1804375], [0.2126729, 0.7151522, 0.0721750],
                        [0.0193339, 0.1191920, 0.9503041]])
MIN = 300  # positions a band (and its support) needs before it is reported


def linear(a):
    c = np.asarray(a, np.float64) / 65535
    return np.where(c <= 0.04045, c / 12.92, ((c + 0.055) / 1.055) ** 2.4)


def lab(a):
    xyz = linear(a) @ SRGB_TO_XYZ.T / np.array([0.95047, 1.0, 1.08883])
    f = np.where(xyz > (6 / 29) ** 3, np.cbrt(xyz), xyz / (3 * (6 / 29) ** 2) + 4 / 29)
    return np.stack([116 * f[..., 1] - 16, 500 * (f[..., 0] - f[..., 1]), 200 * (f[..., 1] - f[..., 2])], -1)


def transitions(ref, rec, y_ref, y_rec, m):
    """Each engine against its own reference: newly clipped / recovered, newly black / lifted from black."""
    c_ref, c_rec = ref.max(-1) >= CLIP, rec.max(-1) >= CLIP
    b_ref, b_rec = y_ref < BLACK, y_rec < BLACK

    def share(num, den):
        d = int((den & m).sum())
        return {"share": float((num & den & m).sum() / d) if d else None, "of": d}

    return {"clip_new": share(c_rec, ~c_ref), "recovered": share(~c_rec, c_ref), "black_new": share(b_rec, ~b_ref),
            "black_recovered": share(~b_rec, b_ref)}


def cells(lr_ref, lr_rec, lc_ref, lc_rec):
    """Per band of Lightroom's reference luminance: the two engines' interval responses and their agreement."""
    y = {k: linear(v) @ Y for k, v in (("lr_ref", lr_ref), ("lr_rec", lr_rec), ("lc_ref", lc_ref), ("lc_rec", lc_rec))}
    support = ((lr_ref.max(-1) < CLIP) & (lr_rec.max(-1) < CLIP) & (y["lr_ref"] >= BLACK) & (y["lr_rec"] >= BLACK))
    d_lr = np.log2(np.maximum(y["lr_rec"], FLOOR)) - np.log2(np.maximum(y["lr_ref"], FLOOR))
    d_lc = np.log2(np.maximum(y["lc_rec"], FLOOR)) - np.log2(np.maximum(y["lc_ref"], FLOOR))
    L = {k: lab(v) for k, v in (("lr_ref", lr_ref), ("lr_rec", lr_rec), ("lc_ref", lc_ref), ("lc_rec", lc_rec))}
    de = np.sqrt(((L["lc_rec"] - L["lr_rec"]) ** 2).sum(-1))
    dab_lr, dab_lc = L["lr_rec"][..., 1:] - L["lr_ref"][..., 1:], L["lc_rec"][..., 1:] - L["lc_ref"][..., 1:]
    colour = np.sqrt(((dab_lc - dab_lr) ** 2).sum(-1))
    lc_clipped, lc_black = lc_rec.max(-1) >= CLIP, y["lc_rec"] < BLACK
    enc_err = np.abs(lc_rec.astype(np.float64) - lr_rec.astype(np.float64)).mean(-1) / 65535
    out = {}
    for lo, hi, name in [*BANDS, (0.0, 1.01, "all")]:
        band = (y["lr_ref"] >= lo) & (y["lr_ref"] < hi)
        if band.sum() < MIN:
            continue
        m = support & band
        outside = band & ~support
        cell = {"population": int(band.sum()), "support": int(m.sum()), "retained": float(m.sum() / band.sum()),
                "outside": {"positions": int(outside.sum()),
                            "dE": float(de[outside].mean()) if outside.any() else None,
                            "encoded_error": float(enc_err[outside].mean()) if outside.any() else None},
                "lr_transitions": transitions(lr_ref, lr_rec, y["lr_ref"], y["lr_rec"], band),
                "lc_transitions": transitions(lc_ref, lc_rec, y["lc_ref"], y["lc_rec"], band)}
        if m.sum() >= MIN:
            lr, lc = float(d_lr[m].mean()), float(d_lc[m].mean())
            cell.update({"lr": lr, "lc": lc, "bias": lc - lr, "share": lc / lr if abs(lr) >= 0.05 else None,
                         "error": float(np.abs(d_lc[m] - d_lr[m]).mean()), "dE": float(de[m].mean()),
                         "colour": float(colour[m].mean()), "lc_clipped_on_support": int((lc_clipped & m).sum()),
                         "lc_black_on_support": int((lc_black & m).sum())})
        out[name] = cell
    return out


def neutral_look(lc_n, lr_n):
    a, b = lab(lc_n), lab(lr_n)
    de = np.sqrt(((a - b) ** 2).sum(-1))
    bright = b[..., 0] >= 70
    return {"dE76": float(de.mean()), "dE76_bright": float(de[bright].mean()) if bright.sum() >= MIN else None,
            "dL": float((a[..., 0] - b[..., 0]).mean()), "bright_share": float(bright.mean())}


def ranks(a):
    """Ranks with ties averaged (Spearman): a clipped or black area is one tie, not a ramp in raster order."""
    v = a.ravel()
    order = np.argsort(v, kind="stable")
    s = v[order]
    starts = np.concatenate([[0], np.flatnonzero(s[1:] != s[:-1]) + 1])
    ends = np.concatenate([starts[1:], [s.size]])
    r = np.empty(s.size)
    r[order] = np.repeat((starts + ends - 1) / 2.0, ends - starts)
    return r.reshape(a.shape)


def orientations(a):
    """The rotations and reflections of a 2-D array that keep its shape: four, or all eight when it is square."""
    out = {"as is": a, "rotated 180": a[::-1, ::-1], "mirrored": a[:, ::-1], "flipped": a[::-1, :]}
    if a.shape[0] == a.shape[1]:
        out.update({"rotated 90": np.rot90(a, 1), "rotated 270": np.rot90(a, 3), "transposed": a.T,
                    "anti-transposed": np.rot90(a, 2).T})
    return out


FRAMING_MARGIN = 0.05  # the frame as it is must beat every other orientation by this much (real pairs: >= 0.10)


def framing_problem(lr_rec, lr_ref):
    """Why a pair's two Lightroom exports don't show the same framing, or None. Their luminances (sampled on the
    same grid) must rank-correlate best as they are, by at least FRAMING_MARGIN, against every other orientation that
    keeps the shape: a slider changes tones, not where things are, while a rotated or mirrored virtual copy (a quarter
    turn too, when the frame is square) keeps its size and would otherwise pair. A pair whose framing can't be told
    apart from another orientation (a flat or symmetric scene) is refused as well."""
    a, b = ranks(linear(lr_rec) @ Y), ranks(linear(lr_ref) @ Y)
    if not (a.std() > 0 and b.std() > 0):
        return "the two Lightroom exports' framing can't be checked (a flat frame)"
    corr = {name: float(np.corrcoef(a.ravel(), v.ravel())[0, 1]) for name, v in orientations(b).items()}
    same = corr.pop("as is")
    name, other = max(corr.items(), key=lambda kv: kv[1])
    if other >= same:
        return f"the two Lightroom exports aren't framed alike (luminance matches best {name}: a rotated or " \
               f"mirrored copy?)"
    if same - other < FRAMING_MARGIN:
        return f"the two Lightroom exports' framing can't be told from {name} (rank correlation {same:.3f} against " \
               f"{other:.3f}: a symmetric scene?)"
    return None


def look_label(p):
    return f"{p['profile']} / {p['look'] or 'no look'}, process {p['process']}"


def score_run(cases_path, cases, run):
    manifest = run_state(run / "manifest.json")
    check_manifest(manifest, run / "manifest.json")  # structure first, so nothing below meets damaged values
    if manifest.get("cases_sha256") != sha256_file(cases_path):
        sys.exit(f"{cases_path} is not the cases file {run} was rendered from")
    if not manifest.get("raw_check"):
        sys.exit(f"{run} was rendered before `render` checked each raw with LightCraft's own importer, so nothing "
                 f"shows its raws were clean: render into a new --run folder")
    reg_path, fail_path = run / "registration.json", run / "failures.json"
    reg, failures = run_state(reg_path, {}), run_state(fail_path, {})
    check_registration(reg, reg_path)
    check_failures(failures, fail_path)
    salt = run_salt(run)
    rows, looks, missing, rejected = [], {}, [], []
    checks = manifest.get("importer_checks", {})
    for photo, c in sorted(cases["photos"].items()):
        # nothing is scored without an affirmative check of this raw by this CLI's importer
        check = checks.get(f"{c['raw_sha256']}:{manifest.get('cli_sha256')}")
        if not check or check.get("clean") is not True:
            for p in c["pairs"]:
                where = {"photo": photo, "name": c["name"], "pair": p["key"], "long_edge": p["long_edge"]}
                if check:
                    rejected.append({**where, "reason": "the raw's own XMP changes LightCraft's develop settings on "
                                                        "import (" + ", ".join(check.get("changed", [])[:6])
                                                        + "): use the original raw"})
                else:
                    failed = failures.get(photo)
                    missing.append({**where, "reason": "not checked by LightCraft's importer yet"
                                    + (f" ({failed.splitlines()[-1][:160]})" if failed else "") + ": resume `render`"})
            continue
        for p in c["pairs"]:
            le = p["long_edge"]
            rid = registration_key(photo, p["session"], le, p["reference"]["export"]["path"])
            where = {"photo": photo, "name": c["name"], "pair": p["key"], "long_edge": le}
            r = reg.get(rid, {})
            if r and r.get("provenance") != registration_provenance(manifest, c["raw_sha256"], le,
                                                                    p["reference"]["export"],
                                                                    p["lightcraft"]["reference"]):
                rejected.append({**where, "reason": "its registration was written by another run (CLI, cases or tool): "
                                                    "resume `render` to redo it"})
                continue
            if "rejected" in r:  # by key presence (reasons are never empty: check_registration)
                rejected.append({**where, "reason": r["rejected"]})
                continue
            if p["id"] in r.get("pairs_rejected", {}):
                rejected.append({**where, "reason": r["pairs_rejected"][p["id"]]})
                continue
            failed = failures.get(f"{photo}/{p['id']}") or failures.get(rid) or failures.get(photo)
            if "dy" not in r:
                missing.append({**where, "reason": f"not rendered: {failed.splitlines()[-1][:200]}" if failed else "not rendered"})
                continue
            shift, frame = (r["dy"], r["dx"]), r["shape"]
            d_lr, d_lc = run / "lr" / photo / str(le), run / "lc" / photo / str(le)
            raw = c["raw_sha256"]
            want = {"lr_rec": (d_lr / f"{lr_key(p['export']['path'])}.npz",
                               provenance(manifest, raw, le, frame, export=p["export"])),
                    "lr_ref": (d_lr / f"{lr_key(p['reference']['export']['path'])}.npz",
                               provenance(manifest, raw, le, frame, export=p["reference"]["export"])),
                    "lc_rec": (d_lc / f"{lc_key(p['lightcraft']['recipe'], shift)}.npz",
                               provenance(manifest, raw, le, frame, recipe=p["lightcraft"]["recipe"], shift=shift)),
                    "lc_ref": (d_lc / f"{lc_key(p['lightcraft']['reference'], shift)}.npz",
                               provenance(manifest, raw, le, frame, recipe=p["lightcraft"]["reference"], shift=shift))}
            arr, absent, broken, foreign = {}, [], [], []
            for k, (path, expected) in want.items():
                a, why = read_sample(path, expected)
                if why == "missing":
                    absent.append(k)
                elif why is not None and why.startswith("unreadable"):
                    broken.append(path.relative_to(run).as_posix())
                elif why is not None:
                    foreign.append(f"{k} ({why})")
                else:
                    arr[k] = a
            if absent:
                missing.append({**where, "reason": f"not rendered: {failed.splitlines()[-1][:200]}" if failed
                                else f"not rendered ({', '.join(absent)}): resume `render`, which makes them"})
                continue
            if broken:
                missing.append({**where, "reason": f"damaged sample {', '.join(broken)}: delete it and resume "
                                                   f"`render`, which rebuilds it"})
                continue
            if foreign:
                rejected.append({**where, "reason": f"sample not of this run: {'; '.join(foreign)} (resume `render`, "
                                                    f"which makes it again)"})
                continue
            if len({a.shape for a in arr.values()}) != 1:
                rejected.append({**where, "reason": f"sample shapes differ {sorted({a.shape for a in arr.values()})}"})
                continue
            why = framing_problem(arr["lr_rec"], arr["lr_ref"])
            if why:
                rejected.append({**where, "reason": why})
                continue
            rows.append({"photo": photo, "name": c["name"], "split": c["split"], "camera": c["camera"],
                         "slider": p["slider"], "amount": p["amount"], "long_edge": le,
                         "reference": p["reference"]["recipe"], "reference_kind": p["reference"]["kind"],
                         "session": p["session"], "look": look_label(p), "lightroom": p["lightroom"],
                         "state": p["lightroom_state"],
                         "cells": cells(arr["lr_ref"], arr["lr_rec"], arr["lc_ref"], arr["lc_rec"])})
            if p["reference"]["kind"] == "neutral":
                key = f"{photo}@{le}@{lr_key(p['reference']['export']['path'])}"
                if key not in looks:
                    looks[key] = {"photo": photo, "split": c["split"], "long_edge": le, "look": look_label(p),
                                  **neutral_look(arr["lc_ref"], arr["lr_ref"])}
    total = sum(len(c["pairs"]) for c in cases["photos"].values())
    # the report names photos by a hash salted per run: the raw-hash ids of the local outputs let anyone holding a
    # raw recognise it. The salt stays in the run folder (here and in scores.json), never in the report.
    scores = {"format": "lightcraft-render-fidelity-scores/1", "tool_sha256": sha256_file(TOOL),
              "cases_sha256": manifest["cases_sha256"], "manifest": manifest, "pairs": total,
              "report_salt": salt,
              "photos": {k: {"split": c["split"], "camera": c["camera"], "name": c["name"]}
                         for k, c in cases["photos"].items()},
              "sessions": len(cases["sessions"]), "rows": rows, "neutral": looks, "missing": missing,
              "rejected": rejected}
    write_json(run / "scores.json", scores)
    return scores


def med(values):
    v = [x for x in values if x is not None]
    return (float(np.median(v)), len(v)) if v else (None, 0)


def ref_label(r):
    return "Neutral" if r == "neutral" else r.split("=")[1]


def group_stats(rows):
    """Median over photos per (look, slider, amount, reference, size, split)."""
    by = defaultdict(list)
    for r in rows:
        by[(r["look"], r["slider"], r["amount"], r["reference"], r["long_edge"], r["split"])].append(r)
    out = []
    for k in sorted(by, key=lambda k: (k[0], k[1], k[2], k[3] != "neutral", k[3], k[4], k[5] != "tune")):
        g = by[k]
        s = {"look": k[0], "slider": k[1], "amount": k[2], "reference": k[3], "long_edge": k[4], "split": k[5],
             "photos": len(g), "bands": {}}
        for b in SHOWN:
            sh, ns = med([r["cells"].get(b, {}).get("share") for r in g])
            er, ne = med([r["cells"].get(b, {}).get("error") for r in g])
            s["bands"][b] = {"share": sh, "share_photos": ns, "error": er, "error_photos": ne}

        def a(f, g=g):
            return med([r["cells"].get("all", {}).get(f) for r in g])[0]

        def t(engine, what, g=g):
            return med([r["cells"].get("all", {}).get(engine, {}).get(what, {}).get("share") for r in g])[0]

        s.update({"error": a("error"), "bias": a("bias"), "dE": a("dE"), "colour": a("colour"),
                  "kept": a("retained"), "clip": [t("lr_transitions", "clip_new"), t("lc_transitions", "clip_new")],
                  "unblack": [t("lr_transitions", "black_recovered"), t("lc_transitions", "black_recovered")],
                  "out_dE": med([r["cells"].get("all", {}).get("outside", {}).get("dE") for r in g])[0]})
        out.append(s)
    return out


def neutral_stats(looks):
    out = []
    for split in ("tune", "check"):
        v = [x for x in looks.values() if x["split"] == split]
        if v:
            de, n = med([x["dE76"] for x in v])
            deb, nb = med([x["dE76_bright"] for x in v])
            out.append({"split": split, "dE76": de, "neutrals": n, "dE76_bright": deb, "bright_neutrals": nb,
                        "dL": med([x["dL"] for x in v])[0]})
    return out


def pct(v):
    return f"{v:.0%}" if v is not None else "-"


def text_summary(scores):
    rows, lines = scores["rows"], []
    problems = scores["missing"] + scores["rejected"]
    lines.append(f"Coverage: {len(rows)} of {scores['pairs']} pairs scored on {len({r['photo'] for r in rows})} "
                 f"photos; {len(scores['missing'])} missing, {len(scores['rejected'])} rejected (registration, size "
                 f"or changed files).")
    grouped = defaultdict(list)
    for m in problems:
        grouped[(m["photo"], m["name"], m["long_edge"], m["reason"])].append(m["pair"])
    for (photo, name, le, reason), pairs in sorted(grouped.items()):
        lines.append(f"    {photo} {name} {le} px, {len(pairs)} pairs: {reason}: {', '.join(sorted(pairs))}")
    for s in neutral_stats(scores["neutral"]):
        bright = f"{s['dE76_bright']:.2f}" if s["dE76_bright"] is not None else "-"
        lines.append(f"Neutral look ({s['split']}): median dE76 {s['dE76']:.2f} over {s['neutrals']} Neutrals, bright "
                     f"areas {bright} ({s['bright_neutrals']}), dL* {s['dL']:+.2f}")
    lines.append("\nInterval target-minus-reference, median over photos per band: LightCraft's share of Lightroom's "
                 "response / response error (stops) [photos with a share]. 'all': error, bias, dE, colour; clip: "
                 "Lightroom vs LightCraft share of newly clipped positions; kept: share of the frame in the support "
                 "(the rest is reported as 'out dE', the absolute dE there); unblack: share of reference-black "
                 "positions lifted above black.")
    head = (f"{'target':>6} {'ref':>7} {'px':>4} {'split':5} {'n':>2} " + " ".join(f"{b:>15}" for b in SHOWN)
            + f" {'all: err   bias   dE  col':>27} {'kept':>5} {'clip LR/LC':>11} {'unblack LR/LC':>13} {'out dE':>6}")
    section = None
    for s in group_stats(rows):
        if (s["look"], s["slider"]) != section:
            section = (s["look"], s["slider"])
            lines.append(f"\n{s['slider']} ({s['look']})\n{head}")
        parts = []
        for b in SHOWN:
            c = s["bands"][b]
            share = f"{c['share']:4.0%}" if c["share"] is not None else "   -"
            parts.append(f"{'':>15}" if c["error_photos"] == 0 else f"{share:>5}/{c['error']:.3f}[{c['share_photos']:>2}]")
        tail = (f"{s['error']:.3f} {s['bias']:+.3f} {s['dE']:4.1f} {s['colour']:4.1f}" if s["error"] is not None else "-")
        clip = f"{s['clip'][0] or 0:.0%}/{s['clip'][1] or 0:.0%}"
        unblack = f"{s['unblack'][0] or 0:.0%}/{s['unblack'][1] or 0:.0%}"
        kept = f"{s['kept']:5.0%}" if s["kept"] is not None else "    -"
        out_de = f"{s['out_dE']:5.1f}" if s["out_dE"] is not None else "    -"
        lines.append(f"{s['amount']:>6g} {ref_label(s['reference']):>7} {s['long_edge']:>4} {s['split']:5} "
                     f"{s['photos']:>2} " + " ".join(parts) + f" {tail:>27} {kept} {clip:>11} {unblack:>13} {out_de:>6}")
    return "\n".join(lines) + "\n"


def cmd_score(args):
    cases = load_cases(args.cases)
    run = Path(args.run)
    if not (run / "manifest.json").exists():
        sys.exit(f"{run}: no manifest.json (run `render` first)")
    with run_lock(run):  # score writes the run's scores, summary and salt: never during a render
        scores = score_run(args.cases, cases, run)
        text = text_summary(scores)
        (run / "summary.txt").write_text(text, encoding="utf-8")
    sys.stdout.write(text)
    print(f"\nwrote {run / 'scores.json'} and {run / 'summary.txt'}")


# ---------------------------------------------------------------------------------------------------------------
# report
# ---------------------------------------------------------------------------------------------------------------

TRANSITIONS = ("clip_new", "recovered", "black_new", "black_recovered")
APPENDIX = (["photo", "split", "camera", "session", "slider", "recipe", "reference", "px", "band", "population",
             "support", "retained", "lr", "lc", "bias", "share", "error", "dE", "colour", "lc_clipped_on_support",
             "lc_black_on_support", "outside", "outside_dE", "outside_encoded"]
            + [f"{e}_{k}{s}" for e in ("lr", "lc") for k in TRANSITIONS for s in ("", "_of")])


def _num(v):
    return "" if v is None else (f"{v:.6g}" if isinstance(v, float) else str(v))


SALT = re.compile(r"[0-9a-f]{32}")


def run_salt(run):
    """The run's report salt: made once (written to a temporary file, fsynced, then moved into place, so an
    interrupted write leaves no salt rather than a short one) and checked on every use: 32 lowercase hex digits."""
    path = Path(run) / "report-salt.txt"
    if not path.exists():
        write_atomic(path, (secrets.token_hex(16) + "\n").encode("ascii"))
    try:
        salt = path.read_text(encoding="ascii").strip()
    except (OSError, UnicodeDecodeError) as e:
        sys.exit(f"{path} can't be read ({type(e).__name__}): delete it and run score again (the report's photo labels "
                 f"change)")
    if not SALT.fullmatch(salt):
        sys.exit(f"{path} is damaged (a report salt is 32 lowercase hex digits): delete it and run score again (the "
                 f"report's photo labels change)")
    return salt


def report_ids(scores):
    """The report's name for each photo: a hash of the run's salt and the photo id (not reversible to the raw, and
    different in every run). There is no unsalted fallback: an invalid salt is refused."""
    salt = scores.get("report_salt")
    if not (isinstance(salt, str) and SALT.fullmatch(salt)):
        raise ValueError("the report salt in scores.json is missing or damaged: run `score` again")
    return lambda pid: hashlib.sha256(f"{salt}/{pid}".encode()).hexdigest()[:10]


def appendix_csv(rows, rid=lambda pid: pid):
    """Every scored pair and populated band with all statistics; no file or folder names (sessions are numbered)."""
    # ordered by the salted names, so a photo's place and its session numbers change between runs too
    sessions = {s: i for i, s in enumerate(sorted({(r["photo"], r["session"]) for r in rows},
                                                  key=lambda s: (rid(s[0]), s[1])), 1)}
    buf = io.StringIO()
    w = csv.writer(buf, lineterminator="\n")
    w.writerow(APPENDIX)
    order = [b[2] for b in BANDS] + ["all"]
    for r in sorted(rows, key=lambda r: (rid(r["photo"]), r["slider"], r["amount"], r["long_edge"], r["reference"])):
        for band in sorted(r["cells"], key=order.index):
            cell = r["cells"][band]
            line = {"photo": rid(r["photo"]), "split": r["split"], "camera": r["camera"],
                    "session": sessions[(r["photo"], r["session"])], "slider": r["slider"],
                    "recipe": f"{r['slider']}={r['amount']:g}", "reference": r["reference"], "px": r["long_edge"],
                    "band": band, "outside": cell["outside"]["positions"], "outside_dE": cell["outside"]["dE"],
                    "outside_encoded": cell["outside"]["encoded_error"]}
            line.update({k: cell.get(k) for k in ("population", "support", "retained", "lr", "lc", "bias", "share",
                                                  "error", "dE", "colour", "lc_clipped_on_support",
                                                  "lc_black_on_support")})
            for e in ("lr", "lc"):
                for k in TRANSITIONS:
                    line[f"{e}_{k}"] = cell[f"{e}_transitions"][k]["share"]
                    line[f"{e}_{k}_of"] = cell[f"{e}_transitions"][k]["of"]
            w.writerow([_num(line.get(k)) for k in APPENDIX])
    return buf.getvalue()


def neutral_csv(looks, rid=lambda pid: pid):
    buf = io.StringIO()
    w = csv.writer(buf, lineterminator="\n")
    keys = ["photo", "split", "px", "dE76", "dE76_bright", "dL", "bright_share"]
    w.writerow(keys)
    for v in sorted(looks.values(), key=lambda v: (rid(v["photo"]), v["long_edge"])):
        w.writerow([_num(rid(v["photo"]) if k == "photo" else v.get("long_edge") if k == "px" else v.get(k))
                    for k in keys])
    return buf.getvalue()


def markdown_report(scores, names=False):
    rows, m = scores["rows"], scores["manifest"]
    rid = report_ids(scores)
    photos = scores["photos"]
    scored = {r["photo"] for r in rows}
    cams = Counter(photos[p]["camera"] or "unknown camera" for p in scored)
    lightroom = sorted({r["lightroom"] or "unknown" for r in rows})
    looks = sorted({r["look"] for r in rows})
    # reasons never hold paths (they name exports by role); this only guards against a path slipping in
    reasons = Counter(re.sub(r"(?:[A-Za-z]:)?[/\\]\S+", "<file>", x["reason"]) for x in scores["rejected"])
    tool = f"`{m['tool_sha256'][:12]}`" + (f", scored by `{scores['tool_sha256'][:12]}`"
                                           if scores["tool_sha256"] != m["tool_sha256"] else "")
    out = ["### LightCraft render fidelity against Lightroom: single-slider responses", "",
           f"- LightCraft: `{m.get('cli_version') or 'lightcraft-cli'}` (binary sha256 `{m['cli_sha256'][:12]}`), "
           f"measured with `tools/render_fidelity.py` (sha256 {tool}, sampling v{m['sampling_version']})",
           f"- Lightroom: {', '.join(lightroom) or 'none'}; {', '.join(looks) or 'no pairs scored'}",
           f"- Cameras: {', '.join(f'{c} ({n} photos)' for c, n in sorted(cams.items())) or 'none'}",
           f"- Scored {len(rows)} of {scores['pairs']} pairs on {len(scored)} photos "
           f"({sum(photos[p]['split'] == 'tune' for p in scored)} tune, "
           f"{sum(photos[p]['split'] == 'check' for p in scored)} check) from "
           f"{len({r['session'] for r in rows})} export sessions; "
           f"{len(scores['missing'])} not rendered, {len(scores['rejected'])} rejected"
           + (f" ({'; '.join(f'{n}: {r}' for r, n in reasons.most_common())})" if reasons else ""),
           "- Every pair is two exports from one Lightroom export session (one slider moved, a Neutral or another "
           "amount of the same slider as reference) against LightCraft's renders of the same two recipes. "
           f"Differences below about 0.02 stop are not resolved. How to read this: {DOC}", ""]
    for s in neutral_stats(scores["neutral"]):
        bright = f"{s['dE76_bright']:.2f}" if s["dE76_bright"] is not None else "-"
        out.append(f"Neutral look ({s['split']}): median CIE76 dE {s['dE76']:.2f} over {s['neutrals']} Neutrals, "
                   f"bright areas {bright}, dL* {s['dL']:+.2f}")
    out += ["", "Per band of Lightroom's reference luminance: LightCraft's share of Lightroom's response / mean "
            "response error in stops (photos). Median over photos.", ""]
    section = None
    for s in group_stats(rows):
        if (s["look"], s["slider"]) != section:
            section = (s["look"], s["slider"])
            out += ["", f"#### {s['slider']} ({s['look']})", "",
                    "| target | ref | px | split | photos | " + " | ".join(SHOWN)
                    + " | error | bias | dE | colour | kept | clip LR/LC | unblack LR/LC | out dE |",
                    "|" + "---:|" * (5 + len(SHOWN) + 8)]
        bands = []
        for b in SHOWN:
            c = s["bands"][b]
            bands.append("" if c["error_photos"] == 0 else f"{pct(c['share'])} / {c['error']:.3f} ({c['share_photos']})")
        f = (lambda v, spec: format(v, spec) if v is not None else "-")
        out.append(f"| {s['amount']:g} | {ref_label(s['reference'])} | {s['long_edge']} | {s['split']} | "
                   f"{s['photos']} | " + " | ".join(bands) + f" | {f(s['error'], '.3f')} | {f(s['bias'], '+.3f')} | "
                   f"{f(s['dE'], '.1f')} | {f(s['colour'], '.1f')} | {pct(s['kept'])} | "
                   f"{pct(s['clip'][0])} / {pct(s['clip'][1])} | {pct(s['unblack'][0])} / {pct(s['unblack'][1])} | "
                   f"{f(s['out_dE'], '.1f')} |")
    out += ["", "<details><summary>Every pair, every populated band (CSV)</summary>", "",
            "One line per pair and band of Lightroom's reference luminance (`all` is the whole frame). Photos are "
            "named by a hash salted for this run (it doesn't identify the raw and differs between runs), sessions by "
            "number; responses in stops; transitions are each "
            "engine's share against its own reference, with the positions they are a share of (`_of`).", "",
            "```csv", appendix_csv(rows, rid).rstrip("\n"), "```", "", "Neutral looks:", "", "```csv",
            neutral_csv(scores["neutral"], rid).rstrip("\n"), "```", "", "</details>"]
    if names:
        out += ["", "Photos:", ""] + [f"- `{rid(p)}`: {photos[p]['name']} ({photos[p]['split']})"
                                      for p in sorted(scored, key=rid)]
    return "\n".join(out) + "\n"


def cmd_report(args):
    path = Path(args.run) / "scores.json"
    if not path.exists():
        sys.exit(f"{path}: run `score` first")
    scores = run_state(path)
    try:
        text = markdown_report(scores, args.names)
    except (ValueError, KeyError, TypeError, AttributeError) as e:
        sys.exit(f"{path} can't be reported ({type(e).__name__}: {e}): run `score` again")
    if args.output:
        Path(args.output).write_text(text, encoding="utf-8")
        print(f"wrote {args.output}")
    else:
        sys.stdout.write(text)


# ---------------------------------------------------------------------------------------------------------------
# selftest: synthetic data only
# ---------------------------------------------------------------------------------------------------------------

def _encode(lin):
    lin = np.clip(lin, 0.0, 1.0)
    e = np.where(lin <= 0.0031308, 12.92 * lin, 1.055 * np.power(lin, 1 / 2.4) - 0.055)
    return np.round(e * 65535).astype(np.uint16)


def _texture(h, w, seed):
    """A linear-light RGB test scene with texture at a few scales (procedural)."""
    rng = np.random.default_rng(seed)
    coarse = rng.uniform(0.02, 0.4, (h // 8 + 1, w // 8 + 1, 3))
    fine = rng.uniform(0.6, 1.4, (h // 2 + 1, w // 2 + 1, 1))
    return (np.kron(coarse, np.ones((8, 8, 1)))[:h, :w] * np.kron(fine, np.ones((2, 2, 1)))[:h, :w]).clip(0, 0.45)


def _icc(desc):
    """A minimal ICC-shaped byte string with one textDescription tag (only what icc_description reads)."""
    text = desc.encode("latin-1") + b"\0"
    tag = b"desc" + b"\0" * 4 + len(text).to_bytes(4, "big") + text
    return b"\0" * 128 + (1).to_bytes(4, "big") + b"desc" + (144).to_bytes(4, "big") + len(tag).to_bytes(4, "big") + tag


def _export_xmp(raw_name, recipe, when, extra=""):
    """An export-shaped XMP packet written for the self-test: our preset's settings plus what Lightroom adds."""
    body = preset_xmp("test", "test", recipe)
    body = body.replace('crs:PresetType="Normal"', f'crs:RawFileName="{raw_name}" crs:ProcessVersion="15.4" '
                        f'crs:CameraProfile="Adobe Standard" crs:AlreadyApplied="True" crs:Temperature="5300" '
                        f'crs:Tint="+16" crs:SharpenRadius="+1.0" crs:ParametricShadowSplit="25" {extra}')
    body = body.replace('xmlns:crs=', f'xmlns:xmp="{NS_XMP}" xmp:MetadataDate="{when}" xmlns:crs=')
    look = ('<crs:Look><rdf:Description crs:Name="Adobe Color" crs:Amount="1"><crs:Group><rdf:Alt>'
            '<rdf:li xml:lang="x-default">Profiles</rdf:li></rdf:Alt></crs:Group></rdf:Description></crs:Look>')
    return body.replace("  </rdf:Description>\n </rdf:RDF>", f"  {look}\n  </rdf:Description>\n </rdf:RDF>")


_FAKE_CLI = r'''#!@PYTHON@ -I
"""A stand-in lightcraft-cli for the self-test: renders procedural scenes, and fails in the way environment variables
choose: FAKE_CLI_FAULT=check|render:MODE@PART (only for raws whose path contains PART), FAKE_CLI_RENDER, FAKE_CLI_HANG,
FAKE_CLI_SLOW, FAKE_CLI_BAD_BYTES and FAKE_CLI_BAD_VERSION."""
import io, json, os, sys, time
sys.dont_write_bytecode = True
HALF = True
a = sys.argv[1:]
if os.environ.get("FAKE_CLI_ONLY_AT") and os.path.realpath(sys.argv[0]) != os.path.realpath(os.environ["FAKE_CLI_ONLY_AT"]):
    sys.stderr.write("this build only runs from where it was installed\n")
    sys.exit(1)
if a == ["--version"]:
    time.sleep(float(os.environ.get("FAKE_CLI_SLOW_VERSION") or 0))
    if os.environ.get("FAKE_CLI_BAD_VERSION"):
        sys.stdout.buffer.write(b"fake-cli \xff 1.0\n")
        sys.exit(0)
    print("fake-cli 1.0")
    sys.exit(0)
sys.path.insert(0, "@TOOLDIR@")
import render_fidelity as rf
import tifffile
importing = a[:2] == ["run", "--import"]
src = os.path.realpath(a[2] if importing else a[1])
if os.environ.get("FAKE_CLI_HANG") and os.environ["FAKE_CLI_HANG"] in src:
    time.sleep(20)
time.sleep(float(os.environ.get("FAKE_CLI_SLOW") or 0))
stage, _, rest = os.environ.get("FAKE_CLI_FAULT", "").partition(":")
fault, _, part = rest.partition("@")
if stage != ("check" if importing else "render") or not part or part not in src:
    fault = None
if fault == "hang":
    time.sleep(20)
if fault == "exit137":
    os._exit(137)
if importing:
    if os.environ.get("FAKE_CLI_BAD_BYTES"):
        sys.stdout.buffer.write(b"\xff\xfe not json\n")
        sys.exit(1)
    if fault == "nonutf8":
        sys.stdout.buffer.write(b"\xff\xfe\n")
        sys.exit(0)
    g = 40.0 if b"hostile" in open(a[2], "rb").read() else 0.0
    steps = (("develop.get", {"grain": {"amount": g}}), ("develop.reset", None), ("develop.get", {"grain": {"amount": 0.0}}))
    good = [json.dumps({"command": cmd, "ok": True, "result": res, "ms": 0.1}) for cmd, res in steps]
    out, code = {
        "exit1": ("\n".join(good), 1), "empty": ("", 0), "garbage": ("hello, not json", 0), "null": ("null", 0),
        "list": ("[]", 1), "truncated": (good[0][:25], 0), "count": ("\n".join(good[:2]), 0),
        "types": ("\n".join(json.dumps({"command": cmd, "ok": True, "result": 5}) for cmd, _ in steps), 0),
        "notok": ("\n".join(good[:2] + [json.dumps({"command": "develop.get", "ok": False, "error": "no photo"})]), 0),
    }.get(fault, ("\n".join(good), 0))
    print(out)
    sys.exit(code)
s = dict(a[i + 1].split("=") for i, x in enumerate(a) if x == "--set")
e, c = float(s["light.exposure"]), float(s["light.contrast"])
if "TEST0002" in src:
    img = rf._encode(rf._texture(640, 960, 3) * 2 ** e * (1 + c / 100))
else:
    img = rf._encode(rf._texture(640, 960, 2) * 2 ** (e / 2 if HALF else e))
path = a[a.index("-o") + 1]
modes = dict(x.split(":") for x in os.environ.get("FAKE_CLI_RENDER", "").split(",") if x)
mode = modes.get(s["light.exposure"]) or fault
if mode == "dir":
    os.mkdir(path)
    sys.exit(0)
if mode == "wrongsize":
    tifffile.imwrite(path, img[:320, :480], photometric="rgb")
    sys.exit(0)
if mode == "partial":
    buf = io.BytesIO()
    tifffile.imwrite(buf, img, photometric="rgb")
    with open(path, "wb") as f:
        f.write(buf.getvalue()[: len(buf.getvalue()) // 2])
    os._exit(0)
if mode not in ("nothing", "nofile"):
    tifffile.imwrite(path, img, photometric="rgb")
if mode is None and os.environ.get("FAKE_CLI_SWAP_TARGET"):
    with open(os.path.realpath(sys.argv[0])) as me:
        swapped = me.read().replace("HALF = True", "HALF = False")
    with open(os.environ["FAKE_CLI_SWAP_TARGET"], "w") as f:
        f.write(swapped)
if mode in ("fail", "failbytes", "exit1"):
    sys.stderr.buffer.write(b"boom \xff\n" if mode == "failbytes" else b"boom\n")
    sys.exit(1)
if mode == "nonutf8":
    sys.stderr.buffer.write(b"\xff\xfe\n")
    sys.exit(1)
if mode == "hang":
    time.sleep(20)
'''
_KILL_WORKER = r'''"""Self-test worker: `render` on a filesystem without hard links, killed (SIGKILL) right after its manifest's
O_EXCL create and before anything is written into it (cases, cli, run)."""
import argparse, errno, os, signal, sys
sys.dont_write_bytecode = True
sys.path.insert(0, "@TOOLDIR@")
import render_fidelity as rf
cases, cli, run = sys.argv[1:4]
real_link, real_open = os.link, os.open


def no_link(src, dst, *a, **k):
    if str(dst).endswith("manifest.json"):
        raise OSError(errno.ENOTSUP, "no hard links here")
    return real_link(src, dst, *a, **k)


def open_then_die(path, flags, *a, **k):
    fd = real_open(path, flags, *a, **k)
    if str(path).endswith("manifest.json") and flags & os.O_EXCL:
        os.kill(os.getpid(), signal.SIGKILL)
    return fd


os.link, os.open = no_link, open_then_die
rf.cmd_render(argparse.Namespace(cases=cases, cli=cli, run=run, split=None, photos=None, sliders=None, timeout=60.0,
                                 check_timeout=60.0))
'''
_RACE_WORKER = r'''"""Self-test worker: `render` with the lock off, its manifest creation ordered by gate files so that A creates
first and B's creation lands after it (role, gates, cases, cli, run)."""
import argparse, os, sys, time
sys.dont_write_bytecode = True
sys.path.insert(0, "@TOOLDIR@")
import render_fidelity as rf
from pathlib import Path
role, gates, cases, cli, run = sys.argv[1:6]
gates = Path(gates)


def wait(p):
    deadline = time.monotonic() + 60
    while not p.exists():
        if time.monotonic() > deadline:
            raise SystemExit(f"gate {p.name} timed out")
        time.sleep(0.01)


def ordered(original):
    def hooked(*a, **k):
        (gates / f"{role}-ready").touch()
        wait(gates / "A-ready")
        wait(gates / "B-ready")
        if role == "A":
            try:
                return original(*a, **k)
            finally:
                (gates / "A-written").touch()
                wait(gates / "B-written")
        wait(gates / "A-written")
        try:
            result = original(*a, **k)
        finally:
            (gates / "B-written").touch()
        wait(gates / "A-done")
        return result
    return hooked


if hasattr(rf, "create_manifest"):
    rf.create_manifest = ordered(rf.create_manifest)
else:  # before 67585c39's successor the manifest was created with write_json
    plain = rf.write_json
    gated = ordered(plain)
    rf.write_json = lambda path, data: (gated if Path(path).name == "manifest.json" and "importer_checks" not in data
                                        else plain)(path, data)
try:
    rf.cmd_render(argparse.Namespace(cases=cases, cli=cli, run=run, split=None, photos=None, sliders=None,
                                     timeout=60.0, check_timeout=60.0))
finally:
    (gates / f"{role}-done").touch()
'''
FAULTS = {"check": ("exit1", "exit137", "empty", "garbage", "null", "list", "truncated", "count", "types", "notok",
                    "nonutf8", "hang"),
          "render": ("exit1", "exit137", "nofile", "dir", "wrongsize", "partial", "nonutf8", "hang")}


def _fault_matrix(check, fake, cases_path, cases, faulty, healthy, base, log=None):
    """Every failure mode of the fake CLI, for one photo of two: the run completes, the failure is recorded for that
    photo only, and no pair of it is scored. Returns {stage:mode: outcome} (for a summary)."""
    outcomes = {}
    for stage, modes in FAULTS.items():
        for mode in modes:
            name = f"{stage}:{mode}"
            run = base / name.replace(":", "-")
            open_run(run, run_binding(cases_path, "fault-matrix", "fake-cli 1.0"), {})
            limit = 3.0 if mode == "hang" else 60.0
            os.environ["FAKE_CLI_FAULT"] = f"{stage}:{mode}@TEST0002"
            completed = True
            try:
                with tempfile.TemporaryDirectory() as tmp, contextlib.redirect_stderr(io.StringIO()):
                    render_cases(cases, run, _identity_of(run), CliRenderer(fake, tmp, limit, limit), sorted(cases["photos"]),
                                 log=lambda *_: None)
            except Exception as e:  # what the fault matrix exists to catch
                completed = f"{type(e).__name__}: {e}"
            finally:
                del os.environ["FAKE_CLI_FAULT"]
            try:
                s = score_run(cases_path, cases, run)
            except (Exception, SystemExit) as e:
                check(f"fault {name}: the run can be scored", False, f"{type(e).__name__}: {e}")
                continue
            problems = s["missing"] + s["rejected"]
            fails = json.loads((run / "failures.json").read_text()) if (run / "failures.json").exists() else {}

            def of(key):
                return json.loads(key)[0] if key.startswith("[") else key.split("/")[0]
            ok_run = completed is True
            only_it = (all(x["photo"] == faulty for x in problems)
                       and len([x for x in problems if x["photo"] == faulty]) == len(cases["photos"][faulty]["pairs"])
                       and all(of(k) == faulty for k in fails) and all(x["reason"] for x in problems))
            unscored = (not [r for r in s["rows"] if r["photo"] == faulty]
                        and len([r for r in s["rows"] if r["photo"] == healthy]) == len(cases["photos"][healthy]["pairs"]))
            check(f"fault {name}: the run completes", ok_run, completed)
            check(f"fault {name}: recorded for that photo only", only_it, (problems, fails))
            check(f"fault {name}: no pair scored from the failed render", unscored, len(s["rows"]))
            outcomes[name] = (ok_run and only_it and unscored,
                              sorted({x["reason"].splitlines()[-1][:110] for x in problems if x["photo"] == faulty}))
    return outcomes


def _identity_of(run_dir):
    """Self-test: the identity a run's manifest names (what its `render` would have got from open_run)."""
    m = json.loads((Path(run_dir) / "manifest.json").read_text())
    return {k: m.get(k) for k in RUN_KEYS}


class _StandIn:
    """A self-test renderer: a function in place of LightCraft, and an importer check that reports `changed`."""

    def __init__(self, render, changed=()):
        self.render, self.changed = render, list(changed)

    def __call__(self, raw, long_edge, recipe):
        return self.render(raw, long_edge, recipe)

    def inspect(self, raw):
        return list(self.changed)


def _linear_dng(path, xmp, pixels=None):
    """A minimal LinearRaw DNG, written for the self-test: LightCraft imports it as a raw (so it applies the XMP
    in tag 700, when given). `pixels`: bytes to place in the image data."""
    if pixels is None:
        img = (np.random.default_rng(1).uniform(0.05, 0.6, (64, 96, 3)) * 65535).astype(np.uint16)
    else:
        raw = (pixels * (64 * 96 * 6 // len(pixels) + 1))[:64 * 96 * 6]
        img = np.frombuffer(raw, np.uint16).reshape(64, 96, 3)
    unit = (10000, 10000)
    tags = [(254, 4, 1, 0, True), (50706, 1, 4, bytes([1, 4, 0, 0]), True), (50708, 2, 0, "Synthetic Test", True),
            (50721, 10, 9, unit + (0, 10000, 0, 10000, 0, 10000) + unit + (0, 10000, 0, 10000, 0, 10000) + unit, True),
            (50778, 3, 1, 21, True), (50728, 5, 3, (1, 1, 1, 1, 1, 1), True)]
    if xmp is not None:
        tags.append((700, 1, len(xmp), xmp, True))
    tifffile.imwrite(path, img, photometric=34892, metadata=None, extratags=tags)


def _importer_cross_check(cli, check):
    """The prefilter and the importer check against LightCraft's own XMP reader: every packet the real importer
    applies must be refused by both; clean files accepted by both."""
    lc_s = b'settings="{&quot;grain&quot;: {&quot;amount&quot;: 40.0}}"'

    def packet(body):
        return (b"<x:xmpmeta xmlns:x='adobe:ns:meta/'><rdf:RDF xmlns:rdf='" + NS_RDF.encode() + b"'>" + body
                + b"</rdf:RDF></x:xmpmeta>")
    applied = {
        "&#+46;": packet(b'<rdf:Description xmlns:q="http://ns.lightcraft&#+46;app/lc/1.0/" q:' + lc_s + b"/>"),
        "&#x+2e;": packet(b'<rdf:Description xmlns:q="http://ns.lightcraft&#x+2e;app/lc/1.0/" q:' + lc_s + b"/>"),
        "4,100 zeros": packet(b'<rdf:Description xmlns:q="http://ns.lightcraft&#' + b"0" * 4100 + b'46;app/lc/1.0/" q:'
                              + lc_s + b"/>"),
        "undeclared crs": packet(b'<rdf:Description crs:Exposure2012="+1.00"/>'),
        "rebound crs": packet(b'<rdf:Description xmlns:crs="http://example.org/x/" crs:Exposure2012="+1.00"/>'),
        "escaped default namespace": packet(b'<rdf:Description xmlns="http://ns.lightcraft&#x2E;app/lc/1.0/" ' + lc_s
                                            + b"/>"),
        "unterminated": b"<x:xmpmeta><rdf:RDF><rdf:Description xmlns:lc='http://ns.lightcraft.app/lc/1.0/' lc:"
                        + lc_s + b">",
        "plain lc": packet(b'<rdf:Description xmlns:lc="http://ns.lightcraft.app/lc/1.0/" lc:' + lc_s + b"/>"),
    }
    clean = {"no XMP": None,
             "dc only": packet(b'<rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/" dc:format="x"/>')}
    with tempfile.TemporaryDirectory(prefix="render-fidelity-importer-") as tmp:
        renderer = CliRenderer(cli, tmp)
        for i, (name, x) in enumerate({**applied, **clean}.items()):
            f = Path(tmp) / f"case{i}.dng"
            _linear_dng(f, x)
            try:
                changed = renderer.inspect(f)
            except RenderFailed as e:
                check(f"importer check ({name}): runs", False, str(e)[-300:])
                continue
            if name in applied:
                check(f"importer check: LightCraft applies {name}, and the importer check refuses it", bool(changed))
                check(f"prefilter: refuses {name} too", bool(hash_raw(f)[1]), hash_raw(f)[1])
            else:
                check(f"importer check: accepts a clean raw ({name})", changed == [], changed)
                check(f"prefilter: accepts a clean raw ({name})", hash_raw(f)[1] == [], hash_raw(f)[1])


def selftest(cli=None):
    failures, checks = [], 0

    def check(name, ok, detail=""):
        nonlocal checks
        checks += 1
        if not ok:
            failures.append(f"{name}: {detail}")

    def close(a, b, tol):
        return a is not None and b is not None and abs(a - b) <= tol

    zero = {b[1]: 0.0 for b in BASIC}

    @contextlib.contextmanager
    def group(name):
        """A group of checks: an error inside it is a failure, never a silent skip, and later groups still run
        (one that needs something the failed group made fails too, with that error)."""
        try:
            yield
        except (Exception, SystemExit) as e:
            check(f"group '{name}' ran to the end", False, f"{type(e).__name__}: {e}")

    before = len(failures)
    with group("a deliberate error"):
        raise RuntimeError("deliberate")
    counted = len(failures) == before + 1
    if counted:
        failures.pop()
        checks -= 1
    check("selftest: an error in a group is counted as a failure, not skipped", counted)

    identity_of = _identity_of

    # XMP parsing and the clean recipe
    crs, el, xmp = parse_xmp(_export_xmp("A.ARW", {**zero, "light.exposure": 1.0}, "2026-10-12T10:00:00+02:00"))
    rec = clean_recipe(crs, el)
    check("xmp: exposure read", rec["recipe"]["light.exposure"] == 1.0 and rec["moved"] == ["light.exposure"], rec)
    check("xmp: look read", (rec["profile"], rec["look"]) == ("Adobe Standard", "Adobe Color"), rec)
    check("xmp: export time", export_time(xmp, None) == datetime.datetime(2026, 10, 12, 8, tzinfo=datetime.timezone.utc).timestamp())
    neutral = clean_recipe(*parse_xmp(_export_xmp("A.ARW", zero, "2026-10-12T10:00:00+02:00"))[:2])
    check("xmp: neutral has no slider", neutral["moved"] == [] and neutral["signature"] == rec["signature"])
    element_form = parse_xmp(f'<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="{NS_RDF}"><rdf:Description '
                             f'xmlns:crs="{NS_CRS}"><crs:Exposure2012>+0.50</crs:Exposure2012></rdf:Description>'
                             f'</rdf:RDF></x:xmpmeta>')[0]
    check("xmp: element-form value", element_form.get("Exposure2012") == "+0.50", element_form)
    base = _export_xmp("A.ARW", zero, "2026-10-12T10:00:00+02:00")
    bent = "<rdf:li>0, 0</rdf:li>\n     <rdf:li>64, 40</rdf:li>\n     <rdf:li>255, 255</rdf:li>"
    refusals = {
        "sharpening on": base.replace('crs:Sharpness="0"', 'crs:Sharpness="40"'),
        "custom white balance": base.replace('crs:WhiteBalance="As Shot"', 'crs:WhiteBalance="Custom"'),
        "cropped": base.replace('crs:HasSettings="True"', 'crs:HasSettings="True" crs:HasCrop="True"'),
        "bent curve": base.replace("<rdf:li>0, 0</rdf:li>\n     <rdf:li>255, 255</rdf:li>", bent, 1),
        "unmapped setting": base.replace('crs:HasSettings="True"', 'crs:HasSettings="True" crs:GrainAmount="20"'),
        "other look": base.replace('crs:Name="Adobe Color"', 'crs:Name="Adobe Vivid"'),
        "not an export": base.replace('crs:AlreadyApplied="True"', 'crs:AlreadyApplied="False"'),
        "masks": base.replace("<crs:Look>", "<crs:MaskGroupBasedCorrections><rdf:Seq><rdf:li>x</rdf:li></rdf:Seq>"
                                            "</crs:MaskGroupBasedCorrections><crs:Look>"),
        "2010 process": base.replace('crs:ProcessVersion="15.4"', 'crs:ProcessVersion="5.0"'),
    }
    for name, packet in refusals.items():
        try:
            clean_recipe(*parse_xmp(packet)[:2])
            check(f"clean recipe refuses {name}", False, "accepted")
        except Refused:
            check(f"clean recipe refuses {name}", True)
    for name, packet in {"zero-valued unknown setting": base.replace('crs:HasSettings="True"', 'crs:HasSettings="True" crs:NewThing="0"'),
                         "sharpen radius while sharpening is off": base.replace('crs:SharpenRadius="+1.0"', 'crs:SharpenRadius="+2.0"'),
                         "SDR settings while HDR editing is off": base.replace('crs:HasSettings="True"', 'crs:HasSettings="True" crs:HDREditMode="0" crs:SDRBrightness="+20"')}.items():
        try:
            clean_recipe(*parse_xmp(packet)[:2])
            check(f"clean recipe accepts {name}", True)
        except Refused as r:
            check(f"clean recipe accepts {name}", False, str(r))
    try:
        parse_xmp('<!DOCTYPE x [<!ENTITY a "aaaa">]><x:xmpmeta xmlns:x="adobe:ns:meta/"/>')
        check("xmp: DOCTYPE refused", False)
    except Refused:
        check("xmp: DOCTYPE refused", True)
    resource_look = base.replace(
        '<crs:Look><rdf:Description crs:Name="Adobe Color" crs:Amount="1">',
        '<crs:Look rdf:parseType="Resource"><crs:Name>Modern 04</crs:Name><rdf:Description>').replace(
        "</rdf:Description></crs:Look>", "</rdf:Description></crs:Look>")
    for name, packet in {"a look written as rdf:parseType=Resource": resource_look,
                         "a look without a readable name": base.replace('crs:Name="Adobe Color" ', ""),
                         "a structure written as attributes": base.replace(
                             "<crs:Look>", '<crs:LensBlur crs:Active="True" crs:BlurAmount="80"/><crs:Look>')}.items():
        try:
            got = clean_recipe(*parse_xmp(packet)[:2])
            check(f"clean recipe refuses {name}", False, got["look"])
        except Refused:
            check(f"clean recipe refuses {name}", True)
    check("xmp: two-digit fractions of a second", export_time({"MetadataDate": "2026-10-12T10:00:00.39+02:00"}, None)
          == datetime.datetime(2026, 10, 12, 8, 0, 0, 390000, tzinfo=datetime.timezone.utc).timestamp())

    # plan: our presets carry their recipe
    recipes = plan_recipes(None)
    check("plan: Neutral first, two amounts per slider", recipes[0][0] == "00 Neutral" and len(recipes) == 1 + 2 * len(BASIC))
    for name, recipe in recipes:
        crs, el, _ = parse_xmp(preset_xmp(name, "g", recipe))
        got = {CRS_TO_LC[k]: float(v) for k, v in crs.items() if k in CRS_TO_LC}
        check(f"plan: {name} round trip", got == recipe and crs["Sharpness"] == "0" and crs["LensProfileEnable"] == "0"
              and all(curve_points(el[c]) == [(0.0, 0.0), (255.0, 255.0)] for c in CURVES), got)
    check("plan: amounts validated", _raises(lambda: plan_recipes(["Contrast=150"])) and _raises(lambda: plan_recipes(["Foo=1"])))
    check("icc: description", icc_description(_icc("sRGB IEC61966-2.1")) == "sRGB IEC61966-2.1")
    check("icc: garbage", icc_description(b"\x01" * 200) is None and icc_description(None) is None)

    # pairing: same session only, Neutral first, then the smallest other amount; one pair per recipe and size
    def ex(path, session, recipe, le=960, sig=None):
        moved = [k for k, v in recipe.items() if v]
        return {"path": path, "session": session, "photo": "p", "long_edge": le, "shape": [640, le],
                "icc_sha256": "i", "signature": sig or [("x", "1")], "recipe": {**zero, **recipe}, "moved": moved,
                "size": 1, "mtime_ns": 1, "profile": "Adobe Standard", "look": "Adobe Color", "process": "15.4",
                "creator": "test", "camera": "test"}
    exports = [ex("a/n.tif", "a", {}), ex("a/e1.tif", "a", {"light.exposure": 1.0}),
               ex("a/c50.tif", "a", {"light.contrast": 50.0}),
               ex("b/e-1.tif", "b", {"light.exposure": -1.0}), ex("b/e1.tif", "b", {"light.exposure": 1.0}),
               ex("b/e2.tif", "b", {"light.exposure": 2.0}),
               ex("c/w10.tif", "c", {"light.whites": 10.0}),
               ex("a/h50.tif", "a", {"light.highlights": 50.0}, sig=[("x", "2")]),
               ex("a/e1-big.tif", "a", {"light.exposure": 1.0}, le=1920)]
    pairs, exploratory = make_pairs(exports)
    got = {p["id"]: (p["session"], p["reference"]["recipe"], p["reference"]["export"]["path"]) for p in pairs["p"]}
    check("pairs: Neutral reference preferred over another session's",
          got.get("light.exposure=1@960") == ("a", "neutral", "a/n.tif"), got)
    check("pairs: same slider, smallest other amount (negative first on a tie), same session",
          got.get("light.exposure=-1@960") == ("b", "light.exposure=1", "b/e1.tif")
          and got.get("light.exposure=2@960") == ("b", "light.exposure=-1", "b/e-1.tif"), got)
    check("pairs: other settings or size never pair",
          "light.highlights=50@960" not in got and "light.exposure=1@1920" not in got, got)
    check("pairs: lone slider export is exploratory",
          {(x["recipe"], x["long_edge"]) for x in exploratory} == {("light.whites=10", 960), ("light.highlights=50", 960),
                                                                   ("light.exposure=1", 1920)}, exploratory)
    check("pairs: count", len(pairs["p"]) == 4, got)

    # sessions: a folder is a session; a long gap between export times splits it
    sess = [{"folder": "f", "path": f"f/{i}.tif", "time": t} for i, t in enumerate((0, 5, 9, 4000, 4003))]
    info = assign_sessions(sess, 10)
    ids = [e["session"] for e in sess]
    check("sessions: split at a gap", len(set(ids[:3])) == 1 and ids[3] == ids[4] and ids[3] != ids[0]
          and len(info) == 2, ids)
    sess2 = [{"folder": "f", "path": "f/0.tif", "time": None}, {"folder": "f", "path": "f/1.tif", "time": 5000.0}]
    assign_sessions(sess2, 10)
    check("sessions: no split without times", len({e["session"] for e in sess2}) == 1)
    clash = [dict(ex("s/a.tif", None, {}), folder="s", time=0.0),
             dict(ex("s/b.tif", None, {"light.exposure": 1.0}), folder="s", time=4000.0),
             dict(ex("s#2/n.tif", None, {}), folder="s#2", time=9000.0)]
    assign_sessions(clash, 10)
    clash_pairs, _ = make_pairs(clash)
    check("sessions: a folder's second time segment never shares a session with a folder named like it",
          len({e["session"] for e in clash}) == 3 and not clash_pairs.get("p"),
          ([e["session"] for e in clash], clash_pairs.get("p")))
    check("split: fixed by hash", split_of("0" * 64) == "check" and split_of("0" * 63 + "1") == "tune")

    # scoring maths
    rng = np.random.default_rng(7)
    lin = rng.uniform(0.002, 0.4, (60, 100, 1)) * np.array([1.0, 0.9, 0.8])
    ref = _encode(lin)
    same = cells(ref, _encode(lin * 2), ref, _encode(lin * 2))["all"]
    check("cells: identical engines agree", close(same["error"], 0, 1e-12) and close(same["bias"], 0, 1e-12)
          and close(same["share"], 1, 1e-12) and close(same["dE"], 0, 1e-12) and close(same["colour"], 0, 1e-12), same)
    half = cells(ref, _encode(lin * 2), ref, _encode(lin * 2 ** 0.5))["all"]
    check("cells: half the response", close(half["lr"], 1, 0.01) and close(half["lc"], 0.5, 0.01)
          and close(half["share"], 0.5, 0.01) and close(half["bias"], -0.5, 0.01) and close(half["error"], 0.5, 0.01), half)
    tint = lin * 2 * np.array([1.0, 1.0, 1.15])
    col = cells(ref, _encode(lin * 2), ref, _encode(tint))["all"]
    check("cells: colour change measured", col["colour"] > 1.0 and col["dE"] >= col["colour"] * 0.5, col)
    check("cells: bands populated", set(cells(ref, _encode(lin * 2), ref, _encode(lin * 2))) >= {"all", ".03-.10", ".10-.25"})
    bright = np.full((40, 100, 3), 0.6)
    bright[:20] = 0.2
    t = cells(_encode(bright), _encode(bright * 2), _encode(bright), _encode(bright * 1.2))["all"]
    check("cells: clipping both ways", close(t["lr_transitions"]["clip_new"]["share"], 0.5, 1e-9)
          and close(t["lc_transitions"]["clip_new"]["share"], 0.0, 1e-9)
          and t["outside"]["positions"] == 2000 and close(t["retained"], 0.5, 1e-9), t)
    dark = np.full((40, 100, 3), 0.0002)
    dark[:20] = 0.05
    t = cells(_encode(dark), _encode(dark * 4), _encode(dark), _encode(dark))["all"]
    check("cells: lifted from black", close(t["lr_transitions"]["black_recovered"]["share"], 1.0, 1e-9)
          and close(t["lc_transitions"]["black_recovered"]["share"], 0.0, 1e-9) and t["support"] == 2000, t)
    small = cells(ref[:10, :20], ref[:10, :20], ref[:10, :20], ref[:10, :20])
    check("cells: fewer than 300 positions reported as nothing", small == {}, small)

    # registration and sampling
    scene = _encode(_texture(640, 960, 1))
    shifted = np.zeros_like(scene)
    shifted[2:, :-3] = scene[:-2, 3:]  # LightCraft pixel (p + (2, -3)) shows Lightroom's p
    shift, why, _ = register(shifted, scene, 960)
    check("register: finds a known shift", shift == (2, -3), (shift, why))
    flat = np.full_like(scene, 30000)
    check("register: refuses a flat frame", register(flat, flat, 960)[0] is None)
    s = sample(scene, 960)
    s2 = sample(shifted, 960, 2, -3)
    check("sample: margin covers the shift", s.shape == s2.shape and np.array_equal(s, s2), (s.shape, s2.shape))
    check("sample: refuses a shift beyond the search", _raises(lambda: sample(scene, 960, 5, 0)))
    s_dark = (decode(s) * 0.5 * 65535).astype(np.uint16)
    check("framing: a pair of the same frame passes", framing_problem(s_dark, s) is None)
    check("framing: a mirrored or rotated copy is refused", framing_problem(s_dark, s[:, ::-1]) is not None
          and framing_problem(s_dark, s[::-1, ::-1]) is not None)
    half_scene = s[:, : s.shape[1] // 2]
    symmetric = np.ascontiguousarray(np.concatenate([half_scene, half_scene[:, ::-1]], axis=1))
    check("framing: a pair whose framing can't be told from a mirror image (symmetric scene) is refused",
          framing_problem((decode(symmetric) * 0.5 * 65535).astype(np.uint16), symmetric) is not None)

    # T2: the prefilter reads the XMP packets LightCraft's reader takes from a file (TIFF tag 700 here) and refuses
    # any develop packet in them, and anything in them it can't inspect: an escaped or overlong namespace
    # declaration, a packet that isn't well-formed
    def packet(body):
        return (b"<x:xmpmeta xmlns:x='adobe:ns:meta/'><rdf:RDF xmlns:rdf='" + NS_RDF.encode() + b"'>" + body
                + b"</rdf:RDF></x:xmpmeta>")
    lc_s = b'settings="{&quot;grain&quot;: {&quot;amount&quot;: 40.0}}"'
    hostile = {
        "an lc namespace escaped with &#+46; (Codex)": packet(b'<rdf:Description xmlns:q="http://ns.lightcraft&#+46;'
                                                              b'app/lc/1.0/" q:' + lc_s + b"/>"),
        "an lc namespace escaped with &#x+2e; (Codex)": packet(b'<rdf:Description xmlns:q="http://ns.lightcraft'
                                                               b'&#x+2e;app/lc/1.0/" q:' + lc_s + b"/>"),
        "a reference with 4,100 leading zeros (Codex)": packet(b'<rdf:Description xmlns:q="http://ns.lightcraft&#'
                                                               + b"0" * 4100 + b'46;app/lc/1.0/" q:' + lc_s + b"/>"),
        "an escaped lc namespace": packet(b'<rdf:Description xmlns:lc="http://ns.lightcraft&#46;app/lc/1.0/" lc:'
                                          + lc_s + b"/>"),
        "an escaped crs namespace": packet(b'<rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings'
                                           b'&#x2F;1.0/" crs:Exposure2012="+1"/>'),
        "another prefix bound to the lc namespace": packet(b'<rdf:Description xmlns:q="http://ns.lightcraft.app/lc/'
                                                           b'1.0/" q:' + lc_s + b"/>"),
        "another prefix bound to the crs namespace": packet(b"<rdf:Description xmlns:z='http://ns.adobe.com/"
                                                            b"camera-raw-settings/1.0/' z:Exposure2012='+1'/>"),
        "an undeclared crs prefix": packet(b'<rdf:Description crs:Exposure2012="+1"/>'),
        "crs bound to another namespace": packet(b'<rdf:Description xmlns:crs="http://example.org/x/" '
                                                 b'crs:Exposure2012="+1"/>'),
        "an escaped default namespace": packet(b'<rdf:Description xmlns="http://ns.lightcraft&#x2E;app/lc/1.0/" '
                                               + lc_s + b"/>"),
        "an unterminated packet": b"<x:xmpmeta><rdf:RDF><rdf:Description xmlns:lc='http://ns.lightcraft.app/lc/1.0/'"
                                  b" lc:" + lc_s + b">",
        "a malformed packet": packet(b'<rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/" dc:format="x">'),
    }
    with tempfile.TemporaryDirectory(prefix="render-fidelity-selftest-") as tmp_raw:
        f = Path(tmp_raw) / "x.DNG"
        for name, body in hostile.items():
            _linear_dng(f, body)
            check(f"raw guard: refuses {name}", bool(hash_raw(f)[1]), hash_raw(f)[1])
        _linear_dng(f, packet(b'<rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/" dc:format="raw"/>'))
        check("raw guard: a raw with only other metadata is accepted", hash_raw(f)[1] == [], hash_raw(f)[1])
        _linear_dng(f, None, pixels=b"pixel data that happens to read  crs:x  and lc:settings > ")
        check("raw guard: bytes in the pixel data aren't XMP (no false refusal)", hash_raw(f)[1] == [], hash_raw(f)[1])
        f.write_bytes(b"\x00unknown container" + packet(b'<rdf:Description xmlns:dc="x" dc:format="raw"/>'))
        check("raw guard: XMP-like bytes in a container it doesn't know are refused (can't inspect)",
              bool(hash_raw(f)[1]), hash_raw(f)[1])

    # end to end on generated files: pair -> render (a stand-in renderer) -> score -> report
    with group("end to end"):
        with tempfile.TemporaryDirectory(prefix="render-fidelity-selftest-") as tmp:
            tmp = Path(tmp)
            (tmp / "raws").mkdir()
            (tmp / "raws" / "TEST0001.ARW").write_bytes(b"not a real raw, only its hash is used" * 100)
            lin = _texture(640, 960, 2)
            icc = _icc("sRGB IEC61966-2.1")

            def write(folder, name, recipe, when, edit=lambda x: x):
                d = tmp / "exports" / folder
                d.mkdir(parents=True, exist_ok=True)
                e = recipe.get("light.exposure", 0.0)
                data = _encode(lin * 2 ** e * (1 + recipe.get("light.contrast", 0) / 100))
                x = edit(_export_xmp("TEST0001.ARW", {**zero, **recipe}, when)).encode()
                tifffile.imwrite(d / name, data, photometric="rgb", metadata=None,
                                 extratags=[(700, 1, len(x), x, True), (34675, 7, len(icc), icc, True)])

            write("s1", "n.tif", {}, "2026-10-12T10:00:00+02:00")
            write("s1", "e1.tif", {"light.exposure": 1.0}, "2026-10-12T10:00:01+02:00")
            write("s1", "c50.tif", {"light.contrast": 50.0}, "2026-10-12T10:00:02+02:00")
            write("s1", "sharp.tif", {"light.exposure": -1.0}, "2026-10-12T10:00:03+02:00",
                  lambda x: x.replace('crs:Sharpness="0"', 'crs:Sharpness="40"'))
            write("s2", "e-1.tif", {"light.exposure": -1.0}, "2026-10-12T11:00:00+02:00")
            write("s2", "e1.tif", {"light.exposure": 1.0}, "2026-10-12T11:00:01+02:00")
            tifffile.imwrite(tmp / "exports" / "s2" / "eight-bit.tif", np.zeros((64, 96, 3), np.uint8), photometric="rgb")
            cases_path = tmp / "cases.json"
            ns = argparse.Namespace(exports=str(tmp / "exports"), raws=[str(tmp / "raws")], output=str(cases_path),
                                    session_gap=10.0, splits=None)
            with contextlib.redirect_stdout(io.StringIO()):
                cmd_pair(ns)
            cases = load_cases(cases_path)
            photo = next(iter(cases["photos"].values()))
            got = {p["id"]: (session_label(p["session"]), p["reference"]["recipe"]) for p in photo["pairs"]}
            check("e2e pair: pairs", got == {"light.exposure=1@960": ("s1", "neutral"),
                                             "light.contrast=50@960": ("s1", "neutral"),
                                             "light.exposure=-1@960": ("s2", "light.exposure=1")}, got)
            kinds = cases["skipped_counts"]
            check("e2e pair: refusals counted", kinds.get("not the clean recipe") == 1
                  and kinds.get("not a 16-bit RGB TIFF") == 1, kinds)

            def stand_in(raw, long_edge, recipe):
                """LightCraft stand-in: half Lightroom's exposure response, no contrast response."""
                return _encode(lin * 2 ** (recipe["light.exposure"] / 2))

            calls = Counter()

            def flaky(raw, long_edge, recipe):
                """Contrast always fails; Exposure +1 fails the first time (session s2 then renders it)."""
                calls[recipe["light.exposure"]] += 1
                if recipe["light.contrast"] or (recipe["light.exposure"] == 1.0 and calls[1.0] == 1):
                    raise RenderFailed("lightcraft-cli render ...\nerror: simulated failure")
                return stand_in(raw, long_edge, recipe)

            run = tmp / "run"
            for missing_pixels in (1, 2):
                run_size = tmp / f"run-size-{missing_pixels}"
                open_run(run_size, run_binding(cases_path, "selftest", "selftest"), {})
                render_cases(cases, run_size, _identity_of(run_size),
                             _StandIn(lambda raw, le, recipe: stand_in(raw, le, recipe)[:, :-missing_pixels]),
                             sorted(cases["photos"]), log=lambda *_: None)
                size_scores = score_run(cases_path, cases, run_size)
                check(f"e2e render: {missing_pixels}-pixel size mismatch is rejected with the right advice",
                      not size_scores["rows"] and len(size_scores["rejected"]) == len(photo["pairs"])
                      and all("LightCraft renders" in r["reason"] and "Lightroom exported" in r["reason"]
                              and ("docs/render-fidelity.md" in r["reason"]) == (missing_pixels == 1)
                              for r in size_scores["rejected"]), size_scores["rejected"])
            open_run(run, run_binding(cases_path, "selftest", "selftest"), {})
            render_cases(cases, run, _identity_of(run), _StandIn(flaky), sorted(cases["photos"]), log=lambda *_: None)
            check("e2e render: registered per session and reference",
                  sorted(session_label(json.loads(k)[1]) for k in json.loads((run / "registration.json").read_text()))
                  == ["s1", "s2"])
            scores = score_run(cases_path, cases, run)
            check("e2e render: a LightCraft failure leaves the pair unscored, with its reason; one whose samples "
                  "another session rendered is scored", len(scores["rows"]) == 2 and [m["reason"] for m in scores["missing"]]
                  == ["not rendered: error: simulated failure"], scores["missing"])
            render_cases(cases, run, _identity_of(run), _StandIn(stand_in), sorted(cases["photos"]), log=lambda *_: None)
            check("e2e render: a resumed run retries failures and clears them",
                  json.loads((run / "failures.json").read_text()) == {})
            scores = score_run(cases_path, cases, run)
            complete = len(scores["rows"])
            photo_id = next(iter(cases["photos"]))
            shutil.rmtree(run / "lr" / photo_id)
            shutil.rmtree(run / "lc" / photo_id)
            render_cases(cases, run, _identity_of(run), _StandIn(stand_in), sorted(cases["photos"]), log=lambda *_: None)
            check("e2e render: deleted samples, references included, are rebuilt on resume",
                  len(score_run(cases_path, cases, run)["rows"]) == complete == 3, complete)
            neutral_ref = next(q["reference"]["export"]["path"] for q in photo["pairs"] if q["reference"]["kind"] == "neutral")
            damaged = run / "lr" / photo_id / "960" / f"{lr_key(neutral_ref)}.npz"
            damaged.write_bytes(b"not a sample")
            told = score_run(cases_path, cases, run)
            rel = damaged.relative_to(run).as_posix()
            check("e2e score: a damaged sample names itself and how to rebuild it",
                  any(rel in m["reason"] and "resume" in m["reason"] for m in told["missing"]), told["missing"] + told["rejected"])
            damaged.unlink()
            render_cases(cases, run, _identity_of(run), _StandIn(stand_in), sorted(cases["photos"]), log=lambda *_: None)
            scores = score_run(cases_path, cases, run)
            check("e2e score: following that instruction gives the complete score", len(scores["rows"]) == complete)
            by = {(r["slider"], r["amount"]): r["cells"]["all"] for r in scores["rows"]}
            e1 = by.get(("light.exposure", 1.0), {})
            check("e2e score: exposure share", close(e1.get("share"), 0.5, 0.02) and close(e1.get("bias"), -0.5, 0.02), e1)
            c50 = by.get(("light.contrast", 50.0), {})
            check("e2e score: no contrast response", close(c50.get("lc"), 0.0, 1e-9) and c50.get("lr", 0) > 0.3, c50)
            em1 = by.get(("light.exposure", -1.0), {})
            check("e2e score: interval between two amounts", close(em1.get("lr"), -2.0, 0.03)
                  and close(em1.get("lc"), -1.0, 0.03), em1)
            check("e2e score: Neutral look", len(scores["neutral"]) == 1, scores["neutral"])
            report = markdown_report(scores)
            check("e2e report: no file names", "TEST0001" not in report and "#### light.exposure" in report, report[:300])
            check("e2e report: photos aren't named by their raw's hash", all(pid not in report for pid in cases["photos"]),
                  [pid for pid in cases["photos"] if pid in report])
            many = [dict(r, photo=f"{i:012x}", session=session_id(f"s{i}", 1)) for i in range(6) for r in scores["rows"][:1]]
            orders = []
            for salt in ("0" * 32, "1" * 32, "abcdef0123456789" * 2, "f" * 32):
                label = report_ids({"report_salt": salt})
                back = {label(f"{i:012x}"): i for i in range(6)}
                lines = list(csv.DictReader(io.StringIO(appendix_csv(many, label))))
                orders.append(tuple((back[x["photo"]], x["session"]) for x in lines if x["band"] == "all"))
            check("e2e report: rows and session numbers follow the salted names, not the photos' ids",
                  len(set(orders)) > 1 and len({o[0] for o in orders}) > 1, orders)
            check("e2e report: names on request", "TEST0001" in markdown_report(scores, names=True))
            check("e2e text summary", "light.contrast" in text_summary(scores))

            # provenance: two renders with different CLIs whose writes interleave on one folder (a lock that doesn't
            # exclude, as on some network shares): nothing is scored that the manifest's CLI didn't render
            run_i = tmp / "run-interleaved"
            open_run(run_i, run_binding(cases_path, "cli-A", "A"), {})
            render_cases(cases, run_i, _identity_of(run_i), _StandIn(stand_in), sorted(cases["photos"]), log=lambda *_: None)  # A: half
            run_b = tmp / "run-b-manifest"
            open_run(run_b, run_binding(cases_path, "cli-B", "B"), {})
            shutil.copy2(run_b / "manifest.json", run_i / "manifest.json")  # B's manifest write lands last
            full = _StandIn(lambda raw, le, recipe: _encode(lin * 2 ** recipe["light.exposure"]))  # B: full response
            render_cases(cases, run_i, _identity_of(run_i), full, sorted(cases["photos"]), log=lambda *_: None)
            s_i = score_run(cases_path, cases, run_i)
            exposure = [r["cells"]["all"].get("share") for r in s_i["rows"] if r["slider"] == "light.exposure"]
            check("provenance: samples another CLI wrote into the folder are never scored as this run's",
                  len(s_i["rows"]) == 3 and exposure and all(close(x, 1.0, 0.03) for x in exposure), (exposure, s_i["rejected"]))
            e1_name = lc_key({**zero, "light.exposure": 1.0}, (0, 0))
            target = next(iter(sorted((run_i / "lc" / photo_id / "960").glob(e1_name + ".*"))), None)
            if target is not None:
                shutil.copy2(run / "lc" / photo_id / "960" / target.name, target)  # A's late write of one sample
                s_i = score_run(cases_path, cases, run_i)
                exposure = [r["cells"]["all"].get("share") for r in s_i["rows"] if r["slider"] == "light.exposure"]
                check("provenance: a sample overwritten by another CLI's render is rejected, not scored",
                      all(close(x, 1.0, 0.03) for x in exposure) and len(s_i["rows"]) < 3
                      and any("sample" in x["reason"] for x in s_i["rejected"]), (exposure, s_i["rejected"]))
                try:
                    z = np.load(target)
                    plain = z["sample"] if hasattr(z, "files") else z
                except Exception:
                    plain = sample(_encode(lin * 2 ** 0.5), 960)
                with open(target, "wb") as f:
                    np.save(f, np.asarray(plain))
                s_i = score_run(cases_path, cases, run_i)
                exposure = [r["cells"]["all"].get("share") for r in s_i["rows"] if r["slider"] == "light.exposure"]
                check("provenance: a sample without provenance is rejected, not scored",
                      all(close(x, 1.0, 0.03) for x in exposure) and len(s_i["rows"]) < 3, (exposure, s_i["rejected"]))
            else:
                check("provenance: the interleaved run has the Exposure +1 sample", False, "not found")
            run_r = tmp / "run-replaced"
            open_run(run_r, run_binding(cases_path, "cli-A", "A"), {})
            ident_a = identity_of(run_r)
            open_run(tmp / "run-replaced-b", run_binding(cases_path, "cli-B", "B"), {})
            shutil.copy2(tmp / "run-replaced-b" / "manifest.json", run_r / "manifest.json")  # B's manifest replaced A's
            try:
                render_cases(cases, run_r, ident_a, _StandIn(stand_in), sorted(cases["photos"]), log=lambda *_: None)
                verdict = "rendered"
            except SystemExit as e:
                verdict = str(e)
            except Exception as e:
                verdict = f"{type(e).__name__}: {e}"
            check("provenance: a render whose manifest is replaced under it stops, stamping nothing",
                  "another render changed this run folder" in verdict and not list(run_r.rglob("*.npz")), verdict)
            run_l = tmp / "run-late"
            open_run(run_l, run_binding(cases_path, "cli-A", "A"), {})
            ident_l = identity_of(run_l)
            open_run(tmp / "run-late-b", run_binding(cases_path, "cli-B", "B"), {})
            last = {**zero, "light.exposure": -1.0}

            def late(raw, le, recipe):
                if recipe == last:  # the final render: another run's manifest lands while it runs
                    shutil.copy2(tmp / "run-late-b" / "manifest.json", run_l / "manifest.json")
                return stand_in(raw, le, recipe)
            try:
                render_cases(cases, run_l, ident_l, _StandIn(late), sorted(cases["photos"]), log=lambda *_: None)
                verdict = "finished"
            except SystemExit as e:
                verdict = str(e)
            stamped = list((run_l / "lc").rglob(lc_key(last, (0, 0)) + ".npz"))
            check("provenance: a manifest replaced during the last render stops it before it writes more",
                  "another render changed this run folder" in verdict and not stamped, (verdict, stamped))
            old_run = json.loads((run_i / "manifest.json").read_text())
            old_run["sampling_version"] = 1
            write_json(run_i / "manifest.json", old_run)
            try:
                score_run(cases_path, cases, run_i)
                verdict = "scored"
            except SystemExit as e:
                verdict = str(e)
            check("provenance: a run sampled before provenance existed is refused", "new --run folder" in verdict, verdict)
            mpath = run / "manifest.json"
            kept = mpath.read_text()
            before_check = json.loads(kept)
            before_check.pop("raw_check", None)
            before_check.pop("importer_checks", None)
            write_json(mpath, before_check)
            try:
                score_run(cases_path, cases, run)
                check("e2e score: a run rendered before the importer check existed is refused", False, "it was scored")
            except SystemExit as e:
                check("e2e score: a run rendered before the importer check existed is refused",
                      "new --run folder" in str(e), str(e))
            unchecked = json.loads(kept)
            unchecked["importer_checks"] = {}
            write_json(mpath, unchecked)
            try:
                s_un = score_run(cases_path, cases, run)
                check("e2e score: a raw without a recorded clean importer check isn't scored",
                      not s_un["rows"] and len(s_un["missing"]) == 3 and all("importer" in m["reason"] for m in s_un["missing"]),
                      (len(s_un["rows"]), s_un["missing"]))
            except SystemExit as e:
                check("e2e score: a raw without a recorded clean importer check isn't scored", False, str(e))
            mpath.write_text(kept)
            binding = run_binding(cases_path, "other build", "selftest")
            check("e2e run: refuses another build's samples", _raises(lambda: open_run(run, binding, {}), SystemExit))
            (run / "manifest.json").unlink()
            check("e2e run: refuses samples without a manifest", _raises(lambda: open_run(run, binding, {}), SystemExit))

            # the shareable report carries every populated band and all four transitions with their counts
            appendix = re.search(r"```csv\n(.*?)```", report, re.S)
            table = list(csv.DictReader(io.StringIO(appendix.group(1)))) if appendix else []
            salt = scores.get("report_salt")
            rid_of = (lambda pid: hashlib.sha256(f"{salt}/{pid}".encode()).hexdigest()[:10]) if salt else (lambda pid: pid)
            want = {(rid_of(r["photo"]), f"{r['slider']}={r['amount']:g}", r["reference"], b) for r in scores["rows"]
                    for b in r["cells"]}
            got = {(x["photo"], x["recipe"], x["reference"], x["band"]) for x in table}
            cols = {f"{e}_{k}{s}" for e in ("lr", "lc") for k in ("clip_new", "recovered", "black_new", "black_recovered")
                    for s in ("", "_of")} | {"population", "support", "outside", "outside_dE"}
            check("e2e report: appendix with every band and all four transitions with counts",
                  bool(table) and got == want and cols <= set(table[0]), (len(table), sorted(cols - set(table[0] if table else {}))))

            # a raw carrying LightCraft's own develop settings (as LightCraft's DNGs do) is refused like a Camera Raw one
            (tmp / "raws-lc").mkdir()
            (tmp / "raws-lc" / "TEST0001.ARW").write_bytes(
                b"raw bytes <x:xmpmeta><rdf:Description xmlns:lc=\"http://ns.lightcraft.app/lc/1.0/\" "
                b"lc:settings=\"{&quot;grain&quot;: {&quot;amount&quot;: 40}}\"/></x:xmpmeta>")
            ns_lc = argparse.Namespace(exports=str(tmp / "exports"), raws=[str(tmp / "raws-lc")],
                                       output=str(tmp / "cases-lc.json"), session_gap=10.0, splits=None)
            with contextlib.redirect_stdout(io.StringIO()):
                cmd_pair(ns_lc)
            lc_cases = load_cases(tmp / "cases-lc.json")
            check("e2e pair: a raw with embedded LightCraft develop settings is refused", not lc_cases["photos"]
                  and lc_cases["skipped_counts"].get("raw carries develop settings", 0) == 5, lc_cases["skipped_counts"])

            # every reference is checked against LightCraft's frame: an interval exported from 180-degree-rotated
            # copies in a session whose other interval is upright is rejected, not scored against upright renders
            (tmp / "raws-rot").mkdir()
            (tmp / "raws-rot" / "TEST0002.ARW").write_bytes(b"a second stand-in raw" * 100)
            lin_rot = _texture(640, 960, 3)

            def write_rot(name, recipe, when, rotated):
                d = tmp / "exports-rot" / "r1"
                d.mkdir(parents=True, exist_ok=True)
                data = _encode(lin_rot * 2 ** recipe.get("light.exposure", 0.0) * (1 + recipe.get("light.contrast", 0) / 100))
                x = _export_xmp("TEST0002.ARW", {**zero, **recipe}, when).encode()
                tifffile.imwrite(d / name, np.ascontiguousarray(data[::-1, ::-1] if rotated else data), photometric="rgb",
                                 metadata=None, extratags=[(700, 1, len(x), x, True), (34675, 7, len(icc), icc, True)])

            write_rot("c-50.tif", {"light.contrast": -50.0}, "2026-10-12T12:00:00+02:00", False)
            write_rot("c-25.tif", {"light.contrast": -25.0}, "2026-10-12T12:00:01+02:00", False)
            write_rot("e-2.tif", {"light.exposure": -2.0}, "2026-10-12T12:00:02+02:00", True)
            write_rot("e2.tif", {"light.exposure": 2.0}, "2026-10-12T12:00:03+02:00", True)
            rot_path = tmp / "cases-rot.json"
            with contextlib.redirect_stdout(io.StringIO()):
                cmd_pair(argparse.Namespace(exports=str(tmp / "exports-rot"), raws=[str(tmp / "raws-rot")],
                                            output=str(rot_path), session_gap=10.0, splits=None))
            rot = load_cases(rot_path)

            def same_engine(raw, long_edge, recipe):
                return _encode(lin_rot * 2 ** recipe["light.exposure"] * (1 + recipe["light.contrast"] / 100))

            run_rot = tmp / "run-rot"
            open_run(run_rot, run_binding(rot_path, "selftest", "selftest"), {})
            render_cases(rot, run_rot, _identity_of(run_rot), _StandIn(same_engine), sorted(rot["photos"]), log=lambda *_: None)
            s_rot = score_run(rot_path, rot, run_rot)
            scored = {f"{r['slider']}={r['amount']:g}" for r in s_rot["rows"]}
            refused = {x["pair"] for x in s_rot["rejected"]}
            check("e2e render: a rotated reference in a registered session is rejected, not scored",
                  scored == {"light.contrast=-50", "light.contrast=-25"}
                  and refused == {"light.exposure=-2", "light.exposure=2"}
                  and all(r["cells"]["all"]["dE"] < 0.01 for r in s_rot["rows"]),
                  (scored, refused, [round(r["cells"]["all"]["dE"], 2) for r in s_rot["rows"]]))

            # a square export rotated by 90 degrees keeps its size: the recipe of an upright Neutral is refused
            (tmp / "raws-sq").mkdir()
            (tmp / "raws-sq" / "TEST0003.ARW").write_bytes(b"a square stand-in raw" * 100)
            quarter = _texture(384, 384, 4)  # a scene symmetric under both flips: those can't reveal a quarter turn
            lin_sq = np.concatenate([np.concatenate([quarter, quarter[:, ::-1]], 1),
                                     np.concatenate([quarter[::-1], quarter[::-1, ::-1]], 1)], 0)
            for name, recipe, when, turn in (("n.tif", {}, "2026-10-12T13:00:00+02:00", 0),
                                             ("e1.tif", {"light.exposure": 1.0}, "2026-10-12T13:00:01+02:00", 1)):
                d = tmp / "exports-sq" / "q1"
                d.mkdir(parents=True, exist_ok=True)
                x = _export_xmp("TEST0003.ARW", {**zero, **recipe}, when).encode()
                img = np.ascontiguousarray(np.rot90(_encode(lin_sq * 2 ** recipe.get("light.exposure", 0.0)), turn))
                tifffile.imwrite(d / name, img, photometric="rgb", metadata=None,
                                 extratags=[(700, 1, len(x), x, True), (34675, 7, len(icc), icc, True)])
            sq_path = tmp / "cases-sq.json"
            with contextlib.redirect_stdout(io.StringIO()):
                cmd_pair(argparse.Namespace(exports=str(tmp / "exports-sq"), raws=[str(tmp / "raws-sq")],
                                            output=str(sq_path), session_gap=10.0, splits=None))
            sq = load_cases(sq_path)
            run_sq = tmp / "run-sq"
            open_run(run_sq, run_binding(sq_path, "selftest", "selftest"), {})
            render_cases(sq, run_sq, _identity_of(run_sq), _StandIn(lambda raw, le, recipe: _encode(lin_sq * 2 ** recipe["light.exposure"])),
                         sorted(sq["photos"]), log=lambda *_: None)
            s_sq = score_run(sq_path, sq, run_sq)
            check("e2e score: a square recipe export rotated by 90 degrees is rejected, not scored",
                  not s_sq["rows"] and len(s_sq["rejected"]) == 1
                  and re.search(r"rotated (90|270)", s_sq["rejected"][0]["reason"]),
                  ([round(r["cells"]["all"]["dE"], 2) for r in s_sq["rows"]], s_sq["rejected"]))

            # `render --cli ./lightcraft-cli`: the version query and the renders use that file, not one found on PATH
            if os.name != "nt":
                fake = tmp / "fake-cli"
                fake.write_text(_FAKE_CLI.replace("@PYTHON@", sys.executable).replace("@TOOLDIR@", str(TOOL.parent)))
                fake.chmod(0o755)
                here = os.getcwd()
                try:
                    os.chdir(tmp)
                    with group("relative --cli path"):
                        with contextlib.redirect_stdout(io.StringIO()):
                            cmd_render(argparse.Namespace(cases=str(cases_path), cli="./fake-cli", run=str(tmp / "run-cli"),
                                                          split=None, photos=None, sliders=None,
                                                          timeout=RENDER_TIMEOUT, check_timeout=CHECK_TIMEOUT))
                        manifest = json.loads((tmp / "run-cli" / "manifest.json").read_text())
                        done = score_run(cases_path, cases, tmp / "run-cli")
                        check("e2e render: a relative --cli path is the binary used", manifest["cli_version"] == "fake-cli 1.0"
                              and len(done["rows"]) == 3, (manifest.get("cli_version"), len(done["rows"]), done["missing"]))
                        check("e2e report: a photo's report name differs between runs",
                              report_ids(done)(photo_id) != report_ids(scores)(photo_id))
                    with group("a raw whose import changes the develop settings (here: the stand-in impo..."):
                        # a raw whose import changes the develop settings (here: the stand-in importer adds grain) is
                        # refused by the importer check, recorded in the manifest by raw and CLI hash
                        (tmp / "raws-h").mkdir()
                        (tmp / "raws-h" / "TEST0001.ARW").write_bytes(b"a hostile stand-in raw" * 50)
                        with contextlib.redirect_stdout(io.StringIO()):
                            cmd_pair(argparse.Namespace(exports=str(tmp / "exports"), raws=[str(tmp / "raws-h")],
                                                        output=str(tmp / "cases-h.json"), session_gap=10.0, splits=None))
                            cmd_render(argparse.Namespace(cases=str(tmp / "cases-h.json"), cli="./fake-cli",
                                                          run=str(tmp / "run-h"), split=None, photos=None, sliders=None,
                                                          timeout=RENDER_TIMEOUT, check_timeout=CHECK_TIMEOUT))
                        cases_h = load_cases(tmp / "cases-h.json")
                        manifest_h = json.loads((tmp / "run-h" / "manifest.json").read_text())
                        (hid, hc), = cases_h["photos"].items()
                        entry = manifest_h.get("importer_checks", {}).get(f"{hc['raw_sha256']}:{manifest_h['cli_sha256']}", {})
                        done_h = score_run(tmp / "cases-h.json", cases_h, tmp / "run-h")
                        check("e2e render: the importer check refuses a raw whose import changes its settings",
                              entry.get("clean") is False and "grain.amount" in entry.get("changed", [])
                              and not done_h["rows"] and len(done_h["rejected"]) == 3
                              and all("on import" in x["reason"] for x in done_h["rejected"]),
                              (entry, done_h["rejected"], done_h["missing"]))

                    with group("a CLI call that hangs times out: that photo is left unrendered with the ..."):
                        # a CLI call that hangs times out: that photo is left unrendered with the reason, the others are
                        # rendered, and a resumed run retries it
                        two_path = tmp / "cases-two.json"
                        write_json(two_path, {**cases, "photos": {**cases["photos"], **rot["photos"]},
                                              "sessions": {**cases["sessions"], **rot["sessions"]}})
                        two, rot_id = load_cases(two_path), next(iter(rot["photos"]))
                        limits = {"timeout": 2.0, "check_timeout": 2.0}
                        os.environ["FAKE_CLI_HANG"] = "TEST0002"
                        try:
                            with contextlib.redirect_stdout(io.StringIO()):
                                cmd_render(argparse.Namespace(cases=str(two_path), cli="./fake-cli", run=str(tmp / "run-two"),
                                                              split=None, photos=None, sliders=None, **limits))
                        finally:
                            del os.environ["FAKE_CLI_HANG"]
                        fails = json.loads((tmp / "run-two" / "failures.json").read_text()) \
                            if (tmp / "run-two" / "failures.json").exists() else {}
                        s_two = score_run(two_path, two, tmp / "run-two")
                        check("e2e render: a hanging CLI call times out; that photo is unrendered with the reason, the others "
                              "rendered", "timed out" in fails.get(rot_id, "") and len(s_two["rows"]) == 3
                              and len(s_two["missing"]) == 4
                              and all(m["photo"] == rot_id and "timed out" in m["reason"] for m in s_two["missing"]),
                              (fails, len(s_two["rows"]), s_two["missing"]))
                        with contextlib.redirect_stdout(io.StringIO()):
                            cmd_render(argparse.Namespace(cases=str(two_path), cli="./fake-cli", run=str(tmp / "run-two"),
                                                          split=None, photos=None, sliders=None, **limits))
                        s_two = score_run(two_path, two, tmp / "run-two")
                        check("e2e render: a resumed run retries the photo that timed out",
                              rot_id not in json.loads((tmp / "run-two" / "failures.json").read_text())
                              and not s_two["missing"] and {r["photo"] for r in s_two["rows"]} == {photo_id, rot_id},
                              (s_two["missing"], {r["photo"] for r in s_two["rows"]}))

                    with group("a CLI that can't be launched: recorded per photo, not an exception that ..."):
                        # a CLI that can't be launched: recorded per photo, not an exception that ends the run
                        broken = tmp / "not-a-cli"
                        broken.write_text("not a program\n")
                        broken.chmod(0o644)
                        run_b = tmp / "run-broken"
                        open_run(run_b, run_binding(cases_path, "broken", ""), {})
                        with tempfile.TemporaryDirectory() as tmp_b:
                            try:
                                render_cases(two, run_b, _identity_of(run_b), CliRenderer(broken, tmp_b), sorted(two["photos"]), log=lambda *_: None)
                                fb = json.loads((run_b / "failures.json").read_text())
                                check("e2e render: a CLI that can't be launched leaves each photo unrendered with the reason",
                                      set(fb) == set(two["photos"]) and all("can't run" in v for v in fb.values()), fb)
                            except Exception as e:
                                check("e2e render: a CLI that can't be launched leaves each photo unrendered with the reason",
                                      False, f"{type(e).__name__}: {e}")

                    with group("a render that wrote its TIFF and then failed or timed out never supplies..."):
                        # a render that wrote its TIFF and then failed or timed out never supplies a later render's pixels
                        zeros = {b[1]: 0.0 for b in BASIC}
                        raw_main = cases["photos"][photo_id]["raw"]
                        for mode, how in (("fail", "failed"), ("hang", "timed out")):
                            os.environ["FAKE_CLI_RENDER"] = f"-1:{mode},1:nothing"
                            try:
                                with tempfile.TemporaryDirectory() as tmp_r:
                                    r = CliRenderer(fake, tmp_r, 2.0, 2.0)
                                    first = _raises(lambda: r(raw_main, 960, {**zeros, "light.exposure": -1.0}), RenderFailed)
                                    try:
                                        r(raw_main, 960, {**zeros, "light.exposure": 1.0})
                                        second = "returned pixels"
                                    except RenderFailed:
                                        second = "RenderFailed"
                                    left = sorted(x.name for x in Path(tmp_r).glob("*.tif"))
                                check(f"cli: a render that wrote and then {how} doesn't supply the next render's pixels",
                                      first and second == "RenderFailed" and not left, (first, second, left))
                            except Exception as e:
                                check(f"cli: a render that wrote and then {how} doesn't supply the next render's pixels",
                                      False, f"{type(e).__name__}: {e}")
                            finally:
                                del os.environ["FAKE_CLI_RENDER"]

                    with group("CLI output that isn't UTF-8 is a recorded failure (the run goes on), nev..."):
                        # CLI output that isn't UTF-8 is a recorded failure (the run goes on), never an exception
                        os.environ["FAKE_CLI_BAD_BYTES"] = "1"
                        run_u = tmp / "run-undecodable"
                        open_run(run_u, run_binding(cases_path, "undecodable", ""), {})
                        try:
                            with tempfile.TemporaryDirectory() as tmp_u:
                                render_cases(cases, run_u, _identity_of(run_u), CliRenderer(fake, tmp_u, 2.0, 2.0), sorted(cases["photos"]),
                                             log=lambda *_: None)
                            fu = json.loads((run_u / "failures.json").read_text())
                            check("cli: importer output that isn't UTF-8 is a recorded failure, not an exception",
                                  "importer check" in fu.get(photo_id, ""), fu)
                        except Exception as e:
                            check("cli: importer output that isn't UTF-8 is a recorded failure, not an exception", False,
                                  f"{type(e).__name__}: {e}")
                        finally:
                            del os.environ["FAKE_CLI_BAD_BYTES"]
                        os.environ["FAKE_CLI_RENDER"] = "1:failbytes"
                        try:
                            with tempfile.TemporaryDirectory() as tmp_r:
                                try:
                                    CliRenderer(fake, tmp_r, 2.0, 2.0)(raw_main, 960, {**zeros, "light.exposure": 1.0})
                                    got = "no error"
                                except RenderFailed as e:
                                    got = str(e).splitlines()[-1]
                            check("cli: a failed render's message that isn't UTF-8 is shown, not raised", "boom" in got, got)
                        except Exception as e:
                            check("cli: a failed render's message that isn't UTF-8 is shown, not raised", False,
                                  f"{type(e).__name__}: {e}")
                        finally:
                            del os.environ["FAKE_CLI_RENDER"]
                        os.environ["FAKE_CLI_BAD_VERSION"] = "1"
                        try:
                            with contextlib.redirect_stdout(io.StringIO()):
                                cmd_render(argparse.Namespace(cases=str(cases_path), cli="./fake-cli", run=str(tmp / "run-v"),
                                                              split=None, photos=None, sliders=None, **limits))
                            mv = json.loads((tmp / "run-v" / "manifest.json").read_text())
                            check("cli: a version line that isn't UTF-8 is recorded, not raised", "fake-cli" in mv["cli_version"],
                                  mv["cli_version"])
                        except (Exception, SystemExit) as e:
                            check("cli: a version line that isn't UTF-8 is recorded, not raised", False,
                                  f"{type(e).__name__}: {e}")
                        finally:
                            del os.environ["FAKE_CLI_BAD_VERSION"]

                    with group("fault injection: every way the CLI can fail, for one photo of two"):
                        # fault injection: every way the CLI can fail, for one photo of two
                        outcomes = _fault_matrix(check, fake, two_path, two, rot_id, photo_id, tmp / "faults")
                        if os.environ.get("RENDER_FIDELITY_FAULT_MATRIX"):
                            print(json.dumps(outcomes, indent=1))

                    with group("the timeouts must be finite, positive and at most a day"):
                        # the timeouts must be finite, positive and at most a day
                        for flag, value in (("--timeout", "nan"), ("--timeout", "inf"), ("--timeout", "-5"), ("--timeout", "0"),
                                            ("--timeout", "1e9"), ("--check-timeout", "nan"), ("--check-timeout", "inf")):
                            target = tmp / f"run-bad-{flag.strip('-')}-{value}"
                            try:
                                with contextlib.redirect_stderr(io.StringIO()), contextlib.redirect_stdout(io.StringIO()):
                                    main(["render", str(cases_path), "--cli", "./fake-cli", "--run", str(target), flag, value])
                                verdict = "accepted"
                            except SystemExit as e:
                                verdict = f"exit {e.code}"
                            except Exception as e:
                                verdict = f"{type(e).__name__}: {e}"
                            check(f"render {flag} {value} is refused before anything runs",
                                  verdict == "exit 2" and not target.exists(), verdict)

                    with group("one run folder, one process: a second render is refused while the first ..."):
                        # one run folder, one process: a second render is refused while the first runs, which isn't affected
                        run_c = tmp / "run-concurrent"
                        first = subprocess.Popen([sys.executable, "-I", "-B", str(TOOL), "render", str(two_path), "--cli",
                                                  str(fake), "--run", str(run_c)], env=dict(os.environ, FAKE_CLI_SLOW="0.4"),
                                                 stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
                        try:
                            deadline = time.time() + 30
                            while not (run_c / "manifest.json").exists() and time.time() < deadline and first.poll() is None:
                                time.sleep(0.05)
                            try:
                                with contextlib.redirect_stdout(io.StringIO()):
                                    cmd_render(argparse.Namespace(cases=str(two_path), cli="./fake-cli", run=str(run_c),
                                                                  split=None, photos=rot_id, sliders=None,
                                                                  timeout=RENDER_TIMEOUT, check_timeout=CHECK_TIMEOUT))
                                second = "accepted"
                            except SystemExit as e:
                                second = str(e)
                            check("lock: a second render of a run folder in use is refused", "in use" in second, second)
                        finally:
                            _, err = first.communicate(timeout=120)
                        s_c = score_run(two_path, two, run_c)
                        check("lock: the first render isn't affected", first.returncode == 0 and len(s_c["rows"]) == 5
                              and not s_c["missing"], (first.returncode, err.decode("utf-8", "replace")[-300:],
                                                       len(s_c["rows"]), s_c["missing"]))

                    with group("the CLI measured is the one hashed: replacing the binary during the run ..."):
                        # the CLI measured is the one hashed: replacing the binary during the run changes nothing
                        swap = tmp / "fake-swap"
                        shutil.copy2(fake, swap)
                        swap_sha = sha256_file(swap)
                        os.environ["FAKE_CLI_SWAP_TARGET"] = str(swap)
                        try:
                            with contextlib.redirect_stdout(io.StringIO()):
                                cmd_render(argparse.Namespace(cases=str(cases_path), cli="./fake-swap", run=str(tmp / "run-swap"),
                                                              split=None, photos=None, sliders=None,
                                                              timeout=RENDER_TIMEOUT, check_timeout=CHECK_TIMEOUT))
                        finally:
                            del os.environ["FAKE_CLI_SWAP_TARGET"]
                        s_sw = score_run(cases_path, cases, tmp / "run-swap")
                        e1_sw = next((r["cells"]["all"] for r in s_sw["rows"] if r["slider"] == "light.exposure"
                                      and r["amount"] == 1.0), {})
                        check("cli: a CLI replaced during the run doesn't change what is measured",
                              sha256_file(swap) != swap_sha and len(s_sw["rows"]) == 3 and close(e1_sw.get("share"), 0.5, 0.02)
                              and json.loads((tmp / "run-swap" / "manifest.json").read_text())["cli_sha256"] == swap_sha,
                              (e1_sw.get("share"), len(s_sw["rows"])))

                    with group("a CLI whose private copy won't run is refused (no fallback to the origin..."):
                        # a CLI whose private copy won't run is refused (no fallback to the original), with advice
                        os.environ["FAKE_CLI_ONLY_AT"] = str(fake)
                        try:
                            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                                cmd_render(argparse.Namespace(cases=str(cases_path), cli="./fake-cli", run=str(tmp / "run-orig"),
                                                              split=None, photos=None, sliders=None,
                                                              timeout=RENDER_TIMEOUT, check_timeout=CHECK_TIMEOUT))
                            verdict = "rendered"
                        except SystemExit as e:
                            verdict = str(e)
                        finally:
                            del os.environ["FAKE_CLI_ONLY_AT"]
                        check("cli: a CLI whose private copy won't run is refused, with advice, never run in place",
                              "private copy" in verdict and "local folder" in verdict
                              and not (tmp / "run-orig" / "manifest.json").exists(), verdict)

                    with group("the report's salt: an empty or damaged one is refused, never used"):
                        # the report's salt: an empty or damaged one is refused, never used
                        run_s = tmp / "run-salt"
                        shutil.copytree(tmp / "run-cli", run_s)
                        (run_s / "report-salt.txt").write_text("")
                        try:
                            score_run(cases_path, cases, run_s)
                            verdict = "scored"
                        except SystemExit as e:
                            verdict = str(e)
                        check("salt: score refuses an empty salt", verdict != "scored" and "report-salt" in verdict, verdict)
                        bad = json.loads((tmp / "run-cli" / "scores.json").read_text())
                        bad["report_salt"] = ""
                        try:
                            markdown_report(bad)
                            verdict = "reported"
                        except (Exception, SystemExit) as e:
                            verdict = f"{type(e).__name__}: {e}"
                        check("salt: report refuses an invalid salt", verdict != "reported" and "salt" in verdict, verdict)

                    with group("damage inside the run-state files is reported as such, never a traceback"):
                        # damage inside the run-state files is reported as such, never a traceback
                        def first_registered(r):
                            return next(k for k, v in r.items() if isinstance(v, dict) and "dy" in v)
                        damage = {
                            "importer_checks is null": lambda m, r, f: m.__setitem__("importer_checks", None),
                            "an importer check without clean": lambda m, r, f: m["importer_checks"].__setitem__(
                                next(iter(m["importer_checks"])), {"changed": []}),
                            "a registration shift that isn't a number": lambda m, r, f: r[first_registered(r)].__setitem__("dy", "a"),
                            "a registration entry that isn't an object": lambda m, r, f: r.__setitem__(first_registered(r), 5),
                            "a failure that isn't text": lambda m, r, f: f.__setitem__("x", 5),
                            "an empty rejection reason": lambda m, r, f: r[first_registered(r)].__setitem__("rejected", ""),
                        }
                        for what, mutate in damage.items():
                            run_d = tmp / "run-damaged"
                            shutil.rmtree(run_d, ignore_errors=True)
                            shutil.copytree(tmp / "run-cli", run_d)
                            m = json.loads((run_d / "manifest.json").read_text())
                            r = json.loads((run_d / "registration.json").read_text())
                            f = json.loads((run_d / "failures.json").read_text()) if (run_d / "failures.json").exists() else {}
                            mutate(m, r, f)
                            write_json(run_d / "manifest.json", m)
                            write_json(run_d / "registration.json", r)
                            write_json(run_d / "failures.json", f)
                            try:
                                score_run(cases_path, cases, run_d)
                                verdict = "scored"
                            except SystemExit as e:
                                verdict = str(e)
                            except Exception as e:
                                verdict = f"{type(e).__name__}: {e}"
                            check(f"run state: {what} is reported as damage", "damaged" in verdict, verdict)

                    with group("a manifest creation killed between its O_EXCL create and its write (no h..."):
                        # a manifest creation killed between its O_EXCL create and its write (no hard links) leaves an empty
                        # manifest that nothing references: a resume replaces it and runs; with run state beside it, refused
                        killer = tmp / "kill-worker.py"
                        killer.write_text(_KILL_WORKER.replace("@TOOLDIR@", str(TOOL.parent)))
                        run_k = tmp / "run-killed"
                        w = subprocess.run([sys.executable, "-I", "-B", str(killer), str(cases_path), str(fake), str(run_k)],
                                           capture_output=True, timeout=120)
                        empty = (run_k / "manifest.json").exists() and (run_k / "manifest.json").stat().st_size == 0
                        try:
                            with contextlib.redirect_stdout(io.StringIO()):
                                cmd_render(argparse.Namespace(cases=str(cases_path), cli="./fake-cli", run=str(run_k), split=None,
                                                              photos=None, sliders=None, timeout=RENDER_TIMEOUT,
                                                              check_timeout=CHECK_TIMEOUT))
                            verdict = f"{len(score_run(cases_path, cases, run_k)['rows'])} rows"
                        except (Exception, SystemExit) as e:
                            verdict = f"{type(e).__name__}: {e}"
                        check("manifest: a creation killed before its write is recovered on resume",
                              w.returncode == -signal.SIGKILL and empty and verdict == "3 rows", (w.returncode, empty, verdict))
                        run_k2 = tmp / "run-killed-state"
                        run_k2.mkdir()
                        (run_k2 / "manifest.json").write_bytes(b"")
                        write_json(run_k2 / "registration.json", {})
                        try:
                            with contextlib.redirect_stdout(io.StringIO()):
                                cmd_render(argparse.Namespace(cases=str(cases_path), cli="./fake-cli", run=str(run_k2), split=None,
                                                              photos=None, sliders=None, timeout=RENDER_TIMEOUT,
                                                              check_timeout=CHECK_TIMEOUT))
                            verdict = "rendered"
                        except SystemExit as e:
                            verdict = str(e)
                        check("manifest: an empty manifest with run state beside it is still refused",
                              verdict != "rendered" and "new --run folder" in verdict, verdict)

                    with group("the version query has its own start-up allowance, apart from the importe..."):
                        # the version query has its own start-up allowance, apart from the importer check's limit
                        os.environ["FAKE_CLI_SLOW_VERSION"] = "3"
                        try:
                            with contextlib.redirect_stdout(io.StringIO()):
                                cmd_render(argparse.Namespace(cases=str(cases_path), cli="./fake-cli", run=str(tmp / "run-slow"),
                                                              split=None, photos=None, sliders=None, timeout=60.0,
                                                              check_timeout=2.0))
                            verdict = json.loads((tmp / "run-slow" / "manifest.json").read_text())["cli_version"]
                        except (Exception, SystemExit) as e:
                            verdict = f"{type(e).__name__}: {e}"
                        finally:
                            del os.environ["FAKE_CLI_SLOW_VERSION"]
                        check("cli: a CLI slow to start gets its own --version allowance", verdict == "fake-cli 1.0", verdict)

                    with group("two renders with different CLIs create the same run's manifest at once, ..."):
                        # two renders with different CLIs create the same run's manifest at once, the lock off (a share whose
                        # locks don't exclude), ordered so that the second creation lands after the first: one must refuse,
                        # and nothing may be scored under the wrong CLI's hash
                        cli_b = tmp / "fake-cli-full"
                        cli_b.write_text(fake.read_text().replace("HALF = True", "HALF = False"))
                        cli_b.chmod(0o755)
                        worker = tmp / "race-worker.py"
                        worker.write_text(_RACE_WORKER.replace("@TOOLDIR@", str(TOOL.parent)))
                        race, gates = tmp / "run-race", tmp / "race-gates"
                        gates.mkdir()
                        env_r = dict(os.environ, RENDER_FIDELITY_LOCK="off")
                        workers = {role: subprocess.Popen([sys.executable, "-I", "-B", str(worker), role, str(gates),
                                                           str(cases_path), str(path), str(race)], env=env_r,
                                                          stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
                                   for role, path in (("A", fake), ("B", cli_b))}
                        said = {role: w.communicate(timeout=180)[1].decode("utf-8", "replace") for role, w in workers.items()}
                        codes = {role: w.returncode for role, w in workers.items()}
                        s_r = score_run(cases_path, cases, race)
                        expected = 0.5 if s_r["manifest"]["cli_sha256"] == sha256_file(fake) else 1.0
                        shares = [r["cells"]["all"].get("share") for r in s_r["rows"] if r["slider"] == "light.exposure"]
                        refused = [role for role in codes if codes[role] != 0 and "another render changed this run folder"
                                   in said[role]]
                        check("provenance: of two renders creating one run folder at once, one refuses",
                              len(refused) == 1, (codes, {k: v[-200:] for k, v in said.items()}))
                        check("provenance: nothing is scored under the wrong CLI's hash",
                              shares and all(close(x, expected, 0.02) for x in shares) and len(s_r["rows"]) == 3,
                              (expected, shares, len(s_r["rows"])))

                    with group("two real processes on one folder with a lock that always succeeds (RENDE..."):
                        # two real processes on one folder with a lock that always succeeds (RENDER_FIDELITY_LOCK=off simulates
                        # a share whose locks don't exclude): lost writes leave pairs unrendered, nothing is misattributed,
                        # and a resume completes the run
                        run_n = tmp / "run-nolock"
                        env_n = dict(os.environ, FAKE_CLI_SLOW="0.3", RENDER_FIDELITY_LOCK="off")
                        first = subprocess.Popen([sys.executable, "-I", "-B", str(TOOL), "render", str(two_path), "--cli",
                                                  str(fake), "--run", str(run_n), "--photos", photo_id], env=env_n,
                                                 stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
                        try:
                            deadline = time.time() + 30
                            while not (run_n / "manifest.json").exists() and time.time() < deadline and first.poll() is None:
                                time.sleep(0.05)
                            os.environ["RENDER_FIDELITY_LOCK"] = "off"
                            try:
                                with contextlib.redirect_stdout(io.StringIO()):
                                    cmd_render(argparse.Namespace(cases=str(two_path), cli="./fake-cli", run=str(run_n),
                                                                  split=None, photos=rot_id, sliders=None,
                                                                  timeout=RENDER_TIMEOUT, check_timeout=CHECK_TIMEOUT))
                                second = "ran"
                            except SystemExit as e:
                                second = str(e)
                            finally:
                                del os.environ["RENDER_FIDELITY_LOCK"]
                        finally:
                            first.communicate(timeout=120)
                        s_n = score_run(two_path, two, run_n)
                        main_shares = [r["cells"]["all"].get("share") for r in s_n["rows"]
                                       if r["photo"] == photo_id and r["slider"] == "light.exposure"]
                        rot_errors = [r["cells"]["all"]["error"] for r in s_n["rows"] if r["photo"] == rot_id]
                        check("provenance: two processes without a working lock: nothing misattributed",
                              second == "ran" and all(close(x, 0.5, 0.02) for x in main_shares)
                              and all(x < 1e-6 for x in rot_errors), (second, main_shares, rot_errors))
                        with contextlib.redirect_stdout(io.StringIO()):
                            cmd_render(argparse.Namespace(cases=str(two_path), cli="./fake-cli", run=str(run_n), split=None,
                                                          photos=None, sliders=None, timeout=RENDER_TIMEOUT,
                                                          check_timeout=CHECK_TIMEOUT))
                        s_n = score_run(two_path, two, run_n)
                        check("provenance: a resume after them completes the run", len(s_n["rows"]) == 5 and not s_n["missing"],
                              (len(s_n["rows"]), s_n["missing"]))
                finally:
                    os.chdir(here)

    if cli:
        path = Path(cli).expanduser().resolve()
        if not (path.is_file() and os.access(path, os.X_OK)):
            check("importer cross-check: --cli is a runnable lightcraft-cli", False, str(path))
        else:
            with group("importer cross-check"):
                _importer_cross_check(path, check)
    else:
        print("selftest: importer cross-check against LightCraft's XMP reader skipped (pass --cli PATH to run it)")
    print(f"selftest: {checks - len(failures)} of {checks} checks passed")
    for f in failures:
        print("  FAIL", f)
    return 1 if failures else 0


def _raises(f, kind=Exception):
    try:
        f()
    except kind:
        return True
    return False


# ---------------------------------------------------------------------------------------------------------------

def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0],
                                 formatter_class=argparse.RawDescriptionHelpFormatter,
                                 epilog=f"Method and how to read the numbers: {DOC}")
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("plan", help="write Lightroom presets for the recipes and export instructions")
    p.add_argument("out", help="folder for presets/, recipes.json and INSTRUCTIONS.txt")
    p.add_argument("--recipe", action="append", metavar="SLIDER=A,B",
                   help="a Basic slider and its amounts, repeatable (default: every Basic slider at two amounts)")
    p = sub.add_parser("pair", help="check exports' recipes and pair them within each export session")
    p.add_argument("--exports", required=True, help="folder of Lightroom TIFF exports, one subfolder per session")
    p.add_argument("--raws", required=True, action="append", help="folder (or file) of the raws, repeatable")
    p.add_argument("-o", "--output", default="cases.json")
    p.add_argument("--session-gap", type=float, default=10.0, metavar="MIN",
                   help="split a folder into sessions where export times are further apart (0: never; default 10)")
    p.add_argument("--splits", help="JSON {photo id, raw name or stem: tune|check} overriding the hash split")
    p = sub.add_parser("render", help="render the pairs with LightCraft and sample both engines")
    p.add_argument("cases")
    p.add_argument("--cli", required=True, help="the lightcraft-cli binary to measure")
    p.add_argument("--run", required=True, help="run folder (resumed only with the same inputs)")
    p.add_argument("--split", choices=["tune", "check"])
    p.add_argument("--photos", help="photo ids or raw stems, comma-separated")
    p.add_argument("--sliders", help="LightCraft controls, comma-separated (e.g. light.whites,light.blacks)")
    p.add_argument("--timeout", type=_seconds_arg, default=RENDER_TIMEOUT, metavar="S",
                   help=f"seconds one render may take before it counts as failed (default {RENDER_TIMEOUT:g}, "
                        f"at most {MAX_TIMEOUT:g})")
    p.add_argument("--check-timeout", type=_seconds_arg, default=CHECK_TIMEOUT, metavar="S",
                   help=f"seconds for the version query and each raw's importer check (default {CHECK_TIMEOUT:g})")
    p = sub.add_parser("score", help="score a run: RUN/scores.json and RUN/summary.txt")
    p.add_argument("cases")
    p.add_argument("--run", required=True)
    p = sub.add_parser("report", help="Markdown summary of a scored run, for an issue")
    p.add_argument("--run", required=True)
    p.add_argument("--names", action="store_true", help="include the raw file names")
    p.add_argument("-o", "--output")
    p = sub.add_parser("selftest", help="check the maths and parsing on synthetic data")
    p.add_argument("--cli", help="also check the raw guard against this lightcraft-cli's own XMP reader")
    args = ap.parse_args(argv)
    if args.cmd == "selftest":
        return selftest(args.cli)
    {"plan": cmd_plan, "pair": cmd_pair, "render": cmd_render, "score": cmd_score, "report": cmd_report}[args.cmd](args)
    return 0


if __name__ == "__main__":
    sys.exit(main())
