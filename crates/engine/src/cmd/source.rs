//! `photo.sourceReport`: how the app reads photos' originals before any edit ([`crate::source_report`]).

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd};
use crate::{Result, Session};

/// Most photos one `photo.sourceReport` decodes: each is read and decoded again before the command returns, a
/// second or so for a large raw.
pub const MAX_PHOTOS: usize = 10;

fn source_report(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "photo.sourceReport";
    let mut ids = s.targets(p)?;
    if ids.is_empty() {
        ids.extend(s.selection.active);
    }
    if ids.is_empty() {
        return Err(bad(C, "no photo"));
    }
    if ids.len() > MAX_PHOTOS {
        return Err(bad(C, format!("at most {MAX_PHOTOS} photos at a time")));
    }
    let read = s.media.file_bytes.clone();
    let photos: Vec<Value> = ids
        .into_iter()
        .map(|id| {
            let Some(ph) = s.catalog.photo(id) else { return json!({"id": id.0, "error": "no such photo"}) };
            let lcc_catalog::Source::File { path } = &ph.source else { return json!({"id": id.0, "error": "a demo photo has no file"}) };
            let bytes = match &read {
                Some(read) => read(path),
                None => std::fs::read(path).map_err(|e| format!("{path}: {e}")),
            };
            match bytes.and_then(|b| crate::source_report::report(&b, &ph.develop.profile.camera)) {
                Ok(r) => json!({"id": id.0, "path": path, "report": r}),
                Err(e) => json!({"id": id.0, "path": path, "error": e}),
            }
        })
        .collect();
    Ok(json!({"photos": photos}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        query "photo.sourceReport",
        "Source Report",
        [],
        None,
        "{ids? (default: the active photo; at most 10)} → {photos: [{id, path, report: {raw, format, make, model, decode: {kind: sensor | embeddedPreview | image, reason?}, colour: {kind: file | measuredDatabase | cameraJpeg | cameraProfile | database | spectralStandIn | neutralFallback, make?, model?, provenance?, cameraProfile?}, cameraProfile: {name, model, source: dcp | calibrated, look: cal:… | null} | null, matrixIsFallback, look: {hueSat} | null, tone: none | cameraJpeg | monochromeJpeg | dngToneFit | profilePooled | default | canonMedian | dngProfileCurve, defaultExposure: EV added for a DNG profile without a tone curve (0 if none), whiteBalance: relative | absolute, dngProfile: {toneCurve, lookTable, gainTableMap}, dynamicRangeOptimizer, lens: {source: embedded | chosen | found | none, profile?, warp, warpStages, vignette}} | error}]}: how each original is read before any edit: sensor data or the embedded preview, where its starting colour and tone come from, and the lens correction that applies by itself. Read-only; decodes each file again",
        always,
        source_report
    )]
}
