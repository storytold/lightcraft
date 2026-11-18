// Ported from RADcamProfiles (github.com/OneSlips/RADcamProfiles, commit dd8003f, crates/engine/src/data/illuminants.rs),
// Copyright (c) 2026 RAD Films - Roman Alurkoff, MIT; relicensed MIT OR Apache-2.0 by its author.
// Modified by the porting contributors (2026): no panics on bad input, the app's frame type.
//! Relative spectral power distributions of the standard illuminants.
//!
//! Six distributions are tabulated: the tungsten standard illuminant A, the
//! daylight phases D50, D55, D65 and D75, and the equal-energy illuminant E,
//! which is included because it is the reference white the profiler falls
//! back on when no measured illuminant is available.
//!
//! The amplitudes are the published values, in ascending wavelength order and
//! spaced `spacing` nanometres apart from `start`. They are published on a
//! 0..100 scale, so each table also records the divisor `scale` it was
//! published with: dividing the amplitudes by `scale` gives the
//! unity-normalised weights.

/// StdA, 300-830 nm in 5 nm steps.
#[rustfmt::skip]
pub(crate) static STD_A: [f64; 107] = [
    0.930483, 1.128210, 1.357690, 1.622190, 1.925080,
    2.269800, 2.659810, 3.098610, 3.589680, 4.136480,
    4.742380, 5.410700, 6.144620, 6.947200, 7.821350,
    8.769800, 9.795100, 10.899600, 12.085300, 13.354300,
    14.708, 16.148, 17.6753, 19.2907, 20.995,
    22.7883, 24.6709, 26.6425, 28.7027, 30.8508,
    33.0859, 35.4068, 37.8121, 40.3002, 42.8693,
    45.5174, 48.2423, 51.0418, 53.9132, 56.8539,
    59.8611, 62.9320, 66.0635, 69.2525, 72.4959,
    75.7903, 79.1326, 82.5193, 85.947, 89.4124,
    92.912, 96.4423, 100.000, 103.582, 107.184,
    110.803, 114.436, 118.080, 121.731, 125.386,
    129.043, 132.697, 136.346, 139.988, 143.618,
    147.235, 150.836, 154.418, 157.979, 161.516,
    165.028, 168.510, 171.963, 175.383, 178.769,
    182.118, 185.429, 188.701, 191.931, 195.118,
    198.261, 201.359, 204.409, 207.411, 210.365,
    213.268, 216.120, 218.920, 221.667, 224.361,
    227.000, 229.585, 232.115, 234.589, 237.008,
    239.370, 241.675, 243.924, 246.116, 248.251,
    250.329, 252.350, 254.314, 256.221, 258.071,
    259.865, 261.602,
];

/// D50, 300-830 nm in 5 nm steps.
#[rustfmt::skip]
pub(crate) static D50: [f64; 107] = [
    0.02, 1.03, 2.05, 4.91, 7.78,
    11.26, 14.75, 16.35, 17.95, 19.48,
    21.01, 22.48, 23.94, 25.45, 26.96,
    25.72, 24.49, 27.18, 29.87, 39.59,
    49.31, 52.91, 56.51, 58.27, 60.03,
    58.93, 57.82, 66.32, 74.82, 81.04,
    87.25, 88.93, 90.61, 90.99, 91.37,
    93.24, 95.11, 93.54, 91.96, 93.84,
    95.72, 96.17, 96.61, 96.87, 97.13,
    99.61, 102.10, 101.43, 100.75, 101.54,
    102.32, 101.16, 100.00, 98.87, 97.74,
    98.33, 98.92, 96.21, 93.50, 95.59,
    97.69, 98.48, 99.27, 99.16, 99.04,
    97.38, 95.72, 97.29, 98.86, 97.26,
    95.67, 96.93, 98.19, 100.60, 103.00,
    101.07, 99.13, 93.26, 87.38, 89.49,
    91.60, 92.25, 92.89, 84.87, 76.85,
    81.68, 86.51, 89.55, 92.58, 85.40,
    78.23, 67.96, 57.69, 70.31, 82.92,
    80.60, 78.27, 78.91, 79.55, 76.48,
    73.40, 68.66, 63.92, 67.35, 70.78,
    72.61, 74.44,
];

/// D55, 300-780 nm in 5 nm steps.
#[rustfmt::skip]
pub(crate) static D55: [f64; 97] = [
    0.024, 1.048, 2.072, 6.648, 11.224,
    15.936, 20.647, 22.266, 23.885, 25.851,
    27.817, 29.219, 30.621, 32.464, 34.308,
    33.446, 32.584, 35.335, 38.087, 49.518,
    60.949, 64.751, 68.554, 70.065, 71.577,
    69.746, 67.914, 76.760, 85.605, 91.799,
    97.993, 99.228, 100.463, 100.188, 99.913,
    101.326, 102.739, 100.409, 98.078, 99.379,
    100.680, 100.688, 100.695, 100.341, 99.987,
    102.098, 104.210, 103.156, 102.102, 102.535,
    102.968, 101.484, 100.000, 98.608, 97.216,
    97.482, 97.749, 94.590, 91.432, 92.926,
    94.419, 94.780, 95.140, 94.680, 94.220,
    92.334, 90.448, 91.389, 92.330, 90.592,
    88.854, 89.586, 90.317, 92.133, 93.950,
    91.953, 89.956, 84.817, 79.677, 81.258,
    82.840, 83.842, 84.844, 77.539, 70.235,
    74.768, 79.301, 82.147, 84.993, 78.437,
    71.880, 62.337, 52.793, 64.360, 75.927,
    73.872, 71.818,
];

