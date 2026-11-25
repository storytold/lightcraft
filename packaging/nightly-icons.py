#!/usr/bin/env python3
"""Derive the LightCraft Nightly icon set from the committed stable renders.

The stable artwork is untouched: only the background contribution changes from LightCraft amber
to the deeper Nightly amber. Every pixel is decomposed in the stable drawing's three-colour
palette (ink, paper, background) and rebuilt with the Nightly background, so the foreground lines,
the paper, the alpha and the macOS padding stay exactly as they were drawn. There is no letter,
crescent or other badge; the app and the Launcher name the channel in text.

Sources (all committed, all rendered from assets/app-icon/lightcraft.svg):
  * assets/app-icon/lightcraft.ico                    nine full-bleed frames, 16-256 px
  * assets/app-icon/lightcraft.icns                   padded macOS frames, 32-1024 px
  * assets/app-icon/hicolor/256x256/...png           the 256 px full-bleed frame
  * assets/app-icon/lightcraft-macos-512.png          the padded macOS master

Outputs:
  * assets/app-icon/lightcraft-nightly.png            full-bleed 256 px (Windows/Linux tar)
  * assets/app-icon/lightcraft-nightly.ico            nine full-bleed frames, 16-256 px
  * assets/app-icon/lightcraft-nightly-1024.png       padded macOS 1024 px master
  * assets/app-icon/lightcraft-nightly.icns           eleven padded macOS frames, 16-1024 px

Usage: python3 packaging/nightly-icons.py [--check]
`--check` re-derives into memory and compares the committed outputs' SHA-256 instead of writing.
"""
import argparse
import hashlib
import io
import struct
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ICONS = ROOT / "assets" / "app-icon"

# The stable drawing's three colours (assets/app-icon/README.md) and the Nightly background.
INK = (0x0B, 0x0B, 0x0C)
PAPER = (0xEF, 0xE9, 0xDC)
BACKGROUND = (0xF2, 0xA5, 0x16)
NIGHTLY_BACKGROUND = (0x9A, 0x66, 0x10)

ICO_FRAMES = (16, 20, 24, 32, 40, 48, 64, 128, 256)
# Padded macOS slots by pixel size: PNG slots the committed icns already holds, and the three
# small ones derived from the padded 512 px master.
ICNS_FROM_MASTER = {16: "icp4", 32: "icp5", 64: "icp6"}
# The padded frames the committed icns already holds, by slot and pixel size (16@2x, 32@2x, ...).
ICNS_EXISTING = {"ic11": 32, "ic12": 64, "ic07": 128, "ic08": 256, "ic13": 256, "ic09": 512, "ic14": 512, "ic10": 1024}


def read_png(data: bytes):
    """Decode an 8-bit RGB/RGBA, non-interlaced PNG into (width, height, RGBA bytes)."""
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a PNG")
    offset, header, compressed = 8, None, bytearray()
    while offset < len(data):
        length, kind = struct.unpack_from(">I4s", data, offset)
        body = data[offset + 8:offset + 8 + length]
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            compressed += body
        elif kind == b"IEND":
            break
        offset += 12 + length
    width, height, depth, colour, _, _, interlace = header
    if depth != 8 or colour not in (2, 6) or interlace != 0:
        raise ValueError("expected an 8-bit non-interlaced RGB/RGBA PNG")
    channels = 3 if colour == 2 else 4
    stride = width * channels
    raw = zlib.decompress(bytes(compressed))
    if len(raw) != height * (stride + 1):
        raise ValueError("unexpected PNG payload length")
    pixels = bytearray(width * height * 4)
    previous = bytearray(stride)
    for y in range(height):
        start = y * (stride + 1)
        filter_kind = raw[start]
        line = bytearray(raw[start + 1:start + 1 + stride])
        if filter_kind == 1:
            for i in range(channels, stride):
                line[i] = (line[i] + line[i - channels]) & 0xFF
        elif filter_kind == 2:
            for i in range(stride):
                line[i] = (line[i] + previous[i]) & 0xFF
        elif filter_kind == 3:
            for i in range(stride):
                left = line[i - channels] if i >= channels else 0
                line[i] = (line[i] + ((left + previous[i]) >> 1)) & 0xFF
        elif filter_kind == 4:
            for i in range(stride):
                a = line[i - channels] if i >= channels else 0
                b = previous[i]
                c = previous[i - channels] if i >= channels else 0
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[i] = (line[i] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 0xFF
        elif filter_kind != 0:
            raise ValueError("unknown PNG filter")
        previous = line
        out = y * width * 4
        if channels == 4:
            pixels[out:out + width * 4] = line
        else:
            for x in range(width):
                pixels[out + x * 4:out + x * 4 + 3] = line[x * 3:x * 3 + 3]
                pixels[out + x * 4 + 3] = 255
    return width, height, pixels


def write_png(width: int, height: int, pixels: bytearray) -> bytes:
    stride = width * 4
    raw = bytearray()
    for y in range(height):
        raw.append(0)
        raw += pixels[y * stride:(y + 1) * stride]

    def chunk(kind: bytes, body: bytes) -> bytes:
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF)

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + chunk(b"IEND", b""))


