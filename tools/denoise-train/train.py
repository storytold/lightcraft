"""Train LightCraft's AI Denoise model on clean CC0 raws with synthetic, calibrated sensor noise.

Offline training tooling; not part of LightCraft (Python is never in the product).

Data: the clean pictures `fetch_rpu.py` makes (each 2 x 2 Bayer cell of a low-ISO CC0 raw from raw.pixls.us became
one camera-RGB pixel, so the reference is clean and needs no demosaicing). For each patch:

1. the clean camera-RGB patch (2t x 2t) is flipped/rotated, and sometimes 2x downscaled for denser detail;
2. it is darkened by a random exposure `k` (pushed shadows, dim rooms) and re-mosaicked into the RGGB planes the
   model takes (t x t cells);
3. heteroscedastic Poisson-Gaussian noise `variance = a * x + b` is added, with `a` spread over ISO 50 to beyond
   102400 and `b` following a read-noise relation `b = c * a^2` (jittered), plus a little row noise; the sensor
   clips at 1. Without `--noise-cal`, `c` is the built-in EOS R5 C fit (a = 2.1e-7 x ISO, b ~ 1.25e-6 x
   (ISO / 3200)^2). With `--noise-cal <calibrate_noise.py output>`, `c` is fitted to the file's cameras and a share
   of each batch (`--cal-share`, default 0.5) gets one measured camera and ISO's per-plane `a`, `b` exactly.

The model (model.py) gives camera RGB at the mosaic's resolution, compared with the clean patch x k in a
gamma-like space (shadow noise is what people see after brightening) plus a small linear term. Clipped cells are
left out of the loss.

    python train.py --data <scratch>/data --out <scratch>/run1 [--steps 200000] [--duty 0.75] [--noise-cal cam.json]

`--duty` paces the graphics card (0.75: it rests a quarter of the time) so other work on the machine keeps
going. Checkpoints are written every `--ckpt` steps; `--resume` continues the latest one.
"""

import argparse
import csv
import glob
import json
import math
import os
import random
import sys
import time

import numpy as np
import torch
import torch.nn.functional as F

import model as M

GAMMA = 1 / 2.2


# ----------------------------------------------------------------------------------------------------------------------
# data


class Pics:
    def __init__(self, data, names):
        self.items = []
        for n in names:
            meta = json.load(open(os.path.join(data, "pics", n + ".json")))
            w, h = meta["width"], meta["height"]
            self.items.append((os.path.join(data, "pics", n + ".u16"), w, h))
        self.maps = {}

    def map(self, i):
        m = self.maps.get(i)
        if m is None:
            path, w, h = self.items[i]
            m = np.memmap(path, dtype="<u2", mode="r", shape=(3, h, w))
            self.maps[i] = m
        return m


def texture(p):
    """Mean absolute gradient of a (3, s, s) patch in gamma space."""
    g = np.power(np.clip(p, 0, 1) + 1e-3, GAMMA)
    return float(np.abs(np.diff(g, axis=1)).mean() + np.abs(np.diff(g, axis=2)).mean())


class PatchSet(torch.utils.data.IterableDataset):
    def __init__(self, pics, side, seed):
        self.pics, self.side, self.seed = pics, side, seed

    def __iter__(self):
        info = torch.utils.data.get_worker_info()
        rng = random.Random(self.seed + (info.id if info else 0) * 7919 + int(time.time()))
        while True:
            p = self.patch(rng)
            if p is not None:
                yield p

    def patch(self, rng):
        i = rng.randrange(len(self.pics.items))
        _, w, h = self.pics.items[i]
        scale = 2 if rng.random() < 0.3 else 1
        s = self.side * scale
        if w < s + 8 or h < s + 8:
            return None
        x, y = rng.randrange(4, w - s - 4), rng.randrange(4, h - s - 4)
        raw = np.asarray(self.pics.map(i)[:, y:y + s, x:x + s], dtype=np.uint16)
        clipped = (raw == 65535).any(0)
        if clipped.mean() > 0.05:
            return None
        p = raw.astype(np.float32) / 65534.0
        valid = (~clipped).astype(np.float32)
        if scale == 2:
            p = p.reshape(3, self.side, 2, self.side, 2).mean((2, 4))
            valid = (valid.reshape(self.side, 2, self.side, 2).min((1, 3)))
        mean = float(p.mean())
        if mean < 0.004:
            return None
        # favour texture, keep some flat patches (skin, skies, walls)
        if rng.random() > 0.3 + texture(p) / 0.04:
            return None
        k = rng.randrange(4)
        p, valid = np.rot90(p, k, (1, 2)), np.rot90(valid, k)
        if rng.random() < 0.5:
            p, valid = p[:, :, ::-1], valid[:, ::-1]
        return torch.from_numpy(np.ascontiguousarray(p)), torch.from_numpy(np.ascontiguousarray(valid))


