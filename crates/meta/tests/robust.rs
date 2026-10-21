use lightcraft_meta::{MergeRules, Metadata, extract, merge_xmp, parse_iptc, parse_xmp, read_exif, write_xmp};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig { cases: 1500, .. ProptestConfig::default() })]

    #[test]
    fn random_containers_never_panic(mut data in proptest::collection::vec(any::<u8>(), 0..600), kind in 0usize..6) {
        let heads: [&[u8]; 6] = [b"\xff\xd8\xff\xe1", b"\x89PNG\r\n\x1a\n", b"RIFF\0\0\0\0WEBP", b"II*\0", b"MM\0*", b"\0\0\0\x18ftypcrx "];
        let h = heads[kind];
        if data.len() >= h.len() {
            data[..h.len()].copy_from_slice(h);
        }
        let _ = extract(&data);
        assert!(lightcraft_meta::search_metadata(&data).valid());
        let _ = read_exif(&data);
        let _ = parse_iptc(&data);
    }

    #[test]
    fn random_text_xmp_never_panics(s in ".{0,400}") {
        let _ = parse_xmp(&s);
    }

    #[test]
    fn random_comfy_text_chunks_never_panic(data in proptest::collection::vec(any::<u8>(), 0..1000), kind in 0usize..3) {
        let typ = [b"tEXt", b"zTXt", b"iTXt"][kind];
        let mut payload = b"prompt\0".to_vec();
        payload.extend_from_slice(&data);
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        png.extend_from_slice(typ);
        png.extend_from_slice(&payload);
        let mut crc = crc32fast::Hasher::new();
        crc.update(typ);
        crc.update(&payload);
        png.extend_from_slice(&crc.finalize().to_be_bytes());
        let _ = extract(&png);
        assert!(lightcraft_meta::search_metadata(&png).valid());
        let fields=lightcraft_meta::png_text_fields(&png);
        let _=lightcraft_meta::comfy_display_fields(&fields,None);
    }

    /// Merging into arbitrary (also damaged) packets never panics, and a merge that succeeds
    /// reads back with the written values.
    #[test]
    fn merge_never_panics(cut in 0usize..900, junk in ".{0,20}", rating in 0i8..=5) {
        let base = write_xmp(&Metadata { title: Some("t".into()), rating: Some(1), ..Default::default() }, Some("{}"));
        let mut s: String = base.chars().take(cut).collect();
        s.push_str(&junk);
        s.extend(base.chars().skip(cut));
        let fresh = write_xmp(&Metadata { rating: Some(rating), ..Default::default() }, Some("{\"x\":1}"));
        let rules = MergeRules { owned: &["xmp:Rating", "dc:title", "lc:*"], owned_if_present: &[] };
        if let Ok(out) = merge_xmp(&s, &fresh, rules) {
            let d = parse_xmp(&out).unwrap();
            prop_assert_eq!(d.metadata.rating, Some(rating));
            prop_assert_eq!(d.lc_settings.as_deref(), Some("{\"x\":1}"));
        }
    }

    #[test]
    fn user_text_roundtrips(title in "[a-zA-Z0-9 &<>\"'éü€;|/\\\\]{1,40}", kw in proptest::collection::vec("[a-zA-Z0-9 &<>\"'|]{1,12}", 0..5), rating in -1i8..=5) {
        let title = title.trim().to_string();
        prop_assume!(!title.is_empty());
        let kw: Vec<String> = kw.into_iter().map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect();
        let m = Metadata { title: Some(title.clone()), keywords: kw.clone(), rating: Some(rating), ..Default::default() };
        let d = parse_xmp(&write_xmp(&m, Some(&title))).unwrap();
        prop_assert_eq!(d.metadata.title, Some(title.clone()));
        prop_assert_eq!(d.metadata.keywords, kw);
        prop_assert_eq!(d.metadata.rating, Some(rating));
        prop_assert_eq!(d.lc_settings, Some(title));
    }
}
