## Fixes

- **No abort at launch on a busy GPU:** when the window's surface failed its first configure (Metal: "Failed to wait for GPU to come idle"), the first frame asked the unconfigured surface for a texture and wgpu panicked inside a callback that can't unwind, so the app aborted during startup. The window now configures the surface again before each frame, skips frames until a configure succeeds, and never asks an unconfigured surface for a texture.
