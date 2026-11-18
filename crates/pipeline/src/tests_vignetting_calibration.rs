use super::*;
use lcc_develop::VignettingMapOrigin;

/// The Z 7's sensor, the 3:2 frame on all of it and the 5:4 frame inside it (as CropHiSpeed records them).
const SENSOR: [usize; 2] = [8288, 5520];
const FIVE_FOUR: SensorWindow = SensorWindow { sensor: SENSOR, origin: [688, 0], size: [6912, 5520] };
const THREE_TWO: SensorWindow = SensorWindow { sensor: SENSOR, origin: [0, 0], size: SENSOR };

/// A 48 × 32 map of a lens falling off 1.4 EV to the corners, off centre, as the develop settings carry it.
fn embedded() -> EmbeddedVignettingMap {
    let grid = [48usize, 32];
    let ev: Vec<f64> = (0..grid[0] * grid[1])
        .map(|i| {
            let (c, r) = ((i % grid[0]) as f64 + 0.5, (i / grid[0]) as f64 + 0.5);
            let (dx, dy) = (c / grid[0] as f64 - 0.52, (r / grid[1] as f64 - 0.49) * 5520.0 / 8288.0);
            let d2 = (dx * dx + dy * dy) / (0.25 + 0.25 * (5520.0f64 / 8288.0).powi(2));
            1.4 * d2 * d2 + 0.2 * d2
        })
        .collect();
    let w = THREE_TWO;
    let window = Some([w.sensor[0], w.sensor[1], w.origin[0], w.origin[1], w.size[0], w.size[1]]);
    EmbeddedVignettingMap::from_ev(SENSOR, grid, window, VignettingMapOrigin::default(), &ev).unwrap()
}

use lcc_raw::gaintable::SourcePlacement;

thread_local! {
    /// The maps the tests' settings reference: the catalog's table, as the session hands it to renders.
    static STORE: std::cell::RefCell<Vec<EmbeddedVignettingMap>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Settings referencing `map` (stored for [`store`]) at `amount`.
fn settings(map: Option<EmbeddedVignettingMap>, amount: f64) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.optics.vignetting_calibration = map.as_ref().map(VignettingMapId::of);
    if let Some(m) = map {
        STORE.with(|st| st.borrow_mut().push(m));
    }
    s.optics.vignetting_calibration_amount = amount;
    s
}

/// Every map the tests' settings referenced so far.
fn store() -> VignettingMapStore {
    VignettingMapStore::of(STORE.with(|st| st.borrow().clone()))
}

/// [`super::field_for`] for a source placed at `placement`, with the maps of [`store`].
fn field_for(placement: Option<&SourcePlacement>, s: &DevelopSettings, user: Orientation) -> Option<Arc<FieldGain>> {
    let info = crate::SourceInfo { raw: true, placement: placement.copied(), vignetting_maps: Some(store()), ..Default::default() };
    super::field_for(&info, s, user)
}

#[test]
fn a_map_missing_from_the_store_applies_nothing() {
    let e = embedded();
    let s = settings(Some(e.clone()), 100.0);
    let p = placed(THREE_TWO, Orientation::Normal);
    let bare = crate::SourceInfo { raw: true, placement: Some(p), ..Default::default() };
    assert!(super::field_for(&bare, &s, Orientation::Normal).is_none(), "no store");
    let other = crate::SourceInfo { vignetting_maps: Some(VignettingMapStore::of([])), ..bare.clone() };
    assert!(super::field_for(&other, &s, Orientation::Normal).is_none(), "the map isn't in it");
    let with = crate::SourceInfo { vignetting_maps: Some(VignettingMapStore::of([e])), ..bare };
    assert!(super::field_for(&with, &s, Orientation::Normal).is_some());
}

