//! High Quality Preview ([`crate::hq_preview`]): the loupe's view of a sparse, near-binary image
//! under a non-monotonic curve is the full-size render scaled down, not a develop of averaged
//! pixels.

use serde_json::json;

use crate::Session;
use crate::hq_preview::HqState;
use crate::tests_xmp::temp_dir;
use lightcraft_catalog::PhotoId;
use lightcraft_pipeline::PixelWindow;
use lightcraft_raster::Rgba8;

/// Larger than the Preview source level (2560 px), so the fit view and the native frame come from
/// different sources, as for a real photo.
const W: usize = 3000;
const H: usize = 2000;

/// A deterministic star field: about 5 % of the pixels white, the rest black (generated here,
/// never committed), imported into a fresh library, with a curve that turns white grey and
/// lifts the dark mid-tones a lot (non-monotonic).
fn sparse(tag: &str) -> (Session, PhotoId, std::path::PathBuf) {
    let dir = temp_dir(&format!("hq-{tag}"));
    let src = dir.join("stars.png");
    let img = Rgba8::from_fn(W, H, |x, y| {
        let mut h = (x as u32).wrapping_mul(0x9e37_79b9) ^ (y as u32).wrapping_mul(0x85eb_ca6b);
        h ^= h >> 15;
        h = h.wrapping_mul(0x2c1b_3c6d);
        h ^= h >> 13;
        if h.is_multiple_of(20) { [255, 255, 255, 255] } else { [0, 0, 0, 255] }
    });
    let o = crate::export::ExportOptions { format: crate::export::ExportFormat::Png, ..Default::default() };
    std::fs::write(&src, crate::export::encode_image(&img, &o).unwrap()).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.execute("develop.curve", &json!({"channel": "master", "points": [[0.0, 0.0], [0.4, 1.0], [0.85, 0.15], [1.0, 0.55]]})).unwrap();
    (s, id, dir)
}

fn rendered(job: crate::RenderJob) -> lightcraft_pipeline::Rendered {
    job.run().rendered.unwrap()
}

/// The pixels a full-size export encodes: its size, and the render of its own job.
fn full_size_export(s: &mut Session, id: PhotoId) -> Rgba8 {
    let p = s.catalog.photo(id).unwrap().clone();
    let (w, h) = crate::export::output_size(&p, &crate::export::ExportOptions::default());
    assert_eq!((w, h), (W, H));
    let job = s.export_job(id, w, h, lightcraft_pipeline::OutputSpace::Srgb, lightcraft_pipeline::OutputDepth::U8).unwrap();
    rendered(job).image
}

/// `img` reduced `k`× by plain averaging of light (the reference, independent of the code under
/// test): exact sRGB decode, mean, exact encode.
fn reference_average(img: &Rgba8, k: usize) -> Rgba8 {
    use lightcraft_color::transfer::{linear_to_srgb, srgb_to_linear};
    Rgba8::from_fn(img.width / k, img.height / k, |x, y| {
        let mut acc = [0.0f64; 3];
        for j in 0..k {
            for i in 0..k {
                let p = img.get(x * k + i, y * k + j);
                for c in 0..3 {
                    acc[c] += srgb_to_linear(p[c] as f32 / 255.0) as f64;
                }
            }
        }
        let e = |v: f64| (linear_to_srgb((v / (k * k) as f64) as f32) * 255.0).round() as u8;
        [e(acc[0]), e(acc[1]), e(acc[2]), 255]
    })
}

fn mean_abs_diff(a: &Rgba8, b: &Rgba8) -> f64 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let sum: u64 = a.data.iter().zip(&b.data).map(|(p, q)| (0..3).map(|c| p[c].abs_diff(q[c]) as u64).sum::<u64>()).sum();
    sum as f64 / (3 * a.data.len()) as f64
}

fn max_diff(a: &Rgba8, b: &Rgba8) -> u8 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    a.data.iter().zip(&b.data).map(|(p, q)| (0..3).map(|c| p[c].abs_diff(q[c])).max().unwrap_or(0)).max().unwrap_or(0)
}

