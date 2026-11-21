# lightcraft-reflections

DSRNet transmission inference in native Rust/Candle (L3), with CPU and Apple Metal
execution. No Python process runs inside LightCraft. Native-only; web reports the
feature unavailable. Source-derived architecture: Apache-2.0; see root NOTICE.

The model directory contains `model.safetensors` and `manifest.json`. Installation
and provenance: [docs/ai-reflections.md](../../docs/ai-reflections.md). Weights are
not committed or shipped. SHA256 is checked before loading. Inference accepts
bounded 8-bit sRGB input, at most 1024 pixels on either edge, and returns clamped
sRGB transmission pixels. Original dimensions and develop/export blending are
the engine's responsibility.

DSRNet-S, DSRNet-L, and DSRNet-L Nature are equivalence-tested against their
upstream PyTorch references on deterministic 64×64 input (maximum differences
below 0.000001 on Metal and 0.000002 on CPU). They require their separate author checkpoints. The LRM head computes only the auxiliary training
residue and is not evaluated: it does not feed transmission.

A guarded convolution view avoids a Candle CPU channel-layout ambiguity when
channel and spatial dimensions coincide; a synthetic regression checks channel order.

`examples/infer.rs` is developer tooling for comparison using a raw interleaved
RGB input and little-endian f32 output. No real images or model weights are needed
by ordinary CI tests. End-user operations go through reflection commands.
