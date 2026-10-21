//! Pin clustering: counts per zoom, splitting as the zoom grows, stability under panning, the
//! antimeridian and the poles, and the cost for many photos.

use lightcraft_geom::Point;

use crate::cluster::*;
use crate::mercator::*;

fn ll(lat: f64, lon: f64) -> LatLon {
    LatLon::new(lat, lon).unwrap()
}

fn view(lat: f64, lon: f64, zoom: f64) -> Viewport {
    Viewport::new(ll(lat, lon), zoom, 1600.0, 1000.0)
}

/// Three photos in Paris (a few hundred metres apart), two in London, one in New York.
fn places() -> Vec<MapPoint> {
    [(48.8584, 2.2945), (48.8606, 2.3376), (48.8530, 2.3499), (51.5007, -0.1246), (51.5081, -0.0759), (40.7484, -73.9857)]
        .iter()
        .enumerate()
        .map(|(i, &(lat, lon))| MapPoint::new(i as u64 + 1, ll(lat, lon)))
        .collect()
}

/// The groups, each once (a narrow world shows a cluster once per copy).
fn groups(c: &[Cluster]) -> Vec<Vec<u64>> {
    let mut v: Vec<Vec<u64>> = c.iter().map(|c| c.ids.clone()).collect();
    v.sort();
    v.dedup();
    v
}

fn counts(c: &[Cluster]) -> Vec<usize> {
    let mut n: Vec<usize> = c.iter().map(Cluster::len).collect();
    n.sort();
    n
}

#[test]
fn clusters_split_as_the_zoom_grows() {
    let pts = places();
    // the North Atlantic at zoom 2: Paris and London one cluster or two, New York apart
    let c = cluster(&pts, &view(48.0, -35.0, 2.0), CLUSTER_CELL);
    assert_eq!(c.iter().map(Cluster::len).sum::<usize>(), 6, "every photo is in exactly one cluster");
    assert!(c.len() <= 3, "{:?}", counts(&c));
    // zoom 1: the world is narrower than the view, so groups repeat, but each photo is in one group
    let g = groups(&cluster(&pts, &view(48.0, -35.0, 1.0), CLUSTER_CELL));
    assert_eq!(g.iter().map(Vec::len).sum::<usize>(), 6);
    // zoom 6 over Europe: Paris (3) and London (2) apart; New York off screen
    let c = cluster(&pts, &view(50.0, 1.0, 6.0), CLUSTER_CELL);
    assert_eq!(counts(&c), vec![2, 3]);
    let paris = c.iter().find(|c| c.len() == 3).unwrap();
    assert_eq!(paris.ids, vec![1, 2, 3], "in the order given");
    assert!((paris.pos.lat - 48.857).abs() < 0.01 && (paris.pos.lon - 2.327).abs() < 0.01, "{:?}", paris.pos);
    let at = view(50.0, 1.0, 6.0).to_screen(paris.pos);
    assert!((at.x - paris.center.x).abs() < 1e-6 && (at.y - paris.center.y).abs() < 1e-6);
    // zoom 14 over Paris: three single pins
    let c = cluster(&pts, &view(48.857, 2.32, 14.0), CLUSTER_CELL);
    assert_eq!(counts(&c), vec![1, 1, 1]);
    // whole zooms nest their cells, so the Paris photos never regroup into fewer clusters
    let mut last = 0;
    for z in 0..=14 {
        let n = groups(&cluster(&pts, &view(48.857, 2.32, f64::from(z)), CLUSTER_CELL)).iter().filter(|g| g.iter().any(|i| *i <= 3)).count();
        assert!(n >= last && n >= 1, "zoom {z}: {n} < {last}");
        last = n;
    }
    assert_eq!(last, 3);
}

#[test]
fn panning_keeps_the_groups() {
    let pts = places();
    let a = view(50.0, 1.0, 7.3);
    let b = a.panned(37.0, -21.0);
    let (ca, cb) = (cluster(&pts, &a, CLUSTER_CELL), cluster(&pts, &b, CLUSTER_CELL));
    let ids = |c: &[Cluster]| {
        let mut v: Vec<Vec<u64>> = c.iter().map(|c| c.ids.clone()).collect();
        v.sort();
        v
    };
    assert_eq!(ids(&ca), ids(&cb));
    for x in &ca {
        let y = cb.iter().find(|y| y.ids == x.ids).unwrap();
        assert!((y.center.x - x.center.x - 37.0).abs() < 1e-6 && (y.center.y - x.center.y + 21.0).abs() < 1e-6);
    }
}