/// (a) The normal fit view differs strongly from the full-size export scaled down; (b) the High
/// Quality fit view is that, within one level, and its histogram is the export's.
#[test]
fn hq_fit_view_is_the_full_size_render_scaled_down() {
    let (mut s, id, dir) = sparse("fit");
    let export = full_size_export(&mut s, id);
    // 3000 → 600: 5 × 5 blocks
    let want = reference_average(&export, 5);
    let normal = s.loupe_job(id, 600, 600, true).unwrap();
    let (hq, state) = s.high_quality(normal.clone());
    assert_eq!(state, HqState::Applied { native: (W, H) });
    assert_eq!(hq.level, crate::SourceLevel::Full, "the export's source level");
    assert_ne!(hq.key, normal.key, "a result key of its own");
    let n = rendered(normal).image;
    assert_eq!((n.width, n.height), (600, 400));
    let d = mean_abs_diff(&n, &want);
    assert!(d > 20.0, "the normal view develops averaged pixels: {d:.1} levels from the full-size render");
    let h = rendered(hq);
    assert_eq!((h.image.width, h.image.height), (600, 400));
    let m = max_diff(&h.image, &want);
    assert!(m <= 1, "the High Quality view is the full-size render scaled down: {m} levels off");
    let native = rendered(s.render_job(id, W, H, false, true).unwrap());
    assert!(native.histogram == h.histogram, "the histogram is the full-size render's");
    let _ = std::fs::remove_dir_all(&dir);
}

