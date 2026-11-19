//! Photo Merge commands: `merge.hdr`, `merge.panorama`, `merge.hdrPanorama`, `merge.hdrBatch`, `merge.focusStack`, `merge.pixelShift`.
//!
//! Run synchronously here (CLI, MCP, control channel); the desktop UI plans the same job with
//! [`Session::plan_merge`] and runs it on a worker thread. `preview: true` merges ≤ 1024 px frames
//! and (with `previewPath`) writes the rendered preview as PNG instead of adding a photo.

use lcc_catalog::PhotoId;
use serde_json::{Value, json};

use super::{CommandSpec, bad, bool_or, cmd, str_param};
use crate::{Result, Session};

fn can_merge(s: &Session) -> std::result::Result<(), String> {
    if cfg!(target_arch = "wasm32") && s.media.file_bytes.is_none() {
        return Err("Photo Merge needs access to the photo files".into());
    }
    // the photo count is checked when run (an explicit `ids` parameter may name them)
    if s.catalog.photos().nth(1).is_none() { Err("the catalog has fewer than 2 photos".into()) } else { Ok(()) }
}

fn run(s: &mut Session, id: &str, p: &Value) -> Result<Value> {
    let (kind, finish) = crate::merge::parse(id, p)?;
    let ids: Vec<PhotoId> = s.targets(p)?;
    if !bool_or(p, "preview", false) {
        return s.merge_now(kind, finish, &ids);
    }
    let job = s.plan_merge(kind, finish, &ids, true)?;
    let out = job.run(&|_, _| true).map_err(|e| bad(id, e))?;
    let mut info = out.info;
    if let (Some(path), Some(img)) = (str_param(p, "previewPath"), &out.preview) {
        let png =
            lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(img), &lcc_codecs::EncodeMeta::default()).map_err(|e| bad(id, e.to_string()))?;
        s.check_write_target(path).map_err(|e| bad(id, e))?;
        crate::export::write_file(path, &png).map_err(|e| bad(id, e))?;
        info["previewPath"] = json!(path);
    }
    if let Some(img) = &out.preview {
        info["previewSize"] = json!([img.width, img.height]);
    }
    Ok(info)
}

/// `merge.hdrBatch`: an HDR merge of each bracket set among the photos ([`lcc_catalog::Catalog::bracket_sets`]),
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

/// A merged pixel-shift photo is selected and still has frames other than its reference.
fn can_delete_sources(s: &Session) -> std::result::Result<(), String> {
    let id = s.active().ok_or("no photo selected")?;
    match s.merge_sources_to_delete(id) {
        None => Err("the photo is not a pixel-shift merge".into()),
        Some((present, _)) if present.is_empty() => Err("no merge sources other than frame 01 are left".into()),
        Some(_) => Ok(()),
    }
}

/// `merge.deleteSources`: the merged photo's frames other than its reference (frame 01) go to Recently Deleted.
fn delete_sources(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "merge.deleteSources";
    let id = match p.get("id").and_then(Value::as_u64) {
        Some(n) => PhotoId(n),
        None => s.active().ok_or_else(|| bad(ID, "no photo selected"))?,
    };
    let (present, missing) = s.merge_sources_to_delete(id).ok_or_else(|| bad(ID, "the photo is not a pixel-shift merge"))?;
    if present.is_empty() {
        return Err(bad(ID, "no merge sources other than the reference frame are left"));
    }
    let ids: Vec<u64> = present.iter().map(|p| p.0).collect();
    s.execute("photo.delete", &json!({"ids": ids}))?;
    // the merged photo and its reference frame stay, with the merged photo selected
    s.selection = crate::Selection::single(id);
    Ok(json!({"deleted": present.len(), "missing": missing, "ids": ids}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "merge.hdr",
            "HDR Merge",
            [],
            None,
            "{ids?, align=true, deghost=none|low|medium|high, autoSettings=true, stack=false, preview=false, showOverlay=false, previewPath?, layers=false, layersFormat=psd|psb, bitDepth=16|8, colorSpace=srgb|adobeRgb|displayP3|proPhoto, includeMerged=false, deghostMask=false}: layers also writes <name>-HDR-Layers.psd, one layer per photo aligned as the merge aligned it (transparent where the warp left the frame), the merged result at the bottom and the deghost map as a hidden layer on top when asked; added to the library and stacked under the DNG → {id, path, …, layers?: {id, path, bytes}}",
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
        cmd!(
            "merge.pixelShift",
            "Pixel Shift Merge",
            [],
            None,
            "{ids?, autoSettings=false, stack=true, outputDir?}: the frames of one complete Nikon pixel-shift series (16 shots; the photos' own pixel-shift numbers say whether they are one) merged on the raw mosaics into one image with full colour at twice the resolution, written next to frame 01 as <name>-PixelShift.dng (16-bit LinearRaw on the camera's own scale, carrying frame 01's colour, lens data and metadata and where it came from), added to the library and stacked on its frames. A selection that is not a complete series is refused with the reason → {id, path, shots, width, height, …}",
            can_merge,
            |s, p| run(s, "merge.pixelShift", p)
        ),
        cmd!(
            "merge.deleteSources",
            "Delete Merge Sources",
            [],
            None,
            "{id?}: the merged pixel-shift photo's frames other than frame 01 (the reference) are moved to Recently Deleted like any deleted photo; frame 01 and the merged photo stay, stacked. Frames already gone are counted → {deleted, missing, ids}",
            can_delete_sources,
            delete_sources
        ),
        cmd!(
            "merge.focusStack",
            "Focus Stack Merge",
            [],
            None,
            "{ids?, align=true, autoSettings=true, stack=false, preview=false, previewPath?}",
            can_merge,
            |s, p| run(s, "merge.focusStack", p)
        ),
    ]
}
