use super::*;
use crate::{Cfa, Normalized};

const W: usize = 1200;
const H: usize = 800;

/// Deterministic noise.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    /// About N(0, 1) (Irwin–Hall of 4).
    fn gauss(&mut self) -> f64 {
        (self.next() + self.next() + self.next() + self.next() - 2.0) * 3f64.sqrt()
    }
}

/// A fall-off in EV (positive = darker): `k · d⁴`, `d` = distance from `(cx·W, cy·H)` over the half diagonal.
fn radial(k: f64, cx: f64, cy: f64) -> impl Fn(f64, f64) -> f64 {
    let hd = (W as f64).hypot(H as f64) / 2.0;
    move |x, y| {
        let d = ((x - cx * W as f64).powi(2) + (y - cy * H as f64).powi(2)).sqrt() / hd;
        k * d.powi(4)
    }
}

struct Scene<'a> {
    falloff: &'a dyn Fn(f64, f64) -> f64,
    seed: u64,
    /// Dust discs: centre x, y and radius (px); each halves the light inside.
    dust: Vec<(f64, f64, f64)>,
    /// Fraction of photosites that are hot (read 0.9; a quarter of them 1.0).
    hot: f64,
}

/// An RGGB mosaic of a flat field at mid grey through `falloff`, with 1 % noise.
fn mosaic(s: &Scene) -> Normalized {
    let mut rng = Lcg(s.seed);
    let mut data = vec![0f32; W * H];
    for y in 0..H {
        for x in 0..W {
            let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
            let mut v = 0.5 * (-(s.falloff)(px, py)).exp2() * (1.0 + 0.01 * rng.gauss());
            if s.dust.iter().any(|&(dx, dy, r)| (px - dx).hypot(py - dy) <= r) {
                v *= 0.5;
            }
            if rng.next() < s.hot {
                v = if rng.next() < 0.25 { 1.0 } else { 0.9 };
            }
            data[y * W + x] = v as f32;
        }
    }
    Normalized { width: W, height: H, cpp: 1, data, cfa: Cfa::bayer("RGGB") }
}

fn clean(falloff: &dyn Fn(f64, f64) -> f64, seed: u64) -> Normalized {
    mosaic(&Scene { falloff, seed, dust: Vec::new(), hot: 0.0 })
}

fn dust(seed: u64, big: bool) -> Vec<(f64, f64, f64)> {
    let mut rng = Lcg(seed);
    let mut d: Vec<(f64, f64, f64)> = (0..12).map(|_| (rng.next() * W as f64, rng.next() * H as f64, 2.0 + rng.next() * 2.0)).collect();
    if big {
        d.push((310.0, 220.0, 8.0));
        d.push((900.0, 600.0, 8.0));
    }
    d
}

/// The options these tests were made for: 48 cells along the long side (the default is 96, for real raws).
/// The synthetic 1200 × 800 references have 72 green samples per cell at 96, too few for the 1 % clip threshold.
fn opts() -> VignettingCalibrationOptions {
    VignettingCalibrationOptions { long_cells: 48, ..VignettingCalibrationOptions::default() }
}

fn map_of(n: &Normalized) -> VignettingMeasurement {
    build(n, VignettingCalibrationSource::default(), &opts()).unwrap()
}

/// The map's error against the true fall-off, both taken relative to the frame centre: (largest |error| over the
/// interior, the four corners' errors, signed: positive = the map corrects more than the truth).
fn errors(map: &VignettingMeasurement, truth: &dyn Fn(f64, f64) -> f64) -> (f64, [f64; 4]) {
    let (cx, cy) = (W as f64 / 2.0, H as f64 / 2.0);
    let rel = |x: f64, y: f64| (map.ev_at(x, y) - map.ev_at(cx, cy)) - (truth(x, y) - truth(cx, cy));
    let mut interior = 0.0f64;
    // interior: inside the outermost cell centres
    let (mx, my) = (W as f64 / 96.0, H as f64 / 64.0);
    for j in 0..=40 {
        for i in 0..=40 {
            let x = mx + (W as f64 - 2.0 * mx) * i as f64 / 40.0;
            let y = my + (H as f64 - 2.0 * my) * j as f64 / 40.0;
            interior = interior.max(rel(x, y).abs());
        }
    }
    let (w, h) = (W as f64, H as f64);
    (interior, [rel(0.0, 0.0), rel(w, 0.0), rel(0.0, h), rel(w, h)])
}

