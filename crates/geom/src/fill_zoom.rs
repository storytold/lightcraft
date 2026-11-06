//! The frame-filling zoom of a radial lens correction.

/// Crop border radii scanned by [`fill_zoom`].
const ZOOM_SCAN: usize = 4096;

/// The frame-filling zoom `z` of a radial correction: `source` maps a corrected radius to the source radius it reads,
/// and the output then shows the corrected image zoomed by `z` (output radius = `z` · corrected radius), centred on a
/// crop of half size `x × y` (in the units of `source`). `z` makes the largest `source(ρ/z)/ρ` over the crop border
/// radii `ρ` in [min(x, y), hypot(x, y)] equal 1, so the output crop reads only inside the source crop and touches it
/// (at the corner for pincushion, the short-edge midpoint for barrel). Bisection over [0.5, 2]; `None` when that range
/// holds no solution or a ratio is not finite.
pub fn fill_zoom(source: &dyn Fn(f64) -> f64, x: f64, y: f64) -> Option<f64> {
    let (near, far) = (x.min(y), x.hypot(y));
    let overshoots = |z: f64| -> Option<bool> {
        let mut worst = f64::NEG_INFINITY;
        for k in 0..=ZOOM_SCAN {
            let rb = near + k as f64 * (far - near) / ZOOM_SCAN as f64;
            let ratio = source(rb / z) / rb;
            if !ratio.is_finite() {
                return None;
            }
            worst = worst.max(ratio);
        }
        Some(worst > 1.0)
    };
    let (mut lo, mut hi) = (0.5, 2.0);
    if !overshoots(lo)? || overshoots(hi)? {
        return None;
    }
    for _ in 0..100 {
        let z = 0.5 * (lo + hi);
        if overshoots(z)? { lo = z } else { hi = z }
    }
    Some(0.5 * (lo + hi))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The largest `source(ρ/z)/ρ` over the border of the `x × y` half-size crop, from its points.
    fn worst(source: &dyn Fn(f64) -> f64, z: f64, x: f64, y: f64) -> f64 {
        let border = (0..=200).flat_map(|i| {
            let t = i as f64 / 200.0;
            [(x * t, y), (x, y * t)]
        });
        border.map(|(px, py)| px.hypot(py)).map(|r| source(r / z) / r).fold(f64::NEG_INFINITY, f64::max)
    }

    #[test]
    fn pincushion_zooms_in_until_the_corner_touches() {
        // reads 4 % past the corner of a 3:2 crop
        let (x, y) = (1.5f64, 1.0);
        let corner = x.hypot(y);
        let source = |r: f64| r * (1.0 + 0.04 * (r / corner).powi(2));
        let z = fill_zoom(&source, x, y).unwrap();
        assert!(z > 1.0, "{z}");
        assert!((worst(&source, z, x, y) - 1.0).abs() < 1e-9);
        assert!((source(corner / z) - corner).abs() < 1e-9, "the corner reads the corner");
    }

    #[test]
    fn barrel_zooms_out_until_the_short_edge_touches() {
        let (x, y) = (1.5, 1.0);
        let source = |r: f64| r * (1.0 - 0.03 * r * r);
        let z = fill_zoom(&source, x, y).unwrap();
        assert!(z < 1.0, "{z}");
        assert!((worst(&source, z, x, y) - 1.0).abs() < 1e-9);
        assert!((source(y / z) - y).abs() < 1e-9, "the short-edge midpoint reads the midpoint");
    }

    #[test]
    fn no_distortion_needs_no_zoom_and_wild_maps_none() {
        assert!((fill_zoom(&|r| r, 1.5, 1.0).unwrap() - 1.0).abs() < 1e-12);
        assert_eq!(fill_zoom(&|r| 3.0 * r, 1.5, 1.0), None, "past the zoom range");
        assert_eq!(fill_zoom(&|r| r / 3.0, 1.5, 1.0), None);
        assert_eq!(fill_zoom(&|_| f64::NAN, 1.5, 1.0), None);
    }
}