/// (c) A window at 37 % is the matching part of the High Quality whole frame at that scale.
#[test]
fn hq_window_matches_the_hq_whole_frame() {
    let (mut s, id, dir) = sparse("window");
    let (fw, fh) = (1110, 740);
    let (whole, state) = {
        let j = s.loupe_job(id, fw, fh, true).unwrap();
        s.high_quality(j)
    };
    assert!(matches!(state, HqState::Applied { .. }), "{state:?}");
    let whole = rendered(whole).image;
    assert_eq!((whole.width, whole.height), (fw, fh));
    // two neighbouring windows, cut at pixels that are fractional in the native frame
    for win in [PixelWindow { x: 256, y: 256, w: 512, h: 256 }, PixelWindow { x: 768, y: 256, w: 342, h: 484 }] {
        let j = s.region_job(id, fw, fh, win, true).unwrap();
        let (j, state) = s.high_quality(j);
        assert!(matches!(state, HqState::Applied { .. }), "{state:?}");
        let w = j.request.window.unwrap();
        assert!(w.w < W && w.x > 0, "a native window, not the whole frame: {w:?}");
        let tile = rendered(j).image;
        let crop = Rgba8::from_fn(win.w, win.h, |x, y| whole.get(win.x + x, win.y + y));
        let m = max_diff(&tile, &crop);
        assert!(m <= 2, "{win:?}: {m} levels off the whole frame");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// (d) At 100 % and more the window job is the normal one: same key, same pixels.
#[test]
fn hq_at_full_size_changes_nothing() {
    let (mut s, id, dir) = sparse("native");
    let win = PixelWindow { x: 512, y: 256, w: 256, h: 256 };
    for (fw, fh) in [(W, H), (4500, 3000)] {
        let j = s.region_job(id, fw, fh, win, true).unwrap();
        let (hq, state) = s.high_quality(j.clone());
        assert_eq!(state, HqState::Native);
        assert_eq!(hq.key, j.key);
        assert!(hq.hq.is_none() && hq.request.window == j.request.window);
        if fw == W {
            assert_eq!(rendered(hq).image, rendered(j).image);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// (e) A High Quality render is never kept as the photo's view preview or thumbnail, and drafts,
/// thumbnails and HDR requests are left alone.
#[test]
fn hq_jobs_never_write_the_preview_caches() {
    let (mut s, id, dir) = sparse("caches");
    let view_cached = |s: &mut Session| {
        let q = s.quick_view_job(id, 600, true).unwrap();
        q.cached.iter().any(|(c, k)| c.get(*k).is_some())
    };
    let j = s.loupe_job(id, 600, 600, true).unwrap();
    assert!(j.view_cache.is_some());
    let (hq, _) = s.high_quality(j.clone());
    assert!(hq.view_cache.is_none() && hq.cache.is_none());
    rendered(hq);
    assert!(!view_cached(&mut s), "the High Quality render is not the cached view preview");
    rendered(j);
    assert!(view_cached(&mut s), "(the normal render is)");

    let thumb = s.thumb_job(id, 256).unwrap();
    let (t, state) = s.high_quality(thumb.clone());
    assert!(matches!(state, HqState::Skipped { .. }) && t.key == thumb.key && t.hq.is_none(), "{state:?}");
    let draft = s.loupe_job(id, 600, 600, true).unwrap().draft();
    let (d, state) = s.high_quality(draft.clone());
    assert!(matches!(&state, HqState::Skipped { reason } if reason.contains("draft")) && d.key == draft.key);
    let mut deep = s.loupe_job(id, 600, 600, true).unwrap();
    deep.request.depth = lightcraft_pipeline::OutputDepth::F32Hdr;
    let (_, state) = s.high_quality(deep);
    assert!(matches!(&state, HqState::Skipped { reason } if reason.contains("HDR")), "{state:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A soft-proofed and a display-profile High Quality job still scale the native render (the
/// display's correction comes after it) and keep the native histogram.
#[test]
fn hq_with_a_display_profile_keeps_the_native_histogram() {
    let (mut s, id, dir) = sparse("display");
    let icc = lightcraft_codecs::icc::write_named(lightcraft_codecs::NamedSpace::DisplayP3);
    let d = std::sync::Arc::new(crate::display::Display::from_bytes("p3.icc", &icc).unwrap());
    let j = s.loupe_job(id, 600, 600, true).unwrap();
    let (hq, _) = s.high_quality(j);
    let hq = hq.with_display(Some(d.clone()));
    let r = hq.run();
    assert_eq!(r.display, Some(d.id()));
    let img = r.rendered.unwrap();
    assert_eq!((img.image.width, img.image.height), (600, 400));
    let native = rendered(s.render_job(id, W, H, false, true).unwrap());
    // the same pixels' sRGB histogram (to rounding of the display round trip)
    let (a, b) = (&native.histogram, &img.histogram);
    let total = |h: &lightcraft_raster::Histogram| h.luma.iter().map(|v| *v as u64).sum::<u64>();
    assert_eq!(total(a), total(b), "sampled from the native render, not the 600 px one");
    let _ = std::fs::remove_dir_all(&dir);
}

/// At rest the loupe asks for at least the preview level (2560 px here) and shrinks it to the
/// drawn size ([`crate::RenderJob::with_display_fit`]). A High Quality job averages the native
/// render straight to the drawn size instead: no 2560 px step and no Lanczos pass over averaged,
/// display-referred pixels (which rings around sparse detail).
#[test]
fn hq_averages_straight_to_the_display_fit() {
    let (mut s, id, dir) = sparse("display-fit");
    let export = full_size_export(&mut s, id);
    let want = reference_average(&export, 5);
    let job = s.loupe_job(id, 2560, 1707, true).unwrap().with_display_fit(600, 600);
    let (hq, state) = s.high_quality(job.clone());
    assert_eq!(state, HqState::Applied { native: (W, H) });
    assert_eq!(hq.display_fit, None, "the display fit is folded into the downscale");
    assert_eq!(hq.hq.map(|h| (h.src, h.out)), Some((None, (600, 400))));
    let h = rendered(hq).image;
    assert_eq!((h.width, h.height), (600, 400));
    let m = max_diff(&h, &want);
    assert!(m <= 1, "the full-size render averaged to the drawn size: {m} levels off");
    // (the same view without High Quality: the same size, by its Lanczos display fit)
    let n = rendered(job).image;
    assert_eq!((n.width, n.height), (600, 400));
    // drawn at 100 % or more: the normal render is already the native one
    let at_full = s.loupe_job(id, W, H, true).unwrap().with_display_fit(W, H);
    assert_eq!(s.high_quality(at_full).1, HqState::Native);
    // a window keeps its own size (windows are not display-fitted)
    let mut win = s.region_job(id, 1110, 740, PixelWindow { x: 0, y: 0, w: 256, h: 256 }, true).unwrap();
    win.display_fit = Some((100, 100));
    let (w, _) = s.high_quality(win);
    assert_eq!(w.hq.map(|h| h.out), Some((256, 256)));
    let _ = std::fs::remove_dir_all(&dir);
}
