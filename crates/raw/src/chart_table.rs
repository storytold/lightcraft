// Camera colour matrices fitted to our own photographs of a ColorChecker Classic (post-November-2014 formulation),
// for cameras with no published spectral sensitivities. Kept by hand: each fitted row's capture, fit and checks are in
// clean-room/crw/10-chart-calibration.md (scripts local-only). Reference colours: X-Rite's published L*a*b* (D50) for
// the chart, as packaged by colour-science (BSD-3-Clause). No number comes from Adobe or from a GPL raw decoder.
// The Nikon D750 row is the exception: it is not fitted to a D750 chart, it reuses the D850's matrices as an
// approximation (see its entry), so it does not meet the fitted-row contract above.
use super::Camera;
use lightcraft_color::Mat3;

#[rustfmt::skip]
pub(super) const CAMERAS: &[Camera] = &[
    // Canon EOS D60 (CRW), the maintainer's body, ISO 100. Matrix 1: one ~2,900 K tungsten bulb, 11 frames, filed as
    // Standard Illuminant A; matrix 2: direct afternoon sun, ~4,850 K, 20 frames, filed as D50 (both temperatures from
    // the camera's own white-balance presets). Fitted on chromaticity with neutral held at D50 (the sunlit chart was
    // unevenly lit): CIEDE2000 mean 2.42 / 1.70 on the 24 patches, 2.74 / 1.97 leaving each patch out.
    Camera {
        make: "Canon",
        model: "EOS D60",
        illuminant: [17, 23],
        color_matrix: [Mat3([[0.950961, -0.345297, 0.112165], [-0.545579, 1.400621, 0.558365], [-0.123107, 0.222004, 1.026236]]), Mat3([[0.609694, -0.127035, -0.045387], [-0.461201, 1.16644, 0.337283], [-0.115992, 0.229363, 0.791647]])],
        forward_matrix: [Mat3([[0.763921, 0.183562, 0.016813], [0.268545, 0.944022, -0.212567], [0.100742, -0.431647, 1.15601]]), Mat3([[0.757277, 0.190573, 0.016446], [0.291782, 1.009128, -0.300909], [0.026419, -0.264451, 1.063137]])],
    },
    // Nikon D750, APPROXIMATE: these are the D850's matrices (the nearest full-frame Nikon body in the spectral
    // table), not a fit to a D750 ColorChecker. They replace the neutral camera-RGB fallback for D750 raws.
    // Real D750 chart captures would replace them with a fitted row.
    Camera {
        make: "NIKON CORPORATION",
        model: "D750",
        illuminant: [17, 21],
        color_matrix: [Mat3([[1.17359, -0.461087, -0.011934], [-0.607581, 1.52285, 0.4063], [-0.096127, 0.196063, 1.00005]]), Mat3([[0.773836, -0.196466, -0.097677], [-0.609077, 1.35526, 0.205388], [-0.190117, 0.290252, 0.676129]])],
        forward_matrix: [Mat3([[0.713588, 0.158895, 0.091813], [0.273041, 0.838318, -0.111359], [0.055293, -0.367196, 1.13701]]), Mat3([[0.669707, 0.227388, 0.0672], [0.291703, 0.871502, -0.163205], [0.042827, -0.229884, 1.01216]])],
    },
];
