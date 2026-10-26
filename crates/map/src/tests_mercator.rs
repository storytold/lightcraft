//! Web Mercator maths: known values, round trips, viewport tile sets, bounds and fitting.

use lightcraft_geom::Point;

use crate::mercator::*;

fn ll(lat: f64, lon: f64) -> LatLon {
    LatLon::new(lat, lon).unwrap()
}

fn close(a: f64, b: f64, eps: f64) -> bool {
    (a - b).abs() <= eps
}

#[test]
fn checked_points_reject_nan_and_out_of_range() {
    assert!(LatLon::new(51.5, -0.12).is_some());
    assert!(LatLon::new(90.0, 180.0).is_some() && LatLon::new(-90.0, -180.0).is_some());
    for (lat, lon) in
        [(f64::NAN, 0.0), (0.0, f64::NAN), (f64::INFINITY, 0.0), (0.0, f64::NEG_INFINITY), (90.0001, 0.0), (-91.0, 0.0), (0.0, 180.5), (0.0, -720.0)]
    {
        assert_eq!(LatLon::new(lat, lon), None, "{lat} {lon}");
    }
    assert_eq!(LatLon::from_gps(Some((1.0, 2.0))), Some(ll(1.0, 2.0)));
    assert_eq!(LatLon::from_gps(Some((f64::NAN, 2.0))), None);
    assert_eq!(LatLon::from_gps(None), None);
}

#[test]
fn tile_corners_at_known_zooms() {
    // zoom 0: the whole square
    let (nw, se) = TileKey::new(0, 0, 0).unwrap().corners();
    assert!(close(nw.lat, MAX_LATITUDE, 1e-8) && close(nw.lon, -180.0, 1e-12));
    assert!(close(se.lat, -MAX_LATITUDE, 1e-8) && close(se.lon, 180.0, 1e-12));
    // zoom 1: four quadrants meeting at (0, 0)
    let (nw, se) = TileKey::new(1, 1, 0).unwrap().corners();
    assert!(close(nw.lat, MAX_LATITUDE, 1e-8) && close(nw.lon, 0.0, 1e-12));
    assert!(close(se.lat, 0.0, 1e-12) && close(se.lon, 180.0, 1e-12));
    let (nw, se) = TileKey::new(1, 0, 1).unwrap().corners();
    assert!(close(nw.lat, 0.0, 1e-12) && close(nw.lon, -180.0, 1e-12) && close(se.lat, -MAX_LATITUDE, 1e-8) && close(se.lon, 0.0, 1e-12));
    // zoom 10: central London is tile 511/340 (the tile OpenStreetMap serves for it)
    let london = ll(51.5074, -0.1278);
    assert_eq!(TileKey::containing(london, 10), TileKey::new(10, 511, 340).unwrap());
    let (nw, se) = TileKey::new(10, 511, 340).unwrap().corners();
    assert!(nw.lat > london.lat && se.lat < london.lat && nw.lon < london.lon && se.lon > london.lon);
    assert!(close(nw.lon, -0.3515625, 1e-12) && close(se.lon, 0.0, 1e-12));
    assert!(close(nw.lat, 51.618_017, 1e-5) && close(se.lat, 51.399_206, 1e-5), "{nw:?} {se:?}");
    // the poles and the antimeridian clamp/wrap onto the grid
    assert_eq!(TileKey::containing(ll(90.0, 180.0), 3), TileKey::new(3, 0, 0).unwrap());
    assert_eq!(TileKey::containing(ll(-90.0, 179.999), 3), TileKey::new(3, 7, 7).unwrap());
    assert_eq!(TileKey::new(3, 8, 0), None);
    assert_eq!(TileKey::new(25, 0, 0), None);
}

