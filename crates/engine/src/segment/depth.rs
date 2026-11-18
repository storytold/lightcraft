//! The depth model behind Depth Range masks: Depth Anything V2 Small (`lcc_segment::depth`,
//! Apache-2.0 code and weights), an on-request download of [`lcc_segment::models::DEPTH`]
//! (`ai.models.download {"id": "depth"}`).
//!
//! Once its files are in place it is installed as the pipeline's depth model
//! ([`lcc_pipeline::depth_model::set_depth_model`]): photos with a Depth Range mask get a
//! distance grid, computed once per photo (cached) with an activity row while it runs. A
//! failure to load or run it is logged; the Depth Range then selects nothing.

use std::path::{Path, PathBuf};

use crate::activity::Activity;

/// Which folder the installed pipeline model was loaded from.
#[derive(Default)]
pub struct DepthMasks {
    installed_from: Option<PathBuf>,
}

impl DepthMasks {
    /// Install the model from `want` (idempotent), or remove it with `None`.
    pub fn sync(&mut self, want: Option<PathBuf>, activity: &Activity) {
        if want == self.installed_from {
            return;
        }
        match &want {
            Some(dir) => install(dir, activity.clone()),
            None => lcc_pipeline::depth_model::set_depth_model(None),
        }
        self.installed_from = want;
    }

    /// Whether the pipeline uses the depth model now.
    pub fn active(&self) -> bool {
        self.installed_from.is_some() && lcc_pipeline::depth_model::has_depth_model()
    }
}

/// The model's relative inverse depth (larger = nearer, 0..1) as the pipeline's distance grid
/// (0 = nearest, 1 = farthest). Only the AI-masks build's model callback and the tests call it.
#[cfg(any(feature = "sam", test))]
pub(crate) fn distance(inverse: &[f32], width: usize, height: usize) -> Option<lcc_raster::Plane> {
    if width == 0 || height == 0 || inverse.len() != width.checked_mul(height)? {
        return None;
    }
    let data = inverse.iter().map(|v| if v.is_finite() { (1.0 - v).clamp(0.0, 1.0) } else { 1.0 }).collect();
    Some(lcc_raster::Plane { width, height, data })
}

/// Install the model in `dir` as the pipeline's depth model (loaded on first use).
#[cfg(feature = "sam")]
fn install(dir: &Path, activity: Activity) {
    use std::sync::{Arc, Mutex, PoisonError};

    use lcc_segment::depth::DepthModel;

    let dir = dir.to_path_buf();
    let model: Mutex<Option<DepthModel>> = Mutex::new(None);
    let run = move |rgb: &[u8], w: usize, h: usize| -> Option<lcc_raster::Plane> {
        let _task = activity.start("segment", "Estimating depth", crate::activity::Cancel::No);
        let mut m = model.lock().unwrap_or_else(PoisonError::into_inner);
        if m.is_none() {
            match DepthModel::load(&dir).or_else(|e| {
                log::warn!("depth model on the GPU: {e}; using the CPU");
                DepthModel::load_cpu(&dir)
            }) {
                Ok(d) => *m = Some(d),
                Err(e) => {
                    log::warn!("depth model: {e}");
                    return None;
                }
            }
        }
        let started = std::time::Instant::now();
        match m.as_ref()?.estimate(rgb, w, h) {
            Ok(d) => {
                log::info!("depth model: {w}×{h} in {:?}", started.elapsed());
                distance(&d.data, d.width, d.height)
            }
            Err(e) => {
                log::warn!("depth model: {e}");
                None
            }
        }
    };
    lcc_pipeline::depth_model::set_depth_model(Some(Arc::new(run)));
}

#[cfg(not(feature = "sam"))]
fn install(_: &Path, _: Activity) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearer_is_a_smaller_distance_and_bad_grids_are_refused() {
        let p = distance(&[1.0, 0.0, f32::NAN, 0.25], 2, 2).unwrap();
        assert_eq!(p.data, vec![0.0, 1.0, 1.0, 0.75]);
        assert!(distance(&[0.5; 3], 2, 2).is_none());
        assert!(distance(&[], 0, 0).is_none());
        assert!(distance(&[0.5], usize::MAX, 2).is_none());
    }

    #[cfg(feature = "sam")]
    #[test]
    fn a_damaged_model_selects_nothing_and_never_panics() {
        let dir = std::env::temp_dir().join(format!("lcc-depth-engine-damaged-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("config.json"), br#"{"model_type": "depth_anything"}"#).unwrap();
        std::fs::write(dir.join("model.safetensors"), b"not weights").unwrap();
        let activity = Activity::default();
        let mut d = DepthMasks::default();
        d.sync(Some(dir.clone()), &activity);
        assert!(d.active());
        let src = lcc_raster::Rgb32f::new(64, 48);
        let info = lcc_pipeline::SourceInfo::default();
        let mut set = lcc_develop::DevelopSettings::default();
        set.masks.push(lcc_develop::Mask {
            id: 1,
            components: vec![lcc_develop::MaskComponent {
                name: None,
                anchor: None,
                op: Default::default(),
                invert: false,
                shape: lcc_develop::MaskShape::DepthRange { lo: 0.0, hi: 0.5, feather: 0.1 },
            }],
            ..Default::default()
        });
        let a = lcc_pipeline::auto_mask::for_source(&src, &info, &set);
        assert!(a.depth.is_none());
        d.sync(None, &activity);
        assert!(!d.active());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
