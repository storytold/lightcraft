//! MRW container API retained for LightCraft callers; framing is implemented by craft-raw.
//! The shared parser at the workspace pin preserves the original recovery and 64-record cap.

pub use craft_raw::mrw::{Mrw, parse};

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
    fn shared_container_preserves_duplicates_cap_and_borrowed_payloads() {
        let mut body = block(b"TTW", &[1, 2, 3]);
        for _ in 0..65 {
            body.extend(block(b"TTW", &[9]));
        }
        let mut bytes = b"\0MRM".to_vec();
        bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
        bytes.extend(body);
        bytes.extend([7, 8]);
        let parsed: Mrw<'_> = parse(&bytes).unwrap();
        assert_eq!(parsed.blocks.len(), 64);
        assert_eq!(parsed.block(b"TTW"), Some(&bytes[16..19]));
        assert_eq!(parsed.block(b"TTW").unwrap().as_ptr(), bytes[16..].as_ptr());
        assert_eq!(parsed.data_offset, bytes.len() - 2);
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