#[test]
fn projection_round_trips() {
    for z in [0.0, 1.0, 5.5, 10.0, 19.0, 24.0] {
        for p in [ll(0.0, 0.0), ll(51.5074, -0.1278), ll(-33.8688, 151.2093), ll(85.0, 179.9), ll(-85.0, -179.9), ll(35.0, 139.0)] {
            let back = from_world(to_world(p, z), z);
            assert!(close(back.lat, p.lat, 1e-9) && close(back.lon, p.lon, 1e-9), "z{z} {p:?} → {back:?}");
        }
    }
    // latitude beyond the square clamps, longitude wraps
    let w = to_world(ll(89.0, 0.0), 2.0);
    assert!(close(w.y, 0.0, 1e-6));
    assert_eq!(wrap_lon(180.0), -180.0);
    assert_eq!(wrap_lon(190.0), -170.0);
    assert_eq!(wrap_lon(-540.0), -180.0);
    assert_eq!(wrap_lon(f64::NAN), 0.0);
    assert!(from_world(Point::new(f64::NAN, f64::NAN), 3.0).lat.is_finite());
    assert!(close(to_world(ll(0.0, 0.0), 0.0).x, 128.0, 1e-12) && close(to_world(ll(0.0, 0.0), 0.0).y, 128.0, 1e-9));
}

#[test]
fn viewport_tiles_cover_the_view_exactly() {
    // zoom 2 (a 1024 px world) in a 512 × 512 view centred on (0, 0): the four middle tiles
    let v = Viewport::new(ll(0.0, 0.0), 2.0, 512.0, 512.0);
    let tiles = v.tiles(19);
    let mut keys: Vec<_> = tiles.iter().map(|t| (t.key.x, t.key.y)).collect();
    keys.sort();
    assert_eq!(keys, vec![(1, 1), (1, 2), (2, 1), (2, 2)]);
    for t in &tiles {
        assert!(close(t.rect.x1 - t.rect.x0, 256.0, 1e-9));
        assert!(t.rect.x0 >= -1e-9 && t.rect.x1 <= 512.0 + 1e-9, "{t:?}");
    }
    // the one-tile margin adds a ring, at the same zoom only
    let keys = v.tile_keys(19, 1);
    assert_eq!(keys.len(), 16);
    assert!(keys.iter().all(|k| k.z == 2));
    // asking for more margin still gives one ring (the tile policy)
    assert_eq!(v.tile_keys(19, 5), keys);
    // no margin = what is drawn
    assert_eq!(v.tile_keys(19, 0).len(), 4);
    // centre first
    let first = keys.first().unwrap();
    assert!(matches!((first.x, first.y), (1..=2, 1..=2)));
}

#[test]
fn fractional_zoom_scales_tiles() {
    // zoom 2.4 draws zoom-2 tiles at 2^0.4 scale; 2.6 draws zoom-3 tiles at 2^-0.4
    let v = Viewport::new(ll(10.0, 20.0), 2.4, 800.0, 600.0);
    assert_eq!(v.tile_zoom(19), 2);
    let t = v.tiles(19);
    assert!(t.iter().all(|t| t.key.z == 2 && close(t.rect.x1 - t.rect.x0, 256.0 * 0.4f64.exp2(), 1e-9)));
    let v = Viewport::new(ll(10.0, 20.0), 2.6, 800.0, 600.0);
    assert_eq!(v.tile_zoom(19), 3);
    assert!(v.tiles(19).iter().all(|t| t.key.z == 3 && close(t.rect.x1 - t.rect.x0, 256.0 * (-0.4f64).exp2(), 1e-9)));
    // the drawn tiles cover the whole view
    for v in [Viewport::new(ll(10.0, 20.0), 2.6, 800.0, 600.0), Viewport::new(ll(48.85, 2.35), 13.3, 1280.0, 720.0)] {
        let t = v.tiles(19);
        for (x, y) in [(0.5, 0.5), (v.width - 0.5, 0.5), (0.5, v.height - 0.5), (v.width - 0.5, v.height - 0.5), (v.width / 2.0, v.height / 2.0)] {
            assert!(t.iter().any(|t| t.rect.x0 <= x && x < t.rect.x1 && t.rect.y0 <= y && y < t.rect.y1), "({x}, {y}) uncovered at {v:?}");
        }
    }
    // past the source's max zoom: its deepest tiles, scaled up
    let v = Viewport::new(ll(48.85, 2.35), 21.0, 400.0, 400.0);
    assert_eq!(v.tile_zoom(19), 19);
    assert!(v.tiles(19).iter().all(|t| close(t.rect.x1 - t.rect.x0, 1024.0, 1e-6)));
}

