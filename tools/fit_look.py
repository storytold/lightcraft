# Fits a camera look's 3D grid (assets/camera-looks/<model>.json, crates/pipeline/src/looks.rs) node by node to the
# camera's own conversions of raws, given what LightCraft hands the look for each of their pixels.
#
#   cargo run --release -p lightcraft-engine --example probe_look -- look:x-t5/provia 900 probe/ RAW...
#   python3 -m venv .venv-fit-look && .venv-fit-look/bin/pip install -r tools/requirements-fit-look.txt
#   .venv-fit-look/bin/python tools/fit_look.py probe/ list.txt assets/camera-looks/X-T5.json out.json
#
# PROBE_DIR holds <stem>.tif per raw from the probe_look example: the CIELAB colour each pixel hands the look, as the
# app renders the photo (the look's input is the same for every look of a model, so one probe serves them all).
# LIST names, per look, the raws to fit and the folder of the camera's conversions in that look: a line
# "#look ID" starts a block, then "STEM<TAB>JPEG_DIR" lines (JPEG_DIR/<stem>.jpg). LOOKS is the look file to start
# from: its forward matrix and grid are kept, and its nodes are the prior where the photos have no colour.
#
# Method: both images are shrunk to EDGE px (box filter; the probe in CIELAB, which it is linear in), pixels on edges
# and near the border are left out (the camera's and LightCraft's lens corrections and resampling differ there), and
# per look the nodes minimise, per Adobe RGB channel (gamma 563/256, as the look stores them),
#   sum over pixels |trilinear(grid, lab) - camera|^2 + S |second differences of the grid|^2 + P |grid - prior|^2
# through the normal equations (the pixel term scaled to 10,000 pixels, so S and P mean the same for any count).
# --val LIST reports the mean CIE76 dE of the prior and the fit on other raws. No camera maker's data is used.
#
# usage: fit_look.py PROBE_DIR LIST LOOKS.json OUT.json [--smooth S (0.3)] [--prior P (0.001)] [--edge E (300)] [--val LIST]
import json, os, sys
import numpy as np
import scipy.sparse as sp
import scipy.sparse.linalg as spl
import tifffile
from PIL import Image, ImageOps

G = 563 / 256
SRGB_TO_XYZ = np.array([[0.4124564, 0.3575761, 0.1804375], [0.2126729, 0.7151522, 0.0721750], [0.0193339, 0.1191920, 0.9503041]])
ADOBE_TO_XYZ = np.array([[0.5767309, 0.1855540, 0.1881852], [0.2973769, 0.6273491, 0.0752741], [0.0270343, 0.0706872, 0.9911085]])
SRGB_TO_ADOBE = np.linalg.inv(ADOBE_TO_XYZ) @ SRGB_TO_XYZ


def srgb_lin(u8):
    c = u8.astype(np.float64) / 255.0
    return np.where(c <= 0.04045, c / 12.92, ((c + 0.055) / 1.055) ** 2.4)


def lab_of_xyz(xyz):
    w = np.array([0.95047, 1.0, 1.08883])
    t = xyz / w
    f = np.where(t > (6 / 29) ** 3, np.cbrt(np.maximum(t, 0)), t / (3 * (6 / 29) ** 2) + 4 / 29)
    return np.stack([116 * f[..., 1] - 16, 500 * (f[..., 0] - f[..., 1]), 200 * (f[..., 1] - f[..., 2])], -1)


