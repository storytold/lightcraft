//! Fuzz target `meta` (`cargo xtask fuzz meta`): metadata containers (JPEG APP1, TIFF, corpus files)
//! through extraction, EXIF and IPTC, and XMP packets (ours and sidecars) through parsing and merging.
//! A short run in `cargo test`; time-boxed long runs under `LCC_FUZZ_SECS` (see `lcc-fuzzkit`).
//! Crash inputs go into `tests/it/robust.rs`.

use lcc_meta::{MergeRules, Metadata, extract, merge_xmp, parse_iptc, parse_xmp, read_exif, write_xmp};

#[test]
fn fuzz_target() {
    let ours = write_xmp(
        &Metadata { title: Some("t & <u>".into()), keywords: vec!["a|b".into()], rating: Some(3), ..Default::default() },
        Some("{\"x\":1}"),
    );
    let fresh = write_xmp(&Metadata { rating: Some(5), ..Default::default() }, Some("{}"));
    let sidecar = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
        <rdf:Description xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" \
        crs:Exposure2012=\"+0.50\" xmp:Rating=\"2\"><crs:ToneCurvePV2012><rdf:Seq><rdf:li>0, 0</rdf:li><rdf:li>255, 255</rdf:li>\
        </rdf:Seq></crs:ToneCurvePV2012></rdf:Description></rdf:RDF></x:xmpmeta>";
    let mut jpeg = b"\xff\xd8\xff\xe1\0\0Exif\0\0".to_vec();
    jpeg.extend_from_slice(b"II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0");
    let n = u16::try_from(jpeg.len() - 4).unwrap().to_be_bytes();
    jpeg[4..6].copy_from_slice(&n);
    jpeg.extend_from_slice(b"\xff\xd9");
    let seeds = [ours.into_bytes(), sidecar.as_bytes().to_vec(), jpeg];
    let rules = MergeRules { owned: &["xmp:Rating", "dc:title", "lc:*"], owned_if_present: &[] };
    lcc_fuzzkit::run_target("meta", &seeds, &["xmp", "jpg", "jpeg", "tif", "dng", "png", "webp", "heic"], 1500, |bytes| {
        let _ = extract(bytes);
        let _ = read_exif(bytes);
        let _ = parse_iptc(bytes);
        let text = String::from_utf8_lossy(bytes);
        let _ = parse_xmp(&text);
        let _ = merge_xmp(&text, &fresh, rules);
    });
}
