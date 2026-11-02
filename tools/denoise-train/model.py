"""The AI Denoise network: a small U-Net for LightCraft's `bayerToRgb` contract.

Input: four planes [R, G1, G2, B] of a Bayer mosaic forced to RGGB, black subtracted, white at 1, not white
balanced, `[1, 4, t, t]`. Output: camera RGB at the mosaic's resolution, `[1, 3, 2t, 2t]`, on the input's scale
(the manifest's gain is `none`).

Only what LightCraft's own pure-Rust runners read (`crates/denoise/src/onnx.rs`) is used: 3 x 3 / 1 x 1
convolutions with stride 1 and "same" padding, each followed by a LeakyReLU, 2 x 2 max-pooling, 2 x 2 stride-2
transposed convolutions, two-input channel concatenations that feed a convolution, and a final depth-to-space
(PixelShuffle, which ONNX exports as DepthToSpace in CRD order). No additions, normalisation layers or other
activations.
"""

import torch
import torch.nn as nn

SLOPE = 0.2


def block(cin, cout):
    return nn.Sequential(
        nn.Conv2d(cin, cout, 3, padding=1), nn.LeakyReLU(SLOPE),
        nn.Conv2d(cout, cout, 3, padding=1), nn.LeakyReLU(SLOPE),
    )


class Up(nn.Module):
    def __init__(self, cin, cout):
        super().__init__()
        self.up = nn.ConvTranspose2d(cin, cout, 2, stride=2)
        self.act = nn.LeakyReLU(SLOPE)
        # the first convolution reads [up, skip]
        self.conv = block(2 * cout, cout)

    def forward(self, x, skip):
        return self.conv(torch.cat([self.act(self.up(x)), skip], 1))


class BayerUNet(nn.Module):
    def __init__(self, widths=(48, 96, 192, 384)):
        super().__init__()
        self.widths = tuple(widths)
        self.down = nn.ModuleList()
        cin = 4
        for w in widths:
            self.down.append(block(cin, w))
            cin = w
        self.pool = nn.MaxPool2d(2, 2)
        self.ups = nn.ModuleList(Up(widths[i + 1], widths[i]) for i in reversed(range(len(widths) - 1)))
        # head at the cell resolution: 12 = 3 colours x 2 x 2 pixels per cell
        self.head = nn.Sequential(nn.Conv2d(widths[0], widths[0], 3, padding=1), nn.LeakyReLU(SLOPE), nn.Conv2d(widths[0], 12, 1))
        self.shuffle = nn.PixelShuffle(2)

    @property
    def depth(self):
        return len(self.widths) - 1

    def forward(self, x):
        skips = []
        for i, d in enumerate(self.down):
            if i > 0:
                x = self.pool(x)
            x = d(x)
            skips.append(x)
        skips.pop()
        for up in self.ups:
            x = up(x, skips.pop())
        return self.shuffle(self.head(x))


def export_onnx(model, path, tile):
    """Write `model` as an ONNX file LightCraft can read, with a fixed `[1, 4, tile, tile]` input."""
    model = model.eval().float().cpu()
    x = torch.zeros(1, 4, tile, tile)
    torch.onnx.export(
        model, x, path, input_names=["bayer"], output_names=["rgb"], opset_version=17,
        do_constant_folding=True, dynamo=False,
    )