#[test]
fn edges_wrap_east_west_and_stop_at_the_poles() {
    // at the antimeridian the tiles of both sides are loaded
    let v = Viewport::new(ll(0.0, 180.0), 3.0, 512.0, 256.0);
    let keys = v.tile_keys(19, 0);
    let xs: std::collections::BTreeSet<u32> = keys.iter().map(|k| k.x).collect();
    assert_eq!(xs, [0, 7].into_iter().collect());
    // at the top of the map no row above 0 is asked for
    let v = Viewport::new(ll(85.0, 0.0), 4.0, 512.0, 512.0);
    assert!(v.tile_keys(19, 1).iter().all(|k| k.y < 16));
    assert!(v.tiles(19).iter().all(|t| t.key.y == 0 || t.key.y == 1));
    // at zoom 0 a wide view repeats the one tile; it is loaded once
    let v = Viewport::new(ll(0.0, 0.0), 0.0, 1500.0, 256.0);
    assert!(v.tiles(19).len() > 1);
    assert_eq!(v.tile_keys(19, 1), vec![TileKey::new(0, 0, 0).unwrap()]);
    // empty or absurd views are safe
    assert!(Viewport::new(ll(0.0, 0.0), 3.0, 0.0, 100.0).tiles(19).is_empty());
    let huge = Viewport { center: LatLon { lat: f64::NAN, lon: 1e300 }, zoom: f64::INFINITY, width: 1e12, height: f64::NAN };
    assert!(huge.tile_keys(19, 1).len() <= 4096);
    let tiny = Viewport { center: LatLon { lat: 0.0, lon: 0.0 }, zoom: -5.0, width: 300.0, height: 300.0 };
    assert_eq!(tiny.tile_keys(19, 1), vec![TileKey::new(0, 0, 0).unwrap()]);
}

#[test]
fn screen_positions_pan_and_zoom() {
    let v = Viewport::new(ll(48.85, 2.35), 12.0, 800.0, 600.0);
    let c = v.to_screen(ll(48.85, 2.35));
    assert!(close(c.x, 400.0, 1e-6) && close(c.y, 300.0, 1e-6));
    for p in [Point::new(0.0, 0.0), Point::new(800.0, 600.0), Point::new(123.0, 456.0)] {
        let q = v.to_screen(v.to_lat_lon(p));
        assert!(close(q.x, p.x, 1e-6) && close(q.y, p.y, 1e-6), "{p:?} → {q:?}");
    }
    // pins near the antimeridian show on the nearest copy of the world
    let v = Viewport::new(ll(0.0, 179.0), 5.0, 800.0, 600.0);
    let east = v.to_screen(ll(0.0, -179.0));
    assert!(east.x > 400.0 && east.x < 800.0, "{east:?}");
    // panning moves content with the drag
    let v = Viewport::new(ll(48.85, 2.35), 12.0, 800.0, 600.0);
    let p = ll(48.86, 2.36);
    let before = v.to_screen(p);
    let after = v.panned(30.0, -20.0).to_screen(p);
    assert!(close(after.x - before.x, 30.0, 1e-6) && close(after.y - before.y, -20.0, 1e-6));
    // zooming around a point keeps it under the pointer
    let anchor = Point::new(100.0, 500.0);
    let under = v.to_lat_lon(anchor);
    let z = v.zoomed_around(anchor, 14.5);
    assert!(close(z.zoom, 14.5, 1e-12));
    let q = z.to_screen(under);
    assert!(close(q.x, anchor.x, 1e-6) && close(q.y, anchor.y, 1e-6), "{q:?}");
    assert!(v.to_screen(LatLon { lat: f64::NAN, lon: f64::NAN }).x.is_finite());
}