def _pairs():
    """The three two-colour mixtures the artwork can contain.

    The stable drawing is a flat amber field, then paper shapes and ink lines drawn over it, so an
    edge pixel is a blend of exactly two of the three colours (a triple junction of three opacity
    layers is not something the renderer produces). Solving all three weights at once is numerically
    unstable here — ink and paper are nearly collinear with the neutral axis, and a half-unit of
    rounding moves the ink/paper split by more than one — so each pixel is projected onto the nearest
    two-colour segment instead. That is exact for a real blend, and a neutral pixel keeps its exact
    ink/paper colour because the background's weight in that pair is zero.
    """
    return ((INK, PAPER), (INK, BACKGROUND), (PAPER, BACKGROUND))


def _project(colour, a, b):
    """`(t, residual)` for the closest point to `colour` on the segment a + t (b - a)."""
    direction = [b[i] - a[i] for i in range(3)]
    length = sum(c * c for c in direction)
    t = sum((colour[i] - a[i]) * direction[i] for i in range(3)) / length
    t = min(1.0, max(0.0, t))
    residual = sum((colour[i] - (a[i] + t * direction[i])) ** 2 for i in range(3))
    return t, residual


def background_weight(colour) -> float:
    """How much of this stable pixel is the amber field, in [0, 1]."""
    if colour == BACKGROUND:
        return 1.0
    best = (0.0, None, 0.0)
    for a, b in _pairs():
        t, residual = _project(colour, a, b)
        if best[1] is None or residual < best[2]:
            best = (t, (a, b), residual)
    t, (a, b), _ = best
    if a == BACKGROUND:
        return 1.0 - t
    if b == BACKGROUND:
        return t
    return 0.0


def recolour(pixels: bytearray) -> bytearray:
    """Rebuild every pixel with the Nightly background, keeping its ink and paper exactly.

    The field's share of a blend pixel moves from BACKGROUND to NIGHTLY_BACKGROUND along that one
    axis, so anti-aliasing against the new field is what the drawing would have looked like on it.
    """
    shift = [NIGHTLY_BACKGROUND[i] - BACKGROUND[i] for i in range(3)]
    out = bytearray(len(pixels))
    for i in range(0, len(pixels), 4):
        alpha = pixels[i + 3]
        if alpha == 0:
            out[i + 3] = 0
            continue
        colour = (pixels[i], pixels[i + 1], pixels[i + 2])
        weight = background_weight(colour)
        out[i:i + 3] = bytes(min(255, max(0, round(colour[j] + weight * shift[j]))) for j in range(3))
        out[i + 3] = alpha
    return out


def downsample(width: int, height: int, pixels: bytearray, size: int) -> bytearray:
    """Box-filter `pixels` to `size` x `size`: only used to shrink the padded master, never to grow a frame."""
    if size > width or size > height:
        raise ValueError("a frame is never upscaled")
    factor = width // size
    if factor * size != width or height != width:
        raise ValueError("the master must divide evenly into each slot")
    out = bytearray(size * size * 4)
    area = factor * factor
    for y in range(size):
        for x in range(size):
            weighted = [0, 0, 0]
            alpha = 0
            for dy in range(factor):
                row = ((y * factor + dy) * width + x * factor) * 4
                for dx in range(factor):
                    at = row + dx * 4
                    # PNG stores straight alpha, so the colour is its alpha-weighted share of the
                    # block: averaging RGB by area would let transparent padding darken the edge.
                    coverage = pixels[at + 3]
                    for channel in range(3):
                        weighted[channel] += pixels[at + channel] * coverage
                    alpha += coverage
            at = (y * size + x) * 4
            if alpha == 0:
                continue
            out[at] = round(weighted[0] / alpha)
            out[at + 1] = round(weighted[1] / alpha)
            out[at + 2] = round(weighted[2] / alpha)
            out[at + 3] = round(alpha / area)
    return out


