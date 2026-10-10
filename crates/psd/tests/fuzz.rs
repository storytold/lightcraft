//! P6.2: seeded mutation fuzzing of the PSD/PSB reader (beyond the truncation sweeps and
//! proptests): every parsed file is then walked like a reader would (layers, tree, channel and
//! merged-image decoding, re-encoding); nothing panics or allocates without bound.

use dac_psd::testgen;
use dac_psd::*;

fn walk(f: &PsdFile) {
    let h = &f.header;
    let _ = f.layer_tree();
    let _ = f.image_data.decode(h);
    for i in 0..usize::from(h.channels.min(8)) {
        let _ = f.image_data.decode_channel(h, i);
    }
    for l in f.layers() {
        let _ = l.name();
        if let Ok((w, ht)) = l.rect.size()
            && w.saturating_mul(ht) <= 1 << 22
        {
            for c in &l.channels {
                let _ = c.decode(w, ht, h.depth, h.version);
            }
        }
    }
    let _ = f.to_bytes();
}

#[test]
fn mutated_psd_files_never_panic() {
    let files = [
        testgen::small(Version::Psd, Compression::Rle),
        testgen::small(Version::Psd, Compression::ZipPrediction),
        testgen::small(Version::Psb, Compression::Zip),
        testgen::layered(Version::Psd, ColorMode::Rgb, 16, Compression::Rle),
        testgen::merged_only(Version::Psd, ColorMode::Cmyk, 8, Compression::Raw, 5, 3),
    ];
    let bytes: Vec<Vec<u8>> = files.iter().map(|f| f.to_bytes().unwrap()).collect();
    let refs: Vec<&[u8]> = bytes.iter().map(Vec::as_slice).collect();
    fuzz("psd.file", &refs, 2000, |b| {
        if let Ok(f) = PsdFile::from_bytes(b) {
            walk(&f);
        }
    });
}

#[test]
fn mutated_descriptors_never_panic() {
    let v = testgen::sample_descriptor();
    let seeds = [v.to_bytes(), v.descriptor.to_bytes()];
    let refs: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
    assert!(VersionedDescriptor::from_bytes(refs[0]).is_ok());
    fuzz("psd.descriptor", &refs, 4000, |b| {
        if let Ok(d) = VersionedDescriptor::from_bytes(b) {
            let _ = d.to_bytes();
        }
        let _ = Descriptor::from_bytes(b);
        let _ = VersionedDescriptor::parse_prefix(b);
    });
}

/// A tiny seeded mutator (this crate is standalone: no workspace dev-dependencies, so not
/// `dac-fuzzkit`). Bit flips, byte writes, truncation, deletion, duplication and inserted
/// tokens; `DAC_FUZZ_ITERS` raises the count.
fn fuzz(name: &str, seeds: &[&[u8]], default: usize, mut check: impl FnMut(&[u8])) {
    let mut x = name.bytes().fold(0x9E37_79B9_7F4A_7C15u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)) | 1;
    let mut next = move |n: usize| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        if n == 0 { 0 } else { (x % n as u64) as usize }
    };
    const TOKENS: &[&[u8]] = &[b"\0", b"\xff\xff\xff\xff", b"-1", b"1e309", b"null", b"[]", b"{}", b"\"\"", b"[[[[", b"99999999999999999999"];
    let iters = std::env::var("DAC_FUZZ_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(default);
    for s in seeds {
        check(s);
    }
    for i in 0..iters {
        let mut v = seeds[i % seeds.len()].to_vec();
        for _ in 0..1 + next(6) {
            let len = v.len();
            match next(6) {
                0 if len > 0 => {
                    let k = next(len);
                    v[k] ^= 1 << next(8);
                }
                1 if len > 0 => {
                    let k = next(len);
                    v[k] = next(256) as u8;
                }
                2 => v.truncate(next(len + 1)),
                3 if len > 0 => {
                    let a = next(len);
                    let b = (a + 1 + next(16)).min(len);
                    v.drain(a..b);
                }
                4 if len > 0 => {
                    let a = next(len);
                    let b = (a + 1 + next(64)).min(len);
                    let chunk = v[a..b].to_vec();
                    let at = next(v.len() + 1);
                    v.splice(at..at, chunk);
                }
                _ => {
                    let t = TOKENS[next(TOKENS.len())];
                    let at = next(len + 1);
                    v.splice(at..at, t.iter().copied());
                }
            }
        }
        check(&v);
    }
}
