//! `faces.detect`: find faces with the detector that ships inside LightCraft (YuNet).
//!
//! The photo is rendered upright and uncropped with default settings (so edits, crops and rotations you have
//! made do not matter), the detector runs on it, and the faces come back as fractions of the photo's width
//! and height, the same frame face regions from XMP use. Unless `apply` is false, they are added to the photo as
//! unnamed face regions in one undoable step; earlier detections are replaced, and regions that came from XMP (or
//! that you drew or named) are never touched. Like every region edit it is catalog-only: no sidecar is written.

use std::sync::OnceLock;
use std::time::Instant;

use lightcraft_catalog::Op;
use lightcraft_develop::DevelopSettings;
use lightcraft_faces::yunet::{BUNDLED, Detector, Face, Options};
use lightcraft_meta::{Region, RegionKind};
use serde_json::{Value, json};

use super::{CommandSpec, bad, cmd, has_selection};
use crate::{Result, Session};

const C: &str = "faces.detect";
/// Regions made by this command say so in their description; only those are replaced on a new run.
const MARK: &str = "Detected by ";
const LABEL: &str = "YuNet 2023mar";
/// Long edge of the picture the detector looks at (it shrinks it to its own 640 anyway).
const LOOK_EDGE: usize = 1280;
/// How much a detection must overlap a region the photo already has to count as the same face.
const EXISTING_IOU: f64 = 0.3;

fn detector() -> std::result::Result<&'static Detector, String> {
    static DETECTOR: OnceLock<std::result::Result<Detector, String>> = OnceLock::new();
    DETECTOR.get_or_init(|| Detector::new(BUNDLED).map_err(|e| e.to_string())).as_ref().map_err(Clone::clone)
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

fn is_detected(r: &Region) -> bool {
    r.description.as_deref().is_some_and(|d| d.starts_with(MARK))
}

fn detect(s: &mut Session, p: &Value) -> Result<Value> {
    let apply = p.get("apply").and_then(Value::as_bool).unwrap_or(true);
    let defaults = Options::default();
    let opts = Options {
        score: p.get("score").and_then(Value::as_f64).map_or(defaults.score, |v| v as f32),
        nms_iou: p.get("nmsIou").and_then(Value::as_f64).map_or(defaults.nms_iou, |v| v as f32),
        max_faces: p.get("maxFaces").and_then(Value::as_u64).map_or(defaults.max_faces, |v| usize::try_from(v).unwrap_or(defaults.max_faces)),
    };
    let detector = detector().map_err(|e| bad(C, e))?;
    let started = Instant::now();
    let (mut results, mut ops) = (Vec::new(), Vec::new());
    for id in s.targets(p) {
        let settings = DevelopSettings::default();
        let Some(job) = s.preview_job(id, LOOK_EDGE, LOOK_EDGE, false, &settings) else { continue };
        let rendered = match job.run().rendered {
            Ok(r) => r.image,
            Err(e) => {
                results.push(json!({"id": id.0, "error": e}));
                continue;
            }
        };
        let rgb: Vec<u8> = rendered.data.iter().flat_map(|px| [px[0], px[1], px[2]]).collect();
        let faces = match detector.detect(&rgb, rendered.width, rendered.height, &opts) {
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
                    rect: lightcraft_geom::Rect { x0: f64::from(f.x0), y0: f64::from(f.y0), x1: f64::from(f.x1), y1: f64::from(f.y1) },
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
        "{ids?, apply?: bool (default true), score?: 0.6, nmsIou?: 0.3, maxFaces?} → {photos: [{id, faces: [{rect, score, landmarks}]}]}. Finds faces with the bundled detector and, unless `apply` is false, adds them as unnamed face regions (one undo step), replacing earlier detections but never regions from XMP or ones you named; the sidecar is not touched",
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
        Region { rect: lightcraft_geom::Rect { x0, y0, x1, y1 }, kind: RegionKind::Face, name: Some("Ann".into()), description: None }
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