/// A raw's placement: a default crop inside the active area, the file's orientation, the active size and window.
fn placed(win: SensorWindow, exif: Orientation) -> SourcePlacement {
    let [aw, ah] = win.size.map(|v| v as f64);
    let rect = [8.0 / aw, 4.0 / ah, (aw - 16.0) / aw, (ah - 8.0) / ah];
    SourcePlacement { active_size: Some(win.size), window: Some(win), ..SourcePlacement::new(rect, exif) }
}

#[test]
fn the_settings_map_converts_to_the_measured_form_and_back() {
    let e = embedded();
    let m = map_of(&e).unwrap();
    assert_eq!((m.sensor, m.grid, m.window), (SENSOR, [48, 32], Some(THREE_TWO)));
    assert_eq!(embed(&m).as_ref(), Some(&e));
    for i in [0, 100, 48 * 32 - 1] {
        assert!((f64::from(m.gains[i]).log2() - e.ev_of(i)).abs() < 1e-6);
    }
}

/// Plan change 2: 64 × 64 nodes span the whole frame (corners included); the gain at the frame's four corner pixels
/// is the map's at the sensor positions under them within 0.005 EV, for a 3:2 and a 5:4 frame, upright and turned.
#[test]
fn the_field_matches_the_map_at_the_frames_corner_pixels() {
    let e = embedded();
    let m = map_of(&e).unwrap();
    for (name, win) in [("3:2", THREE_TWO), ("5:4", FIVE_FOUR)] {
        for exif in [Orientation::Normal, Orientation::Rotate90] {
            for user in [Orientation::Normal, Orientation::Rotate270, Orientation::FlipH] {
                let p = placed(win, exif);
                let f = field_for(Some(&p), &settings(Some(e.clone()), 100.0), user).unwrap();
                assert_eq!(f.ev.len(), FIELD_NODES * FIELD_NODES);
                // the frame the geometry stage samples: the developed crop after both orientations, a fitted copy
                let (cw, ch) = ((win.size[0] - 16) as f64 / 4.0, (win.size[1] - 8) as f64 / 4.0);
                let swaps = exif.swaps_axes() != user.swaps_axes();
                let (w, h) = if swaps { (ch, cw) } else { (cw, ch) };
                let mut worst: f64 = 0.0;
                for (x, y) in [(0.5, 0.5), (w - 0.5, 0.5), (0.5, h - 0.5), (w - 0.5, h - 0.5), (w / 2.0, h / 2.0)] {
                    let (u, v) = (x / w, y / h);
                    let (su, sv) = user.inverse().map(u, v, 1.0, 1.0);
                    let want = m.ev_on(&p, su, sv).unwrap();
                    let got = f.ev_at(u, v);
                    worst = worst.max((got - want).abs());
                    // and the gain itself, in the frame's pixels
                    assert!((f.gain(Point::new(x, y), w, h).log2() - got).abs() < 1e-9);
                }
                eprintln!("{name} {exif:?} {user:?}: worst corner error {worst:.5} EV");
                assert!(worst <= 0.005, "{name} {exif:?} {user:?}: {worst} EV");
            }
        }
        // the frame's corner is a node: no extrapolation past it
        let f = field_for(Some(&placed(win, Orientation::Normal)), &settings(Some(e.clone()), 100.0), Orientation::Normal).unwrap();
        assert_eq!(f.ev_at(0.0, 0.0), f64::from(f.ev[0]));
        assert_eq!(f.ev_at(1.0, 1.0), f64::from(f.ev[FIELD_NODES * FIELD_NODES - 1]));
        assert_eq!(f.ev_at(-3.0, 9.0), f.ev_at(0.0, 1.0));
        assert_eq!(f.ev_at(f64::NAN, 0.0), f.ev_at(0.0, 0.0));
    }
}