# ----------------------------------------------------------------------------------------------------------------------
# noise


def mosaic(rgb):
    """(n, 3, 2t, 2t) camera RGB -> (n, 4, t, t) RGGB planes."""
    return torch.stack([rgb[:, 0, 0::2, 0::2], rgb[:, 1, 0::2, 1::2], rgb[:, 1, 1::2, 0::2], rgb[:, 2, 1::2, 1::2]], 1)


PLANES = ("R", "G1", "G2", "B")  # the order of mosaic()'s planes and of calibrate_noise.py's names
# the built-in read-noise relation b = c a^2: the EOS R5 C fit b = 1.25e-6 at a = 6.6e-4 (ISO 3200)
R5C_READ_C = 1.25e-6 / 6.6e-4 ** 2
READ_FLOOR = 2e-8  # a quantisation-like floor under every read noise


def read_noise(a, g, c=R5C_READ_C):
    """b for shot-noise coefficient a: the relation b = c a^2 (read noise in electrons stays put while the gain, and
    with it a, rises), jittered (other sensors have relatively more or less read noise), plus a floor."""
    b = c * a ** 2 * torch.exp(torch.randn(a.shape, generator=g, device=a.device) * 0.9)
    return b + READ_FLOOR


class CalibrationError(ValueError):
    """A noise calibration file that can't be trained on, with what is wrong and where."""


def _number(v, where, positive):
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        raise CalibrationError(f"{where} is {v!r}, not a number")
    v = float(v)
    if not math.isfinite(v):
        raise CalibrationError(f"{where} is {v}, not a finite number")
    if v < 0 or (positive and v == 0):
        raise CalibrationError(f"{where} is {v}; it must be {'above' if positive else 'at least'} 0")
    return v


def load_calibration(path):
    """Read and check a `calibrate_noise.py` file: a list of measured (camera, ISO) entries, each with per-plane
    `a`, `b` in mosaic() order. Anything that would put a wrong number into training raises CalibrationError: an
    unreadable or empty file, a missing key, NaN or infinity, a negative value, a shot-noise slope of 0. A plane
    the calibration left out (too few flat areas in the photos) takes the mean of the measured planes, and the entry
    says so in `filled` (it is logged)."""
    try:
        with open(path, encoding="utf8") as f:
            text = f.read()
    except OSError as e:
        raise CalibrationError(f"{path}: can't be read: {e}") from e

    def no_constant(name):
        raise CalibrationError(f"{path}: holds {name}, not a finite number")

    try:
        doc = json.loads(text, parse_constant=no_constant)
    except json.JSONDecodeError as e:
        raise CalibrationError(f"{path}: not JSON ({e})") from e
    if not isinstance(doc, dict) or not isinstance(doc.get("cameras"), dict):
        raise CalibrationError(f'{path}: no "cameras" object (is it a calibrate_noise.py output?)')
    if not doc["cameras"]:
        raise CalibrationError(f'{path}: "cameras" is empty')
    entries = []
    for cam, isos in sorted(doc["cameras"].items()):
        if not isinstance(isos, dict) or not isos:
            raise CalibrationError(f"{path}: camera {cam!r} has no ISOs")
        for iso_key, e in sorted(isos.items()):
            where = f"{path}: {cam} ISO {iso_key}"
            try:
                iso = int(iso_key)
            except ValueError:
                raise CalibrationError(f"{where}: the ISO is not a whole number") from None
            if iso <= 0:
                raise CalibrationError(f"{where}: the ISO must be above 0")
            if not isinstance(e, dict):
                raise CalibrationError(f"{where}: not an object")
            unknown = sorted(set(e) - set(PLANES) - {"photos"})
            if unknown:
                raise CalibrationError(f"{where}: unknown keys {unknown} (planes are {', '.join(PLANES)})")
            measured = {}
            for pl in PLANES:
                if pl not in e:
                    continue
                v = e[pl]
                if not isinstance(v, dict) or "a" not in v or "b" not in v:
                    raise CalibrationError(f'{where} {pl}: needs both "a" and "b"')
                measured[pl] = (_number(v["a"], f"{where} {pl} a", True), _number(v["b"], f"{where} {pl} b", False))
            if not measured:
                raise CalibrationError(f"{where}: no plane has a measurement")
            ma = sum(v[0] for v in measured.values()) / len(measured)
            mb = sum(v[1] for v in measured.values()) / len(measured)
            filled = [pl for pl in PLANES if pl not in measured]
            entries.append({"camera": cam, "iso": iso, "photos": e.get("photos"),
                            "a": [measured.get(pl, (ma, mb))[0] for pl in PLANES],
                            "b": [measured.get(pl, (ma, mb))[1] for pl in PLANES], "filled": filled})
    return sorted(entries, key=lambda e: (e["camera"], e["iso"]))