def read_ico(data: bytes):
    """Every PNG frame of an .ico as {size: png bytes}."""
    if data[:4] != b"\x00\x00\x01\x00":
        raise ValueError("not an .ico")
    count = struct.unpack_from("<H", data, 4)[0]
    frames = {}
    for index in range(count):
        width, height, _, _, _, _, size, offset = struct.unpack_from("<BBBBHHII", data, 6 + 16 * index)
        body = data[offset:offset + size]
        if body[:8] != b"\x89PNG\r\n\x1a\n":
            raise ValueError("only PNG-compressed .ico frames are expected")
        size_px = width or 256
        if height and height != width:
            raise ValueError("square .ico frames are expected")
        frames[size_px or 256] = body
    return frames


def write_ico(frames) -> bytes:
    header = struct.pack("<HHH", 0, 1, len(frames))
    entries, bodies, offset = bytearray(), bytearray(), 6 + 16 * len(frames)
    for size in sorted(frames):
        body = frames[size]
        width = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", width, width, 0, 0, 1, 32, len(body), offset)
        bodies += body
        offset += len(body)
    return header + bytes(entries) + bytes(bodies)


def read_icns(data: bytes):
    """The PNG slots of an .icns as {four-letter type: png bytes}, in file order."""
    if data[:4] != b"icns":
        raise ValueError("not an .icns")
    offset, slots = 8, {}
    while offset < len(data):
        kind, length = struct.unpack_from(">4sI", data, offset)
        body = data[offset + 8:offset + length]
        if body[:8] == b"\x89PNG\r\n\x1a\n":
            slots[kind.decode("ascii")] = body
        offset += length
    return slots


def write_icns(slots) -> bytes:
    order = ["icp4", "icp5", "icp6", "ic07", "ic08", "ic09", "ic10", "ic11", "ic12", "ic13", "ic14"]
    body = bytearray()
    for kind in order:
        payload = slots[kind]
        body += kind.encode("ascii") + struct.pack(">I", len(payload) + 8) + payload
    return b"icns" + struct.pack(">I", len(body) + 8) + bytes(body)


def derive():
    """Build every Nightly artifact in memory: {relative path: bytes}."""
    outputs = {}
    ico = read_ico((ICONS / "lightcraft.ico").read_bytes())
    nightly_frames = {}
    for size in ICO_FRAMES:
        width, height, pixels = read_png(ico[size])
        if width != size:
            raise ValueError(f"the {size} px .ico frame is {width} px")
        nightly_frames[size] = write_png(width, height, recolour(pixels))
    outputs["lightcraft-nightly.ico"] = write_ico(nightly_frames)

    two_fifty_six = read_png((ICONS / "hicolor/256x256/apps/ai.storyteller.lightcraft.png").read_bytes())
    outputs["lightcraft-nightly.png"] = write_png(two_fifty_six[0], two_fifty_six[1], recolour(two_fifty_six[2]))

    master = read_png((ICONS / "lightcraft-macos-512.png").read_bytes())
    recoloured_master = recolour(master[2])
    existing = read_icns((ICONS / "lightcraft.icns").read_bytes())
    slots = {}
    for size, kind in ICNS_FROM_MASTER.items():
        slots[kind] = write_png(size, size, downsample(master[0], master[1], recoloured_master, size))
    for kind, size in ICNS_EXISTING.items():
        width, height, pixels = read_png(existing[kind])
        if width != size or height != size:
            raise ValueError(f"the {kind} slot is {width}x{height}, not {size}x{size}")
        slots[kind] = write_png(width, height, recolour(pixels))
        if size == 1024:
            outputs["lightcraft-nightly-1024.png"] = slots[kind]
    outputs["lightcraft-nightly.icns"] = write_icns(slots)
    return outputs


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="compare the committed outputs instead of writing")
    args = parser.parse_args()
    outputs = derive()
    failed = False
    for name, content in sorted(outputs.items()):
        path = ICONS / name
        digest = hashlib.sha256(content).hexdigest()
        if args.check:
            current = hashlib.sha256(path.read_bytes()).hexdigest() if path.exists() else "missing"
            status = "ok" if current == digest else "differs"
            failed |= current != digest
            print(f"{status:8} {name} {digest}")
        else:
            path.write_bytes(content)
            print(f"wrote    {name} {digest} ({len(content)} bytes)")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