#[test]
fn nothing_applies_without_a_map_a_raw_or_cover() {
    let e = embedded();
    let p = placed(FIVE_FOUR, Orientation::Normal);
    let on = settings(Some(e.clone()), 100.0);
    assert!(field_for(Some(&p), &on, Orientation::Normal).is_some());
    assert!(field_for(Some(&p), &settings(None, 100.0), Orientation::Normal).is_none(), "no map");
    assert!(field_for(None, &on, Orientation::Normal).is_none(), "not a raw");
    let mut off = on.clone();
    off.disabled_sections.push("optics".into());
    assert!(field_for(Some(&p), &off, Orientation::Normal).is_none(), "the Optics eye off");
    // a 5:4 map can't serve the 3:2 frame
    let w = FIVE_FOUR;
    let small = EmbeddedVignettingMap {
        sensor: w.size,
        window: Some([w.sensor[0], w.sensor[1], w.origin[0], w.origin[1], w.size[0], w.size[1]]),
        ..e.clone()
    };
    assert!(field_for(Some(&placed(THREE_TWO, Orientation::Normal)), &settings(Some(small.clone()), 100.0), Orientation::Normal).is_none());
    assert!(field_for(Some(&placed(FIVE_FOUR, Orientation::Normal)), &settings(Some(small), 100.0), Orientation::Normal).is_some());
    // another body (another sensor size), and a placement without an active size (a gain table's)
    let other = SensorWindow { sensor: [8256, 5504], origin: [0, 0], size: [8256, 5504] };
    assert!(field_for(Some(&placed(other, Orientation::Normal)), &on, Orientation::Normal).is_none());
    assert!(field_for(Some(&SourcePlacement::new([0.0, 0.0, 1.0, 1.0], Orientation::Normal)), &on, Orientation::Normal).is_none());
}

#[test]
fn the_amount_is_a_fraction_and_the_debug_form_is_short() {
    let e = embedded();
    let p = placed(THREE_TWO, Orientation::Normal);
    let f = |a: f64| field_for(Some(&p), &settings(Some(e.clone()), a), Orientation::Normal).unwrap();
    assert_eq!((f(100.0).amount, f(0.0).amount, f(120.0).amount, f(500.0).amount), (1.0, 0.0, 1.2, 1.2));
    assert_eq!(f(100.0).key, f(50.0).key, "the key is the grid's");
    let corner = f(120.0).gain(Point::new(0.0, 0.0), 10.0, 10.0).log2();
    assert!((corner - 1.2 * f(100.0).ev_at(0.0, 0.0)).abs() < 1e-6);
    assert_eq!(f(0.0).gain(Point::new(0.0, 0.0), 10.0, 10.0), 1.0);
    let d = format!("{:?}", f(80.0));
    assert!(d.len() < 48 && d.contains("0.8"), "{d}");
}

// ---- the CPU render (§10 step 3)

use crate::{PixelWindow, SourceInfo, frame_for};
use lcc_raster::Rgb32f;

/// A small raw's active area (`W × H`, no default crop) whose light falls off as `falloff` (EV, darker = positive)
/// at sensor position `(x, y)`, developed upright: the source pixels, the map of it, and the source's facts.
const W: usize = 240;
const H: usize = 160;

fn falloff(x: f64, y: f64) -> f64 {
    let (dx, dy) = ((x / W as f64 - 0.5) * 2.0, (y / H as f64 - 0.5) * 2.0 * H as f64 / W as f64);
    let d2 = (dx * dx + dy * dy) / (1.0 + (H as f64 / W as f64).powi(2));
    1.2 * d2 * d2 + 0.3 * d2
}

