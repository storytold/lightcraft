# Writes ONNX Runtime reference tensors for the YuNet interpreter test
# (crates/faces/src/yunet.rs, `the_network_matches_onnx_runtime`, read from LC_YUNET_REF).
# /// script
# requires-python = ">=3.10"
# dependencies = ["onnxruntime", "numpy", "onnx"]
# ///
"""Runs YuNet (face_detection_yunet_2023mar.onnx) with onnxruntime (CPU, fp32) on a seeded, photo-like input at five
shapes: the declared 640 x 640 square (input.bin, out_<name>.bin) and, as input_<H>x<W>.bin / out_<name>_<H>x<W>.bin
(H high by W wide), a non-square 704x1024 plus the extremes a 1024 px scan can feed it: a thin strip 32x1024, a thin
column 1024x32 and the smallest input 32x32, all raw little-endian float32, NCHW, BGR 0..255 as the model takes
them. A real model run at these shapes is what shows that the interpreter handles non-square and extreme inputs. The file declares
its input as 640 x 640, which onnxruntime enforces, so for the other shape a copy is made with the declared input
(and the shapes it infers for the outputs) changed; if the graph had a shape-fixed operator onnxruntime would then
refuse to run it, which is the check.

Usage: uv run tools/yunet_reference.py <face_detection_yunet_2023mar.onnx> <output folder>
then: LC_YUNET_MODEL=<the same onnx> LC_YUNET_REF=<output folder> cargo test -p lightcraft-faces --lib -- --ignored --nocapture the_network_matches_onnx_runtime
"""
import sys
from pathlib import Path

import numpy as np
import onnx
import onnxruntime as ort

SEED = 3092
SHAPES = (("", (640, 640)), ("_704x1024", (704, 1024)), ("_32x1024", (32, 1024)), ("_1024x32", (1024, 32)), ("_32x32", (32, 32)))


def photo_like(rng, h, w):
    """Smooth gradients plus texture, 0..255, shape [1, 3, h, w]."""
    y, x = np.mgrid[0:h, 0:w].astype(np.float32)
    planes = []
    for c in range(3):
        base = 127 + 100 * np.sin(x / (40 + 17 * c) + c) * np.cos(y / (55 - 9 * c))
        planes.append(np.clip(base + rng.normal(0, 12, (h, w)), 0, 255))
    return np.stack(planes)[None].astype("<f4")


def reshaped(model_path, h, w):
    """The model with its input declared as [1, 3, h, w] and no stale output shapes, as bytes."""
    m = onnx.load(model_path)
    dims = m.graph.input[0].type.tensor_type.shape.dim
    dims[2].dim_value, dims[3].dim_value = h, w
    del m.graph.value_info[:]
    for o in m.graph.output:
        for d in o.type.tensor_type.shape.dim:
            d.Clear()
    return m.SerializeToString()


def main():
    if len(sys.argv) != 3:
        sys.exit("usage: uv run tools/yunet_reference.py <face_detection_yunet_2023mar.onnx> <output folder>")
    model, out = sys.argv[1], Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    rng = np.random.default_rng(SEED)
    for suffix, (h, w) in SHAPES:
        session = ort.InferenceSession(model if (h, w) == (640, 640) else reshaped(model, h, w), providers=["CPUExecutionProvider"])
        name = session.get_inputs()[0].name
        x = photo_like(rng, h, w)
        x.tofile(out / f"input{suffix}.bin")
        outputs = session.run(None, {name: x})
        for meta, value in zip(session.get_outputs(), outputs):
            np.ascontiguousarray(value, dtype="<f4").tofile(out / f"out_{meta.name}{suffix}.bin")
        print(f"{w} x {h}: {len(outputs)} outputs", [list(o.shape) for o in outputs][:3], "...")


if __name__ == "__main__":
    main()
