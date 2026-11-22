## Performance

- **GPU face detection and recognition (FP16 + batching):** added pure-Rust `wgpu` mixed-precision `f16` compute kernels
  with `f32` accumulators, batched `[B, C, H, W]` network and `detect_far` execution, NVIDIA-first automatic GPU
  selection, persistent on-disk plan caching, and **Settings ▸ Faces ▸ Face models device / GPU precision** selectors
  with automatic FP16 to FP32 to CPU fallback.