/// D65, 300-830 nm in 5 nm steps.
#[rustfmt::skip]
pub(crate) static D65: [f64; 107] = [
    0.03410, 1.66430, 3.29450, 11.76520, 20.23600,
    28.64470, 37.05350, 38.50110, 39.94880, 42.43020,
    44.91170, 45.77500, 46.63830, 49.36370, 52.08910,
    51.03230, 49.97550, 52.31180, 54.64820, 68.70150,
    82.75490, 87.12040, 91.48600, 92.45890, 93.43180,
    90.05700, 86.68230, 95.77360, 104.86500, 110.93600,
    117.00800, 117.41000, 117.81200, 116.33600, 114.86100,
    115.39200, 115.92300, 112.36700, 108.81100, 109.08200,
    109.35400, 108.57800, 107.80200, 106.29600, 104.79000,
    106.23900, 107.68900, 106.04700, 104.40500, 104.22500,
    104.04600, 102.02300, 100.00000, 98.16710, 96.33420,
    96.06110, 95.78800, 92.23680, 88.68560, 89.34590,
    90.00620, 89.80260, 89.59910, 88.64890, 87.69870,
    85.49360, 83.28860, 83.49390, 83.69920, 81.86300,
    80.02680, 80.12070, 80.21460, 81.24620, 82.27780,
    80.28100, 78.28420, 74.00270, 69.72130, 70.66520,
    71.60910, 72.97900, 74.34900, 67.97650, 61.60400,
    65.74480, 69.88560, 72.48630, 75.08700, 69.33980,
    63.59270, 55.00540, 46.41820, 56.61180, 66.80540,
    65.09410, 63.38280, 63.84340, 64.30400, 61.87790,
    59.45190, 55.70540, 51.95900, 54.69980, 57.44060,
    58.87650, 60.31250,
];

/// D75, 300-780 nm in 5 nm steps.
#[rustfmt::skip]
pub(crate) static D75: [f64; 97] = [
    0.043, 2.588, 5.133, 17.470, 29.808,
    42.369, 54.930, 56.095, 57.259, 60.000,
    62.740, 62.861, 62.982, 66.647, 70.312,
    68.507, 66.703, 68.333, 69.963, 85.946,
    101.929, 106.911, 111.894, 112.346, 112.798,
    107.945, 103.092, 112.145, 121.198, 127.104,
    133.010, 132.682, 132.355, 129.838, 127.322,
    127.061, 126.800, 122.291, 117.783, 117.186,
    116.589, 115.146, 113.702, 111.181, 108.659,
    109.552, 110.445, 108.367, 106.289, 105.596,
    104.904, 102.452, 100.000, 97.808, 95.616,
    94.914, 94.213, 90.605, 86.997, 87.112,
    87.227, 86.684, 86.140, 84.861, 83.581,
    81.164, 78.747, 78.587, 78.428, 76.614,
    74.801, 74.562, 74.324, 74.873, 75.422,
    73.499, 71.576, 67.714, 63.852, 64.464,
    65.076, 66.573, 68.070, 62.256, 56.443,
    60.343, 64.242, 66.697, 69.151, 63.890,
    58.629, 50.623, 42.617, 51.985, 61.352,
    59.838, 58.324,
];

/// StdE, 300-830 nm in 5 nm steps.
#[rustfmt::skip]
pub(crate) static STD_E: [f64; 107] = [
    100.0, 100.0, 100.0, 100.0, 100.0,
    100.0, 100.0, 100.0, 100.0, 100.0,
    100.0, 100.0, 100.0, 100.0, 100.0,
    100.0, 100.0, 100.0, 100.0, 100.0,
    100.0, 100.0, 100.0, 100.0, 100.0,
    100.0, 100.0, 100.0, 100.0, 100.0,
    100.0, 100.0, 100.0, 100.0, 100.0,
    100.0, 100.0, 100.0, 100.0, 100.0,
    100.0, 100.0, 100.0, 100.0, 100.0,
    100.0, 100.0, 100.0, 100.0, 100.0,
    100.0, 100.0, 100.0, 100.0, 100.0,
    100.0, 100.0, 100.0, 100.0, 100.0,
    100.0, 100.0, 100.0, 100.0, 100.0,
    100.0, 100.0, 100.0, 100.0, 100.0,
    100.0, 100.0, 100.0, 100.0, 100.0,
    100.0, 100.0, 100.0, 100.0, 100.0,
    100.0, 100.0, 100.0, 100.0, 100.0,
    100.0, 100.0, 100.0, 100.0, 100.0,
    100.0, 100.0, 100.0, 100.0, 100.0,
    100.0, 100.0, 100.0, 100.0, 100.0,
    100.0, 100.0, 100.0, 100.0, 100.0,
    100.0, 100.0,
];

