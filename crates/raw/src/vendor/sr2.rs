//! Sony's encrypted `SR2SubIFD`.
//!
//! Sony ARWs from the DSLR-A200 on carry a private directory, the `SR2SubIFD`, reached from IFD0's
//! `DNGPrivateData` through the `SR2Private` IFD (`SR2SubIFDOffset` `0x7200`, `SR2SubIFDLength` `0x7201`,
//! `SR2SubIFDKey` `0x7221`; tag names from ExifTool's Sony tag documentation). The block is a TIFF directory in the
//! file's byte order whose value offsets are file offsets into the block itself, and the whole block is encrypted.
//! Among its entries are the black level (`0x7310`) and the white balance the camera applied, `WB_RGGBLevels`
//! (`0x7313`: R, G, G, B); bodies from about 2017 on also write both in plain form in the raw IFD.
//!
//! This module is the only place the block is decrypted: the black level and the white balance are both read through
//! [`SubIfd`] (issue #535; it replaces positional keystream tables recovered earlier for the black level alone).
//!
//! **Clean-room provenance.** The decryption was derived only by black-box known-plaintext analysis of CC0 files. No
//! raw-decoder source (dcraw, LibRaw, rawspeed, rawloader/rawler, darktable, RawTherapee, ExifTool's Perl code, …)
//! and no description of Sony's cipher or keystream generator was read or consulted. The steps:
//!
//! 1. **Files.** The 126 ARWs with an `SR2SubIFD` among the CC0 Sony samples of raw.pixls.us collected for issue
//!    #535: 97 bodies from the DSLR-A200 to the ILCE-9M3 and ZV-E1, every Sony body on the site that has one, with
//!    every coding of the bodies named in open issues.
//! 2. **Plaintext.** `exiftool -v5 FILE`, run as a black-box tool, prints the decrypted `SR2SubIFD` as a hex dump
//!    with file offsets. XOR-ing each dumped byte with the byte at the same offset in the file gives the keystream
//!    byte at that position of the block.
//! 3. **Comparison.** All 126 files store the key `11 22 33 44`, and their keystreams agree byte for byte at every
//!    position they share: 15 layouts from 25150 to 62112 bytes, 5.2 MB of keystream in all. Bytes after the last
//!    whole 4-byte word (two, in every block whose length is not a multiple of four) are stored in the clear.
//! 4. **Structure.** Read as 32-bit words, the longest keystream (the DSLR-A700 sample `DSC07249.ARW`, SHA-256
//!    `3159e288…`, 15528 words) was tested for every recurrence `k[n] = k[n − a] ⊕ k[n − b]` with
//!    `1 ≤ a < b < 300`: an exhaustive search, not a guided one. Exactly two pairs hold on every word: `(63, 127)`
//!    and `(126, 254)`, the second a consequence of the first. With `(63, 127)`, any 127 consecutive words fix all
//!    later ones, so [`SEED`] is simply the first 127 words of that file's keystream.
//! 5. **Checks.** Generated from [`SEED`], the keystream equals the one recovered from each of the 126 files at every
//!    position. It also reproduces bytes recovered independently from plain tag values before the generator existed
//!    (8 bytes at position 2510, issue #148, from the plain black levels of the DSC-RX100M6 and ILCE-7M3) and the
//!    positional tables the black level was read with until issue #535; the unit tests pin all of them.
//!
//! Only that key is supported. The one other key seen, in a DSC-R1 SR2 file, gives a different keystream that obeys
//! the same recurrence from its own first 127 words, but one sample per key cannot show how a seed follows from a
//! key, so files with another key get `None`.

use lightcraft_tiff::{ByteOrder, Ifd, ParseOptions, tags as t};

/// `SR2Private` tags.
const SUBIFD_OFFSET: u16 = 0x7200;
const SUBIFD_LENGTH: u16 = 0x7201;
const SUBIFD_KEY: u16 = 0x7221;
/// The key stored by every ARW examined; [`SEED`] is its keystream.
const KEY: [u8; 4] = [0x11, 0x22, 0x33, 0x44];
/// Upper bound on the block length we decrypt. The largest seen is 62112 bytes (DSLR-A700).
const MAX_LEN: usize = 1 << 20;
/// TIFF field types of the levels: SHORT and SSHORT.
const SHORT: u16 = 3;
const SSHORT: u16 = 8;

