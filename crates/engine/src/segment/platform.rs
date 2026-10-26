//! A segmenter the host platform provides (Apple Vision on iPad): nothing to download, and
//! it takes the place of SAM 3 while registered. The host registers a plain function; the
//! worker thread calls it with the photo as 8-bit RGB.

use std::sync::OnceLock;

/// A coverage grid over the whole photo, as `side × side` logits (positive: selected).
pub struct PlatformMask {
    pub side: usize,
    pub logits: Vec<f32>,
}

/// `(rgb, width, height, key, clicks)`: the photo (the same `key` means the same photo and look,
/// so the host may keep its analysis), and the clicks as `[x, y, 1 include | 0 exclude]` in
/// normalized photo coordinates. No clicks asks for the whole foreground. `Ok(None)`: nothing
/// there to select.
pub type PlatformFn = fn(&[u8], usize, usize, u64, &[[f32; 3]]) -> Result<Option<PlatformMask>, String>;

static PLATFORM: OnceLock<PlatformFn> = OnceLock::new();

/// Register the host's segmenter (once; later calls are ignored). `true` when it took effect.
pub fn set_platform_segmenter(f: PlatformFn) -> bool {
    PLATFORM.set(f).is_ok()
}

/// The registered segmenter, if the host has one.
pub fn platform_segmenter() -> Option<PlatformFn> {
    PLATFORM.get().copied()
}