/// One tabulated illuminant spectrum.
#[derive(Clone, Copy)]
pub(crate) struct IlluminantTable {
    /// The distribution's name, as published.
    pub name: &'static str,
    /// EXIF LightSource tag value, or `-1` when the illuminant has none.
    pub exif_tag: i32,
    /// First sampled wavelength in nanometres.
    pub start: f64,
    /// Distance between neighbouring samples in nanometres.
    pub spacing: f64,
    /// Divisor the amplitudes were published with.
    pub scale: f64,
    /// The amplitudes, in ascending wavelength order.
    pub amps: &'static [f64],
}

/// Every tabulated distribution, keyed by its published name.
pub(crate) static TABLES: [(&str, IlluminantTable); 6] = [
    ("StdA", IlluminantTable { name: "StdA", exif_tag: 17, start: 300.0, spacing: 5.0, scale: 100.0, amps: &STD_A }),
    ("D50", IlluminantTable { name: "D50", exif_tag: 23, start: 300.0, spacing: 5.0, scale: 100.0, amps: &D50 }),
    ("D55", IlluminantTable { name: "D55", exif_tag: 20, start: 300.0, spacing: 5.0, scale: 100.0, amps: &D55 }),
    ("D65", IlluminantTable { name: "D65", exif_tag: 21, start: 300.0, spacing: 5.0, scale: 100.0, amps: &D65 }),
    ("D75", IlluminantTable { name: "D75", exif_tag: 22, start: 300.0, spacing: 5.0, scale: 100.0, amps: &D75 }),
    ("StdE", IlluminantTable { name: "StdE", exif_tag: -1, start: 300.0, spacing: 5.0, scale: 100.0, amps: &STD_E }),
];

/// Look up a distribution by name.
///
/// Matching ignores case and any `-` or `_` separators, so `"D-50"`,
/// `"d_50"` and `"D50"` are the same table. The bare aliases `A` and `E`
/// stand for the standard light A and the equal-energy distribution.
pub(crate) fn lookup(name: &str) -> Option<IlluminantTable> {
    let key: String = name.trim().to_ascii_lowercase().chars().filter(|c| *c != '-' && *c != '_').collect();
    let key = match key.as_str() {
        "a" => "stda",
        "e" => "stde",
        _ => key.as_str(),
    };
    TABLES.iter().find(|(canonical, _)| canonical.to_ascii_lowercase() == key).map(|(_, table)| *table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amplitudes_are_positive_and_declared_scale() {
        for (name, table) in TABLES.iter() {
            assert_eq!(table.scale, 100.0, "{name}");
            assert!(!table.amps.is_empty(), "{name}");
            for amp in table.amps {
                assert!(amp.is_finite() && *amp > 0.0, "{name}: {amp}");
            }
        }
        // band count matches declared range
        {
            // (published name, band_end) exactly as the source table declares.
            let declared: [(&str, i32); 6] = [("StdA", 830), ("D50", 830), ("D55", 780), ("D65", 830), ("D75", 780), ("StdE", 830)];
            assert_eq!(declared.len(), TABLES.len());
            for (name, band_end) in declared {
                let table =
                    TABLES.iter().find(|(canonical, _)| *canonical == name).map(|(_, table)| table).unwrap_or_else(|| panic!("missing {name}"));
                let last = table.start + table.spacing * (table.amps.len() - 1) as f64;
                assert_eq!(last as i32, band_end, "{name}");
            }
        }
    }

    #[test]
    fn lookup_aliases_resolve() {
        let aliases = [
            ("A", "StdA"),
            ("a", "StdA"),
            ("std_a", "StdA"),
            ("E", "StdE"),
            ("e", "StdE"),
            ("std-e", "StdE"),
            ("D-50", "D50"),
            ("d_50", "D50"),
            ("d65", "D65"),
            ("d-75", "D75"),
        ];
        for (alias, canonical) in aliases {
            let got = lookup(alias).unwrap_or_else(|| panic!("alias {alias}"));
            let want = lookup(canonical).unwrap_or_else(|| panic!("name {canonical}"));
            assert_eq!(got.name, want.name, "{alias}");
            assert_eq!(got.exif_tag, want.exif_tag, "{alias}");
            assert_eq!(got.start, want.start, "{alias}");
            assert_eq!(got.spacing, want.spacing, "{alias}");
            assert_eq!(got.amps.len(), want.amps.len(), "{alias}");
            assert_eq!(got.amps[0], want.amps[0], "{alias}");
        }
        assert!(lookup("nonexistent").is_none());
    }
}
