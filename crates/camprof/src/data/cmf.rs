// Ported from RADcamProfiles (github.com/OneSlips/RADcamProfiles, commit dd8003f, crates/engine/src/data/cmf.rs),
// Copyright (c) 2026 RAD Films - Roman Alurkoff, MIT; relicensed MIT OR Apache-2.0 by its author.
// Modified by the porting contributors (2026): no panics on bad input, the app's frame type.
//! CIE standard colour-matching functions sampled every 5 nm.
//!
//! Only the CIE 1931 2° standard observer is carried here (CIE 015:2018,
//! ISO/CIE 11664-1): the observer DNG profiles and the ColorChecker reference
//! values are defined with. (RADcamProfiles also tabulates the 1964 10°, 2006
//! and Judd-Vos observers; the app does not need them.)
//!
//! Values are transcribed from the published CIE tables without
//! re-rounding and are stored at the published normalised amplitude, so the
//! `ȳ` maximum sits close to (but not exactly at) one for every set. Each row
//! is one flat record so callers can slice the three curves out without
//! copying.

/// Colour-matching function rows: `[wavelength_nm, x̄, ȳ, z̄]`.
pub(crate) type CmfRow = [f64; 4];

/// CIE 1931 2° standard observer, 360-830 nm in 5 nm steps.
#[rustfmt::skip]
pub(crate) static CMF_1931_2: [CmfRow; 95] = [
    [360.0, 1.29900000e-04, 3.917000e-06, 6.06100000e-04],
    [365.0, 2.32100000e-04, 6.965000e-06, 1.086000000e-03],
    [370.0, 4.14900000e-04, 1.2390000e-05, 1.946000000e-03],
    [375.0, 7.41600000e-04, 2.2020000e-05, 3.486000000e-03],
    [380.0, 1.368000000e-03, 3.9000000e-05, 6.450001000e-03],
    [385.0, 2.236000000e-03, 6.4000000e-05, 1.0549990000e-02],
    [390.0, 4.243000000e-03, 1.20000000e-04, 2.0050010000e-02],
    [395.0, 7.650000000e-03, 2.17000000e-04, 3.6210000000e-02],

    [400.0, 1.4310000000e-02, 3.96000000e-04, 6.7850010000e-02],
    [405.0, 2.3190000000e-02, 6.40000000e-04, 1.10200000000e-01],
    [410.0, 4.3510000000e-02, 1.210000000e-03, 2.07400000000e-01],
    [415.0, 7.7630000000e-02, 2.180000000e-03, 3.71300000000e-01],
    [420.0, 1.34380000000e-01, 4.000000000e-03, 6.45600000000e-01],
    [425.0, 2.14770000000e-01, 7.300000000e-03, 1.039050100000e+00],
    [430.0, 2.83900000000e-01, 1.1600000000e-02, 1.385600000000e+00],
    [435.0, 3.28500000000e-01, 1.6840000000e-02, 1.622960000000e+00],

    [440.0, 3.48280000000e-01, 2.3000000000e-02, 1.747060000000e+00],
    [445.0, 3.48060000000e-01, 2.9800000000e-02, 1.782600000000e+00],
    [450.0, 3.36200000000e-01, 3.8000000000e-02, 1.772110000000e+00],
    [455.0, 3.18700000000e-01, 4.8000000000e-02, 1.744100000000e+00],
    [460.0, 2.90800000000e-01, 6.0000000000e-02, 1.669200000000e+00],
    [465.0, 2.51100000000e-01, 7.3900000000e-02, 1.528100000000e+00],
    [470.0, 1.95360000000e-01, 9.0980000000e-02, 1.287640000000e+00],
    [475.0, 1.42100000000e-01, 1.12600000000e-01, 1.041900000000e+00],

    [480.0, 9.5640000000e-02, 1.39020000000e-01, 8.12950100000e-01],
    [485.0, 5.7950010000e-02, 1.69300000000e-01, 6.16200000000e-01],
    [490.0, 3.2010000000e-02, 2.08020000000e-01, 4.65180000000e-01],
    [495.0, 1.4700000000e-02, 2.58600000000e-01, 3.53300000000e-01],
    [500.0, 4.900000000e-03, 3.23000000000e-01, 2.72000000000e-01],
    [505.0, 2.400000000e-03, 4.07300000000e-01, 2.12300000000e-01],
    [510.0, 9.300000000e-03, 5.03000000000e-01, 1.58200000000e-01],
    [515.0, 2.9100000000e-02, 6.08200000000e-01, 1.11700000000e-01],

    [520.0, 6.3270000000e-02, 7.10000000000e-01, 7.8249990000e-02],
    [525.0, 1.09600000000e-01, 7.93200000000e-01, 5.7250010000e-02],
    [530.0, 1.65500000000e-01, 8.62000000000e-01, 4.2160000000e-02],
    [535.0, 2.25749900000e-01, 9.14850100000e-01, 2.9840000000e-02],
    [540.0, 2.90400000000e-01, 9.54000000000e-01, 2.0300000000e-02],
    [545.0, 3.59700000000e-01, 9.80300000000e-01, 1.3400000000e-02],
    [550.0, 4.33449900000e-01, 9.94950100000e-01, 8.749999000e-03],
    [555.0, 5.12050100000e-01, 1.000000000000e+00, 5.749999000e-03],

    [560.0, 5.94500000000e-01, 9.95000000000e-01, 3.900000000e-03],
    [565.0, 6.78400000000e-01, 9.78600000000e-01, 2.749999000e-03],
    [570.0, 7.62100000000e-01, 9.52000000000e-01, 2.100000000e-03],
    [575.0, 8.42500000000e-01, 9.15400000000e-01, 1.800000000e-03],
    [580.0, 9.16300000000e-01, 8.70000000000e-01, 1.650001000e-03],
    [585.0, 9.78600000000e-01, 8.16300000000e-01, 1.400000000e-03],
    [590.0, 1.026300000000e+00, 7.57000000000e-01, 1.100000000e-03],
    [595.0, 1.056700000000e+00, 6.94900000000e-01, 1.000000000e-03],

    [600.0, 1.062200000000e+00, 6.31000000000e-01, 8.00000000e-04],
    [605.0, 1.045600000000e+00, 5.66800000000e-01, 6.00000000e-04],
    [610.0, 1.002600000000e+00, 5.03000000000e-01, 3.40000000e-04],
    [615.0, 9.38400000000e-01, 4.41200000000e-01, 2.40000000e-04],
    [620.0, 8.54449900000e-01, 3.81000000000e-01, 1.90000000e-04],
    [625.0, 7.51400000000e-01, 3.21000000000e-01, 1.00000000e-04],
    [630.0, 6.42400000000e-01, 2.65000000000e-01, 4.9999990e-05],
    [635.0, 5.41900000000e-01, 2.17000000000e-01, 3.0000000e-05],

    [640.0, 4.47900000000e-01, 1.75000000000e-01, 2.0000000e-05],
    [645.0, 3.60800000000e-01, 1.38200000000e-01, 1.0000000e-05],
    [650.0, 2.83500000000e-01, 1.07000000000e-01, 0.0e+00],
    [655.0, 2.18700000000e-01, 8.1600000000e-02, 0.0e+00],
    [660.0, 1.64900000000e-01, 6.1000000000e-02, 0.0e+00],
    [665.0, 1.21200000000e-01, 4.4580000000e-02, 0.0e+00],
    [670.0, 8.7400000000e-02, 3.2000000000e-02, 0.0e+00],
    [675.0, 6.3600000000e-02, 2.3200000000e-02, 0.0e+00],

    [680.0, 4.6770000000e-02, 1.7000000000e-02, 0.0e+00],
    [685.0, 3.2900000000e-02, 1.1920000000e-02, 0.0e+00],
    [690.0, 2.2700000000e-02, 8.210000000e-03, 0.0e+00],
    [695.0, 1.5840000000e-02, 5.723000000e-03, 0.0e+00],
    [700.0, 1.1359160000e-02, 4.102000000e-03, 0.0e+00],
    [705.0, 8.110916000e-03, 2.929000000e-03, 0.0e+00],
    [710.0, 5.790346000e-03, 2.091000000e-03, 0.0e+00],
    [715.0, 4.109457000e-03, 1.484000000e-03, 0.0e+00],

    [720.0, 2.899327000e-03, 1.047000000e-03, 0.0e+00],
    [725.0, 2.049190000e-03, 7.40000000e-04, 0.0e+00],
    [730.0, 1.439971000e-03, 5.20000000e-04, 0.0e+00],
    [735.0, 9.99949300e-04, 3.61100000e-04, 0.0e+00],
    [740.0, 6.90078600e-04, 2.49200000e-04, 0.0e+00],
    [745.0, 4.76021300e-04, 1.71900000e-04, 0.0e+00],
    [750.0, 3.32301100e-04, 1.20000000e-04, 0.0e+00],
    [755.0, 2.34826100e-04, 8.4800000e-05, 0.0e+00],

    [760.0, 1.66150500e-04, 6.0000000e-05, 0.0e+00],
    [765.0, 1.17413000e-04, 4.2400000e-05, 0.0e+00],
    [770.0, 8.3075270e-05, 3.0000000e-05, 0.0e+00],
    [775.0, 5.8706520e-05, 2.1200000e-05, 0.0e+00],
    [780.0, 4.1509940e-05, 1.4990000e-05, 0.0e+00],
    [785.0, 2.9353260e-05, 1.0600000e-05, 0.0e+00],
    [790.0, 2.0673830e-05, 7.465700e-06, 0.0e+00],
    [795.0, 1.4559770e-05, 5.257800e-06, 0.0e+00],

    [800.0, 1.0253980e-05, 3.702900e-06, 0.0e+00],
    [805.0, 7.221456e-06, 2.607800e-06, 0.0e+00],
    [810.0, 5.085868e-06, 1.836600e-06, 0.0e+00],
    [815.0, 3.581652e-06, 1.293400e-06, 0.0e+00],
    [820.0, 2.522525e-06, 9.10930e-07, 0.0e+00],
    [825.0, 1.776509e-06, 6.41530e-07, 0.0e+00],
    [830.0, 1.251141e-06, 4.51810e-07, 0.0e+00],
];