def fit_read_c(entries):
    """c of b = c a^2 over the measured entries (geometric mean over ISOs, planes averaged). Entries whose read
    noise is 0 (too small for the photos to resolve) say nothing about c; all of them 0 is an error."""
    logs = []
    for e in entries:
        a, b = sum(e["a"]) / 4, sum(e["b"]) / 4
        if b > 0:
            logs.append(math.log(b / a ** 2))
    if not logs:
        raise CalibrationError("no camera in the calibration has a measured read noise (b = 0 at every ISO)")
    return math.exp(sum(logs) / len(logs))


class NoiseModel:
    """Where each training patch's noise parameters come from: the built-in relation, or a calibration file."""

    def __init__(self, entries=None, share=0.5, source=None):
        if not (math.isfinite(share) and 0.0 <= share <= 1.0):
            raise CalibrationError(f"--cal-share is {share}; it must be between 0 and 1")
        self.entries = entries or []
        self.source = source
        self.share = share if self.entries else 0.0
        self.c = fit_read_c(self.entries) if self.entries else R5C_READ_C
        self.table_a = torch.tensor([e["a"] for e in self.entries], dtype=torch.float32) if self.entries else None
        self.table_b = torch.tensor([e["b"] for e in self.entries], dtype=torch.float32) if self.entries else None

    @classmethod
    def from_file(cls, path, share=0.5):
        return cls(load_calibration(path), share, source=path)

    def describe(self):
        """The log lines that say which noise the run trains on."""
        if not self.entries:
            return ["noise: no --noise-cal given; synthetic noise uses the built-in EOS R5 C read-noise relation "
                    f"b = {self.c:.4g} a^2"]
        lines = [f"noise: calibration {self.source}: {len(self.entries)} camera/ISO entries; "
                 f"{self.share:.0%} of patches use a measured entry exactly, the rest the fitted relation "
                 f"b = {self.c:.4g} a^2 (built-in EOS R5 C: {R5C_READ_C:.4g})"]
        for e in self.entries:
            note = f"; {', '.join(e['filled'])} not measured, mean of the others used" if e["filled"] else ""
            lines.append(f"noise:   {e['camera']} ISO {e['iso']}: a = {sum(e['a']) / 4:.3e}, "
                         f"b = {sum(e['b']) / 4:.3e} (plane mean){note}")
        return lines

    def summary(self):
        """A JSON-able record of the noise source, for log.jsonl."""
        return {"source": self.source, "share": self.share, "read_c": self.c,
                "entries": [{k: e[k] for k in ("camera", "iso", "a", "b", "filled")} for e in self.entries]}

    def sample(self, a, g):
        """Per-plane (n, 4) a and b for shot-noise coefficients a (n,) drawn by sample_conditions."""
        n = a.shape[0]
        b = read_noise(a, g, self.c)
        a4, b4 = a.view(-1, 1).expand(n, 4), b.view(-1, 1).expand(n, 4)
        if self.share > 0:
            dev = a.device
            use = (torch.rand(n, generator=g, device=dev) < self.share).view(-1, 1)
            pick = torch.randint(0, self.table_a.shape[0], (n,), generator=g, device=dev)
            a4 = torch.where(use, self.table_a.to(dev)[pick], a4)
            b4 = torch.where(use, self.table_b.to(dev)[pick], b4)
        return a4.contiguous(), b4.contiguous()


