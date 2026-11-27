//! What a product's making reserves at the memory gate.

use super::reservation;
use crate::denoise::{Domain, LinearSpace};

#[test]
fn a_bayer_picture_reserves_its_mosaic_and_tiles() {
    assert_eq!(reservation(Domain::BayerToRgb, LinearSpace::CameraRgb, 12_000_000, 1, 0, 1), 12_000_000 * 20);
}

/// A linear-RGB picture's demosaic and its tiles do not overlap in time, but the RGB the demosaic makes lives through
/// both: the reservation must cover the larger of the two from before the decode, so neither the samples the decode
/// leaves nor that RGB is ever held uncharged.
#[test]
fn a_linear_picture_reserves_its_demosaic_as_well_as_its_tiles() {
    let pixels = 256 * 256;
    // a small picture: the demosaic's own 16 MiB of headroom is what decides
    assert!(reservation(Domain::LinearRgb, LinearSpace::CameraRgb, pixels, 1, 0, 1) >= pixels * 20 + (16 << 20));
    // a converted picture keeps the camera-domain clip map as well
    assert!(reservation(Domain::LinearRgb, LinearSpace::LinRec2020, pixels, 1, 0, 1) >= pixels * 32);
    // a three-sample picture keeps its own samples beside the RGB it makes
    assert!(reservation(Domain::LinearRgb, LinearSpace::CameraRgb, pixels, 3, 0, 1) >= pixels * 36 + (16 << 20));
    // a large picture is reserved for the larger phase, not both added together (36 B/px of demosaic against 32 of tiles
    // would be the demosaic's, so a one-sample converted picture is the one where the tiles decide)
    assert_eq!(reservation(Domain::LinearRgb, LinearSpace::LinRec2020, 12_000_000, 1, 0, 1), 12_000_000 * 32);
    assert_eq!(reservation(Domain::LinearRgb, LinearSpace::CameraRgb, 12_000_000, 3, 0, 1), 12_000_000 * 36 + (16 << 20));
    // and the model's own scratch, for every tile that runs at once, comes on top
    assert_eq!(reservation(Domain::LinearRgb, LinearSpace::CameraRgb, 12_000_000, 1, 1000, 4), 12_000_000 * 28 + 1000 * 4);
}
