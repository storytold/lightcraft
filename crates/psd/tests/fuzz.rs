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
    dac_fuzzkit::run("psd.file", &refs, 2000, |b| {
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
    dac_fuzzkit::run("psd.descriptor", &refs, 4000, |b| {
        if let Ok(d) = VersionedDescriptor::from_bytes(b) {
            let _ = d.to_bytes();
        }
        let _ = Descriptor::from_bytes(b);
        let _ = VersionedDescriptor::parse_prefix(b);
    });
}
