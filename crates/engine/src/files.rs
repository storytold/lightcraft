//! Real files: probing (dimensions + metadata for import) and loading (decode → linear Rec.2020,
//! oriented, downscaled to the requested level). Standard formats go through `lightcraft-codecs`,
//! camera raws through `lightcraft-raw` (demosaic + DNG colour model).

use std::sync::Arc;

use lightcraft_catalog::{MediaKind, Meta};
use lightcraft_color::cct::xy_to_temp_tint;
use lightcraft_geom::Orientation;
use lightcraft_pipeline::SourceInfo;
use lightcraft_raster::Rgb32f;
use lightcraft_raster::resample::{Filter, fit};

use crate::media::{DenoiseSpec, FileLoader, FileProbe, PairLoader, PreviewLoader, ProbeInfo, ReducedLoader};

mod colour;
mod fs;
mod load;
mod preview;
mod probe;

pub use colour::bin_factor;
pub(crate) use colour::clipped_white;
#[cfg(test)]
use fs::quick_image_srgb;
#[cfg(test)]
use fs::{PARTIAL_MIN, probe_file, probe_partial};
pub use fs::{fs_header_probe, fs_hooks, fs_pair_loader, fs_reduced_loader};
pub(crate) use load::{DngWhite, WbScaleOf, dng_tone_curve};
pub use load::{load_bytes, load_vec, load_vec_pair, load_vec_reduced};
#[cfg(test)]
use preview::PLAN_PREFIX;
pub(crate) use preview::decode_raw_preview;
pub use preview::{PreviewRead, embedded_preview_srgb, fs_preview_loader, load_embedded_preview, preview_window};
pub use probe::{embedded_lens, probe_bytes};
#[cfg(test)]
use probe::{meta_of, preview_reason};

/// Sensor clip level (normalised) for highlight reconstruction.
const HIGHLIGHT_CLIP: f32 = 0.99;

/// How a raw's camera RGB becomes the linear Rec.2020 the pipeline takes.
pub(crate) struct CameraColourModel<'a> {
    /// White-balance multipliers (as shot).
    pub wb: [f32; 3],
    /// White-balanced camera RGB → linear Rec.2020: the camera matrix, or a look fitted to the camera's JPEG on top.
    pub matrix: lightcraft_color::Mat3,
    /// Baseline exposure, as a gain.
    pub gain: f32,
    /// A DNG profile's hue/saturation map and look table.
    pub tables: Option<&'a lightcraft_raw::profile::ProfileTables>,
    /// A fitted camera look's hue/saturation table.
    pub hue_sat: Option<&'a crate::camera_preview::HueSat<'a>>,
}

impl CameraColourModel<'_> {
    /// Rebuild sensor-clipped channels; fully clipped highlights become [`clipped_white`] of the matrix, so they
    /// come out of [`Self::apply`] neutral.
    pub fn rebuild_highlights(&self, img: &mut Rgb32f) {
        lightcraft_raw::highlight::reconstruct_with(img, self.wb, HIGHLIGHT_CLIP, clipped_white(&self.matrix));
    }

    /// [`Self::rebuild_highlights`] for a binned image whose clipped channels `mask` tells
    /// ([`lightcraft_raw::RawImage::develop_binned_masked`]).
    pub fn rebuild_highlights_masked(&self, img: &mut Rgb32f, mask: &[u8]) {
        lightcraft_raw::highlight::reconstruct_masked(img, self.wb, HIGHLIGHT_CLIP, clipped_white(&self.matrix), mask);
    }

    /// White balance, matrix, tables and baseline exposure, in place.
    pub fn apply(&self, img: &mut Rgb32f) {
        let (wb, m, gain) = (self.wb, self.matrix.to_f32(), self.gain);
        if let (None, Some(hue_sat)) = (self.tables, self.hue_sat) {
            // a fitted camera look: `pixel`, `LANES` pixels at a time so the table step vectorises (same bits)
            const LANES: usize = 16;
            lightcraft_raster::par_rows(&mut img.data, img.width, |_, row| {
                for chunk in row.chunks_mut(LANES) {
                    let mut rgb = [[0.0f32; LANES]; 3];
                    for (l, p) in chunk.iter().enumerate() {
                        let c = [p[0] * wb[0], p[1] * wb[1], p[2] * wb[2]];
                        for (i, rgb) in rgb.iter_mut().enumerate() {
                            rgb[l] = (m[i][0] * c[0] + m[i][1] * c[1] + m[i][2] * c[2]) * gain;
                        }
                    }
                    let out = hue_sat.apply_lanes(rgb);
                    for (l, p) in chunk.iter_mut().enumerate() {
                        *p = [out[0][l], out[1][l], out[2][l]].map(|v| v.max(0.0));
                    }
                }
            });
            return;
        }
        img.map_in_place(|p| self.pixel(&m, p));
    }

    /// [`Self::apply`] to one pixel; `m` is the matrix as `f32`.
    fn pixel(&self, m: &[[f32; 3]; 3], p: [f32; 3]) -> [f32; 3] {
        let (wb, gain) = (self.wb, self.gain);
        let c = [p[0] * wb[0], p[1] * wb[1], p[2] * wb[2]];
        let rgb = [
            m[0][0] * c[0] + m[0][1] * c[1] + m[0][2] * c[2],
            m[1][0] * c[0] + m[1][1] * c[1] + m[1][2] * c[2],
            m[2][0] * c[0] + m[2][1] * c[1] + m[2][2] * c[2],
        ];
        let rgb = match self.tables {
            Some(tables) => tables.apply(rgb, gain),
            None => rgb.map(|v| v * gain),
        };
        self.hue_sat.map_or(rgb, |h| h.apply(rgb)).map(|v| v.max(0.0))
    }
}

#[cfg(test)]
pub(crate) mod tests;
