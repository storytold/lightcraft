//! Tests of track drawing: simplification error and end points, antimeridian cuts, poles, hostile
//! points, clipping, and the cost of a big track.

use lightcraft_geom::Point;

use crate::mercator::{LatLon, Viewport};
use crate::track::{TOLERANCE_PX, TrackShape};

/// A wiggly walk of `n` points around (46°, 7°) (a deterministic pseudo-random walk).
fn walk(n: usize) -> Vec<(f64, f64)> {
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % 10_000) as f64 / 10_000.0 - 0.5
    };
    let (mut lat, mut lon) = (46.0, 7.0);
    let mut heading: f64 = 0.0;
    (0..n)
        .map(|_| {
            heading += rnd() * 0.8;
            lat += heading.cos() * 0.0002;
            lon += heading.sin() * 0.0003;
            (lat, lon)
        })
        .collect()
}

fn dist_to_polyline(p: Point, lines: &[Vec<Point>]) -> f64 {
    let mut best = f64::INFINITY;
    for l in lines {
        if let [only] = l.as_slice() {
            best = best.min(((p.x - only.x).powi(2) + (p.y - only.y).powi(2)).sqrt());
        }
        for w in l.windows(2) {
            let (a, b) = (w[0], w[1]);
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let len2 = dx * dx + dy * dy;
            let t = if len2 > 0.0 { (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
            best = best.min(((a.x + t * dx - p.x).powi(2) + (a.y + t * dy - p.y).powi(2)).sqrt());
        }
    }
    best
}

#[test]
fn simplification_keeps_the_ends_and_stays_within_the_tolerance() {
    let pts = walk(5000);
    let shape = TrackShape::new([pts.clone()]);
    assert_eq!(shape.points(), 5000);
    assert_eq!(shape.pieces(), 1);
    let b = shape.bounds().unwrap();
    let mut previous = 0;
    let mut checked = 0;
    for z in [6u8, 10, 12, 13, 16] {
        let level = shape.level(z);
        // more points the further in, never more than the track has
        assert!(level.points <= 5000 && level.points >= previous, "zoom {z}: {}", level.points);
        previous = level.points;
        // a view at exactly that zoom holding the whole track (16 384 px at most)
        let vp = Viewport::new(b.center(), f64::from(z), 16_000.0, 16_000.0);
        let lines = level.screen_lines(&vp, 10.0);
        assert!(!lines.is_empty());
        let first = vp.to_screen(LatLon::new(pts[0].0, pts[0].1).unwrap());
        let last = vp.to_screen(LatLon::new(pts[4999].0, pts[4999].1).unwrap());
        if (0.0..16_000.0).contains(&first.x) && (0.0..16_000.0).contains(&first.y) && (0.0..16_000.0).contains(&last.x) {
            let start = lines[0][0];
            let end = *lines.last().unwrap().last().unwrap();
            assert!((start.x - first.x).abs() < 1e-6 && (start.y - first.y).abs() < 1e-6, "zoom {z}: the first point is kept");
            assert!((end.x - last.x).abs() < 1e-6 && (end.y - last.y).abs() < 1e-6, "zoom {z}: the last point is kept");
            assert_eq!(lines.len(), 1, "zoom {z}: one unbroken line");
            // no point of the track is further than the tolerance from what is drawn
            let worst = pts.iter().map(|(la, lo)| dist_to_polyline(vp.to_screen(LatLon::new(*la, *lo).unwrap()), &lines)).fold(0.0, f64::max);
            assert!(worst <= TOLERANCE_PX + 1e-6, "zoom {z}: {worst} px");
            checked += 1;
        }
    }
    assert!(checked >= 3, "{checked}");
    assert!(shape.level(6).points < 200, "zoomed out the walk is a few points: {}", shape.level(6).points);
    assert!(shape.level(18).points > 2000, "zoomed in most points stay: {}", shape.level(18).points);
}

#[test]
fn segments_are_not_joined() {
    let a = vec![(46.0, 7.0), (46.01, 7.01)];
    let b = vec![(46.02, 7.02), (46.03, 7.03)];
    let shape = TrackShape::new([a, b]);
    assert_eq!(shape.pieces(), 2);
    let vp = Viewport::new(shape.bounds().unwrap().center(), 13.0, 2000.0, 2000.0);
    let lines = shape.level(13).screen_lines(&vp, 0.0);
    assert_eq!(lines.len(), 2, "{lines:?}");
}

#[test]
fn a_line_across_the_antimeridian_is_cut_and_drawn_the_short_way() {
    let shape = TrackShape::new([vec![(-17.0, 179.0), (-17.2, -179.0), (-17.4, -178.0)]]);
    assert_eq!(shape.pieces(), 2, "cut at ±180°");
    let b = shape.bounds().unwrap();
    assert!(b.crosses_antimeridian() && b.lon_span() < 3.5, "{b:?}");
    // seen from the antimeridian: one unbroken line through the middle (the cut ends meet:
    // ±180° is the same meridian), nothing drawn across the world
    let vp = Viewport::new(LatLon { lat: -17.2, lon: 180.0 }, 7.0, 1200.0, 800.0);
    let lines = shape.level(7).screen_lines(&vp, 0.0);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let l = &lines[0];
    assert!(l.iter().any(|p| (p.x - 600.0).abs() < 1e-6), "through the centre column: {l:?}");
    assert!(l.windows(2).all(|w| w[1].x > w[0].x && w[1].x - w[0].x < 200.0), "eastward in short steps: {l:?}");
    for p in l {
        assert!(p.x > 0.0 && p.x < 1200.0 && p.y > 0.0 && p.y < 800.0, "{p:?} on screen");
    }
    // and seen from far away (the other side of the world) it is still two short lines
    let far = Viewport::new(LatLon { lat: 0.0, lon: 0.0 }, 1.0, 1200.0, 800.0);
    for l in shape.level(1).screen_lines(&far, 0.0) {
        let span = l.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max) - l.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
        assert!(span < 20.0, "{l:?}");
    }
}

#[test]
fn poles_nan_and_degenerate_tracks_never_crash() {
    // points at and beyond the Mercator edge are drawn on it
    let shape = TrackShape::new([vec![(90.0, 0.0), (89.99, 10.0), (-90.0, 20.0)]]);
    let vp = Viewport::new(LatLon { lat: 0.0, lon: 10.0 }, 1.0, 600.0, 600.0);
    for l in shape.level(1).screen_lines(&vp, 50.0) {
        assert!(l.iter().all(|p| p.x.is_finite() && p.y.is_finite()), "{l:?}");
    }
    // invalid points are skipped, the line goes on
    let shape = TrackShape::new([vec![(f64::NAN, 1.0), (1.0, f64::INFINITY), (1.0, 1.0), (95.0, 1.0), (1.1, 1.1), (1.0, 200.0)]]);
    assert_eq!(shape.points(), 2);
    assert_eq!(shape.pieces(), 1);
    // nothing at all
    let empty = TrackShape::new(Vec::<Vec<(f64, f64)>>::new());
    assert!(empty.bounds().is_none());
    assert!(empty.level(10).screen_lines(&vp, 0.0).is_empty());
    let none_valid = TrackShape::new([vec![(f64::NAN, f64::NAN)]]);
    assert_eq!((none_valid.points(), none_valid.pieces()), (0, 0));
    // one point: a dot where it is
    let one = TrackShape::new([vec![(10.0, 20.0)]]);
    let vp = Viewport::new(LatLon { lat: 10.0, lon: 20.0 }, 12.0, 400.0, 300.0);
    let lines = one.level(12).screen_lines(&vp, 0.0);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].len(), 1);
    assert!((lines[0][0].x - 200.0).abs() < 1e-6 && (lines[0][0].y - 150.0).abs() < 1e-6);
    // a zero-size view, NaN in the view, an absurd zoom and margin
    let bad = Viewport { center: LatLon { lat: f64::NAN, lon: f64::NAN }, zoom: f64::NAN, width: 0.0, height: 0.0 };
    assert!(one.level(255).screen_lines(&bad, f64::NAN).is_empty());
    let bad = Viewport { center: LatLon { lat: f64::NAN, lon: 1e300 }, zoom: 1e9, width: 1e9, height: f64::INFINITY };
    let _ = one.level(30).screen_lines(&bad, f64::INFINITY);
}