/// The first 127 keystream words for [`KEY`] (each XOR-ed with four block bytes, most significant byte first).
/// Later words follow `k[n] = k[n − 63] ⊕ k[n − 127]`; see the module docs.
const SEED: [u32; 127] = [
    0x54c2c4d6, 0x49c478bc, 0x34ae2f8e, 0x25d80504, 0xc0d9d6b0, 0xd838fb71, 0xe8eff27d, 0xfbc1fcea, 0x506c499a, 0x47f20f37, 0x710777cf, 0x7867e7ba,
    0x42d67caa, 0x7f2bd11a, 0x67a216ca, 0x0e986d40, 0x4ae8d4c0, 0xe36778b4, 0x5a958415, 0xdbfe2be8, 0x20faa1aa, 0x7132a6b8, 0xf4de4b7f, 0x55991aa1,
    0xa849d5aa, 0x49577832, 0xb92f3daa, 0x399cc526, 0x22cdd000, 0xe1977a29, 0x37c5db55, 0xb0177e1e, 0x2a1016aa, 0xa300086e, 0x3bab9bfe, 0x262eece0,
    0x23771aa9, 0x0a5dc91c, 0x31b902ae, 0x58e64bf8, 0x259c300e, 0xa57705c8, 0x284a6541, 0xfb229c60, 0x1bacaa9e, 0xbcab3350, 0x67cd9fbe, 0x8f135e60,
    0xf8c26a40, 0x6770da61, 0x3e1febfd, 0xd0c70803, 0x8dbb037b, 0x6f6fa4c5, 0x6749d10d, 0x7f51598d, 0xd5e5a4ec, 0x207dfa91, 0x6558ebc2, 0xbe594639,
    0x617a9e5d, 0x3c497950, 0x0844eb3f, 0x04207ed2, 0xd27ceac4, 0x70d20f05, 0xb47003f6, 0xe9e4e3ae, 0xcc19d265, 0x326dd956, 0xf0d3a327, 0xb71275f0,
    0x7994e285, 0x0aff594d, 0x128e8345, 0x7bda597a, 0xd634c380, 0xe24a006f, 0x8974818b, 0x3320b22a, 0xbe808417, 0xa2d5648a, 0x6fe80b39, 0x23ebad41,
    0xa2d11e5d, 0x027d9397, 0x9a722ac8, 0x432c7dac, 0x7146692a, 0x82a3dc77, 0xd66887c5, 0x831f43b7, 0x4e5dddde, 0x03793f81, 0x306ab437, 0x00ccf86c,
    0xfc6ed3d2, 0x076b8fdb, 0x9808cfca, 0x0f4eef6e, 0xc8cc3830, 0x104ac16a, 0xa189eff4, 0x3e085c08, 0xd28baf88, 0x5c853ac4, 0xe60480f8, 0xc51acd98,
    0x691e5ee1, 0x333feeb9, 0x1e35bc33, 0xec4a4642, 0xee57c5a5, 0xbeeb51f7, 0xe0c4f32c, 0xa5422f6a, 0x1d266d12, 0x3752fd3b, 0xfbc53c7d, 0x2421a4a3,
    0xcdc6a2de, 0x26e6b330, 0x6c073d46, 0x058e2f27, 0x43833f30, 0x46d1382e, 0x5f0804ec,
];

/// XOR `block` with the keystream from its first byte on (whole 4-byte words; a shorter tail stays as it is).
/// Encrypts and decrypts alike.
fn apply_keystream(block: &mut [u8]) {
    let mut ring = SEED;
    for (n, word) in block.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        // `ring[i]` holds k[n − 127] until it is replaced by k[n]; k[n − 63] sits at (n − 63) mod 127
        let i = n % SEED.len();
        if n >= SEED.len() {
            ring[i] ^= ring[(i + 64) % SEED.len()];
        }
        for (b, k) in word.iter_mut().zip(ring[i].to_be_bytes()) {
            *b ^= k;
        }
    }
}

