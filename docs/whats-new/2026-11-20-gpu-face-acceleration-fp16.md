### Faster GPU face detection and recognition
- Face detection and recognition now batch multiple faces and detection windows together in single GPU passes and use half-precision (FP16) compute with full-precision accumulation on Vulkan, DX12, and Metal cards that support it, with automatic fallback to FP32 or the CPU.
- **Settings ▸ Faces** now includes **Face models device** (Automatic prefers dedicated NVIDIA GPUs, or choose CPU or a specific card) and **GPU precision** (Automatic, FP16, or FP32) controls, and caches verified GPU benchmark plans on disk to speed up subsequent startups.