def add_noise(clean, a, b, g, row=True):
    """Poisson-Gaussian noise on (n, 4, t, t) planes, white = 1; a, b of shape (n,) or per plane (n, 4)."""
    count = clean.shape[0]
    a4, b4 = a.reshape(count, -1, 1, 1), b.reshape(count, -1, 1, 1)
    var = (a4 * clean.clamp(min=0) + b4).clamp(min=0)
    noisy = clean + var.sqrt() * torch.randn(clean.shape, generator=g, device=clean.device)
    if row:
        # row offsets, a fraction of the read noise, on some frames
        on = (torch.rand(count, generator=g, device=clean.device) < 0.3).float().view(-1, 1, 1, 1)
        rows = torch.randn(count, 1, clean.shape[2], 1, generator=g, device=clean.device)
        noisy = noisy + on * 0.3 * b4.sqrt() * rows
    return noisy.clamp(max=1.0)


def sample_conditions(n, g, device):
    """Shot-noise coefficient a (ISO ~50 .. ~100k on a full-frame sensor) and exposure k (1/16 .. 1.2)."""
    u = torch.rand(n, generator=g, device=device)
    a = torch.exp(math.log(1e-5) + u * (math.log(2.5e-2) - math.log(1e-5)))
    v = torch.rand(n, generator=g, device=device)
    k = torch.exp(math.log(1 / 16) + v * (math.log(1.2) - math.log(1 / 16)))
    return a, k


def make_batch(clean_rgb, valid, g, colour_jitter=True, noise=None):
    n = clean_rgb.shape[0]
    dev = clean_rgb.device
    if colour_jitter:
        gains = torch.ones(n, 3, 1, 1, device=dev)
        gains[:, 0] = 0.7 + 0.6 * torch.rand(n, 1, 1, generator=g, device=dev)
        gains[:, 2] = 0.7 + 0.6 * torch.rand(n, 1, 1, generator=g, device=dev)
        clean_rgb = clean_rgb * gains
    a, k = sample_conditions(n, g, dev)
    a, b = (noise or NoiseModel()).sample(a, g)
    target = clean_rgb * k.view(-1, 1, 1, 1)
    valid = valid * (target.max(1).values < 0.98).float()
    noisy = add_noise(mosaic(target), a, b, g)
    return noisy, target, valid, k


def tone(x):
    return (x.clamp(min=0) + 1e-3) ** GAMMA


def loss_fn(out, target, valid, k):
    kk = k.view(-1, 1, 1, 1)
    o, t = out / kk, target / kk
    m = valid.unsqueeze(1)
    denom = m.sum().clamp(min=1) * 3
    l_tone = ((tone(o) - tone(t)).abs() * m).sum() / denom
    l_lin = ((o - t).abs() * m).sum() / denom
    return l_tone + 0.25 * l_lin


def psnr(out, target, valid, k):
    kk = k.view(-1, 1, 1, 1)
    m = valid.unsqueeze(1)
    mse = (((tone(out / kk) - tone(target / kk)) ** 2) * m).sum() / (m.sum() * 3).clamp(min=1)
    return float(-10 * torch.log10(mse.clamp(min=1e-12)))


# ----------------------------------------------------------------------------------------------------------------------
# validation: fixed patches at fixed noise levels


VAL_LEVELS = {  # name: (a, b, k)  -- EOS R5 C calibration, extrapolated above ISO 3200
    "iso800": (1.8e-4, 2.1e-8, 1.0),
    "iso3200": (6.6e-4, 1.25e-6, 1.0),
    "iso12800": (2.65e-3, 2.0e-5, 1.0),
    "iso25600-dark": (5.3e-3, 8.0e-5, 0.25),
}


def fixed_val(pics, side, count, device):
    ds = PatchSet(pics, side, seed=12345)
    rng = random.Random(4242)
    out = []
    while len(out) < count:
        p = ds.patch(rng)
        if p is not None:
            out.append(p)
    rgb = torch.stack([o[0] for o in out]).to(device)
    valid = torch.stack([o[1] for o in out]).to(device)
    sets = {}
    for name, (a, b, k) in VAL_LEVELS.items():
        g = torch.Generator(device=device).manual_seed(99)
        n = rgb.shape[0]
        target = rgb * k
        v = valid * (target.max(1).values < 0.98).float()
        aa, bb = torch.full((n,), a, device=device), torch.full((n,), b, device=device)
        noisy = add_noise(mosaic(target), aa, bb, g, row=False)
        sets[name] = (noisy, target, v, torch.full((n,), k, device=device))
    return sets


