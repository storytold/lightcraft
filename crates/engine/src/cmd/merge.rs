//! Photo Merge commands: `merge.hdr`, `merge.panorama`, `merge.hdrPanorama`, `merge.hdrBatch`.
//!
//! Run synchronously here (CLI, MCP, control channel); the desktop UI plans the same job with
//! [`Session::plan_merge`] and runs it on a worker thread. `preview: true` merges ≤ 1024 px frames
//! and (with `previewPath`) writes the rendered preview as PNG instead of adding a photo.

use lightcraft_catalog::PhotoId;
use serde_json::{Value, json};

use super::{CommandSpec, bad, bool_or, cmd, str_param};
use crate::{Result, Session};

fn can_merge(s: &Session) -> std::result::Result<(), String> {
    if cfg!(target_arch = "wasm32") && s.media.file_bytes.is_none() {
        return Err("Photo Merge needs access to the photo files".into());
    }
    // the photo count is checked when run (an explicit `ids` parameter may name them)
    if s.catalog.photos().nth(1).is_none() { Err("the library has fewer than 2 photos".into()) } else { Ok(()) }
}

fn run(s: &mut Session, id: &str, p: &Value) -> Result<Value> {
    let (kind, finish) = crate::merge::parse(id, p)?;
    let ids: Vec<PhotoId> = s.targets(p)?;
    if !bool_or(p, "preview", false) {
        return s.merge_now(kind, finish, &ids);
    }
    let jobs = s.plan_merges(kind, finish, &ids, true)?;
    let single = jobs.len() == 1;
    let mut previews = Vec::new();
    let mut first_preview = None;
    for job in &jobs {
        let out = job.run(&|_, _| true).map_err(|e| bad(id, e))?;
        let mut info = out.info;
        if let Some(img) = &out.preview {
            info["previewSize"] = json!([img.width, img.height]);
        }
        if single {
            first_preview = out.preview;
        }
        previews.push(info);
    }
    let mut info = previews.first().cloned().unwrap_or(Value::Null);
    if single {
        if let (Some(path), Some(img)) = (str_param(p, "previewPath"), &first_preview) {
            let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default())
                .map_err(|e| bad(id, e.to_string()))?;
            s.check_write_target(path).map_err(|e| bad(id, e))?;
            crate::export::write_file(path, &png).map_err(|e| bad(id, e))?;
            info["previewPath"] = json!(path);
        }
    } else if let Some(obj) = info.as_object_mut() {
        obj.insert("previews".into(), Value::Array(previews));
    }
    Ok(info)
}

/// `merge.hdrBatch`: an HDR merge of each bracket set among the photos ([`lightcraft_catalog::Catalog::bracket_sets`]),
/// one after the other. A set that fails is reported and the others still run.
fn run_batch(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "merge.hdrBatch";
    let (kind, finish) = crate::merge::parse("merge.hdr", p)?;
    let ids: Vec<PhotoId> = s.targets(p)?;
    let sets = s.catalog.bracket_sets(&ids);
    if sets.is_empty() {
        return Err(bad(ID, crate::merge::NO_BRACKET_SETS));
    }
    let (mut merged, mut failed) = (Vec::new(), Vec::new());
    for set in sets {
        match s.merge_now(kind.clone(), finish.clone(), &set) {
            Ok(v) => merged.push(v),
            Err(e) => failed.push(json!({"photos": set.iter().map(|p| p.0).collect::<Vec<_>>(), "error": e.to_string()})),
        }
    }
    Ok(json!({"merged": merged, "failed": failed}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "merge.hdr",
            "HDR Merge",
            [],
            None,
            "{ids?, align=true, deghost=none|low|medium|high, autoSettings=true, stack=false, preview=false, showOverlay=false, previewPath?}",
            can_merge,
            |s, p| run(s, "merge.hdr", p)
        ),
        cmd!(
            "merge.panorama",
            "Panorama Merge",
            [],
            None,
            "{ids?, projection=auto|spherical|cylindrical|perspective, boundaryWarp=0..100, autoCrop=false, fillEdges=false, autoSettings=true, stack=false, maxMegapixels=full size (≤ 512, held to memory), preview=false, previewPath?}",
            can_merge,
            |s, p| run(s, "merge.panorama", p)
        ),
        cmd!(
            "merge.hdrPanorama",
            "HDR Panorama Merge",
            [],
            None,
            "{ids?, bracket=0 (auto from EXIF), align, deghost, projection, boundaryWarp, autoCrop, fillEdges, autoSettings, stack, preview, previewPath?}",
            can_merge,
            |s, p| run(s, "merge.hdrPanorama", p)
        ),
        cmd!(
            "merge.hdrBatch",
            "HDR for Each Bracket Set",
            ["Photo", "Photo Merge"],
            None,
            "{ids?, align=true, deghost=none|low|medium|high, autoSettings=true, stack=false}: an HDR merge of each bracket set among the photos (each stack, then photos taken less than a second apart at exposures ≥ ½ EV apart) → {merged: [{id, path, …}], failed: [{photos, error}]}",
            can_merge,
            run_batch
        ),
    ]
}