/// A decrypted `SR2SubIFD`.
pub(crate) struct SubIfd {
    /// File offset of the block: the directory's value offsets are file offsets.
    start: usize,
    plain: Vec<u8>,
    order: ByteOrder,
}

impl SubIfd {
    /// Find and decrypt the file's `SR2SubIFD`; `None` when it has none, it lies outside the file, is larger than
    /// [`MAX_LEN`] or uses another key than [`KEY`].
    pub(crate) fn read(bytes: &[u8], ifd0: &Ifd, order: ByteOrder) -> Option<Self> {
        let at = order.read_u32(ifd0.bytes(t::DNG_PRIVATE_DATA)?, 0)?;
        let opts = ParseOptions { max_ifds: 1, max_depth: 0, follow_children: false, ..Default::default() };
        let (private, _) = lightcraft_tiff::parse_ifd_at(bytes, u64::from(at), order, 0, false, &opts).ok()?;
        if private.bytes(SUBIFD_KEY)? != KEY {
            return None;
        }
        let start = usize::try_from(private.u64(SUBIFD_OFFSET)?).ok()?;
        let len = usize::try_from(private.u64(SUBIFD_LENGTH)?).ok()?;
        if len > MAX_LEN {
            return None;
        }
        Some(Self::decrypt(bytes.get(start..start.checked_add(len)?)?, start, order))
    }

    /// Decrypt `block`, the encrypted directory found at file offset `start`.
    pub(crate) fn decrypt(block: &[u8], start: usize, order: ByteOrder) -> Self {
        let mut plain = block.to_vec();
        apply_keystream(&mut plain);
        Self { start, plain, order }
    }

    /// The byte order of the directory's values (the file's).
    pub(crate) fn order(&self) -> ByteOrder {
        self.order
    }

    /// The field type and value bytes of a SHORT or SSHORT entry; `None` when the tag is absent, has another type
    /// or its values lie outside the block.
    pub(crate) fn short_bytes(&self, tag: u16) -> Option<(u16, &[u8])> {
        let o = self.order;
        let entries = usize::from(o.read_u16(&self.plain, 0)?);
        let entry = (0..entries).map_while(|i| self.plain.get(2 + 12 * i..14 + 12 * i)).find(|e| o.read_u16(e, 0) == Some(tag))?;
        let kind = o.read_u16(entry, 2)?;
        if kind != SHORT && kind != SSHORT {
            return None;
        }
        let size = usize::try_from(o.read_u32(entry, 4)?).ok()?.checked_mul(2)?;
        let values = if size <= 4 {
            entry.get(8..8 + size)?
        } else {
            let at = usize::try_from(o.read_u32(entry, 8)?).ok()?.checked_sub(self.start)?;
            self.plain.get(at..at.checked_add(size)?)?
        };
        Some((kind, values))
    }