#[test]
fn bounds_skip_bad_points_and_handle_the_antimeridian() {
    assert_eq!(Bounds::of(Vec::<(f64, f64)>::new()), None);
    assert_eq!(Bounds::of([(f64::NAN, 1.0), (100.0, 0.0), (0.0, 200.0)]), None);
    let b = Bounds::of([(48.85, 2.35), (51.5, -0.12), (f64::NAN, 50.0), (95.0, 0.0), (40.4, -3.7)]).unwrap();
    assert_eq!(b, Bounds { south: 40.4, west: -3.7, north: 51.5, east: 2.35 });
    assert!(!b.crosses_antimeridian() && b.contains(ll(45.0, 0.0)) && !b.contains(ll(45.0, 10.0)));
    // Fiji-ish: both sides of 180°, a small box crossing it (not the whole world)
    let b = Bounds::of([(-17.0, 178.0), (-16.5, -179.5), (-18.0, 179.5)]).unwrap();
    assert!(b.crosses_antimeridian());
    assert_eq!((b.west, b.east), (178.0, -179.5));
    assert!(close(b.lon_span(), 2.5, 1e-12));
    assert!(close(b.center().lon, 179.25, 1e-9));
    assert!(b.contains(ll(-17.0, 180.0)) && b.contains(ll(-17.0, -180.0)) && !b.contains(ll(-17.0, 0.0)));
    // ±180 is one meridian
    let b = Bounds::of([(0.0, 170.0), (0.0, 180.0)]).unwrap();
    assert_eq!((b.west, b.east), (170.0, 180.0));
    let b = Bounds::of([(0.0, -180.0), (0.0, 180.0)]).unwrap();
    assert_eq!(b.lon_span(), 0.0);
    // one point
    let b = Bounds::of([(10.0, 20.0)]).unwrap();
    assert_eq!(b, Bounds { south: 10.0, west: 20.0, north: 10.0, east: 20.0 });
}

#[test]
fn fit_bounds_shows_every_point_with_the_margin() {
    let pts = [(48.85, 2.35), (51.5, -0.12), (40.4, -3.7), (52.52, 13.4)];
    let b = Bounds::of(pts).unwrap();
    let v = fit_bounds(&b, 1000.0, 700.0, 40.0, 18.0);
    assert!(v.zoom > 3.0 && v.zoom < 6.0, "{v:?}");
    for (lat, lon) in pts {
        let s = v.to_screen(ll(lat, lon));
        assert!(s.x >= 40.0 - 1e-6 && s.x <= 960.0 + 1e-6 && s.y >= 40.0 - 1e-6 && s.y <= 660.0 + 1e-6, "{lat},{lon} at {s:?}");
    }
    // tight on one side
    let lo = pts.iter().map(|p| v.to_screen(ll(p.0, p.1)).x).fold(f64::INFINITY, f64::min);
    let hi = pts.iter().map(|p| v.to_screen(ll(p.0, p.1)).x).fold(f64::NEG_INFINITY, f64::max);
    let top = pts.iter().map(|p| v.to_screen(ll(p.0, p.1)).y).fold(f64::INFINITY, f64::min);
    let bottom = pts.iter().map(|p| v.to_screen(ll(p.0, p.1)).y).fold(f64::NEG_INFINITY, f64::max);
    assert!(close(hi - lo, 920.0, 1e-3) || close(bottom - top, 620.0, 1e-3), "{lo} {hi} {top} {bottom}");
    // a single point gets the max zoom; across the antimeridian stays small
    let one = fit_bounds(&Bounds::of([(10.0, 20.0)]).unwrap(), 800.0, 600.0, 20.0, 16.0);
    assert_eq!(one.zoom, 16.0);
    assert!(close(one.center.lat, 10.0, 1e-9) && close(one.center.lon, 20.0, 1e-9));
    let fiji = fit_bounds(&Bounds::of([(-17.0, 178.0), (-16.5, -179.5)]).unwrap(), 800.0, 600.0, 20.0, 18.0);
    assert!(fiji.zoom > 6.0, "{fiji:?}");
    for (lat, lon) in [(-17.0, 178.0), (-16.5, -179.5)] {
        let s = fiji.to_screen(ll(lat, lon));
        assert!((0.0..=800.0).contains(&s.x), "{s:?}");
    }
    // the whole world fits at zoom 0 at most; silly sizes don't produce NaN
    let world = fit_bounds(&Bounds { south: -85.0, west: -180.0, north: 85.0, east: 180.0 }, 200.0, 200.0, 10.0, 18.0);
    assert_eq!(world.zoom, 0.0);
    let silly = fit_bounds(&Bounds::of(pts).unwrap(), f64::NAN, -5.0, f64::INFINITY, f64::NAN);
    assert!(silly.zoom.is_finite() && silly.center.lat.is_finite());
}

