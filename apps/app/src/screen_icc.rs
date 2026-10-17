//! Checks shared by the screen-profile readers (Linux, Windows): a monitor profile read from the
//! system must look like an ICC profile of sane size before the previews are shown through it.

/// A profile larger than this is not a monitor profile (they are a few KB).
pub const MAX_ICC: u32 = 16 << 20;

/// Whether the bytes look like an ICC profile: a header whose size fits and the `acsp` signature.
pub fn is_icc(bytes: &[u8]) -> bool {
    let Some(size) = bytes.get(0..4).and_then(|b| <[u8; 4]>::try_from(b).ok()).map(u32::from_be_bytes) else { return false };
    bytes.len() >= 128
        && (128..=MAX_ICC).contains(&size)
        && usize::try_from(size).is_ok_and(|s| s <= bytes.len())
        && bytes.get(36..40) == Some(b"acsp")
}

/// Reads a profile file: `None` when it is missing, too large or not an ICC profile.
pub fn read_icc_file(path: &std::path::Path) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > u64::from(MAX_ICC) {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    is_icc(&bytes).then_some(bytes)
}
