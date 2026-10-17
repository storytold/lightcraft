//! Golden PDF test for slideshow export (Phase 3 gate): a PDF of the demo library is parsed back
//! with `lopdf` (valid xref, one page per frame, page size = slide at 96 ppi, one JPEG per page
//! at the slide's pixel size) and every page is rasterised with `hayro` (pure Rust,
//! MIT/Apache-2.0, dev-only) and compared with the embedded frame decoded on its own: the PDF
//! must show the frame exactly where and how it was composed.

use std::sync::Arc;

use lcc_catalog::PhotoId;
use lcc_engine::Session;
use lcc_slideshow::export::prepare_pdf;
use lcc_slideshow::settings::Settings;
use lopdf::Object;

#[test]
fn slideshow_pdf_pages_match_their_frames() {
    let mut session = Session::with_demo();
    let ids: Vec<PhotoId> = session.catalog.photos().take(2).map(|p| p.id).collect();
    let mut s = Settings::default();
    s.titles.intro.enabled = true;
    s.titles.intro.text = "Golden".into();
    let dir = std::env::temp_dir().join(format!("lcc-slideshow-golden-{}", std::process::id()));
    let p = prepare_pdf(&mut session, &ids, &s, 320, 180, 90, &dir, "Golden", "").unwrap();
    let files = std::thread::spawn(move || p.run(&mut |_, _| true)).join().unwrap().unwrap();
    let bytes = std::fs::read(&files[0]).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    let doc = lopdf::Document::load_mem(&bytes).expect("valid xref and trailer");
    let size = doc.trailer.get(b"Size").unwrap().as_i64().unwrap() as u32;
    assert!(doc.objects.keys().all(|(id, _)| *id < size));
    let pages = doc.get_pages();
    assert_eq!(pages.len(), 3, "intro + two slides");
    let mut frames = Vec::new();
    for (_, id) in pages {
        let pd = doc.get_dictionary(id).unwrap();
        let mb: Vec<i64> = match pd.get(b"MediaBox").unwrap() {
            Object::Array(a) => a.iter().map(|v| v.as_float().map(|f| f as i64).or_else(|_| v.as_i64()).unwrap()).collect(),
            o => panic!("{o:?}"),
        };
        assert_eq!(mb, [0, 0, 240, 135], "320×180 px at 96 ppi");
        let res = doc.get_dictionary(id).unwrap().get(b"Resources").unwrap();
        let res = match res {
            Object::Reference(r) => doc.get_dictionary(*r).unwrap(),
            Object::Dictionary(d) => d,
            o => panic!("{o:?}"),
        };
        let xo = match res.get(b"XObject").unwrap() {
            Object::Reference(r) => doc.get_dictionary(*r).unwrap(),
            Object::Dictionary(d) => d,
            o => panic!("{o:?}"),
        };
        assert_eq!(xo.len(), 1);
        let im = doc.get_object(xo.iter().next().unwrap().1.as_reference().unwrap()).unwrap().as_stream().unwrap();
        assert_eq!(im.dict.get(b"Width").unwrap().as_i64().unwrap(), 320, "frame embedded at full resolution");
        assert_eq!(im.dict.get(b"Height").unwrap().as_i64().unwrap(), 180);
        assert_eq!(im.dict.get(b"Filter").unwrap().as_name().unwrap(), b"DCTDecode");
        frames.push(lcc_codecs::decode_thumbnail(&im.content, 4096).unwrap().image);
    }

    // Rasterise at 4/3 px per pt = the frame's own pixel grid, compare with the decoded frame.
    let pdf = hayro::hayro_syntax::Pdf::new(Arc::new(bytes)).unwrap();
    let rendered = hayro::render_pdf(&pdf, 320.0 / 240.0, hayro::hayro_interpret::InterpreterSettings::default(), None).unwrap();
    assert_eq!(rendered.len(), 3);
    for (k, (r, f)) in rendered.iter().zip(&frames).enumerate() {
        assert_eq!((usize::from(r.width()), usize::from(r.height())), (320, 180), "page {k}");
        assert_eq!((f.width, f.height), (320, 180));
        let px = r.data_as_u8_slice();
        let mut diff = 0u64;
        let mut worst = 0u8;
        for y in 0..180 {
            for x in 0..320 {
                let a = f.get(x, y);
                let i = (y * 320 + x) * 4;
                for c in 0..3 {
                    let d = a[c].abs_diff(px[i + c]);
                    diff += u64::from(d);
                    worst = worst.max(d);
                }
            }
        }
        let mean = diff as f64 / (320.0 * 180.0 * 3.0);
        // Same pixels up to resampling at the edges and JPEG decoder differences.
        assert!(mean < 3.0, "page {k}: mean abs difference {mean:.2} (worst {worst})");
    }
}