def pairs(probe_dir, stem, jpg_dir, edge, lax, abx):
    """(grid coordinates of the look input, target Adobe RGB encoded) for the usable pixels of one raw."""
    p = tifffile.imread(os.path.join(probe_dir, stem + ".tif")).astype(np.float64)[..., :3]
    j = ImageOps.exif_transpose(Image.open(os.path.join(jpg_dir, stem + ".jpg"))).convert("RGB")
    h, w = p.shape[:2]
    if abs(j.width / j.height - w / h) > 0.02:
        print(f"{stem}: aspect {j.size} vs {(w, h)}", file=sys.stderr)
        return None
    s = edge / max(w, h)
    size = (max(1, round(w * s)), max(1, round(h * s)))
    # the probe is linear in CIELAB: average it as encoded values would not be; shrink both with a box
    enc = np.clip(p, 0, None) ** (1 / G)
    lab_in = np.stack([enc[..., 0] * (lax[1] - lax[0]) + lax[0], enc[..., 1] * (abx[1] - abx[0]) + abx[0], enc[..., 2] * (abx[1] - abx[0]) + abx[0]], -1)
    small_in = np.stack([np.asarray(Image.fromarray(lab_in[..., k].astype(np.float32)).resize(size, Image.BOX)) for k in range(3)], -1).astype(np.float64)
    jt = np.asarray(j.resize(size, Image.BOX))
    lin = srgb_lin(jt)
    target = np.clip(lin @ SRGB_TO_ADOBE.T, 0, 1) ** (1 / G)
    lab_t = lab_of_xyz(lin @ SRGB_TO_XYZ.T)
    # leave out edges, where the two images' geometry (lens corrections, resampling) differs most
    def grad(l):
        g = np.zeros(l.shape)
        g[1:-1, 1:-1] = np.maximum(np.abs(l[2:, 1:-1] - l[:-2, 1:-1]), np.abs(l[1:-1, 2:] - l[1:-1, :-2]))
        return g
    keep = (grad(lab_t[..., 0]) < 6) & (grad(small_in[..., 0]) < 6)
    m = max(2, round(0.02 * max(size)))
    keep[:m] = keep[-m:] = False
    keep[:, :m] = keep[:, -m:] = False
    return small_in[keep], target[keep], lab_t[keep]


def weights(lab, lax, abx):
    """Sparse trilinear interpolation matrix (pixels x nodes), as `Look::lookup` interpolates (clamped)."""
    nl, nab = lax[2], abx[2]
    def loc(x, lo, hi, n):
        f = (np.clip(x, lo, hi) - lo) / ((hi - lo) / (n - 1))
        i = np.minimum(f.astype(int), n - 2)
        return i, f - i
    il, tl = loc(lab[:, 0], *lax)
    ia, ta = loc(lab[:, 1], *abx)
    ib, tb = loc(lab[:, 2], *abx)
    rows, cols, vals = [], [], []
    r = np.arange(len(lab))
    for dl in (0, 1):
        for da in (0, 1):
            for db in (0, 1):
                wgt = (tl if dl else 1 - tl) * (ta if da else 1 - ta) * (tb if db else 1 - tb)
                rows.append(r); cols.append(((il + dl) * nab + ia + da) * nab + ib + db); vals.append(wgt)
    return sp.csr_matrix((np.concatenate(vals), (np.concatenate(rows), np.concatenate(cols))), shape=(len(lab), nl * nab * nab))


def second_diff(lax, abx):
    nl, nab = lax[2], abx[2]
    idx = np.arange(nl * nab * nab).reshape(nl, nab, nab)
    rows, cols, vals, k = [], [], [], 0
    for axis in range(3):
        a = np.moveaxis(idx, axis, 0)
        lo, mid, hi = a[:-2].ravel(), a[1:-1].ravel(), a[2:].ravel()
        n = len(mid)
        r = np.arange(k, k + n)
        rows += [r, r, r]; cols += [lo, mid, hi]; vals += [np.ones(n), -2 * np.ones(n), np.ones(n)]
        k += n
    return sp.csr_matrix((np.concatenate(vals), (np.concatenate(rows), np.concatenate(cols))), shape=(k, nl * nab * nab))


def decode(hexs, off, step):
    v = np.frombuffer(bytes.fromhex(hexs), dtype=">u2").astype(np.float64)
    return (v * step + off).reshape(-1, 3)


def encode(nodes, off, step):
    q = np.clip(np.round((nodes - off) / step), 0, 65535).astype(">u2")
    return q.tobytes().hex()


