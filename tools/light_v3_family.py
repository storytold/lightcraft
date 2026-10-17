"""Process V3's Light operator family (docs/light-v3.md), as the fitting tool defines it.

This is the part of the fitting tool (`fit_pointwise.py`, kept with the private fitting data) that
`light_v3_constants.py` needs to write the Rust tests' reference values: `Family.curve`, `Family.apply`,
`Family.law` and `Family.unpack` for Contrast and Blacks. `crates/pipeline/src/light_v3.rs` is the Rust port
of the same functions, and its tests compare the two.

F_a(v) = v + s(a) D+(v) for a > 0, max(0, v - s(a) D-(v)) for a < 0, with D piecewise linear on KNOTS (D(1) = 0;
signed shapes also D(0) = 0) and s(t) = ((1 - exp(-t / tau)) / (1 - exp(-1 / tau)))^q, t = |a| / 100. v is a channel value of linear ProPhoto RGB (D50) through the sRGB
transfer curve. Modes: pc (per channel), lum (on the encoded ProPhoto luminance, linear RGB scaled by the ratio),
mix (w pc + (1 - w) lum in linear light, a weight per sign).
"""

import numpy as np

PRO_Y = np.array([0.2880402, 0.7118741, 0.0000857])
KNOTS = np.array([0.0, 0.02, 0.05, 0.09, 0.14, 0.2, 0.28, 0.38, 0.5, 0.65, 0.82, 1.0])


def lin(e):
    return np.where(e <= 0.04045, e / 12.92, ((e + 0.055) / 1.055) ** 2.4)


def enc(l):
    l = np.clip(l, 0.0, None)
    return np.where(l <= 0.0031308, 12.92 * l, 1.055 * np.power(l, 1 / 2.4) - 0.055)


class Family:
    def __init__(self, mode, signed=False):
        self.mode = mode
        # Contrast moves tones both ways around a pivot: signed shapes, anchored at black and white
        self.signed = signed
        self.n_shape = len(KNOTS) - 1  # D(1) = 0
        self.size = 2 * self.n_shape + 4 + (2 if mode == "mix" else 0)

    def unpack(self, p):
        n = self.n_shape
        d_pos = np.append(p[:n], 0.0)
        d_neg = np.append(p[n:2 * n], 0.0)
        if self.signed:
            d_pos[0] = d_neg[0] = 0.0
        i = 2 * n
        s_pos = (float(np.exp(p[i])), float(np.exp(p[i + 1])))      # (tau, q), positive amounts
        s_neg = (float(np.exp(p[i + 2])), float(np.exp(p[i + 3])))  # negative amounts
        i += 4
        w = (1 / (1 + np.exp(-p[i])), 1 / (1 + np.exp(-p[i + 1]))) if self.mode == "mix" else None
        return d_pos, d_neg, s_pos, s_neg, w

    @staticmethod
    def law(tq, a):
        tau, q = tq
        t = min(abs(a) / 100.0, 1.0)
        tau = min(max(tau, 1e-3), 1e3)
        return float(((1 - np.exp(-t / tau)) / (1 - np.exp(-1 / tau))) ** q)

    def curve(self, p, a, v):
        """F_a on encoded values v (any shape)."""
        if a == 0:
            return v
        d_pos, d_neg, s_pos, s_neg, _ = self.unpack(p)
        if a > 0:
            return v + self.law(s_pos, a) * np.interp(v, KNOTS, d_pos)
        return np.maximum(v - self.law(s_neg, a) * np.interp(v, KNOTS, d_neg), 0.0)

    def apply(self, p, a, v):
        """F_a applied to (n, 3) encoded ProPhoto pixels per the mode."""
        if self.mode == "pc":
            return self.curve(p, a, v)
        yl = lin(v) @ PRO_Y
        ye = enc(yl)
        y2 = lin(self.curve(p, a, ye))
        ratio = np.where(yl > 1e-6, y2 / np.maximum(yl, 1e-6), 1.0)
        lum = enc(lin(v) * ratio[:, None])
        if self.mode == "lum":
            return lum
        w = self.unpack(p)[4][0 if a > 0 else 1]
        return enc(w * lin(self.curve(p, a, v)) + (1 - w) * lin(lum))