#[test]
fn a_logger_lying_still_costs_nothing_and_draws_little() {
    let mut pts = vec![(46.0, 7.0); 200_000];
    pts.push((46.001, 7.001));
    let t0 = std::time::Instant::now();
    let shape = TrackShape::new([pts]);
    let level = shape.level(18);
    assert!(level.points <= 3, "{}", level.points);
    assert!(t0.elapsed().as_secs_f64() < 5.0, "{:?}", t0.elapsed());
}

#[test]
fn deep_zoom_lines_are_clipped_to_the_view() {
    let pts: Vec<(f64, f64)> = (0..2000).map(|i| (46.0 + f64::from(i) * 0.001, 7.0 + f64::from(i) * 0.0013)).collect();
    let shape = TrackShape::new([pts]);
    let vp = Viewport::new(LatLon { lat: 46.5, lon: 7.65 }, 19.0, 800.0, 600.0);
    let lines = shape.level(19).screen_lines(&vp, 8.0);
    assert!(!lines.is_empty(), "the track passes through the view");
    let n: usize = lines.iter().map(Vec::len).sum();
    assert!(n < 50, "only what is on screen: {n}");
    for p in lines.iter().flatten() {
        assert!(p.x >= -8.0 - 1e-6 && p.x <= 808.0 + 1e-6 && p.y >= -8.0 - 1e-6 && p.y <= 608.0 + 1e-6, "{p:?}");
    }
}

/// The cost of a big track (2 million points): `cargo test -p lightcraft-map --release big_track
/// -- --ignored --nocapture`.
#[test]
#[ignore]
fn big_track_cost() {
    let pts = walk(2_000_000);
    let t0 = std::time::Instant::now();
    let shape = TrackShape::new([pts]);
    let build = t0.elapsed();
    let b = shape.bounds().unwrap();
    for z in [4u8, 8, 12, 16, 19] {
        let t1 = std::time::Instant::now();
        let level = shape.level(z);
        let lv = t1.elapsed();
        let vp = Viewport::new(b.center(), f64::from(z), 1600.0, 1000.0);
        let t2 = std::time::Instant::now();
        let lines = level.screen_lines(&vp, 8.0);
        let draw = t2.elapsed();
        let n: usize = lines.iter().map(Vec::len).sum();
        println!("zoom {z}: level {lv:?} ({} points kept), frame {draw:?} ({n} points on screen)", level.points);
    }
    println!("build (project, cut, rank, bounds): {build:?}");
}
