//! A film scanner's RGBI TIFF (VueScan "64-bit RGBI"): RGB plus the infrared pass as a fourth
//! 16-bit sample, with no ExtraSamples tag. It decodes as an opaque RGB image with the infrared
//! plane beside it, not as a see-through RGBA one.

use lcc_codecs::*;

use lcc_testkit::tiff16;

#[test]
fn a_vuescan_rgbi_tiff_keeps_its_infrared_pass_and_is_opaque() {
    // a mid-grey negative with one dark dust speck in the infrared pass at (5, 3)
    let px = |x: u32, y: u32| vec![30000, 20000, 10000, if (x, y) == (5, 3) { 2000 } else { 40000 }];
    let bytes = tiff16(8, 6, 4, "VueScan 9 x64 (9.8.52)", None, px);
    let d = decode(&bytes, DecodeOptions::default()).unwrap();
    assert!(!d.has_alpha && d.alpha.is_none(), "the infrared pass is not transparency");
    let ir = d.infrared.as_ref().expect("the infrared plane is kept");
    assert_eq!((ir.width, ir.height), (8, 6));
    assert!((ir.get(0, 0) - 40000.0 / 65535.0).abs() < 1e-3);
    assert!(ir.get(5, 3) < 0.05 && ir.get(4, 3) > 0.6, "the speck is dark in the infrared");
    // the colour is the first three samples, untouched by the fourth
    let p = d.image.get(5, 3);
    let q = d.image.get(0, 0);
    assert!((p[0] - q[0]).abs() < 1e-6 && (p[2] - q[2]).abs() < 1e-6);
    // fitted: the plane follows the picture's size
    let small = decode(&bytes, DecodeOptions { max_size: Some((4, 4)), ..Default::default() }).unwrap();
    let ir = small.infrared.as_ref().unwrap();
    assert_eq!((ir.width, ir.height), (small.image.width, small.image.height));

    // the same layout from other software stays alpha, as before; one with ExtraSamples too
    for (software, extra) in [("GIMP 3.0", None), ("VueScan 9 x64", Some(2u16))] {
        let d = decode(&tiff16(8, 6, 4, software, extra, px), DecodeOptions::default()).unwrap();
        assert!(d.has_alpha && d.infrared.is_none(), "{software} {extra:?}");
    }
    // an 8-bit four-sample VueScan file is not the RGBI layout
    // (hostile: a truncated file errors, never panics)
    let mut cut = tiff16(8, 6, 4, "VueScan 9 x64", None, px);
    cut.truncate(cut.len() - 40);
    assert!(decode(&cut, DecodeOptions::default()).is_err());
}