#[test]
fn a_radial_falloff_is_recovered() {
    let truth = radial(1.5, 0.5, 0.5);
    let map = map_of(&clean(&truth, 1));
    let (interior, corners) = errors(&map, &truth);
    eprintln!("1.5 EV r⁴: interior max |error| {interior:.4} EV, corners {corners:.4?} EV");
    assert!(interior <= 0.03, "interior error {interior}");
    assert!(corners.iter().all(|e| e.abs() <= 0.05), "corner errors {corners:?}");
}

#[test]
fn a_steep_falloff_is_not_understated_or_overshot_at_the_corners() {
    let truth = radial(3.0, 0.5, 0.5);
    let map = map_of(&clean(&truth, 2));
    let (interior, corners) = errors(&map, &truth);
    eprintln!("3 EV r⁴: interior max |error| {interior:.4} EV, corners {corners:.4?} EV");
    assert!(corners.iter().all(|e| (-0.05..=0.1).contains(e)), "corner errors {corners:?}");
}

#[test]
fn dust_and_hot_pixels_do_not_change_the_map() {
    let truth = radial(1.5, 0.5, 0.5);
    let a = map_of(&clean(&truth, 3));
    let b = map_of(&mosaic(&Scene { falloff: &truth, seed: 3, dust: dust(7, true), hot: 0.002 }));
    let worst = a.ev_grid().ev.iter().zip(&b.ev_grid().ev).map(|(x, y)| (x - y).abs()).fold(0.0f64, f64::max);
    eprintln!("dust + hot pixels: largest cell difference {worst:.4} EV");
    assert!(worst <= 0.02, "{worst}");
}

/// Greens of the cell (col, row) of the default 48 × 32 grid over W × H.
fn greens_of_cell(n: &Normalized, col: usize, row: usize) -> Vec<usize> {
    let (x0, x1) = cell_span(col, 48, W);
    let (y0, y1) = cell_span(row, 32, H);
    let cfa = n.cfa.clone().unwrap();
    (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| cfa.color_at(x, y) == 1).map(|(x, y)| y * W + x).collect()
}