/// One tabulated colour-matching function set.
#[derive(Clone, Copy)]
pub(crate) struct CmfTable {
    /// First sampled wavelength in nanometres.
    pub start: f64,
    /// Distance between neighbouring samples in nanometres.
    pub spacing: f64,
    /// The tabulated rows.
    pub rows: &'static [CmfRow],
}

/// Every tabulated set, keyed by its canonical name.
#[rustfmt::skip]
pub(crate) static TABLES: [(&str, CmfTable); 1] = [
    ("1931_2", CmfTable { start: 360.0, spacing: 5.0, rows: &CMF_1931_2 }),
];

/// Look up a set by name.
///
/// Matching is case-insensitive, `-` is accepted where `_` is written, and a
/// leading `cie` prefix is ignored, so `"CIE_1931_2"`, `"1931-2"` and
/// `"1931_2"` all name the same table.
pub(crate) fn lookup(name: &str) -> Option<CmfTable> {
    let mut wanted = name.trim().to_ascii_lowercase().replace('-', "_");
    if let Some(rest) = wanted.strip_prefix("cie") {
        wanted = rest.trim_start_matches('_').to_string();
    }
    TABLES.iter().find(|(canonical, _)| *canonical == wanted).map(|(_, table)| *table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_table_is_a_well_formed_observer() {
        for (name, table) in TABLES.iter() {
            assert_eq!(table.rows[0][0], table.start, "{name}");
            for pair in table.rows.windows(2) {
                assert_eq!(pair[1][0] - pair[0][0], table.spacing, "{name}");
            }
        }
        // all four components are present and finite
        {
            for (name, table) in TABLES.iter() {
                for row in table.rows {
                    assert!(row.iter().all(|v| v.is_finite()), "{name}");
                    // The wavelength dwarfs every amplitude, so a swapped
                    // column would show up here immediately.
                    assert!(row[1] < row[0], "{name}");
                    assert!(row[2] < row[0], "{name}");
                    assert!(row[3] < row[0], "{name}");
                }
            }
        }
        // ybar peaks in the green
        {
            for (name, table) in TABLES.iter() {
                let mut peak = table.rows[0];
                for row in table.rows {
                    if row[2] > peak[2] {
                        peak = *row;
                    }
                }
                let peak_nm = peak[0];
                assert!((500.0..=600.0).contains(&peak_nm), "{name}: luminance peaks at {peak_nm} nm");
            }
        }
        // row counts match nominal ranges
        {
            for (name, table) in TABLES.iter() {
                let last = table.start + table.spacing * (table.rows.len() - 1) as f64;
                let expected = if *name == "judd_vos_1978_2" { 825 } else { 830 };
                assert_eq!(last as i32, expected, "{name}");
            }
        }
    }

    #[test]
    fn lookup_is_case_and_separator_insensitive() {
        assert!(lookup("1931-2").is_some());
        assert!(lookup("1931_2").is_some());
        assert!(lookup("CIE_1931_2").is_some());
        assert!(lookup("nope").is_none());
    }
}
