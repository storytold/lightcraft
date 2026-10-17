//! A cheap look at a module's export section, without compiling it.

/// Reads an unsigned LEB128 `u32` at `*pos`.
fn leb(b: &[u8], pos: &mut usize) -> Option<u32> {
    let mut out: u32 = 0;
    for shift in (0..35).step_by(7) {
        let byte = *b.get(*pos)?;
        *pos = pos.checked_add(1)?;
        out |= u32::from(byte & 0x7f).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some(out);
        }
    }
    None
}

/// Names exported by the module, or `None` if its sections don't parse.
fn export_names(bytes: &[u8]) -> Option<Vec<&[u8]>> {
    if bytes.get(..8)? != b"\0asm\x01\0\0\0" {
        return None;
    }
    let mut pos = 8usize;
    while pos < bytes.len() {
        let id = *bytes.get(pos)?;
        pos += 1;
        let size = leb(bytes, &mut pos)? as usize;
        let end = pos.checked_add(size)?;
        let body = bytes.get(pos..end)?;
        if id == 7 {
            let mut p = 0usize;
            let count = leb(body, &mut p)?;
            let mut names = Vec::new();
            for _ in 0..count.min(100_000) {
                let len = leb(body, &mut p)? as usize;
                let e = p.checked_add(len)?;
                names.push(body.get(p..e)?);
                p = e.checked_add(1)?; // kind
                leb(body, &mut p)?; // index
            }
            return Some(names);
        }
        pos = end;
    }
    Some(Vec::new())
}

/// True when `bytes` looks like a PhotoCraft ABI pixel filter: a WebAssembly binary exporting
/// `pc_abi_version`, `pc_manifest`, `pc_alloc` and `pc_filter`. Only the export section is read;
/// [`crate::Filter::load`] does the full validation.
pub fn is_filter_module(bytes: &[u8]) -> bool {
    export_names(bytes)
        .is_some_and(|names| ["pc_abi_version", "pc_manifest", "pc_alloc", "pc_filter"].iter().all(|want| names.contains(&want.as_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_is_not_a_filter() {
        for b in [&b""[..], b"\0asm", b"\0asm\x01\0\0\0\x07\xff\xff\xff\xff\x0f", b"\0asm\x01\0\0\0\x07\x05\x7f", b"hello world"] {
            assert!(!is_filter_module(b));
        }
    }
}