def bilinear_baseline(noisy):
    """Plain bilinear demosaic of the noisy planes (what no denoising looks like)."""
    r, g1, g2, b = noisy[:, 0:1], noisy[:, 1:2], noisy[:, 2:3], noisy[:, 3:4]
    up = lambda x: F.interpolate(x, scale_factor=2, mode="bilinear", align_corners=False)
    return torch.cat([up(r), up(0.5 * (g1 + g2)), up(b)], 1)


def autocast(device):
    """bfloat16 on the graphics card; full precision on the CPU (the smoke-test path)."""
    return torch.autocast(torch.device(device).type, dtype=torch.bfloat16, enabled=torch.device(device).type == "cuda")


@torch.no_grad()
def evaluate(net, sets, bs=8):
    net.eval()
    res = {}
    for name, (noisy, target, valid, k) in sets.items():
        outs = []
        for i in range(0, noisy.shape[0], bs):
            with autocast(noisy.device):
                outs.append(net(noisy[i:i + bs]).float())
        out = torch.cat(outs)
        res[name] = round(psnr(out, target, valid, k), 3)
        res[name + "-noisy"] = round(psnr(bilinear_baseline(noisy), target, valid, k), 3)
    net.train()
    return res


# ----------------------------------------------------------------------------------------------------------------------


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--steps", type=int, default=200000)
    ap.add_argument("--batch", type=int, default=8)
    ap.add_argument("--cells", type=int, default=128, help="patch side in Bayer cells (output is twice that)")
    ap.add_argument("--lr", type=float, default=3e-4)
    ap.add_argument("--duty", type=float, default=0.75)
    ap.add_argument("--ckpt", type=int, default=2000)
    ap.add_argument("--val-every", type=int, default=2000)
    ap.add_argument("--widths", default="32,64,128,256,512")
    ap.add_argument("--workers", type=int, default=3)
    ap.add_argument("--resume", action="store_true")
    ap.add_argument("--noise-cal", help="a calibrate_noise.py file to take the noise from (default: the built-in "
                    "EOS R5 C relation)")
    ap.add_argument("--cal-share", type=float, default=0.5,
                    help="with --noise-cal, the share of patches that get a measured camera/ISO's noise exactly")
    ap.add_argument("--device", default="cuda", help="cuda (default) or cpu (slow; for a smoke test)")
    ap.add_argument("--log-every", type=int, default=200, help="steps between loss lines")
    a = ap.parse_args()
    if a.log_every < 1:
        ap.error("--log-every must be at least 1")
    # a bad calibration stops the run here, before any training, with what is wrong in it
    try:
        noise = NoiseModel.from_file(a.noise_cal, a.cal_share) if a.noise_cal else NoiseModel(share=a.cal_share)
    except CalibrationError as e:
        print(f"train.py: {e}", file=sys.stderr)
        return 2
    os.makedirs(a.out, exist_ok=True)
    torch.set_num_threads(2)
    dev = a.device
    cuda = torch.device(dev).type == "cuda"
    for line in noise.describe():
        print(line, flush=True)

    names = sorted(os.path.basename(p)[:-4] for p in glob.glob(os.path.join(a.data, "pics", "*.u16")))
    if os.path.exists(os.path.join(a.data, "sources.csv")):
        listed = {r["file"] for r in csv.DictReader(open(os.path.join(a.data, "sources.csv"), encoding="utf8"))}
        names = [n for n in names if n in listed]
    rnd = random.Random(7)
    rnd.shuffle(names)
    n_val = max(4, len(names) // 20)
    split = {"val": sorted(names[:n_val]), "train": sorted(names[n_val:])}
    split_path = os.path.join(a.out, "split.json")
    if a.resume and os.path.exists(split_path):
        old = json.load(open(split_path))
        # keep validation pictures out of training; new pictures join training
        split["val"] = old["val"]
        split["train"] = sorted(set(names) - set(old["val"]))
    json.dump(split, open(split_path, "w"))
    print(f"{len(split['train'])} training pictures, {len(split['val'])} validation", flush=True)

    widths = tuple(int(x) for x in a.widths.split(","))
    net = M.BayerUNet(widths).to(dev)
    ema = M.BayerUNet(widths).to(dev)
    ema.load_state_dict(net.state_dict())
    ema.requires_grad_(False)
    opt = torch.optim.AdamW(net.parameters(), lr=a.lr, weight_decay=1e-4, betas=(0.9, 0.99))
    step = 0
    ck = os.path.join(a.out, "last.pt")
    if a.resume and os.path.exists(ck):
        s = torch.load(ck, map_location=dev)
        net.load_state_dict(s["net"]); ema.load_state_dict(s["ema"]); opt.load_state_dict(s["opt"]); step = s["step"]
        print(f"resumed at step {step}", flush=True)

    train_pics = Pics(a.data, split["train"])
    val_pics = Pics(a.data, split["val"])
    side = 2 * a.cells
    val_sets = fixed_val(val_pics, side, 48, dev)
    loader = torch.utils.data.DataLoader(PatchSet(train_pics, side, seed=step), batch_size=a.batch,
                                         num_workers=a.workers, persistent_workers=a.workers > 0, pin_memory=cuda)
    g = torch.Generator(device=dev).manual_seed(1000 + step)
    log = open(os.path.join(a.out, "log.jsonl"), "a")
    log.write(json.dumps({"step": step, "noise": noise.summary(), "time": time.time()}) + "\n"); log.flush()
    t0, run_loss, n_loss = time.time(), 0.0, 0
    warm = 2000
    for rgb, valid in loader:
        if step >= a.steps:
            break
        started = time.time()
        rgb, valid = rgb.to(dev, non_blocking=True), valid.to(dev, non_blocking=True)
        noisy, target, v, k = make_batch(rgb, valid, g, noise=noise)
        lr = a.lr * min(1.0, (step + 1) / warm) * (0.02 + 0.98 * 0.5 * (1 + math.cos(math.pi * min(1.0, step / a.steps))))
        for pg in opt.param_groups:
            pg["lr"] = lr
        with autocast(dev):
            out = net(noisy)
        loss = loss_fn(out.float(), target, v, k)
        opt.zero_grad(set_to_none=True)
        loss.backward()
        torch.nn.utils.clip_grad_norm_(net.parameters(), 1.0)
        opt.step()
        with torch.no_grad():
            d = min(0.999, (1 + step) / (10 + step))
            for pe, pn in zip(ema.parameters(), net.parameters()):
                pe.mul_(d).add_(pn.detach(), alpha=1 - d)
        step += 1
        run_loss += loss.item(); n_loss += 1
        if a.duty < 1.0 and cuda:
            torch.cuda.synchronize()
            spent = time.time() - started
            time.sleep(spent * (1 - a.duty) / max(a.duty, 0.05))
        if step % a.log_every == 0:
            rate = n_loss / max(time.time() - t0, 1e-9)
            print(f"step {step} loss {run_loss / n_loss:.5f} lr {lr:.2e} {rate:.1f} it/s", flush=True)
            log.write(json.dumps({"step": step, "loss": run_loss / n_loss, "lr": lr, "time": time.time()}) + "\n"); log.flush()
            t0, run_loss, n_loss = time.time(), 0.0, 0
        if step % a.val_every == 0:
            r = evaluate(ema, val_sets)
            print(f"val {step} {r}", flush=True)
            log.write(json.dumps({"step": step, "val": r, "time": time.time()}) + "\n"); log.flush()
        if step % a.ckpt == 0 or step == a.steps:
            torch.save({"net": net.state_dict(), "ema": ema.state_dict(), "opt": opt.state_dict(), "step": step,
                        "widths": widths}, ck + ".tmp")
            os.replace(ck + ".tmp", ck)
            if step % (a.ckpt * 10) == 0:
                torch.save({"ema": ema.state_dict(), "step": step, "widths": widths}, os.path.join(a.out, f"ema-{step:07d}.pt"))
    torch.save({"ema": ema.state_dict(), "step": step, "widths": widths}, os.path.join(a.out, "final.pt"))
    print("done", flush=True)


if __name__ == "__main__":
    sys.exit(main())
