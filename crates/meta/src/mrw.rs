//! Minolta / Konica Minolta MRW block container.
//!
//! Sources: the public prose description of the MRW layout (Dalibor Jelínek's MRW notes) and our own black-box
//! analysis of the CC0 samples on raw.pixls.us. The file starts with `\0MRM` and a big-endian `u32` length; the
//! sensor data begins right after that many bytes (at `8 + length`). In between is a sequence of blocks, each a
//! four-byte name (`\0PRD`, `\0TTW`, `\0WBG`, `\0RIF`, `\0PAD`), a big-endian `u32` length and that many bytes.
//! `\0TTW` holds a complete big-endian TIFF (IFD0, Exif, maker note) whose offsets are relative to the block.

/// The blocks and the offset of the sensor data in an MRW file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mrw<'a> {
    /// `(name without the leading NUL, contents)`, in file order.
    pub blocks: Vec<([u8; 3], &'a [u8])>,
    /// Absolute offset of the sensor data.
    pub data_offset: usize,
}

impl<'a> Mrw<'a> {
    /// The first block called `name` (e.g. `b"PRD"`).
    pub fn block(&self, name: &[u8; 3]) -> Option<&'a [u8]> {
        self.blocks.iter().find(|(n, _)| n == name).map(|(_, d)| *d)
    }
}

/// At most this many blocks are walked (real files have four or five).
const MAX_BLOCKS: usize = 64;

fn be32(b: &[u8], at: usize) -> Option<usize> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as usize)
}

/// Parse the block list of an MRW file. `None` when the magic is missing or the header length points past the
/// file; a block that runs past the header is dropped together with everything after it.
pub fn parse(bytes: &[u8]) -> Option<Mrw<'_>> {
    if !bytes.starts_with(b"\0MRM") {
        return None;
    }
    let data_offset = be32(bytes, 4)?.checked_add(8).filter(|&o| o <= bytes.len())?;
    let mut blocks = Vec::new();
    let mut at = 8usize;
    while blocks.len() < MAX_BLOCKS && at.saturating_add(8) <= data_offset {
        let (Some(name), Some(len)) = (bytes.get(at..at + 4), be32(bytes, at + 4)) else { break };
        let start = at + 8;
        let Some(end) = start.checked_add(len).filter(|&e| e <= data_offset) else { break };
        let Some(data) = bytes.get(start..end) else { break };
        if name[0] == 0 {
            blocks.push(([name[1], name[2], name[3]], data));
        }
        at = end;
    }
    Some(Mrw { blocks, data_offset })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(name: &[u8; 3], data: &[u8]) -> Vec<u8> {
        let mut v = vec![0];
        v.extend_from_slice(name);
        v.extend_from_slice(&(data.len() as u32).to_be_bytes());
        v.extend_from_slice(data);
        v
    }

    #[test]
    fn walks_blocks_and_finds_the_data() {
        let mut body = block(b"PRD", &[1, 2, 3]);
        body.extend(block(b"WBG", &[4; 12]));
        let mut f = b"\0MRM".to_vec();
        f.extend_from_slice(&(body.len() as u32).to_be_bytes());
        f.extend_from_slice(&body);
        f.extend_from_slice(&[9, 9]);
        let m = parse(&f).unwrap();
        assert_eq!(m.block(b"PRD"), Some(&[1u8, 2, 3][..]));
        assert_eq!(m.block(b"WBG").map(<[u8]>::len), Some(12));
        assert_eq!(m.block(b"TTW"), None);
        assert_eq!(m.data_offset, f.len() - 2);
    }

    #[test]
    fn hostile_lengths_never_panic() {
        assert_eq!(parse(b"\0MRM"), None);
        assert_eq!(parse(b"\0MRM\xff\xff\xff\xff"), None);
        // a block claiming more than the header holds is dropped
        let f = b"\0MRM\0\0\0\x0c\0PRD\xff\xff\xff\xff\0\0\0\0";
        assert_eq!(parse(f).map(|m| m.blocks.len()), Some(0));
        for n in 0..f.len() {
            let _ = parse(&f[..n]);
        }
    }
}
