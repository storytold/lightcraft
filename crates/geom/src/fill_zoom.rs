//! The frame-filling zoom of a radial lens correction.

/// Crop border radii scanned by [`fill_zoom`] and [`fill_zoom_in`].
const ZOOM_SCAN: usize = 4096;

/// The frame-filling zoom `z` of a radial correction: `source` maps a corrected radius to the source radius it reads,
/// and the output then shows the corrected image zoomed by `z` (output radius = `z` · corrected radius), centred on a
/// crop of half size `x × y` (in the units of `source`). `z` makes the largest `source(ρ/z)/ρ` over the crop border
/// radii `ρ` in [min(x, y), hypot(x, y)] equal 1, so the output crop reads only inside the source crop and touches it
/// (at the corner for pincushion, the short-edge midpoint for barrel). Bisection over [1, 2] when the crop reads past
/// its edge unzoomed, else over [0.5, 1]: a zoom in only reads radii within the crop, so what `source` does beyond
/// it cannot hide a solution. `None` when the range holds no solution or a ratio is not finite.
pub fn fill_zoom(source: &dyn Fn(f64) -> f64, x: f64, y: f64) -> Option<f64> {
    let scan = Scan { source, x, y };
    if scan.worst(1.0)? > 1.0 { scan.bisect(1.0, 2.0) } else { scan.bisect(0.5, 1.0) }
}

/// [`fill_zoom`] when only a zoom in is wanted: 1 when the crop already reads inside the source unzoomed (one scan,
/// no search), else the zoom in over [1, 2]. `None` when no zoom up to 2 fills the crop or a ratio is not finite.
pub fn fill_zoom_in(source: &dyn Fn(f64) -> f64, x: f64, y: f64) -> Option<f64> {
    let scan = Scan { source, x, y };
    if scan.worst(1.0)? > 1.0 { scan.bisect(1.0, 2.0) } else { Some(1.0) }
}

/// The crop border radii of [`fill_zoom`] and the map they are read through.
struct Scan<'a> {
    source: &'a dyn Fn(f64) -> f64,
    x: f64,
    y: f64,
}

impl Scan<'_> {
    /// The largest `source(ρ/z)/ρ` over the crop border radii; `None` when one is not finite.
    fn worst(&self, z: f64) -> Option<f64> {
        let (near, far) = (self.x.min(self.y), self.x.hypot(self.y));
        let mut worst = f64::NEG_INFINITY;
        for k in 0..=ZOOM_SCAN {
            let rb = near + k as f64 * (far - near) / ZOOM_SCAN as f64;
            let ratio = (self.source)(rb / z) / rb;
            if !ratio.is_finite() {
                return None;
            }
            worst = worst.max(ratio);
        }
        Some(worst)
    }

    /// The zoom between `lo` and `hi` where the crop stops reading past its edge, by bisection. `None` unless the crop
    /// reaches its edge at `lo` (exactly reaching it counts, so a solution at `lo` itself is found) and reads within
    /// it at `hi`.
    fn bisect(&self, mut lo: f64, mut hi: f64) -> Option<f64> {
        if self.worst(lo)? < 1.0 || self.worst(hi)? > 1.0 {
            return None;
        }
        for _ in 0..100 {
            let z = 0.5 * (lo + hi);
            // `lo` and `hi` are neighbouring floats: every further step would leave them as they are
            if z <= lo || z >= hi {
                break;
            }
            if self.worst(z)? > 1.0 { lo = z } else { hi = z }
        }
        Some(0.5 * (lo + hi))
    }
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

    /// A wave that reads past the corner but turns down beyond the crop (where only a zoom out reads it): the zoom
    /// in that fills the crop is found, not hidden by the turn (at zoom 0.5 the border reads only 0.8 of itself).
    #[test]
    fn a_wave_turning_down_past_the_crop_still_zooms_in() {
        let (x, y) = (0.5f64.sqrt(), 0.5f64.sqrt());
        let source = |r: f64| r * (1.0 + 0.5 * r * r - 0.4 * r.powi(4));
        assert!(worst(&source, 0.5, x, y) < 1.0 && worst(&source, 1.0, x, y) > 1.0);
        for z in [fill_zoom(&source, x, y).unwrap(), fill_zoom_in(&source, x, y).unwrap()] {
            assert!(z > 1.0, "{z}");
            // the peak lies between the border points `worst` samples
            assert!((worst(&source, z, x, y) - 1.0).abs() < 1e-5, "{z}");
        }
    }

    #[test]
    fn a_solution_at_the_ends_of_the_range_is_found() {
        assert!((fill_zoom(&|r| 0.5 * r, 1.5, 1.0).unwrap() - 0.5).abs() < 1e-12);
        assert!((fill_zoom(&|r| 2.0 * r, 1.5, 1.0).unwrap() - 2.0).abs() < 1e-12);
        assert!((fill_zoom_in(&|r| 2.0 * r, 1.5, 1.0).unwrap() - 2.0).abs() < 1e-12);
    }

    #[test]
    fn zoom_in_leaves_a_crop_that_reads_inside_alone() {
        let barrel = |r: f64| r * (1.0 - 0.03 * r * r);
        assert_eq!(fill_zoom_in(&barrel, 1.5, 1.0), Some(1.0));
        assert_eq!(fill_zoom_in(&|r| r, 1.5, 1.0), Some(1.0));
        let pincushion = |r: f64| r * (1.0 + 0.01 * r * r);
        assert_eq!(fill_zoom_in(&pincushion, 1.5, 1.0), fill_zoom(&pincushion, 1.5, 1.0));
        assert_eq!(fill_zoom_in(&|r| 3.0 * r, 1.5, 1.0), None, "past the zoom range");
        assert_eq!(fill_zoom_in(&|_| f64::NAN, 1.5, 1.0), None);
    }

    #[test]
    fn no_distortion_needs_no_zoom_and_wild_maps_none() {
        assert!((fill_zoom(&|r| r, 1.5, 1.0).unwrap() - 1.0).abs() < 1e-12);
        assert_eq!(fill_zoom(&|r| 3.0 * r, 1.5, 1.0), None, "past the zoom range");
        assert_eq!(fill_zoom(&|r| r / 3.0, 1.5, 1.0), None);
        assert_eq!(fill_zoom(&|_| f64::NAN, 1.5, 1.0), None);
    }
}