#[test]
fn off_screen_points_are_left_out() {
    let pts = places();
    let c = cluster(&pts, &Viewport::new(ll(0.0, 100.0), 8.0, 800.0, 600.0), CLUSTER_CELL);
    assert!(c.is_empty());
    assert!(cluster(&pts, &Viewport::new(ll(48.0, 2.0), 5.0, 0.0, 600.0), CLUSTER_CELL).is_empty(), "empty view");
    assert!(cluster(&[], &view(0.0, 0.0, 3.0), CLUSTER_CELL).is_empty());
    // a hostile cell size doesn't break anything
    for cell in [f64::NAN, -5.0, 0.0] {
        let c = cluster(&pts, &view(50.0, 1.0, 6.0), cell);
        assert_eq!(c.iter().map(Cluster::len).sum::<usize>(), 5, "{cell}");
    }
    for cell in [f64::INFINITY, 1e12] {
        let c = cluster(&pts, &view(50.0, 1.0, 6.0), cell);
        assert!(groups(&c).iter().map(Vec::len).sum::<usize>() <= 6 && c.iter().all(|c| c.center.x.is_finite()), "{cell}");
    }
}

#[test]
fn antimeridian_and_poles() {
    // 179.9° and −179.9° sit side by side across the date line
    let pts = vec![
        MapPoint::new(1, ll(-17.0, 179.9)),
        MapPoint::new(2, ll(-17.0, -179.9)),
        MapPoint::new(3, ll(90.0, 0.0)),
        MapPoint::new(4, ll(-90.0, 180.0)),
    ];
    let c = cluster(&pts, &view(-17.0, 180.0, 10.0), CLUSTER_CELL);
    assert_eq!(counts(&c), vec![1, 1]);
    let xs: Vec<f64> = c.iter().map(|c| c.center.x).collect();
    assert!(xs.iter().all(|x| (x - 800.0).abs() < 200.0), "both near the centre, not a world apart: {xs:?}");
    // poles are drawn at the edge of the map, as finite positions
    let c = cluster(&pts, &Viewport::new(ll(0.0, 0.0), 2.0, 1600.0, 1100.0), CLUSTER_CELL);
    assert!(c.iter().all(|c| c.center.x.is_finite() && c.center.y.is_finite()));
    assert_eq!(groups(&c).iter().map(Vec::len).sum::<usize>(), 4);
}

#[test]
fn a_narrow_world_repeats_its_pins() {
    // zoom 0: the world is 256 px wide, the view 1600: each cluster shows once per copy
    let pts = vec![MapPoint::new(1, ll(0.0, 0.0))];
    let c = cluster(&pts, &Viewport::new(ll(0.0, 0.0), 0.0, 1600.0, 400.0), CLUSTER_CELL);
    assert!(c.len() >= 6, "{}", c.len());
    assert!(c.iter().all(|c| c.ids == vec![1]));
    assert!(c.windows(2).all(|w| (w[1].center.x - w[0].center.x - 256.0).abs() < 1e-6));
}

#[test]
fn hit_finds_the_nearest_cluster_under_the_pointer() {
    let pts = places();
    let c = cluster(&pts, &view(50.0, 1.0, 6.0), CLUSTER_CELL);
    let paris = c.iter().position(|c| c.len() == 3).unwrap();
    let p = c[paris].center;
    assert_eq!(hit(&c, Point::new(p.x + 3.0, p.y - 2.0), |_| 12.0), Some(paris));
    assert_eq!(hit(&c, Point::new(p.x + 30.0, p.y), |_| 12.0), None);
}

#[test]
fn many_photos_cluster_quickly() {
    // 50 000 photos spread over Europe, deterministic
    let mut seed = 12345u64;
    let mut rnd = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    let pts: Vec<MapPoint> = (0..50_000).map(|i| MapPoint::new(i, ll(36.0 + rnd() * 24.0, -10.0 + rnd() * 40.0))).collect();
    let v = view(48.0, 10.0, 5.0);
    let t = std::time::Instant::now();
    let c = cluster(&pts, &v, CLUSTER_CELL);
    let ms = t.elapsed().as_secs_f64() * 1e3;
    eprintln!("cluster: 50 000 points → {} clusters in {ms:.2} ms", c.len());
    assert_eq!(c.iter().map(Cluster::len).sum::<usize>(), 50_000);
    assert!(c.len() < 600, "{}", c.len());
}
