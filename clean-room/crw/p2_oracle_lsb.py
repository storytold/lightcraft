"""P2 — the decisive low-bits test against the oracle (LibRaw unprocessed_raw, black box).

Input: a CRW file and the TIFF written by `unprocessed_raw.exe -T` for it.
Tests, per 03-payload-layout.md:
  - oracle frame size == SensorInfo raw size;
  - oracle code histogram (min/max/percentiles) -> bit depth;
  - the low-bits region equals the 2 LSBs of the oracle's codes, row-major, 4 pixels/byte,
    under one of the two packing orders (pixel0 in bits 7-6 ... pixel3 in bits 1-0, or the
    reverse); reports exact matching byte counts for both orders and both candidate
    positions of the region (EOS prefix [0:Q) / PowerShot [514:514+Q)).

Statistics and equality counts only. Usage:
  python -I p2_oracle_lsb.py file.crw file.crw.tiff
"""

import struct
import sys


def records(buf, start, length, endian, depth=0):
    if depth > 8 or length < 6:
        return
    (tbl,) = struct.unpack_from(endian + "I", buf, start + length - 4)
    if tbl + 2 > length - 4:
        return
    (n,) = struct.unpack_from(endian + "H", buf, start + tbl)
    if tbl + 2 + n * 10 > length:
        return
    for k in range(n):
        tc, ln, off = struct.unpack_from(endian + "HII", buf, start + tbl + 2 + k * 10)
        if (tc >> 14) & 3:
            continue
        if off + ln > length:
            continue
        if ((tc >> 11) & 7) in (5, 6):
            yield from records(buf, start + off, ln, endian, depth + 1)
        else:
            yield (tc & 0x3FFF, start + off, ln)


def read_tiff_gray16(path):
    """Minimal classic-TIFF reader: single image, 16-bit, strips, no compression."""
    buf = open(path, "rb").read()
    endian = {b"II": "<", b"MM": ">"}[buf[0:2]]
    (magic, ifdoff) = struct.unpack_from(endian + "HI", buf, 2)
    assert magic == 42
    (n,) = struct.unpack_from(endian + "H", buf, ifdoff)
    tags = {}
    for i in range(n):
        tag, typ, cnt, val = struct.unpack_from(endian + "HHII", buf, ifdoff + 2 + i * 12)
        tags[tag] = (typ, cnt, val, ifdoff + 2 + i * 12 + 8)
    def val(tag):
        typ, cnt, v, pos = tags[tag]
        size = {1: 1, 3: 2, 4: 4}[typ]
        if size * cnt <= 4:
            if typ == 3 and cnt == 1:
                return [struct.unpack_from(endian + "H", buf, pos)[0]]
            return [v]
        fmt = {1: "B", 3: "H", 4: "I"}[typ]
        return list(struct.unpack_from(endian + fmt * cnt, buf, v))
    w = val(256)[0]
    h = val(257)[0]
    bps = val(258)[0]
    comp = val(259)[0] if 259 in tags else 1
    assert bps == 16 and comp == 1, (bps, comp)
    offs = val(273)
    counts = val(279)
    data = b"".join(buf[o : o + c] for o, c in zip(offs, counts))
    assert len(data) >= w * h * 2, (len(data), w * h * 2)
    import array
    a = array.array("H")
    a.frombytes(data[: w * h * 2])
    if (endian == ">") != (sys.byteorder == "big"):
        a.byteswap()
    return w, h, a


def main():
    crw_path, tiff_path = sys.argv[1], sys.argv[2]
    buf = open(crw_path, "rb").read()
    endian = {b"II": "<", b"MM": ">"}[buf[0:2]]
    (hl,) = struct.unpack_from(endian + "I", buf, 2)
    r = {}
    for t, o, l in records(buf, hl, len(buf) - hl, endian):
        r.setdefault(t, (o, l))
    ro, rl = r[0x2005]
    so, sl = r[0x1031]
    v = struct.unpack_from(endian + "H" * (sl // 2), buf, so)
    w, h = v[1], v[2]
    q = w * h // 4
    data = buf[ro : ro + rl]

    tw, th, pix = read_tiff_gray16(tiff_path)
    print(f"CRW raw {w}x{h}; oracle TIFF {tw}x{th} -> {'MATCH' if (tw, th) == (w, h) else 'MISMATCH'}")
    s = sorted(pix[::997])
    print(f"oracle codes: min {min(pix)}, max {max(pix)}, p1 {s[len(s)//100]}, "
          f"median {s[len(s)//2]}, p99 {s[-len(s)//100]}")

    # build expected low-bits bytes from oracle LSBs, both packing orders
    import array
    lsb = array.array("B", (p & 3 for p in pix))
    exp_msb = bytearray(q)
    exp_lsb = bytearray(q)
    for i in range(q):
        a4 = lsb[4 * i : 4 * i + 4]
        exp_msb[i] = (a4[0] << 6) | (a4[1] << 4) | (a4[2] << 2) | a4[3]
        exp_lsb[i] = a4[0] | (a4[1] << 2) | (a4[2] << 4) | (a4[3] << 6)
    for region_name, reg in (("prefix [0:Q)", data[:q]), ("ps [514:514+Q)", data[514 : 514 + q])):
        if len(reg) < q:
            continue
        m1 = sum(1 for a, b in zip(reg, exp_msb) if a == b)
        m2 = sum(1 for a, b in zip(reg, exp_lsb) if a == b)
        print(f"{region_name}: match MSB-first {m1}/{q} ({m1 / q:.6%}), "
              f"LSB-first {m2}/{q} ({m2 / q:.6%})")


if __name__ == "__main__":
    main()
