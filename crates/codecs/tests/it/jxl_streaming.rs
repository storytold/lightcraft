use lightcraft_codecs::exif::minimal_exif;
use lightcraft_codecs::*;
use lightcraft_raster::Rgba8;

fn codestream(bytes: &[u8]) -> Vec<u8> {
    if bytes.starts_with(b"\xff\x0a") {
        return bytes.to_vec();
    }
    let mut raw = Vec::new();
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let end = if len == 0 { bytes.len() } else { at + len };
        match &bytes[at + 4..at + 8] {
            b"jxlc" => raw.extend_from_slice(&bytes[at + 8..end]),
            b"jxlp" => raw.extend_from_slice(&bytes[at + 12..end]),
            _ => {}
        }
        at = end;
    }
    assert!(raw.starts_with(b"\xff\x0a"));
    raw
}

fn box_bytes(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&u32::try_from(data.len() + 8).unwrap().to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
}

#[test]
fn metadata_in_final_feed_matches_builder() {
    let pixels = Rgba8::from_fn(8, 8, |x, y| [(x * 20) as u8, (y * 20) as u8, 80, 255]);
    let encoded = encode_jxl(&EncodeImage::rgba8(&pixels), &JxlOptions { lossless: true, ..Default::default() }, &EncodeMeta::default()).unwrap();
    let raw = codestream(&encoded);
    let exif = minimal_exif(1);
    let mut exif_box = vec![0; 4]; // TIFF header offset, relative to the end of this field
    exif_box.extend_from_slice(&exif);
    // Split the codestream across two feeds, with Exif and XMP following its last frame.
    let mut bytes = b"\0\0\0\x0cJXL \x0d\x0a\x87\x0a".to_vec();
    box_bytes(&mut bytes, b"ftyp", b"jxl \0\0\0\0jxl ");
    let split = raw.len() / 2;
    let mut part = 0u32.to_be_bytes().to_vec();
    part.extend_from_slice(&raw[..split]);
    box_bytes(&mut bytes, b"jxlp", &part);
    let padding = vec![0; 4096 - bytes.len() - 8];
    box_bytes(&mut bytes, b"free", &padding);
    let mut part = 0x8000_0001u32.to_be_bytes().to_vec();
    part.extend_from_slice(&raw[split..]);
    box_bytes(&mut bytes, b"jxlp", &part);
    box_bytes(&mut bytes, b"Exif", &exif_box);
    box_bytes(&mut bytes, b"xml ", b"<xmp/>");
    assert!(bytes.len() < 8192, "metadata must fit in the completing input chunk");
    let old = jxl_oxide::JxlImage::builder().read(bytes.as_slice()).unwrap();
    let old_exif = match old.aux_boxes().first_exif().unwrap() {
        jxl_oxide::AuxBoxData::Data(ex) => ex.payload().to_vec(),
        _ => panic!("builder must preserve Exif in its final feed"),
    };
    let old_xml = match old.aux_boxes().first_xml() {
        jxl_oxide::AuxBoxData::Data(xml) => xml.to_vec(),
        _ => panic!("builder must preserve XMP in its final feed"),
    };
    let decoded = decode(&bytes, DecodeOptions::default()).unwrap();
    assert_eq!(decoded.exif.as_deref(), Some(old_exif.as_slice()));
    assert_eq!(decoded.xmp.as_deref(), Some(std::str::from_utf8(&old_xml).unwrap()));
    assert_eq!(decoded.to_srgb8(), pixels);
}

#[test]
fn progressive_truncation_matches_builder() {
    let pixels = Rgba8::from_fn(96, 64, |x, y| {
        let mut seed = (y * 96 + x + 1) as u32;
        let mut rgb = [0; 4];
        for channel in &mut rgb[..3] {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            *channel = seed as u8;
        }
        rgb[3] = 255;
        rgb
    });
    for lossless in [false, true] {
        let encoded =
            encode_jxl(&EncodeImage::rgba8(&pixels), &JxlOptions { lossless, progressive: true, ..Default::default() }, &EncodeMeta::default())
                .unwrap();
        let raw = codestream(&encoded);
        assert!(raw.len() > 4096, "fixture must exercise post-header feeds");
        for cut in [0, 1, 2, 12, 4095, 4096, 4097, raw.len() / 2, raw.len() - 1, raw.len()] {
            let bytes = &raw[..cut];
            // `finalize` permits partial images: rendering determines whether frame 0 is usable.
            let old_ok = jxl_oxide::JxlImage::builder().read(bytes).is_ok_and(|image| image.render_frame(0).is_ok());
            assert_eq!(decode(bytes, DecodeOptions::default()).is_ok(), old_ok, "lossless={lossless}, cut={cut}/{}", raw.len());
        }
    }
}