fn raw_scene() -> (Rgb32f, EmbeddedVignettingMap, SourceInfo) {
    let mut img = Rgb32f::new(W, H);
    for (i, px) in img.data.iter_mut().enumerate() {
        let g = (-falloff((i % W) as f64 + 0.5, (i / W) as f64 + 0.5)).exp2() as f32;
        *px = [0.5 * g, 0.4 * g, 0.3 * g];
    }
    let grid = [48usize, 32];
    let ev: Vec<f64> = (0..grid[0] * grid[1])
        .map(|i| {
            let (c, r) = ((i % grid[0]) as f64 + 0.5, (i / grid[0]) as f64 + 0.5);
            falloff(c * W as f64 / grid[0] as f64, r * H as f64 / grid[1] as f64)
        })
        .collect();
    let map = EmbeddedVignettingMap::from_ev([W, H], grid, None, VignettingMapOrigin::default(), &ev).unwrap();
    let placement = SourcePlacement { active_size: Some([W, H]), ..SourcePlacement::new([0.0, 0.0, 1.0, 1.0], Orientation::Normal) };
    let vignetting_maps = Some(VignettingMapStore::of([map.clone()]));
    (img, map, SourceInfo { raw: true, placement: Some(placement), vignetting_maps, ..Default::default() })
}

/// log2 of each output pixel's green over the uncorrected scene's centre value (0.4): 0 everywhere when flat.
fn ev_off_flat(out: &Rgb32f) -> f64 {
    out.data.iter().map(|p| (f64::from(p[1]) / 0.4).log2().abs()).fold(0.0, f64::max)
}

#[test]
fn a_falloff_and_its_own_map_render_flat_in_every_orientation() {
    let (src, map, info) = raw_scene();
    for user in [Orientation::Normal, Orientation::Rotate90, Orientation::Rotate180, Orientation::FlipH, Orientation::Transverse] {
        let mut s = settings(Some(map.clone()), 100.0);
        s.orientation = user;
        let f = frame_for(&src, &info, &s, false);
        assert!(f.warp.as_ref().is_some_and(|w| w.field.is_some()), "{user:?}");
        let (w, h) = (f.ow as usize, f.oh as usize);
        let out = f.sample(&src, w, h);
        let worst = ev_off_flat(&out);
        eprintln!("{user:?}: worst {worst:.4} EV off flat (the scene falls off {:.2} EV)", falloff(0.5, 0.5));
        assert!(worst < 0.01, "{user:?}: {worst} EV");
        // a fitted (smaller) output too
        assert!(ev_off_flat(&f.sample(&src, w / 3, h / 3)) < 0.01, "{user:?} fitted");
    }
}

#[test]
fn the_amount_scales_the_correction_and_zero_is_no_correction() {
    let (src, map, info) = raw_scene();
    let plain = frame_for(&src, &info, &settings(None, 100.0), false).sample(&src, W, H);
    let zero = frame_for(&src, &info, &settings(Some(map.clone()), 0.0), false);
    assert!(zero.warp.is_none(), "amount 0 leaves no warp");
    assert_eq!(zero.sample(&src, W, H).data, plain.data, "amount 0 is the uncorrected source exactly");
    let more = frame_for(&src, &info, &settings(Some(map.clone()), 120.0), false).sample(&src, W, H);
    let half = frame_for(&src, &info, &settings(Some(map), 50.0), false).sample(&src, W, H);
    for (i, ((p, m), hf)) in plain.data.iter().zip(&more.data).zip(&half.data).enumerate() {
        let f = falloff((i % W) as f64 + 0.5, (i / W) as f64 + 0.5);
        let (em, eh) = ((f64::from(m[1]) / f64::from(p[1])).log2(), (f64::from(hf[1]) / f64::from(p[1])).log2());
        assert!((em - 1.2 * f).abs() < 0.012 && (eh - 0.5 * f).abs() < 0.006, "at {i}: {em} / {eh} for {f}");
    }
}