    /// The values of a SHORT or SSHORT entry (see [`SubIfd::short_bytes`]).
    pub(crate) fn shorts(&self, tag: u16) -> Option<Vec<f64>> {
        let (kind, values) = self.short_bytes(tag)?;
        let value = |v: u16| if kind == SSHORT { f64::from(v as i16) } else { f64::from(v) };
        Some(values.as_chunks::<2>().0.iter().map(|&c| value(self.order.u16(c))).collect())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use lightcraft_tiff::ByteOrder::{Big, Little};

    /// An `SR2SubIFD` block for file offset `start`, encrypted: SHORT/SSHORT entries `(tag, type, values)`, values
    /// of more than two shorts stored after the table at file offsets, as Sony does.
    pub(crate) fn block(start: usize, entries: &[(u16, u16, &[u16])], order: ByteOrder) -> Vec<u8> {
        let mut plain = Vec::new();
        order.put_u16(&mut plain, entries.len() as u16);
        let mut data = Vec::new();
        let data_at = start + 2 + 12 * entries.len() + 4;
        for &(tag, kind, values) in entries {
            order.put_u16(&mut plain, tag);
            order.put_u16(&mut plain, kind);
            order.put_u32(&mut plain, values.len() as u32);
            let mut field = Vec::new();
            values.iter().for_each(|&v| order.put_u16(&mut field, v));
            if field.len() <= 4 {
                field.resize(4, 0);
                plain.extend(field);
            } else {
                order.put_u32(&mut plain, (data_at + data.len()) as u32);
                data.extend(field);
            }
        }
        plain.extend([0; 4]);
        plain.extend(data);
        plain.extend([0xa5; 2]); // a length that is not a multiple of four, like most real blocks
        apply_keystream(&mut plain);
        plain
    }

    /// Encrypt (or decrypt) `block` in place, as a file stores an `SR2SubIFD` at its first byte.
    pub(crate) fn encrypt(block: &mut [u8]) {
        apply_keystream(block);
    }

    /// Keystream bytes recovered earlier, each from plain tag values or ExifTool's decrypted dump at one position,
    /// before the generator existed: the black level's 8 bytes at 2510 (#148), and the positional tables the black
    /// level was read with until #535 (#542: the directory's count and first entry, and the black-level value in the
    /// four layouts of bodies without a plain `0x7310`). The generator must reproduce every one of them, so the black
    /// levels it decrypts are those the tables gave.
    #[test]
    fn keystream_matches_bytes_recovered_at_single_positions() {
        let mut ks = vec![0u8; 62114];
        apply_keystream(&mut ks);
        let recovered: [(usize, &[u8]); 5] = [
            (0, &[0x54, 0xc2, 0xc4, 0xd6, 0x49, 0xc4, 0x78, 0xbc, 0x34, 0xae, 0x2f, 0x8e, 0x25, 0xd8]),
            (1638, &[0x18, 0x95, 0x65, 0xf0, 0x18, 0x8f, 0x61, 0xc6]),
            (2166, &[0xc6, 0x17, 0x02, 0xcb, 0x89, 0x21, 0xbc, 0x43]),
            (2418, &[0xd1, 0xef, 0xda, 0xfe, 0x50, 0xf3, 0xd0, 0xbe]),
            (2510, &[0x43, 0xcb, 0x86, 0xb6, 0x11, 0xd2, 0x1a, 0x73]),
        ];
        for (at, bytes) in recovered {
            assert_eq!(&ks[at..at + bytes.len()], bytes, "keystream at {at}");
        }
    }

    /// Known answers recorded from CC0 files without the generator, so that a wrong seed word or recurrence fails
    /// here even without the corpus: the SHA-256 of the whole keystream recovered from the DSLR-A700 sample
    /// (`DSC07249.ARW`: the file's bytes XOR ExifTool's decrypted dump; 62112 bytes, the longest block seen, so it
    /// covers every black-level and white-balance position of every layout), and the encrypted `WB_RGGBLevels` of
    /// one file per layout of bodies without a plain `0x7313`, with the levels ExifTool decrypts from them.
    #[test]
    fn keystream_matches_recorded_known_answers() {
        use sha2::{Digest, Sha256};
        let mut ks = vec![0u8; 62112];
        apply_keystream(&mut ks);
        let digest: String = Sha256::digest(&ks).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(digest, "d55e96d7bb69cb823d9dfa038dd9f3cfb6b080be4df8c8aa2bc56f71a4a3f183");
        // (file, position in the block, bytes stored there, WB_RGGBLevels)
        let cases: [(&str, usize, [u8; 8], [i16; 4]); 4] = [
            ("DSLR-A700 DSC07249", 1654, [0xc9, 0x2a, 0xb6, 0xa4, 0x1f, 0xc1, 0x72, 0x30], [2128, 1024, 1024, 1564]),
            ("DSLR-A500 DSC02421", 2182, [0xb9, 0x6a, 0xed, 0xd3, 0x0d, 0xa8, 0xbb, 0xda], [2212, 1024, 1024, 1416]),
            ("SLT-A33 DSC01867", 2434, [0x3c, 0x43, 0x05, 0x3f, 0x0d, 0xe2, 0x05, 0xcc], [2344, 1024, 1024, 1508]),
            ("ILCE-3500 DSC06923", 2526, [0x37, 0x2e, 0x71, 0x5b, 0x88, 0xb7, 0x21, 0x73], [2932, 1024, 1024, 1576]),
        ];
        for (file, at, stored, levels) in cases {
            let mut block = vec![0u8; (at + 8).next_multiple_of(4)];
            block[at..at + 8].copy_from_slice(&stored);
            apply_keystream(&mut block);
            let got: Vec<i16> = block[at..at + 8].chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
            assert_eq!(got, levels, "{file}");
        }
    }

    #[test]
    fn keystream_follows_its_recurrence() {
        let mut zeros = vec![0u8; 62114];
        apply_keystream(&mut zeros);
        // the recurrence, past the seed, and the clear tail
        let word = |n: usize| u32::from_be_bytes(zeros[4 * n..4 * n + 4].try_into().unwrap());
        assert_eq!(word(127), SEED[64] ^ SEED[0]);
        assert!((200..15528).all(|n| word(n) == word(n - 63) ^ word(n - 127)));
        assert_eq!(zeros[62112..], [0, 0]);
        // applying it twice restores the input
        let mut again = zeros.clone();
        apply_keystream(&mut again);
        assert!(again[..62112].iter().all(|&b| b == 0));
    }

    #[test]
    fn reads_entries_inline_and_at_file_offsets() {
        for order in [Little, Big] {
            let start = 75964;
            let enc = block(start, &[(0x7310, SHORT, &[512; 4]), (0x7311, SHORT, &[256]), (0x7313, SSHORT, &[2212, 1024, 1024, 1416])], order);
            let sub = SubIfd::decrypt(&enc, start, order);
            assert_eq!(sub.shorts(0x7313), Some(vec![2212.0, 1024.0, 1024.0, 1416.0]), "{order:?}");
            assert_eq!(sub.shorts(0x7310), Some(vec![512.0; 4]));
            assert_eq!(sub.shorts(0x7311), Some(vec![256.0]));
            assert_eq!(sub.shorts(0x7312), None);
            // SSHORT is signed
            let enc = block(start, &[(0x7313, SSHORT, &[0xffff, 1024])], order);
            assert_eq!(SubIfd::decrypt(&enc, start, order).shorts(0x7313), Some(vec![-1.0, 1024.0]));
        }
    }

    #[test]
    fn hostile_directories_give_none() {
        let start = 4096;
        let good = block(start, &[(0x7313, SSHORT, &[2212, 1024, 1024, 1416])], Little);
        let read = |b: &[u8], start: usize| SubIfd::decrypt(b, start, Little).shorts(0x7313);
        assert!(read(&good, start).is_some());
        // decrypted at another file offset, the value offset points before or past the block
        assert_eq!(read(&good, start + 64), None);
        assert_eq!(read(&good, start - 64), None);
        // every truncation, down to empty
        for n in 0..good.len() - 2 {
            assert_eq!(read(&good[..n], start), None, "{n} bytes");
        }
        // garbage: still encrypted; 65535 entries of tag 0xffff; a huge count, another type, a wild offset
        assert_eq!(SubIfd { start, plain: good.clone(), order: Little }.shorts(0x7313), None);
        assert_eq!(SubIfd { start, plain: vec![0xff; 1000], order: Little }.shorts(0x7313), None);
        let mut plain = good.clone();
        apply_keystream(&mut plain);
        let patched = |at: usize, bytes: &[u8]| {
            let mut p = plain.clone();
            p[at..at + bytes.len()].copy_from_slice(bytes);
            apply_keystream(&mut p);
            read(&p, start)
        };
        assert_eq!(patched(6, &u32::MAX.to_le_bytes()), None); // count
        assert_eq!(patched(4, &4u16.to_le_bytes()), None); // LONG
        assert_eq!(patched(10, &u32::MAX.to_le_bytes()), None); // value offset
        // an entry count past the end of the block: the scan stops where the block ends, so an entry before that
        // point is still found
        assert_eq!(patched(0, &u16::MAX.to_le_bytes()), Some(vec![2212.0, 1024.0, 1024.0, 1416.0]));
    }
}
