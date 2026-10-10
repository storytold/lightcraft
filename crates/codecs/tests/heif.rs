//! HEIC/HEIF through the codec API (feature `heif`), on lossless synthetic files from
//! `dac_heif::testdata`: an iPhone-like grid with a Display P3 profile, rotation, EXIF, a
//! thumbnail and an HDR gain map; `nclx`-only colour; 10-bit; and hostile input.
#![cfg(feature = "heif")]

use dac_codecs::*;
use dac_heif::testdata::{Prop, Spec, build};

fn grey(x: u32, y: u32) -> [u16; 3] {
    [((x * 7 + y * 13) % 220 + 20) as u16, 128, 128]
}

/// EXIF whose Orientation (6) only mirrors the container's `irot`: it must not turn the photo again.
fn exif_orientation_6() -> Vec<u8> {
    let mut t = b"MM\0\x2a\0\0\0\x08\0\x01".to_vec();
    t.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0]);
    t.extend_from_slice(&[0, 0, 0, 0]);
    t
}

/// The thumbnail is flat mid grey so a test can tell it from the photo.
fn iphone_like() -> Vec<u8> {
    build(&Spec {
        props: vec![Prop::Icc(icc::write_named(NamedSpace::DisplayP3)), Prop::Irot(3)],
        exif: Some(exif_orientation_6()),
        thumbnail: Some(&|_, _| [126, 128, 128]),
        ignored_aux: vec!["urn:com:apple:photo:2020:aux:hdrgainmap"],
        ..Spec::new(96, 64, &grey)
    })
}

#[test]
fn an_iphone_like_heic_decodes_upright_in_display_p3() {
    let bytes = iphone_like();
    assert_eq!(sniff(&bytes), Some(Format::Heif));
    let d = decode(&bytes, DecodeOptions::default()).unwrap();
    // irot 3 = 90° clockwise, applied by the decoder; the EXIF Orientation is not applied again.
    assert_eq!((d.width, d.height, d.source_width, d.source_height, d.orientation), (64, 96, 64, 96, 1));
    assert_eq!(d.space.named, Some(NamedSpace::DisplayP3));
    assert_eq!(d.space.origin, SpaceOrigin::IccMatrixTrc);
    assert_eq!((d.bit_depth, d.has_alpha, d.float), (8, false, false));
    assert!(d.exif.is_some() && d.icc.is_some());
    // No nclx and no VUI: limited range, BT.601; neutral chroma stays neutral.
    let srgb = d.to_srgb8();
    let px = srgb.data[0];
    assert!(px[0].abs_diff(px[1]) <= 1 && px[1].abs_diff(px[2]) <= 1, "{px:?}");

    let h = read_header(&bytes).unwrap();
    assert_eq!((h.format, h.width, h.height, h.orientation), (Format::Heif, 64, 96, 1));
}

#[test]
fn a_small_decode_uses_the_embedded_thumbnail() {
    let bytes = iphone_like();
    // The 48 × 32 thumbnail, turned like the photo, covers a 32-pixel request.
    let d = decode(&bytes, DecodeOptions::fit(32, 32)).unwrap();
    assert!(d.width.max(d.height) <= 32 && d.height > d.width, "{}x{}", d.width, d.height);
    assert_eq!((d.source_width, d.source_height), (64, 96));
    assert_eq!(d.space.named, Some(NamedSpace::DisplayP3));
    let flat = d.to_srgb8();
    assert!(flat.data.iter().all(|p| *p == flat.data[0]), "the flat thumbnail, not the photo");
    // A thumbnail too small for the request: the photo is decoded and resized.
    let d = decode(&bytes, DecodeOptions::fit(60, 60)).unwrap();
    assert_eq!((d.width.max(d.height), d.source_width), (60, 64));
    let photo = d.to_srgb8();
    assert!(photo.data.iter().any(|p| *p != photo.data[0]));
    let t = decode_thumbnail(&bytes, 32).unwrap();
    assert_eq!((t.source_width, t.source_height), (64, 96));
}

#[test]
fn nclx_colour_and_ten_bit() {
    // Display P3 primaries, sRGB transfer, BT.709 matrix, full range: the nclx alone names the space.
    let bytes = build(&Spec { bit_depth: 10, props: vec![Prop::Nclx(12, 13, 1, true)], ..Spec::new(64, 32, &|x, _| [(x * 16) as u16, 512, 512]) });
    let d = decode(&bytes, DecodeOptions::default()).unwrap();
    assert_eq!(d.space.named, Some(NamedSpace::DisplayP3));
    assert_eq!(d.space.origin, SpaceOrigin::Container);
    assert_eq!(d.bit_depth, 10);
    // Full-range 10-bit neutral steps of 16 code values: linear values rise monotonically.
    let row: Vec<f32> = (0..64).map(|x| d.image.row(0)[x][1]).collect();
    assert!(row.windows(2).all(|w| w[1] > w[0]), "{row:?}");
}

#[test]
fn hostile_heic_is_an_error_never_a_panic() {
    let bytes = iphone_like();
    let full = decode(&bytes, DecodeOptions::default()).unwrap();
    for cut in (12..bytes.len()).step_by(11) {
        let b = &bytes[..cut];
        // Cut inside the photo: an error. Cut in a trailing item (gain map, thumbnail): the photo.
        if let (Ok(_), Ok(d)) = (read_header(b), decode(b, DecodeOptions::default())) {
            assert_eq!(d.image.data, full.image.data, "cut at {cut}");
        }
        let _ = decode_thumbnail(b, 16);
    }
    let r = decode(&bytes, DecodeOptions { max_pixels: 64 * 96 - 1, ..Default::default() });
    assert!(matches!(r, Err(Error::TooLarge(64, 96))), "{r:?}");
}
