//! `faces.detect`: find faces with the YuNet detector the user has downloaded (Settings > Faces).
//!
//! The photo is rendered upright and uncropped with default settings (so edits, crops and rotations you have
//! made do not matter), the detector runs on it, and the faces come back as fractions of the photo's width
//! and height, the same frame face regions from XMP use. Unless `apply` is false, they are added to the photo as
//! unnamed face regions in one undoable step; earlier detections are replaced, and regions that came from XMP (or
//! that you drew or named) are never touched. Like every region edit it is catalog-only: no sidecar is written.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use dac_catalog::Op;
use dac_develop::DevelopSettings;
use dac_faces::known::YUNET_ID;
use dac_faces::yunet::{Detector, Face, Options};
use dac_meta::{Region, RegionKind};
use serde_json::{Value, json};

use super::{CommandSpec, bad, cmd, has_selection};
use crate::{Result, Session};

const C: &str = "faces.detect";
/// Regions made by this command say so in their description; only those are replaced on a new run.
pub(crate) const MARK: &str = "Detected by ";
pub(crate) const LABEL: &str = "YuNet 2023mar";
/// Long edge of the picture the detector looks at (it shrinks it to its own 640 anyway).
const LOOK_EDGE: usize = 1280;
/// YuNet is 232 KB; a file this much larger in its folder is not it.
const MAX_MODEL_BYTES: u64 = 8 << 20;
/// How much a detection must overlap a region the photo already has to count as the same face.
const EXISTING_IOU: f64 = 0.3;

/// What to tell someone who has not downloaded the detector yet.
const NOT_INSTALLED: &str = "finding faces needs the YuNet face detector (232 KB, MIT): download it in Settings > Faces";

/// The installed YuNet, loaded once and kept (a new one is loaded when the model folder changes).
/// (also used by the background scan, which aligns faces by the detector's landmarks and finds faces in photos that have none)
pub(crate) fn detector(s: &Session) -> std::result::Result<Arc<Detector>, String> {
    static LOADED: Mutex<Option<(PathBuf, Arc<Detector>)>> = Mutex::new(None);
    let home = s.face_models_dir.as_deref().ok_or("this build has nowhere to keep face models (the desktop app does)")?.join(YUNET_ID);
    let path = home.join("model.onnx");
    // installing writes the manifest last, so a folder without one is not (yet) a model
    if !path.is_file() || !home.join("face-model.json").is_file() {
        return Err(NOT_INSTALLED.into());
    }
    let mut loaded = LOADED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((p, d)) = loaded.as_ref()
        && *p == path
    {
        return Ok(d.clone());
    }
    let len = std::fs::metadata(&path).map_err(|e| format!("could not read the detector: {e}"))?.len();
    if len > MAX_MODEL_BYTES {
        return Err("the detector's file is far bigger than YuNet: remove it in Settings > Faces and download it again".into());
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("could not read the detector: {e}"))?;
    let d = Arc::new(Detector::new(&bytes).map_err(|e| format!("{e}: remove the detector in Settings > Faces and download it again"))?);
    *loaded = Some((path, d.clone()));
    Ok(d)
}

fn face_json(f: &Face) -> Value {
    json!({
        "rect": {"x0": f.x0, "y0": f.y0, "x1": f.x1, "y1": f.y1},
        "score": f.score,
        "landmarks": f.landmarks.iter().map(|(x, y)| json!([x, y])).collect::<Vec<_>>(),
    })
}

/// A detected face that lies on a region the photo already has (from XMP, drawn or named) is that region, not a new face.
fn overlaps_existing(f: &Face, regions: &[Region]) -> bool {
    regions.iter().any(|r| {
        let (w, h) = (
            (r.rect.x1.min(f64::from(f.x1)) - r.rect.x0.max(f64::from(f.x0))).max(0.0),
            (r.rect.y1.min(f64::from(f.y1)) - r.rect.y0.max(f64::from(f.y0))).max(0.0),
        );
        let inter = w * h;
        let union = (r.rect.x1 - r.rect.x0) * (r.rect.y1 - r.rect.y0) + f64::from((f.x1 - f.x0) * (f.y1 - f.y0)) - inter;
        union > 0.0 && inter / union >= EXISTING_IOU
    })
}

pub(crate) fn is_detected(r: &Region) -> bool {
    r.description.as_deref().is_some_and(|d| d.starts_with(MARK))
}

/// The photo as the faces see it: upright, uncropped, default settings, as 8-bit RGB with its size, its long edge at
/// most `edge`.
pub(crate) fn render_rgb(s: &mut Session, id: dac_catalog::PhotoId, edge: usize) -> std::result::Result<(Vec<u8>, usize, usize), String> {
    let settings = DevelopSettings::default();
    let job = s.preview_job(id, edge, edge, false, &settings).ok_or("no such photo")?;
    let image = job.run().rendered?.image;
    let rgb: Vec<u8> = image.data.iter().flat_map(|px| [px[0], px[1], px[2]]).collect();
    Ok((rgb, image.width, image.height))
}