#[test]
fn a_clipped_reference_is_refused_and_names_the_cell() {
    let truth = radial(1.0, 0.5, 0.5);
    let mut n = clean(&truth, 4);
    let greens = greens_of_cell(&n, 5, 3);
    let few = greens.len() / 200; // 0.5 %
    for &i in greens.iter().take(few) {
        n.data[i] = 1.0;
    }
    assert!(build(&n, VignettingCalibrationSource::default(), &opts()).is_ok(), "0.5 % clipped passes");
    let many = (greens.len() * 2).div_ceil(100); // 2 %
    for &i in greens.iter().take(many) {
        n.data[i] = 1.0;
    }
    match build(&n, VignettingCalibrationSource::default(), &opts()) {
        Err(VignettingCalibrationError::Clipped { col, row, percent }) => {
            assert_eq!((col, row), (5, 3));
            assert!((percent - 100.0 * many as f64 / greens.len() as f64).abs() < 1e-9, "{percent}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn amounts_scale_the_correction_in_ev() {
    let map = map_of(&clean(&radial(1.5, 0.5, 0.5), 5));
    let (x, y) = (37.0, 51.0);
    let full = map.gain_at(x, y, 1.0) as f64;
    let ev = map.ev_at(x, y);
    assert!(ev > 0.5, "a corner region is corrected ({ev})");
    assert!((full.log2() - ev).abs() < 1e-5, "1.0 is the stored gain");
    for (amount, factor) in [(0.0, 0.0), (0.5, 0.5), (1.2, 1.2), (2.0, 1.2), (f64::NAN, 0.0), (-1.0, 0.0)] {
        let g = map.gain_at(x, y, amount) as f64;
        assert!((g.log2() - factor * ev).abs() < 1e-5, "amount {amount}: {g}");
    }
    // everywhere at 0
    for (x, y) in [(0.0, 0.0), (600.0, 400.0), (1200.0, 800.0)] {
        assert_eq!(map.gain_at(x, y, 0.0), 1.0);
    }
    assert_eq!(effective_gain(2.0, 0.5), 2f64.sqrt());
    assert_eq!(effective_gain(4.0, f64::NAN), 1.0);
    assert_eq!(DEFAULT_VIGNETTING_CALIBRATION_AMOUNT, 1.0);
}

fn check_of(map: &VignettingMeasurement, n: &Normalized) -> CheckReport {
    let opts = opts();
    let cells = cell_means(n, map.grid, &opts).unwrap();
    check(map, &cells, [0.0, 0.0], &opts).unwrap()
}

#[test]
fn a_map_corrects_another_reference_of_the_same_setup() {
    let truth = radial(1.5, 0.5, 0.5);
    let map = map_of(&clean(&truth, 6));
    let b = mosaic(&Scene { falloff: &truth, seed: 60, dust: dust(61, false), hot: 0.001 });
    let r = check_of(&map, &b);
    eprintln!("check: max |residual| {:.4} EV, peak-to-peak {:.4} EV", r.max_abs, r.peak_to_peak);
    assert!(r.passes(), "{} {}", r.max_abs, r.peak_to_peak);
    // before correction the corner is clearly darker
    assert!(r.before.at(0.0, 0.0) > 1.0);
}

#[test]
fn an_off_centre_falloff_is_built_and_corrected() {
    let truth = radial(1.5, 0.6, 0.5);
    let map = map_of(&clean(&truth, 8));
    let (interior, corners) = errors(&map, &truth);
    eprintln!("shifted 10 %: interior {interior:.4} EV, corners {corners:.4?} EV");
    let r = check_of(&map, &mosaic(&Scene { falloff: &truth, seed: 80, dust: dust(81, false), hot: 0.001 }));
    eprintln!("shifted check: max |residual| {:.4} EV, peak-to-peak {:.4} EV", r.max_abs, r.peak_to_peak);
    assert!(r.passes(), "{} {}", r.max_abs, r.peak_to_peak);
}

#[test]
fn the_optical_centre_is_found_and_a_gradient_fits_badly() {
    let truth = radial(1.5, 0.6, 0.5);
    let shifted = fit_centre(&map_of(&clean(&truth, 9)).ev_grid()).unwrap();
    eprintln!("centre fit: {shifted:?}");
    assert!((shifted.dx_pct - 10.0).abs() < 1.0 && shifted.dy_pct.abs() < 1.0, "{shifted:?}");
    // a sky: brightness falling linearly across the frame by 0.6 EV, no lens fall-off
    let tilt = |x: f64, _y: f64| 0.6 * x / W as f64;
    let sky = fit_centre(&map_of(&clean(&tilt, 10)).ev_grid()).unwrap();
    eprintln!("tilt fit: {sky:?}");
    assert!(sky.rms_ev > 0.01 && sky.rms_ev > 5.0 * shifted.rms_ev, "tilt {sky:?} vs lens {shifted:?}");
}

#[test]
fn padding_continues_a_linear_ramp() {
    let (cols, rows) = (7, 5);
    let v: Vec<f64> = (0..rows).flat_map(|r| (0..cols).map(move |c| 0.3 + 0.2 * c as f64 - 0.15 * r as f64)).collect();
    for (a, b) in median3(&v, cols, rows).iter().zip(&v) {
        assert!((a - b).abs() < 1e-12);
    }
    for (a, b) in gaussian(&v, cols, rows, 1.5).iter().zip(&v) {
        assert!((a - b).abs() < 1e-12);
    }
}

#[test]
fn sensor_and_source_coordinates_round_trip_for_every_orientation() {
    let all = [
        Orientation::Normal,
        Orientation::FlipH,
        Orientation::Rotate180,
        Orientation::FlipV,
        Orientation::Transpose,
        Orientation::Rotate90,
        Orientation::Transverse,
        Orientation::Rotate270,
    ];
    for exif in all {
        for user in all {
            for aspect in [1.0f32, 2.0] {
                let f = SensorFrame::new([640, 480], Rect::new(12, 8, 600, 400), aspect, exif);
                let (ow, oh) = f.oriented_size(user);
                // a fitted copy at half size
                let (sw, sh) = (ow / 2.0, oh / 2.0);
                for p in [Point::new(0.0, 0.0), Point::new(13.25, 101.5), Point::new(sw, sh), Point::new(sw * 0.3, sh * 0.9)] {
                    let s = f.source_to_sensor(p, sw, sh, user);
                    let q = f.sensor_to_source(s, sw, sh, user);
                    assert!((q.x - p.x).abs() < 1e-9 && (q.y - p.y).abs() < 1e-9, "{exif:?} {user:?} {aspect}: {p:?} → {s:?} → {q:?}");
                    // inside the default crop
                    assert!(s.x >= 12.0 - 1e-9 && s.x <= 612.0 + 1e-9 && s.y >= 8.0 - 1e-9 && s.y <= 408.0 + 1e-9, "{s:?}");
                }
            }
        }
    }
}

#[test]
fn source_positions_land_on_the_sensor_pixel_the_picture_shows_there() {
    let all = [
        Orientation::Normal,
        Orientation::FlipH,
        Orientation::Rotate180,
        Orientation::FlipV,
        Orientation::Transpose,
        Orientation::Rotate90,
        Orientation::Transverse,
        Orientation::Rotate270,
    ];
    let (cw, ch) = (5usize, 3usize);
    let crop = Rect::new(2, 1, cw, ch);
    for exif in all {
        for user in all {
            let f = SensorFrame::new([9, 6], crop, 1.0, exif);
            // the developed picture, each pixel labelled with its index, oriented as the app does it
            let mut img = lcc_raster::Rgb32f::new(cw, ch);
            for (i, px) in img.data.iter_mut().enumerate() {
                *px = [i as f32, 0.0, 0.0];
            }
            let img = img.into_oriented(exif).into_oriented(user);
            let (ow, oh) = f.oriented_size(user);
            assert_eq!((img.width as f64, img.height as f64), (ow, oh));
            for y in 0..img.height {
                for x in 0..img.width {
                    let s = f.source_to_sensor(Point::new(x as f64 + 0.5, y as f64 + 0.5), ow, oh, user);
                    let (dx, dy) = ((s.x - crop.x as f64).floor() as usize, (s.y - crop.y as f64).floor() as usize);
                    assert_eq!(img.get(x, y)[0] as usize, dy * cw + dx, "{exif:?} {user:?} at ({x}, {y})");
                }
            }
        }
    }
}

#[test]
fn bad_maps_and_references_are_errors_not_panics() {
    let good = map_of(&clean(&radial(1.0, 0.5, 0.5), 11));
    assert!(good.validate().is_ok());
    let broken = [
        VignettingMeasurement { gains: vec![1.0; 3], ..good.clone() },
        VignettingMeasurement { gains: good.gains.iter().map(|_| f32::NAN).collect(), ..good.clone() },
        VignettingMeasurement { gains: good.gains.iter().map(|_| 0.0).collect(), ..good.clone() },
        VignettingMeasurement { grid: [100_000, 100_000], ..good.clone() },
        VignettingMeasurement { grid: [1, 32], ..good.clone() },
        VignettingMeasurement { sensor: [0, 0], ..good.clone() },
        VignettingMeasurement { version: 99, ..good.clone() },
    ];
    for m in &broken {
        assert!(m.validate().is_err(), "{:?} {:?}", m.grid, m.sensor);
        // sampling a broken map never panics
        let _ = m.gain_at(1e300, -1e300, 1.0);
        let _ = m.gain_at(f64::NAN, 3.0, f64::INFINITY);
    }
    let json = r#"{"version":1,"source":{},"sensor":[10,10],"grid":[300,2],"gains":[]}"#;
    let m: VignettingMeasurement = serde_json::from_str(json).unwrap();
    assert!(m.validate().is_err());

    let opts = opts();
    let src = VignettingCalibrationSource::default;
    let empty = Normalized { width: 0, height: 0, cpp: 1, data: vec![], cfa: Cfa::bayer("RGGB") };
    assert!(build(&empty, src(), &opts).is_err());
    let tiny = Normalized { width: 8, height: 8, cpp: 1, data: vec![0.5; 64], cfa: Cfa::bayer("RGGB") };
    assert!(build(&tiny, src(), &opts).is_err());
    let lying = Normalized { width: 400, height: 300, cpp: 1, data: vec![0.5; 10], cfa: Cfa::bayer("RGGB") };
    assert!(build(&lying, src(), &opts).is_err());
    let black = Normalized { width: 400, height: 300, cpp: 1, data: vec![0.0; 120_000], cfa: Cfa::bayer("RGGB") };
    assert!(matches!(build(&black, src(), &opts), Err(VignettingCalibrationError::TooDark { .. })));
    let white = Normalized { width: 400, height: 300, cpp: 1, data: vec![1.0; 120_000], cfa: Cfa::bayer("RGGB") };
    assert!(matches!(build(&white, src(), &opts), Err(VignettingCalibrationError::Clipped { .. })));
    let nan = Normalized { width: 400, height: 300, cpp: 1, data: vec![f32::NAN; 120_000], cfa: Cfa::bayer("RGGB") };
    assert!(build(&nan, src(), &opts).is_err());
    let two_cpp = Normalized { width: 400, height: 300, cpp: 2, data: vec![0.5; 240_000], cfa: None };
    assert!(matches!(build(&two_cpp, src(), &opts), Err(VignettingCalibrationError::NotCfa)));
    for bad in [
        VignettingCalibrationOptions { long_cells: 1, ..opts },
        VignettingCalibrationOptions { smoothing: f64::NAN, ..opts },
        VignettingCalibrationOptions { trim: 0.5, ..opts },
        VignettingCalibrationOptions { clip: 0.0, ..opts },
    ] {
        assert!(matches!(build(&tiny, src(), &bad), Err(VignettingCalibrationError::BadOptions(_))));
    }
    // a second reference of another size, with nothing saying where it sits on the sensor
    assert!(good.relative_offset([400, 300], None).is_err());
    assert!(good.placement([400, 300], None).is_err());
    // a window that disagrees with the map's own active area
    let wrong = VignettingMeasurement { window: Some(SensorWindow { sensor: [2000, 800], origin: [0, 0], size: [999, 800] }), ..good.clone() };
    assert!(wrong.validate().is_err());
}

/// A centred crop of `full`'s columns `x0..x0 + width` (the CFA phase kept: `x0` even).
fn crop_columns(full: &Normalized, x0: usize, width: usize) -> Normalized {
    let data = (0..full.height).flat_map(|y| full.data[y * full.width + x0..y * full.width + x0 + width].to_vec()).collect();
    Normalized { width, height: full.height, cpp: 1, data, cfa: full.cfa.clone() }
}

/// Full-sensor window of the synthetic W × H sensor, and a 1000-wide centred image area of it (5:4 of 800 rows).
fn windows() -> (SensorWindow, SensorWindow) {
    (SensorWindow { sensor: [W, H], origin: [0, 0], size: [W, H] }, SensorWindow { sensor: [W, H], origin: [100, 0], size: [1000, H] })
}

#[test]
fn a_full_sensor_map_serves_a_smaller_image_area_of_the_same_sensor() {
    let truth = radial(1.5, 0.5, 0.5);
    let (full, area) = windows();
    let mut map = map_of(&clean(&truth, 20));
    map.window = Some(full);
    assert!(map.validate().is_ok());
    let b = crop_columns(&mosaic(&Scene { falloff: &truth, seed: 21, dust: dust(22, false), hot: 0.001 }), 100, 1000);
    let offset = map.placement([1000, H], Some(&area)).unwrap();
    assert_eq!(offset, [100.0, 0.0]);
    let opts = opts();
    let cells = cell_means(&b, grid_for(1000, H, opts.long_cells), &opts).unwrap();
    let r = check(&map, &cells, offset, &opts).unwrap();
    eprintln!("3:2 map → 5:4 shot: max |residual| {:.4} EV, peak-to-peak {:.4} EV, {} cells", r.max_abs, r.peak_to_peak, r.cells_used);
    assert!(r.passes() && r.cells_used == cells.means.len());
    let fit = find_offset(&map, &cells, offset).unwrap();
    eprintln!("offset search: {fit:?}");
    assert!((fit.found[0] - 100.0).abs() <= 8.0 && fit.found[1].abs() <= 8.0, "{fit:?}");
    // ignoring the window (no offset) fits clearly worse than the window says
    assert!(fit.rms_zero.is_some_and(|z| z > 2.0 * fit.rms_expected), "{fit:?}");
}

#[test]
fn a_smaller_image_area_map_checks_over_the_area_it_shares() {
    let truth = radial(1.5, 0.5, 0.5);
    let (full, area) = windows();
    let mut map = map_of(&crop_columns(&clean(&truth, 23), 100, 1000));
    map.window = Some(area);
    // it doesn't cover the full frame, so it doesn't apply there…
    assert!(map.placement([W, H], Some(&full)).is_err());
    // …but compares over the overlap
    let offset = map.relative_offset([W, H], Some(&full)).unwrap();
    assert_eq!(offset, [-100.0, 0.0]);
    let opts = opts();
    let b = mosaic(&Scene { falloff: &truth, seed: 24, dust: dust(25, false), hot: 0.001 });
    let cells = cell_means(&b, grid_for(W, H, opts.long_cells), &opts).unwrap();
    let r = check(&map, &cells, offset, &opts).unwrap();
    eprintln!(
        "5:4 map → 3:2 shot: max |residual| {:.4} EV, peak-to-peak {:.4} EV, {} of {} cells",
        r.max_abs,
        r.peak_to_peak,
        r.cells_used,
        cells.means.len()
    );
    assert!(r.passes() && r.cells_used < cells.means.len());
    assert!(r.after.ev.iter().any(|e| e.is_nan()), "cells outside the map have no residual");
}

#[test]
fn another_sensor_or_an_unknown_position_is_refused() {
    let (full, area) = windows();
    let mut map = map_of(&clean(&radial(1.0, 0.5, 0.5), 26));
    map.window = Some(full);
    let other_body = SensorWindow { sensor: [W - 8, H], origin: [0, 0], size: [W - 8, H] };
    assert!(map.placement([W - 8, H], Some(&other_body)).is_err());
    assert!(map.relative_offset([W - 8, H], Some(&other_body)).is_err());
    assert!(map.placement([1000, H], None).is_err(), "a smaller frame with no window");
    assert!(map.placement([999, H], Some(&area)).is_err(), "a window that isn't the frame's size");
    // the same active area needs no window
    assert_eq!(map.placement([W, H], None).unwrap(), [0.0, 0.0]);
}

#[test]
fn a_nearly_vignetting_calibration_gives_a_sane_centre_fit_or_none() {
    let truth = radial(0.002, 0.5, 0.5);
    let map = map_of(&clean(&truth, 27));
    if let Some(f) = fit_centre(&map.ev_grid()) {
        assert!(f.dx_pct.is_finite() && f.dy_pct.is_finite() && f.rms_ev.is_finite(), "{f:?}");
        assert!(f.dx_pct.abs() <= 25.0 && f.dy_pct.abs() <= 25.0 && f.rms_ev < 0.01, "{f:?}");
        assert!(!f.uneven(0.002));
    }
    // a constant field and a degenerate grid
    let flat = EvGrid { sensor: [W, H], grid: [8, 6], ev: vec![0.0; 48] };
    if let Some(f) = fit_centre(&flat) {
        assert!(f.rms_ev.is_finite() && f.rms_ev < 1e-9, "{f:?}");
    }
    let tiny = EvGrid { sensor: [2, 2], grid: [2, 2], ev: vec![0.1; 4] };
    let _ = fit_centre(&tiny);
    // singular and near-singular systems give no solution, not garbage
    assert_eq!(solve4([[1.0; 4]; 4], [1.0; 4]), None);
    assert_eq!(solve4([[0.0; 4]; 4], [0.0; 4]), None);
    let near = [[1.0, 1.0, 1.0, 1.0], [1.0, 1.0 + 1e-15, 1.0, 1.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
    assert_eq!(solve4(near, [1.0; 4]), None);
    assert_eq!(solve4([[f64::NAN; 4]; 4], [1.0; 4]), None);
    // a regular system is solved
    let x = solve4([[2.0, 0.0, 0.0, 1.0], [0.0, 3.0, 0.0, 0.0], [0.0, 0.0, 4.0, 0.0], [1.0, 0.0, 0.0, 2.0]], [3.0, 3.0, 4.0, 3.0]).unwrap();
    for (a, b) in x.iter().zip([1.0, 1.0, 1.0, 1.0]) {
        assert!((a - b).abs() < 1e-12, "{x:?}");
    }
}

#[test]
fn unevenness_is_judged_relative_to_the_falloff() {
    let fit = |rms_ev| CentreFit { dx_pct: 0.0, dy_pct: 0.0, rms_ev };
    assert!(!fit(0.029).uneven(0.5), "0.03 EV is the floor");
    assert!(fit(0.031).uneven(0.5));
    assert!(!fit(0.05).uneven(3.5), "2 % of a 3.5 EV fall-off is 0.07 EV");
    assert!(fit(0.08).uneven(3.5));
    assert!(fit(f64::NAN).uneven(1.0));
}

/// A check's residual grid marks cells the map doesn't cover with NaN; sampling it anywhere, including the
/// extrapolated border next to such cells, must not panic (`f64::clamp` panics on a NaN bound).
#[test]
fn sampling_a_grid_with_missing_cells_never_panics() {
    let mut ev = vec![0.1; 6 * 4];
    for i in [0, 5, 6, 11, 18, 23] {
        ev[i] = f64::NAN;
    }
    let g = EvGrid { sensor: [600, 400], grid: [6, 4], ev };
    for (x, y) in [(0.0, 0.0), (600.0, 0.0), (0.0, 400.0), (600.0, 400.0), (300.0, 200.0), (5.0, 200.0), (-1e9, f64::NAN), (f64::INFINITY, 3.0)] {
        let _ = g.at(x, y);
    }
    let all = EvGrid { sensor: [600, 400], grid: [6, 4], ev: vec![f64::NAN; 24] };
    assert!(all.at(0.0, 0.0).is_nan());
}

const ALL: [Orientation; 8] = [
    Orientation::Normal,
    Orientation::FlipH,
    Orientation::Rotate180,
    Orientation::FlipV,
    Orientation::Transpose,
    Orientation::Rotate90,
    Orientation::Transverse,
    Orientation::Rotate270,
];

/// Every pixel of every image area (full sensor, 5:4, 1:1, 16:9, DX, as the Z 8 places them, scaled down) lands on
/// the map's sensor position under it, through `SourcePlacement`, for every EXIF and user orientation: ground truth
/// from a picture whose pixels carry their own sensor position, oriented as the app orients it.
#[test]
fn every_image_area_and_orientation_lands_on_the_maps_sensor_position() {
    // a 24 × 16 sensor (the Z 8's 8280 × 5520 at 1/345), the map measured over all of it
    let sensor = [24usize, 16];
    let full = SensorWindow { sensor, origin: [0, 0], size: sensor };
    let map = VignettingMeasurement {
        version: FORMAT_VERSION,
        source: VignettingCalibrationSource::default(),
        sensor,
        grid: [4, 4],
        gains: (0..16).map(|i| 1.0 + i as f32 / 8.0).collect(),
        window: Some(full),
    };
    let areas = [
        ("full", full),
        ("5:4", SensorWindow { sensor, origin: [2, 0], size: [20, 16] }),
        ("1:1", SensorWindow { sensor, origin: [4, 0], size: [16, 16] }),
        ("16:9", SensorWindow { sensor, origin: [0, 1], size: [24, 14] }),
        ("DX", SensorWindow { sensor, origin: [4, 3], size: [16, 10] }),
    ];
    for (name, win) in areas {
        // a default crop inside the active area, as raws have
        let crop = Rect::new(1, 1, win.size[0] - 2, win.size[1] - 3);
        for exif in ALL {
            for user in ALL {
                let (aw, ah) = (win.size[0] as f64, win.size[1] as f64);
                let rect = [crop.x as f64 / aw, crop.y as f64 / ah, crop.width as f64 / aw, crop.height as f64 / ah];
                let p = SourcePlacement { active_size: Some(win.size), window: Some(win), ..SourcePlacement::new(rect, exif) };
                let mut img = lcc_raster::Rgb32f::new(crop.width, crop.height);
                for (i, px) in img.data.iter_mut().enumerate() {
                    let (x, y) = (i % crop.width, i / crop.width);
                    // the map's sensor position of this pixel's centre
                    *px = [(win.origin[0] + crop.x + x) as f32 + 0.5, (win.origin[1] + crop.y + y) as f32 + 0.5, 0.0];
                }
                let img = img.into_oriented(exif).into_oriented(user);
                let (ow, oh) = (img.width as f64, img.height as f64);
                let inv = user.inverse();
                for y in 0..img.height {
                    for x in 0..img.width {
                        let (u, v) = inv.map((x as f64 + 0.5) / ow, (y as f64 + 0.5) / oh, 1.0, 1.0);
                        let (sx, sy) = map.sensor_at(&p, u, v).unwrap_or_else(|| panic!("{name}: refused"));
                        let want = img.get(x, y);
                        assert!(
                            (sx - f64::from(want[0])).abs() < 1e-9 && (sy - f64::from(want[1])).abs() < 1e-9,
                            "{name} {exif:?} {user:?} at ({x}, {y}): ({sx}, {sy}), want {want:?}"
                        );
                        assert_eq!(map.ev_on(&p, u, v), Some(map.ev_at(sx, sy)));
                    }
                }
            }
        }
    }
}

#[test]
fn a_placement_the_map_does_not_cover_is_refused() {
    let sensor = [24usize, 16];
    let full = SensorWindow { sensor, origin: [0, 0], size: sensor };
    let five_four = SensorWindow { sensor, origin: [2, 0], size: [20, 16] };
    let map = |window: Option<SensorWindow>, size: [usize; 2]| VignettingMeasurement {
        version: FORMAT_VERSION,
        source: VignettingCalibrationSource::default(),
        sensor: size,
        grid: [4, 4],
        gains: vec![1.5; 16],
        window,
    };
    let at = |size: [usize; 2], window: Option<SensorWindow>| SourcePlacement {
        active_size: Some(size),
        window,
        ..SourcePlacement::new([0.0, 0.0, 1.0, 1.0], Orientation::Normal)
    };
    // a 5:4 map can't serve the full frame
    assert!(map(Some(five_four), [20, 16]).sensor_at(&at(sensor, Some(full)), 0.5, 0.5).is_none());
    // another sensor
    let other = SensorWindow { sensor: [26, 16], origin: [0, 0], size: [26, 16] };
    assert!(map(Some(full), sensor).sensor_at(&at([26, 16], Some(other)), 0.5, 0.5).is_none());
    // no window and another size
    assert!(map(None, sensor).sensor_at(&at([20, 16], None), 0.5, 0.5).is_none());
    // no active size known (a gain table's placement)
    assert!(map(None, sensor).sensor_at(&SourcePlacement::new([0.0, 0.0, 1.0, 1.0], Orientation::Normal), 0.5, 0.5).is_none());
    // the same active area without windows is fine, and so is the full map for the 5:4 area
    assert!(map(None, sensor).sensor_at(&at(sensor, None), 0.5, 0.5).is_some());
    assert_eq!(map(Some(full), sensor).sensor_at(&at([20, 16], Some(five_four)), 0.0, 0.0), Some((2.0, 0.0)));
}