#[test]
fn manual_vignetting_composes_and_the_profile_vignetting_steps_aside() {
    let (src, map, info) = raw_scene();
    // manual vignetting on top of a flat-field corrected source = manual vignetting on a flat source
    let mut flat = Rgb32f::new(W, H);
    flat.data.iter_mut().for_each(|p| *p = [0.5, 0.4, 0.3]);
    let mut s = settings(Some(map.clone()), 100.0);
    s.optics.vignetting = -60.0;
    let both = frame_for(&src, &info, &s, false).sample(&src, W, H);
    let vig_only = frame_for(&flat, &info, &settings(None, 100.0).tap(|t| t.optics.vignetting = -60.0), false).sample(&flat, W, H);
    let worst = both.data.iter().zip(&vig_only.data).map(|(a, b)| (f64::from(a[1]) / f64::from(b[1])).log2().abs()).fold(0.0, f64::max);
    assert!(worst < 0.01, "{worst} EV");
    // a lens profile's vignetting is off while a map applies, its distortion stays; without a map it's on
    let lens = lcc_develop::EmbeddedLens {
        warp: Some(lcc_develop::EmbeddedWarp { planes: [[1.0, -0.02, 0.0, 0.0, 0.0, 0.0]; 3], center: Point::new(0.5, 0.5), radius: 0.6 }),
        vignette: Some(lcc_develop::EmbeddedVignette { k: [0.4, 0.0, 0.0, 0.0, 0.0], center: Point::new(0.5, 0.5), radius: 0.6 }),
        stages: Default::default(),
    };
    let info = SourceInfo { lens: Some(lens), ..info };
    let mut on = settings(Some(map), 100.0);
    on.optics.lens_profile = true;
    let wp = frame_for(&src, &info, &on, false).warp.unwrap();
    assert_eq!((wp.lens_vig, wp.lens_dist), (0.0, 1.0));
    assert!(wp.field.is_some());
    on.optics.vignetting_calibration = None;
    assert_eq!(frame_for(&src, &info, &on, false).warp.unwrap().lens_vig, 1.0);
    // the map applies with lens corrections off as well
    let off = settings(Some(raw_scene().1), 100.0);
    assert!(!off.optics.lens_profile && frame_for(&src, &info, &off, false).warp.is_some_and(|w| w.field.is_some() && w.lens_dist == 0.0));
}

#[test]
fn windows_sampled_together_match_windows_sampled_alone() {
    let (src, map, info) = raw_scene();
    let mut s = settings(Some(map), 100.0);
    s.orientation = Orientation::Rotate90;
    let f = frame_for(&src, &info, &s, false);
    let (fw, fh) = (f.ow as usize, f.oh as usize);
    let wins =
        [PixelWindow { x: 0, y: 0, w: 40, h: 40 }, PixelWindow { x: fw - 50, y: fh - 30, w: 50, h: 30 }, PixelWindow { x: 0, y: 0, w: fw, h: fh }];
    let together = f.sample_windows(&src, fw, fh, &wins);
    for (t, out) in wins.iter().zip(&together) {
        let alone = if (t.w, t.h) == (fw, fh) { f.sample(&src, fw, fh) } else { f.window(fw, fh, *t).sample(&src, t.w, t.h) };
        assert_eq!(out.data, alone.data, "{t:?}");
    }
    // and the corner tile is corrected like the whole frame's corner
    let whole = &together[2];
    assert!((f64::from(together[0].get(0, 0)[1]) / f64::from(whole.get(0, 0)[1]) - 1.0).abs() < 1e-6);
}

#[test]
fn settings_without_a_map_give_the_frame_they_gave_before() {
    let (src, _, info) = raw_scene();
    let before = SourceInfo { placement: None, ..info.clone() };
    for s in [settings(None, 100.0), settings(None, 40.0), DevelopSettings { orientation: Orientation::Rotate270, ..Default::default() }] {
        assert_eq!(frame_for(&src, &info, &s, true), frame_for(&src, &before, &s, true));
        assert!(frame_for(&src, &info, &s, true).warp.is_none());
    }
}

trait Tap: Sized {
    fn tap(mut self, f: impl FnOnce(&mut Self)) -> Self {
        f(&mut self);
        self
    }
}
impl Tap for DevelopSettings {}