def lab_of_enc(enc):
    lin = np.clip(enc, 0, 1) ** G
    return lab_of_xyz(lin @ ADOBE_TO_XYZ.T)


def main():
    a = sys.argv[1:]
    opt = {"--smooth": "0.3", "--prior": "0.001", "--edge": "300", "--val": None}
    for k in list(opt):
        if k in a:
            i = a.index(k); opt[k] = a[i + 1]; del a[i:i + 2]
    probe_dir, listing, looks_json, out = a
    smooth, prior, edge = float(opt["--smooth"]), float(opt["--prior"]), int(opt["--edge"])
    f = json.load(open(looks_json, encoding="utf-8"))
    lax, abx = tuple(f["input"]["lightness"]), tuple(f["input"]["ab"])
    off, step = f["node_offset"], f["node_step"]
    blocks, cur = {}, None
    for line in open(listing, encoding="utf-8"):
        line = line.strip()
        if line.startswith("#look "):
            cur = line.split()[1]; blocks[cur] = []
        elif line and cur:
            stem, jdir = line.split("\t")
            blocks[cur].append((stem, jdir))
    val = {}
    if opt["--val"]:
        cur = None
        for line in open(opt["--val"], encoding="utf-8"):
            line = line.strip()
            if line.startswith("#look "):
                cur = line.split()[1]; val[cur] = []
            elif line and cur:
                val[cur].append(tuple(line.split("\t")))
    D = second_diff(lax, abx)
    n = lax[2] * abx[2] * abx[2]
    for look in f["looks"]:
        lid = look["id"]
        if lid not in blocks:
            continue
        x0 = decode(look["nodes"], off, step)
        ins, tgs = [], []
        for stem, jdir in blocks[lid]:
            r = pairs(probe_dir, stem, jdir, edge, lax, abx)
            if r is not None:
                ins.append(r[0]); tgs.append(r[1])
        if sum(len(v) for v in ins) == 0:
            print(f"{lid}: no usable pixels in its {len(blocks[lid])} listed raws; its nodes are left as they were", file=sys.stderr, flush=True)
            continue
        lab, y = np.concatenate(ins), np.concatenate(tgs)
        W = weights(lab, lax, abx)
        # normalise so the regularisers mean the same whatever the pixel count: per 10k pixels
        scale = 1e4 / len(lab)
        Amat = (W.T @ W) * scale + smooth * (D.T @ D) + prior * sp.identity(n)
        solve = spl.factorized(Amat.tocsc())
        x = np.stack([solve((W.T @ y[:, k]) * scale + prior * x0[:, k]) for k in range(3)], -1)
        de_fit = np.mean(np.linalg.norm(lab_of_enc(W @ x) - lab_of_enc(y), axis=-1))
        de_old = np.mean(np.linalg.norm(lab_of_enc(W @ x0) - lab_of_enc(y), axis=-1))
        msg = f"{lid}: {len(lab)} px, train dE {de_old:.2f} -> {de_fit:.2f}"
        if lid in val:
            vi, vt = [], []
            for stem, jdir in val[lid]:
                r = pairs(probe_dir, stem, jdir, edge, lax, abx)
                if r is not None:
                    vi.append(r[0]); vt.append(r[1])
            if sum(len(v) for v in vi) == 0:
                msg += ", no usable validation pixels"
            else:
                Wv, yv = weights(np.concatenate(vi), lax, abx), np.concatenate(vt)
                msg += f", val dE {np.mean(np.linalg.norm(lab_of_enc(Wv @ x0) - lab_of_enc(yv), axis=-1)):.2f} -> {np.mean(np.linalg.norm(lab_of_enc(Wv @ x) - lab_of_enc(yv), axis=-1)):.2f}"
        print(msg, flush=True)
        look["nodes"] = encode(x, off, step)
    json.dump(f, open(out, "w", encoding="utf-8"))


if __name__ == "__main__":
    main()