fn detect(s: &mut Session, p: &Value) -> Result<Value> {
    let apply = p.get("apply").and_then(Value::as_bool).unwrap_or(true);
    let defaults = Options::default();
    let opts = Options {
        score: p.get("score").and_then(Value::as_f64).map_or(defaults.score, |v| v as f32),
        nms_iou: p.get("nmsIou").and_then(Value::as_f64).map_or(defaults.nms_iou, |v| v as f32),
        max_faces: p.get("maxFaces").and_then(Value::as_u64).map_or(defaults.max_faces, |v| usize::try_from(v).unwrap_or(defaults.max_faces)),
    };
    let detector = detector(s).map_err(|e| bad(C, e))?;
    let started = Instant::now();
    let (mut results, mut ops) = (Vec::new(), Vec::new());
    for id in s.targets(p) {
        let (rgb, width, height) = match render_rgb(s, id, LOOK_EDGE) {
            Ok(r) => r,
            Err(e) => {
                results.push(json!({"id": id.0, "error": e}));
                continue;
            }
        };
        let faces = match detector.detect(&rgb, width, height, &opts) {
            Ok(f) => f,
            Err(e) => {
                results.push(json!({"id": id.0, "error": e.to_string()}));
                continue;
            }
        };
        if apply && let Some(photo) = s.catalog.photo(id) {
            let mut meta = photo.meta.clone();
            meta.regions.retain(|r| !is_detected(r));
            let fresh: Vec<&Face> = faces.iter().filter(|f| !overlaps_existing(f, &meta.regions)).collect();
            for f in fresh {
                meta.regions.push(Region {
                    rect: dac_geom::Rect { x0: f64::from(f.x0), y0: f64::from(f.y0), x1: f64::from(f.x1), y1: f64::from(f.y1) },
                    kind: RegionKind::Face,
                    name: None,
                    description: Some(format!("{MARK}{LABEL}")),
                });
            }
            ops.push(Op::SetMeta { id, meta: Box::new(meta) });
        }
        results.push(json!({"id": id.0, "faces": faces.iter().map(face_json).collect::<Vec<_>>()}));
    }
    let applied = !ops.is_empty();
    if applied {
        s.commit("Detect Faces", Op::Batch { ops })?;
        // regions are not written to XMP, so a sidecar must not be rewritten for this
        s.skip_auto_write = true;
    }
    Ok(json!({"detector": LABEL, "applied": applied, "photos": results, "ms": started.elapsed().as_secs_f64() * 1000.0}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "faces.detect",
        "Detect Faces",
        ["Photo"],
        None,
        "{ids?, apply?: bool (default true), score?: 0.6, nmsIou?: 0.3, maxFaces?} → {photos: [{id, faces: [{rect, score, landmarks}]}]}. Finds faces with the YuNet detector (download it first: `faces.models.download {id: \"yunet-2023mar\"}`) and, unless `apply` is false, adds them as unnamed face regions (one undo step), replacing earlier detections but never regions from XMP or ones you named; the sidecar is not touched",
        has_selection,
        detect
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(x0: f32, y0: f32, x1: f32, y1: f32) -> Face {
        Face { x0, y0, x1, y1, score: 0.9, landmarks: [(0.0, 0.0); 5] }
    }
    fn region(x0: f64, y0: f64, x1: f64, y1: f64) -> Region {
        Region { rect: dac_geom::Rect { x0, y0, x1, y1 }, kind: RegionKind::Face, name: Some("Ann".into()), description: None }
    }

    #[test]
    fn a_detection_on_an_existing_region_is_not_a_new_face() {
        let existing = [region(0.40, 0.20, 0.60, 0.50), region(0.10, 0.10, 0.20, 0.20)];
        // the same face, a bit larger or shifted (a Lightroom box is not YuNet's box)
        assert!(overlaps_existing(&face(0.38, 0.18, 0.64, 0.54), &existing));
        assert!(overlaps_existing(&face(0.42, 0.22, 0.58, 0.48), &existing));
        // another face, a sliver of overlap, and no regions at all
        assert!(!overlaps_existing(&face(0.70, 0.20, 0.90, 0.50), &existing));
        assert!(!overlaps_existing(&face(0.59, 0.49, 0.80, 0.80), &existing));
        assert!(!overlaps_existing(&face(0.0, 0.0, 0.0, 0.0), &existing), "an empty box overlaps nothing");
        assert!(!overlaps_existing(&face(0.4, 0.2, 0.6, 0.5), &[]));
    }
}