/// The area a view shows: its corners, across the antimeridian (east < west), the whole world
/// when zoomed out past it, and the poles when the view reaches the square's edge.
#[test]
fn viewport_area_is_what_the_view_shows() {
    let v = Viewport::new(ll(48.85, 2.35), 10.0, 800.0, 600.0);
    let a = v.area();
    let nw = v.to_lat_lon(Point::new(0.0, 0.0));
    let se = v.to_lat_lon(Point::new(800.0, 600.0));
    assert!(close(a.north, nw.lat, 1e-9) && close(a.west, nw.lon, 1e-9) && close(a.south, se.lat, 1e-9) && close(a.east, se.lon, 1e-9), "{a:?}");
    assert!(a.contains(ll(48.85, 2.35)) && !a.contains(ll(51.5, -0.12)));
    // centred on the antimeridian: the box wraps
    let pacific = Viewport::new(ll(0.0, 180.0), 4.0, 800.0, 600.0).area();
    assert!(pacific.east < pacific.west, "{pacific:?}");
    assert!(close(pacific.west, 180.0 - 800.0 / 4096.0 * 360.0 / 2.0, 1e-9), "{pacific:?}");
    for p in [ll(-17.7, 178.0), ll(-13.8, -172.0), ll(0.0, 180.0), ll(0.0, -180.0)] {
        assert!(pacific.contains(p), "{p:?} in {pacific:?}");
    }
    assert!(!pacific.contains(ll(0.0, 0.0)) && !pacific.contains(ll(0.0, 120.0)));
    // the right edge exactly on +180 isn't wrapped
    let edge = Viewport::new(ll(0.0, 180.0 - 800.0 / 4096.0 * 360.0 / 2.0), 4.0, 800.0, 600.0).area();
    assert!(edge.west < edge.east && close(edge.east, 180.0, 1e-6), "{edge:?}");
    // zoomed out past the world: every longitude, and both poles
    for z in [0.0, 1.0] {
        let world = Viewport::new(ll(20.0, 100.0), z, 1400.0, 900.0).area();
        assert_eq!((world.west, world.east, world.south, world.north), (-180.0, 180.0, -90.0, 90.0), "zoom {z}");
        assert!(world.contains(ll(89.9, -179.0)) && world.contains(ll(-90.0, 180.0)));
    }
    // hostile viewports give finite areas
    for v in [
        Viewport { center: LatLon { lat: f64::NAN, lon: f64::INFINITY }, zoom: f64::NAN, width: f64::NAN, height: -1.0 },
        Viewport { center: LatLon { lat: 1e300, lon: -1e300 }, zoom: 1e9, width: 1e12, height: 1e12 },
    ] {
        let a = v.area();
        assert!([a.south, a.west, a.north, a.east].iter().all(|x| x.is_finite()), "{a:?}");
        assert!(a.south <= a.north, "{a:?}");
    }
}
