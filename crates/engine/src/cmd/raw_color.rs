//! Explicit selection of sensor colour interpretation; existing edits are never migrated implicitly.
use super::{CommandSpec, bad, cmd, has_active, str_param};
use lightcraft_develop::{RawColorMode, WhiteBalance};
use serde_json::json;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "develop.rawColor",
            "RAW Colour Processing",
            [],
            None,
            "{mode: legacy|base|matchCamera, profile?: calibration JSON|null, profilePath?: path} — changing mode resets WB to As Shot; undo restores it",
            has_active,
            |s, p| {
                let c = "develop.rawColor";
                let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
                let photo = s.catalog.photo(id).ok_or_else(|| bad(c, "no photo"))?;
                if !photo.develops_raw() {
                    return Err(bad(c, "requires a decoded RAW original"));
                }
                let mode: RawColorMode =
                    serde_json::from_value(p.get("mode").cloned().ok_or_else(|| bad(c, "missing mode"))?).map_err(|e| bad(c, e.to_string()))?;
                let mut d = (*photo.develop).clone();
                if p.get("profile").is_some() && p.get("profilePath").is_some() {
                    return Err(bad(c, "give profile or profilePath, not both"));
                }
                if let Some(path) = str_param(p, "profilePath") {
                    use std::io::Read;
                    let file = std::fs::File::open(path).map_err(|e| bad(c, e.to_string()))?;
                    let mut bytes = Vec::new();
                    file.take(131073).read_to_end(&mut bytes).map_err(|e| bad(c, e.to_string()))?;
                    if bytes.len() > 131072 {
                        return Err(bad(c, "calibration profile exceeds 128 KiB"));
                    }
                    d.raw_color.calibration = Some(serde_json::from_slice(&bytes).map_err(|e| bad(c, e.to_string()))?);
                } else if let Some(value) = p.get("profile") {
                    d.raw_color.calibration = serde_json::from_value(value.clone()).map_err(|e| bad(c, e.to_string()))?;
                }
                if let Some(profile) = &d.raw_color.calibration {
                    profile.validate().map_err(|e| bad(c, e))?;
                }
                d.raw_color.mode = mode;
                // Decode before committing: a mismatched/invalid profile is an error, not a broken edit.
                let mut candidate = photo.as_ref().clone();
                candidate.develop = std::sync::Arc::new(d.clone());
                let source = s.media.source_ref(&candidate, crate::media::SourceLevel::Thumb).load_source().map_err(|e| bad(c, e))?;
                let info = source.info_or(crate::media::source_info(&candidate));
                if d.raw_color != photo.develop.raw_color {
                    d.wb = WhiteBalance { temp: info.as_shot_temp, tint: info.as_shot_tint, ..Default::default() };
                }
                s.set_develop(id, d, "RAW Colour Processing")?;
                s.media.insert_source(id, crate::media::SourceLevel::Thumb, source);
                Ok(json!({"mode": mode, "status": info.raw_color_status, "message": info.raw_color_status.label(),
                    "relativeWB": info.relative_wb, "asShot": [info.as_shot_temp, info.as_shot_tint], "cameraMatchApplied": info.camera_match}))
            }
        ),
        cmd!(query "develop.rawColorInfo", "RAW Colour Information", [], None, "{} — decode and inspect the active colour model", has_active, |s, _| {
            let c = "develop.rawColorInfo";
            let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
            s.source_now(id, crate::media::SourceLevel::Thumb).map_err(|e| bad(c, e))?;
            let info = s.source_info(id);
            Ok(json!({"status": info.raw_color_status, "message": info.raw_color_status.label(), "relativeWB": info.relative_wb,
                "asShot": [info.as_shot_temp, info.as_shot_tint], "cameraMatchApplied": info.camera_match, "model": info.camera_wb.map(|c| c.model)}))
        }),
    ]
}
