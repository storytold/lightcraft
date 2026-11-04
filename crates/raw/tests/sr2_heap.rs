//! Peak heap of reading Sony's `SR2Private` directory (`crates/raw/src/vendor/sr2.rs`), measured with dhat's testing
//! mode. Every ARW with `DNGPrivateData` has its `SR2SubIFD` located and decrypted, so the directory that points to
//! it must not cost memory sized by its other entries: a hostile file can give it fields that each alias most of the
//! file. Only the block's offset, length and key may be read. One test per binary (dhat's profiler is global).

use lightcraft_tiff::{ByteOrder, IfdBuilder, ImageData, Tiff, TiffWriter, Value, tags as t};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// A 32 × 24 Sony CFA file of `size` bytes whose `SR2Private` directory holds the block's offset, length and key plus
/// `aliases` UNDEFINED fields that each cover the first `size` bytes of the file. Plain black level and white balance
/// are present, so nothing but the lens record needs the `SR2SubIFD`.
fn hostile_arw(size: usize, aliases: u16) -> Vec<u8> {
    let o = ByteOrder::Little;
    let mut raw = IfdBuilder::new();
    raw.set(t::MAKE, Value::Ascii("SONY".into()));
    raw.set(t::MODEL, Value::Ascii("ILCE-7CR".into()));
    raw.set(t::IMAGE_WIDTH, Value::Long(vec![32]));
    raw.set(t::IMAGE_LENGTH, Value::Long(vec![24]));
    raw.set(t::BITS_PER_SAMPLE, Value::Short(vec![16]));
    raw.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![1]));
    raw.set(t::PHOTOMETRIC, Value::Short(vec![t::photometric::CFA]));
    raw.set(t::COMPRESSION, Value::Short(vec![1]));
    raw.set(t::WHITE_LEVEL, Value::Long(vec![16383]));
    raw.set(0x7310, Value::Short(vec![512; 4]));
    raw.set(0x7313, Value::Short(vec![2048, 1024, 1024, 1536]));
    // a distortion setting, so the file has a lens record that says whether the SR2SubIFD was read
    raw.set(0x7036, Value::Short(vec![0]));
    raw.set(t::DNG_PRIVATE_DATA, Value::Byte(vec![0; 4]));
    raw.set_image(ImageData::Strips { rows_per_strip: 24, strips: vec![vec![0; 32 * 24 * 2]] });
    let mut file = TiffWriter::new(o, false).write(&[raw]).unwrap();
    let pointer = Tiff::parse(&file).unwrap().ifds[0].get(t::DNG_PRIVATE_DATA).unwrap().offset as usize;
    file.resize(size, 0);
    let private = file.len();
    let entries = 3 + aliases;
    let block_at = private + 2 + 12 * usize::from(entries) + 4;
    let block_len = 64u32;
    o.put_u16(&mut file, entries);
    for (tag, kind, count, value) in [(0x7200u16, 4u16, 1u32, block_at as u32), (0x7201, 4, 1, block_len)] {
        o.put_u16(&mut file, tag);
        o.put_u16(&mut file, kind);
        o.put_u32(&mut file, count);
        o.put_u32(&mut file, value);
    }
    o.put_u16(&mut file, 0x7221);
    o.put_u16(&mut file, 7);
    o.put_u32(&mut file, 4);
    file.extend([0x11, 0x22, 0x33, 0x44]);
    for i in 0..aliases {
        o.put_u16(&mut file, 0x7230 + i);
        o.put_u16(&mut file, 7);
        o.put_u32(&mut file, size as u32);
        o.put_u32(&mut file, 0);
    }
    o.put_u32(&mut file, 0);
    file.resize(block_at + block_len as usize, 0);
    let mut at = Vec::new();
    o.put_u32(&mut at, private as u32);
    file[pointer..pointer + 4].copy_from_slice(&at);
    file
}

#[test]
fn sr2_private_fields_cost_no_memory_sized_by_the_file() {
    // eight fields of 16 MiB each: 128 MiB if they were decoded, within the TIFF reader's default value budget
    let file = hostile_arw(16 << 20, 8);
    let _profiler = dhat::Profiler::builder().testing().build();
    let header = lightcraft_raw::probe_info(&file).unwrap();
    let full = lightcraft_raw::decode(&file).unwrap();
    let peak = dhat::HeapStats::get().max_bytes;
    // the block was still found and decrypted (its content is noise: no SR2SubIFD copies)
    for info in [&header, &full.info()] {
        let sony = info.lens_metadata.sony.as_ref().expect("a lens record");
        assert!(sony.sr2_read, "the SR2SubIFD was not read");
    }
    eprintln!("peak heap {peak} bytes decoding a {} byte file", file.len());
    assert!(peak < 2 << 20, "peak heap {peak} bytes decoding a {} byte file", file.len());
}
