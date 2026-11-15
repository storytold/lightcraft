//! Convert to DNG: a raw photo's original is re-encoded as a lossless DNG (its develop settings
//! embedded as XMP) next to it, and the photo is relinked to the DNG. The original stays on
//! disk; undo relinks the photo to it.

use std::path::Path;

use lcc_catalog::{MediaKind, Op, Source};
use serde_json::{Value, json};

use super::{CommandSpec, bad, cmd, has_active, has_selection};
use crate::{Result, Session};

/// `<stem>.dng`, then `<stem>-2.dng`, `<stem>-3.dng`… next to `path`.
fn dng_names(path: &str) -> impl Iterator<Item = std::path::PathBuf> {
    let p = Path::new(path);
    let dir = p.parent().unwrap_or(Path::new("")).to_path_buf();
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "photo".into());
    (1u64..).map(move |n| if n == 1 { dir.join(format!("{stem}.dng")) } else { dir.join(format!("{stem}-{n}.dng")) })
}

/// Write the DNG for raw file `path` (develop settings in `packet`) next to it under a free name;
/// returns the new path. An empty `packet` writes no XMP.
///
/// The DNG is checked before anyone relies on it (Convert to DNG relinks the photo to it, Copy as
/// DNG deletes the raw copy): the encoded file must decode to the same raw data, and it is
/// written to a temp file, synced, read back identical and only then given its name — never
/// replacing an existing file. On any failure no DNG is left behind and the raw stays in use.
pub(crate) fn write_dng_for(s: &Session, path: &str, packet: String) -> std::result::Result<String, String> {
    write_dng_with(s.media.file_bytes.as_ref(), path, packet)
}

/// [`write_dng_for`] without the session (imports convert on a worker thread).
pub(crate) fn write_dng_with(file_bytes: Option<&crate::merge::ByteReader>, path: &str, packet: String) -> std::result::Result<String, String> {
    let bytes = match file_bytes {
        Some(r) => r(path)?,
        None => std::fs::read(path).map_err(|e| format!("{path}: {e}"))?,
    };
    let mut raw = lcc_raw::decode(&bytes).map_err(|e| format!("{path}: {e}"))?;
    crate::camera_preview::with_starting_colour(&mut raw, &bytes);
    drop(bytes);
    let dng = lcc_raw::write_dng(&raw, &lcc_raw::DngWriteOptions { xmp: (!packet.is_empty()).then_some(packet), ..Default::default() })
        .map_err(|e| e.to_string())?;
    let back = lcc_raw::decode(&dng).map_err(|e| format!("{path}: the DNG written for it doesn't decode ({e}); the raw is kept"))?;
    let same = match (&back.data, &raw.data) {
        (lcc_raw::RawData::U16(a), lcc_raw::RawData::U16(b)) => a == b,
        (lcc_raw::RawData::F32(a), lcc_raw::RawData::F32(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits()),
        _ => false,
    };
    if (back.width, back.height, back.cpp) != (raw.width, raw.height, raw.cpp) || !same {
        return Err(format!("{path}: the DNG written for it doesn't hold the same raw data; the raw is kept"));
    }
    drop((raw, back));
    let out = lcc_catalog::safe_file::write_new_unique(&mut dng_names(path), &dng)
        .map_err(|e| format!("{path}: could not write its DNG ({e}); the raw is kept"))?;
    Ok(out.to_string_lossy().to_string())
}

fn convert(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "photo.convertToDng";
    let ids = s.targets(p)?;
    let mut ops = Vec::new();
    let mut converted = Vec::new();
    let mut skipped = Vec::new();
    for id in ids {
        let Some(ph) = s.catalog.photo(id).cloned() else { continue };
        let Source::File { path } = &ph.source else {
            skipped.push(json!([id.0, "not a file"]));
            continue;
        };
        if ph.kind != MediaKind::Raw || ph.format.eq_ignore_ascii_case("DNG") || ph.copy_of.is_some() {
            skipped.push(json!([
                id.0,
                if ph.kind != MediaKind::Raw {
                    "not a raw photo"
                } else if ph.copy_of.is_some() {
                    "a virtual copy"
                } else {
                    "already a DNG"
                }
            ]));
            continue;
        }
        let packet = crate::sidecar::sidecar_packet(&ph, &s.catalog);
        match write_dng_for(s, path, packet) {
            Ok(out) => {
                let file_name = Path::new(&out).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let file_size = std::fs::metadata(&out).map_err(|e| bad(C, format!("{out}: could not read converted file size ({e})")))?.len();
                // virtual copies of this photo follow it to the DNG
                for c in s.catalog.photos().filter(|c| c.copy_of == Some(id)) {
                    ops.push(Op::Relink {
                        id: c.id,
                        file_name: file_name.clone(),
                        source: Source::File { path: out.clone() },
                        format: Some("DNG".into()),
                    });
                    ops.push(Op::SetContent {
                        id: c.id,
                        width: c.width,
                        height: c.height,
                        file_size,
                        content_hash: c.content_hash.clone(),
                        preview_only: c.preview_only.clone(),
                        preview_checked: c.preview_checked.clone(),
                    });
                }
                ops.push(Op::Relink { id, file_name, source: Source::File { path: out.clone() }, format: Some("DNG".into()) });
                ops.push(Op::SetContent {
                    id,
                    width: ph.width,
                    height: ph.height,
                    file_size,
                    content_hash: ph.content_hash.clone(),
                    preview_only: ph.preview_only.clone(),
                    preview_checked: ph.preview_checked.clone(),
                });
                converted.push(json!({"id": id.0, "path": out, "original": path}));
            }
            Err(e) => skipped.push(json!([id.0, e])),
        }
    }
    if !ops.is_empty() {
        let n = converted.len();
        s.commit(&format!("Convert {n} Photo{} to DNG", if n == 1 { "" } else { "s" }), Op::Batch { ops }).map_err(|e| bad(C, e.to_string()))?;
    }
    Ok(json!({"converted": converted, "skipped": skipped}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "photo.convertToDng",
        "Convert to DNG",
        ["Photo", "File"],
        None,
        "{ids?}: write each raw photo as a lossless DNG next to it (settings embedded) and relink the photo to it; originals are kept → {converted: [{id, path, original}], skipped}",
        has_selection,
        convert
    )]
}

/// Edit in an external editor, the engine half: render the photo with its edits as a 16-bit
/// TIFF (or PSD, or 8-bit) next to the original (`<name>-Edit.tif`, never overwriting), add it to
/// the library and stack it on top of the original. The host opens the file in the editor.
fn edit_external(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "photo.editExternal";
    let mut ids = s.targets(p)?;
    ids.dedup();
    if ids.is_empty() {
        return Err(bad(C, "no active photo"));
    }
    if ids.len() > MAX_EDIT_COPIES {
        return Err(bad(C, format!("{} photos selected: edit at most {MAX_EDIT_COPIES} at a time", ids.len())));
    }
    let space = match super::str_param(p, "colorSpace").unwrap_or("adobeRgb") {
        "srgb" => lcc_pipeline::OutputSpace::Srgb,
        "displayP3" => lcc_pipeline::OutputSpace::DisplayP3,
        "prophoto" | "proPhoto" => lcc_pipeline::OutputSpace::ProPhoto,
        _ => lcc_pipeline::OutputSpace::AdobeRgb,
    };
    let format = match super::str_param(p, "format").unwrap_or("tiff") {
        "tiff" | "tif" => crate::export::ExportFormat::Tiff,
        "psd" => crate::export::ExportFormat::Psd,
        f => return Err(bad(C, format!("format {f:?}: use tiff or psd"))),
    };
    let bit_depth = match p.get("bitDepth").and_then(Value::as_u64).unwrap_or(16) {
        8 => 8,
        16 => 16,
        d => return Err(bad(C, format!("bitDepth {d}: use 8 or 16"))),
    };
    let opts = crate::export::ExportOptions {
        format,
        bit_depth: Some(bit_depth),
        color_space: space,
        naming: "{name}-Edit".into(),
        conflict: crate::export::Conflict::Unique,
        ..Default::default()
    };
    let mut copies = Vec::new();
    for id in ids {
        copies.push(edit_copy(s, p, id, &opts, C)?);
    }
    let new: Vec<lcc_catalog::PhotoId> = copies.iter().filter_map(|c| c["id"].as_u64()).map(lcc_catalog::PhotoId).collect();
    s.selection = crate::Selection { active: new.first().copied(), ids: new };
    let first = copies.first().cloned().unwrap_or(Value::Null);
    Ok(json!({"path": first["path"], "id": first["id"], "original": first["original"], "copies": copies}))
}

/// Most photos one Edit In renders (each becomes a full-size file on disk and a library photo).
const MAX_EDIT_COPIES: usize = 50;

/// One photo's edit copy for [`edit_external`]: rendered with `opts` next to its original (a demo
/// photo into `dir`), added to the library and stacked on top of the original, expanded.
fn edit_copy(s: &mut Session, p: &Value, id: lcc_catalog::PhotoId, opts: &crate::export::ExportOptions, cmd: &'static str) -> Result<Value> {
    let ph = s.catalog.photo(id).cloned().ok_or_else(|| bad(cmd, format!("no photo {}", id.0)))?;
    let dir = match &ph.source {
        Source::File { path } => Path::new(path).parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default(),
        Source::Demo { .. } => match super::str_param(p, "dir") {
            Some(d) => d.to_string(),
            None => return Err(bad(cmd, "a generated demo photo has no folder: give `dir`")),
        },
    };
    let mut written = Vec::new();
    let mut write = |path: &str, bytes: &[u8]| -> std::result::Result<(), String> {
        // durable: the TIFF becomes a library photo the external editor then changes
        crate::export::write_file_durable(path, bytes)?;
        written.push(path.to_string());
        Ok(())
    };
    let to = crate::export::Destination { dir: dir.clone(), exact: None };
    crate::export::export_batch(s, &[id], opts, &to, &mut write, &|path| Path::new(path).exists()).map_err(|e| bad(cmd, e))?;
    let ext = format!(".{}", opts.format.extension());
    let out = written.into_iter().find(|w| w.ends_with(&ext)).ok_or_else(|| bad(cmd, "nothing was written"))?;
    let r = s.execute("library.import", &json!({"paths": [out]}))?;
    let Some(new) = r["imported"].get(0).and_then(Value::as_u64) else {
        // the render is the same bytes as a copy the library already has (the last one was never changed): that copy
        // is the one to open, and the file just written is an orphan nobody would ever see (upstream #3617), unless
        // import restored a trashed copy by relinking it to this very file
        let existing = r["restored"]
            .get(0)
            .and_then(Value::as_u64)
            .or_else(|| r["duplicates"].get(0).and_then(|d| d["existing"].as_u64()))
            .and_then(|i| s.catalog.photo(lcc_catalog::PhotoId(i)))
            .filter(|p| !p.deleted)
            .and_then(|p| match &p.source {
                // only a copy this command could have made (next to the original, named `<name>-Edit…`: the same
                // bytes alone don't make another photo this one's copy) and whose file is still there
                Source::File { path } if is_edit_copy_name(&ph.file_name, &dir, path) && Path::new(path).is_file() => Some((p.id.0, path.clone())),
                _ => None,
            });
        if existing.as_ref().is_none_or(|(_, path)| *path != out) {
            let _ = std::fs::remove_file(&out);
        }
        return match existing {
            Some((copy, path)) => {
                // as for a new copy, it ends on top of the original's stack and the stack is open, so both show: a
                // restored copy left its stack when it was deleted, and one still in it may be under the original or collapsed
                let shown =
                    s.catalog.stack_of(lcc_catalog::PhotoId(copy)).is_some_and(|st| st.photos.contains(&id) && st.top().0 == copy && !st.collapsed);
                if !shown {
                    stack_on_top(s, copy, id.0);
                }
                Ok(json!({"path": path, "id": copy, "original": id.0}))
            }
            None => Err(bad(cmd, "the edit copy could not be added")),
        };
    };
    stack_on_top(s, new, id.0);
    Ok(json!({"path": out, "id": new, "original": id.0}))
}

/// Whether `copy` is where and as `edit_copy` names the copy of the photo called `original_name` in `dir`: in `dir`, with a
/// file name that starts with the original's name and `-Edit` (a `-2` for a taken name follows it).
fn is_edit_copy_name(original_name: &str, dir: &str, copy: &str) -> bool {
    let same = |a: &str, b: &str| a.replace('\\', "/").trim_end_matches('/') == b.replace('\\', "/").trim_end_matches('/');
    let copy = Path::new(copy);
    let stem = Path::new(original_name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let copy_stem = copy.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    !stem.is_empty() && copy.parent().is_some_and(|p| same(&p.to_string_lossy(), dir)) && copy_stem.starts_with(&format!("{stem}-Edit"))
}

/// Stack an edit copy on top of its original, expanded so both show.
fn stack_on_top(s: &mut Session, copy: u64, original: u64) {
    let _ = s.execute("stack.group", &json!({"ids": [copy, original], "top": copy, "collapsed": false}));
}

/// The op that brings a photo up to date with what its file is now (`info`, a fresh probe), or
/// `None` when nothing changed. Covers a raw that became decodable (or stopped being: a different
/// file relinked), i.e. a change of [`lcc_catalog::Photo::preview_only`], and a
/// preview-only photo this version hasn't checked yet: its stamp is refreshed so it isn't probed
/// again (see [`lcc_catalog::Photo::preview_checked`]).
pub(crate) fn content_op(id: lcc_catalog::PhotoId, ph: &lcc_catalog::Photo, info: crate::media::ProbeInfo) -> Option<Op> {
    let preview_checked = lcc_catalog::preview_stamp(&info.preview_only);
    let same = info.content_hash == ph.content_hash
        && info.file_size == ph.file_size
        && (info.width, info.height) == (ph.width, ph.height)
        && info.preview_only == ph.preview_only
        && preview_checked == ph.preview_checked;
    (!same).then_some(Op::SetContent {
        id,
        width: info.width,
        height: info.height,
        file_size: info.file_size,
        content_hash: info.content_hash,
        preview_only: info.preview_only,
        preview_checked,
    })
}

/// Ops that fill a photo's empty camera fields (camera, lens, exposure, GPS, capture time) from a fresh probe.
pub(crate) fn fill_missing_meta(id: lcc_catalog::PhotoId, ph: &lcc_catalog::Photo, info: &crate::media::ProbeInfo) -> Vec<Op> {
    let (mut m, src) = (ph.meta.clone(), &info.meta);
    let before = m.clone();
    if m.camera.is_empty() {
        m.camera = src.camera.clone();
    }
    if m.camera_make.is_empty() {
        m.camera_make = src.camera_make.clone();
    }
    if m.camera_model.is_empty() {
        m.camera_model = src.camera_model.clone();
    }
    // A catalog from before Make and Model were kept separately has a label only: once the parts
    // are known, re-derive the label so "Canon Canon IXUS 185" becomes "Canon IXUS 185" (upstream #1618).
    if (before.camera_make.is_empty() || before.camera_model.is_empty()) && (!m.camera_make.is_empty() || !m.camera_model.is_empty()) {
        m.camera = lcc_catalog::camera_label(&m.camera_make, &m.camera_model);
    }
    if m.lens.is_empty() {
        m.lens = src.lens.clone();
    }
    if m.lens_make.is_empty() {
        m.lens_make = src.lens_make.clone();
    }
    if m.shutter.is_empty() {
        m.shutter = src.shutter.clone();
        m.exposure_time = src.exposure_time;
    } else {
        repair_legacy_shutter(&mut m, src);
    }
    m.focal_mm = m.focal_mm.or(src.focal_mm);
    m.aperture = m.aperture.or(src.aperture);
    m.iso = m.iso.or(src.iso);
    m.flash = m.flash.or(src.flash);
    m.exposure_bias = m.exposure_bias.or(src.exposure_bias);
    m.gps = m.gps.or(src.gps);
    // the camera's focus point: photos imported before it was read get it here
    if m.focus.is_none() {
        m.focus = src.focus.clone();
    }
    let mut ops = Vec::new();
    if m != before {
        ops.push(Op::SetMeta { id, meta: Box::new(m) });
    }
    // a time the photo has only as import's file-date guess ([`lcc_catalog::Photo::captured_fallback`]) gives way to the
    // capture time the file states now (written by an editor after import); a stated or edited time is kept
    // (upstream #3137, without its catalog field)
    let guess = ph.captured.is_some() && ph.captured_stated().is_none();
    if (ph.captured.is_none() || guess) && info.captured.is_some() && info.captured != ph.captured {
        ops.push(Op::SetCaptured { id, captured: info.captured.clone() });
    }
    ops
}

/// Repair only the shutter data an older import wrote (a third-stop time rounded, no exact time kept), keeping a
/// shutter speed set by hand (upstream #1692). `true` when `m` changed.
fn repair_legacy_shutter(m: &mut lcc_catalog::Meta, src: &lcc_catalog::Meta) -> bool {
    if m.exposure_time.is_none()
        && let Some(t) = src.exposure_time
        && (m.shutter.trim() == src.shutter || legacy_shutter_texts(t).iter().flatten().any(|old| m.shutter.trim() == old))
    {
        m.shutter = src.shutter.clone();
        m.exposure_time = Some(t);
        true
    } else {
        false
    }
}

/// The shutter texts imports wrote before the exact exposure time was kept, only for recognising them again: the
/// first import's whole seconds or `1/n` with `n` rounded, and the tenth-of-a-second wording that followed it
/// (`0.4`, `1.3″`, issue #1619).
fn legacy_shutter_texts(t: f64) -> [Option<String>; 2] {
    if !(t.is_finite() && t > 0.0) {
        return [None, None];
    }
    let first = if t >= 1.0 { format!("{t:.0}") } else { format!("1/{:.0}", 1.0 / t) };
    let tenth = if t < 0.3 {
        format!("1/{}", (1.0 / t).round())
    } else if t < 1.0 {
        format!("{}", (t * 10.0).round() / 10.0)
    } else {
        format!("{}\u{2033}", (t * 10.0).round() / 10.0)
    };
    [Some(first), Some(tenth)]
}

/// Ops that give a photo the lens data its file carries now (`info`, a fresh probe), or none when
/// it already has them. A catalog from before the lens data was read at import has none, so such a
/// photo's correction stayed off. When the photo gains lens data that import would have turned on
/// ([`lcc_catalog::Photo::lens_correction_built_in`]) and its develop settings are still what
/// import gave it, the correction is turned on as import would; an edited photo keeps its settings.
pub(crate) fn lens_ops(id: lcc_catalog::PhotoId, ph: &lcc_catalog::Photo, info: &crate::media::ProbeInfo) -> Vec<Op> {
    if info.embedded_lens == ph.embedded_lens.as_deref().copied() {
        return Vec::new();
    }
    let mut ops = vec![Op::SetEmbeddedLens { id, lens: info.embedded_lens.map(Box::new) }];
    // (a photo with a default-preset look keeps it: that look is its import state)
    if ph.embedded_lens.is_none()
        && info.embedded_lens.is_some()
        && ph.import_look.is_none()
        && !ph.is_edited()
        && !ph.develop.optics.lens_profile
        && lcc_catalog::lens_built_in_format(&ph.format)
    {
        let mut d = (*ph.develop).clone();
        d.optics.lens_profile = true;
        ops.push(Op::SetDevelop { id, settings: std::sync::Arc::new(d), label: "Lens Corrections".into(), edited: ph.edited.clone() });
    }
    ops
}

/// The op that gives a raw the as-shot white balance its file reads now (`info`, a fresh probe), or none when the
/// catalog already has it or the photo is not a raw edited in Kelvin. A raw imported before its camera's matrices
/// gave it a Kelvin scale keeps the relative scale's 6500 / 0 until a decode or Reload reads the file (issue #730).
pub(crate) fn as_shot_wb_op(id: lcc_catalog::PhotoId, ph: &lcc_catalog::Photo, info: &crate::media::ProbeInfo) -> Option<Op> {
    let read = info.as_shot_wb.filter(|(t, tint)| t.is_finite() && tint.is_finite())?;
    (ph.develops_raw() && !ph.relative_wb() && ph.as_shot_wb != Some(read)).then_some(Op::SetAsShotWb { id, wb: Some(read) })
}

/// The ops that bring photo `id` and its virtual copies up to date with what their file is now
/// (`info`, a fresh probe), appended to `ops`; the photos they change are appended to `changed`.
/// Reload and the preview-only recheck (`library.recheckPreviewOnly`) share it.
pub(crate) fn reload_ops(
    cat: &lcc_catalog::Catalog,
    id: lcc_catalog::PhotoId,
    info: &crate::media::ProbeInfo,
    ops: &mut Vec<Op>,
    changed: &mut Vec<lcc_catalog::PhotoId>,
) {
    let Some(ph) = cat.photo(id) else { return };
    // camera fields the catalog lacks (e.g. a raw imported before its format was read) are filled in;
    // nothing already set is overwritten
    let meta_ops = fill_missing_meta(id, ph, info);
    let mut mine: Vec<Op> =
        content_op(id, ph, info.clone()).into_iter().chain(meta_ops).chain(lens_ops(id, ph, info)).chain(as_shot_wb_op(id, ph, info)).collect();
    if !mine.is_empty() {
        ops.append(&mut mine);
        changed.push(id);
    }
    // A selected copy refreshes its same-file family too, but only the explicit target gets missing descriptive
    // metadata above. Relinked copies keep their own file facts.
    for c in cat.photos().filter(|c| c.id != id && shares_reload_source(c, ph)) {
        // Copies may have independently edited metadata: repair only a recognisable legacy shutter value, not every
        // missing field on the copy (upstream #1692).
        let mut meta = c.meta.clone();
        let shutter_op = repair_legacy_shutter(&mut meta, &info.meta).then(|| Op::SetMeta { id: c.id, meta: Box::new(meta) });
        let theirs: Vec<Op> = content_op(c.id, c, info.clone())
            .into_iter()
            .chain(shutter_op)
            .chain(lens_ops(c.id, c, info))
            .chain(as_shot_wb_op(c.id, c, info))
            .collect();
        if !theirs.is_empty() {
            ops.extend(theirs);
            changed.push(c.id);
        }
    }
}

/// Copies share probe results only while both the family and the actual file agree.
pub(crate) fn shares_reload_source(a: &lcc_catalog::Photo, b: &lcc_catalog::Photo) -> bool {
    a.copy_of.unwrap_or(a.id) == b.copy_of.unwrap_or(b.id)
        && matches!((&a.source, &b.source), (Source::File { path: a }, Source::File { path: b }) if a == b)
}

/// Re-read photos whose files changed on disk (an external editor saved them): new size,
/// dimensions and content hash, cached sources dropped. The app runs the same steps
/// ([`crate::cmd::reload`]) with the files read on a worker thread. → {reloaded: [ids]}
fn reload(s: &mut Session, p: &Value) -> Result<Value> {
    use crate::cmd::reload as r;
    let items = r::plan(s, p)?;
    let probe = r::file_probe(s);
    let results = r::probe(probe.as_ref(), &items, &std::sync::atomic::AtomicBool::new(false), &|_| {});
    let reloaded = r::apply(s, items, results)?;
    Ok(r::to_json(&reloaded))
}

/// Duplicate: copy the file next to itself (`<name>-copy`, never overwriting) and add it with the
/// same settings, metadata, rating, flag, label and albums. → {ids}
fn duplicate(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "photo.duplicate";
    let mut made = Vec::new();
    for id in s.targets(p)? {
        let Some(ph) = s.catalog.photo(id).map(|p| (**p).clone()) else { continue };
        let Source::File { path } = &ph.source else { return Err(bad(C, "a generated demo photo has no file to duplicate")) };
        let src = Path::new(path);
        let stem = src.file_stem().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| "photo".into());
        let ext = src.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        let dir = src.parent().unwrap_or(Path::new(""));
        let mut dst = dir.join(format!("{stem}-copy{ext}"));
        let mut n = 2;
        while dst.exists() {
            dst = dir.join(format!("{stem}-copy-{n}{ext}"));
            n += 1;
        }
        std::fs::copy(src, &dst).map_err(|e| bad(C, format!("{path}: {e}")))?;
        let new = s.catalog.alloc_photo_id();
        let mut q = ph.clone();
        q.id = new;
        q.source = Source::File { path: dst.to_string_lossy().to_string() };
        q.file_name = dst.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
        // the same bytes, but its own photo: no duplicate-content clash, no shared history
        q.content_hash = q.content_hash.map(|h| format!("{h}:dup{}", new.0));
        q.copy_of = None;
        q.copy_name = None;
        q.history.clear();
        q.versions.retain(|v| !v.auto);
        let mut ops = vec![Op::AddPhoto { photo: Box::new(q) }];
        for a in s.catalog.albums().filter(|a| !a.is_smart() && !a.folder && a.photos.contains(&id)) {
            let mut photos = a.photos.clone();
            photos.push(new);
            ops.push(Op::SetAlbumPhotos { id: a.id, photos });
        }
        s.commit("Duplicate", Op::Batch { ops })?;
        made.push(new);
    }
    s.merge_undo(made.len(), "Duplicate");
    if let Some(last) = made.last() {
        s.selection = crate::Selection { ids: made.clone(), active: Some(*last) };
    }
    Ok(json!({"ids": made.iter().map(|i| i.0).collect::<Vec<_>>()}))
}

/// Render to DNG (Lightroom Classic 15.5): see [`crate::render_dng`]. Here all in one go; more
/// than one photo shows in the activity stack and can be cancelled between photos (the copies made
/// until then stay). The desktop app runs the renders on a worker instead.
fn render_to_dng(s: &mut Session, p: &Value) -> Result<Value> {
    let job = crate::render_dng::prepare(s, p)?;
    let n = job.len();
    let task = (n > 1).then(|| s.activity.start("renderDng", "Rendering to DNG", crate::activity::Cancel::Yes));
    let (done, cancelled) = job.run(&mut |path, bytes| crate::export::write_file_durable(path, bytes), &mut |i| {
        let Some(t) = &task else { return true };
        t.progress(i as u64, n as u64);
        !t.is_cancelled()
    });
    drop(task);
    crate::render_dng::finish(s, done, cancelled)
}

/// Enabled when a selected photo is a DNG file (asked every frame: only the selection is looked at).
fn has_dng_selection(s: &Session) -> std::result::Result<(), String> {
    let ids = if s.selection.ids.is_empty() { s.selection.active.iter().copied().collect::<Vec<_>>() } else { s.selection.ids.clone() };
    let any = ids.iter().filter_map(|id| s.catalog.photo(*id)).any(|p| matches!(&p.source, Source::File { path } if crate::dng_update::is_dng(path)));
    if any { Ok(()) } else { Err("no DNG selected".into()) }
}

/// Metadata ▸ Update DNG Preview & Metadata: see [`crate::dng_update`]. Here all in one go; the
/// desktop app runs the rewrites on a worker instead.
fn update_dng_preview(s: &mut Session, p: &Value) -> Result<Value> {
    crate::dng_update::run_command(s, p)
}

/// Read and decode the raw file of library photo `id`; returns its path, the raw and the file bytes.
fn decode_photo(s: &Session, id: lcc_catalog::PhotoId) -> std::result::Result<(String, lcc_raw::RawImage, Vec<u8>), String> {
    let ph = s.catalog.photo(id).ok_or_else(|| format!("no photo {}", id.0))?;
    let Source::File { path } = &ph.source else { return Err(format!("photo {} is not a file", id.0)) };
    if !matches!(ph.kind, MediaKind::Raw | MediaKind::Image) {
        return Err(format!("{}: not a raw photo", ph.file_name));
    }
    let bytes = match s.media.file_bytes.as_ref() {
        Some(r) => r(path)?,
        None => std::fs::read(path).map_err(|e| format!("{path}: {e}"))?,
    };
    let raw = lcc_raw::decode(&bytes).map_err(|e| format!("{path}: {e}"))?;
    Ok((path.clone(), raw, bytes))
}

/// Flat-Field Correction: fit a gain map from the reference raw and write each target as `<name>-FlatField.dng`
/// with it in `OpcodeList2`, imported and stacked on its original.
fn flat_field(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "photo.flatField";
    let reference = match p.get("reference") {
        None | Some(Value::Null) => s.active().ok_or_else(|| bad(C, "no flat-field reference: make it the active photo or give `reference`"))?,
        Some(v) => lcc_catalog::PhotoId(v.as_u64().ok_or_else(|| bad(C, "`reference` must be a photo id"))?),
    };
    let ids: Vec<_> = s.targets(p)?.into_iter().filter(|&i| i != reference).collect();
    if ids.is_empty() {
        return Err(bad(C, "select the photos to correct besides the flat-field reference"));
    }
    let (_, ref_raw, _) = decode_photo(s, reference).map_err(|e| bad(C, format!("flat-field reference: {e}")))?;
    let ff = lcc_raw::flatfield::fit(&ref_raw).map_err(|e| bad(C, e.to_string()))?;
    drop(ref_raw);
    let n = ids.len();
    let task = s.activity.start("flatField", "Flat-field correction", crate::activity::Cancel::Yes);
    let mut copies = Vec::new();
    let mut failed = Vec::new();
    let mut cancelled = false;
    for (i, id) in ids.into_iter().enumerate() {
        task.progress(i as u64, n as u64);
        if task.is_cancelled() {
            cancelled = true;
            break;
        }
        let made = (|| -> std::result::Result<String, String> {
            let (path, mut raw, bytes) = decode_photo(s, id)?;
            crate::camera_preview::with_starting_colour(&mut raw, &bytes);
            drop(bytes);
            lcc_raw::flatfield::apply(&ff, &mut raw).map_err(|e| format!("{path}: {e}"))?;
            let dng = lcc_raw::write_dng(&raw, &Default::default()).map_err(|e| e.to_string())?;
            lcc_raw::decode(&dng).map_err(|e| format!("{path}: the corrected DNG doesn't decode ({e})"))?;
            let pp = Path::new(&path);
            let dir = pp.parent().unwrap_or(Path::new("")).to_path_buf();
            let stem = pp.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "photo".into());
            let mut names =
                (1u64..).map(|k| if k == 1 { dir.join(format!("{stem}-FlatField.dng")) } else { dir.join(format!("{stem}-FlatField-{k}.dng")) });
            let out = lcc_catalog::safe_file::write_new_unique(&mut names, &dng).map_err(|e| format!("{path}: could not write the DNG ({e})"))?;
            Ok(out.to_string_lossy().to_string())
        })();
        match made {
            Ok(out) => {
                let r = s.execute("library.import", &json!({"paths": [out]}))?;
                match r["imported"].get(0).and_then(Value::as_u64) {
                    Some(new) => {
                        stack_on_top(s, new, id.0);
                        copies.push(json!({"path": out, "id": new, "original": id.0}));
                    }
                    None => failed.push(json!({"id": id.0, "error": format!("{out}: written but could not be imported")})),
                }
            }
            Err(e) => failed.push(json!({"id": id.0, "error": e})),
        }
    }
    drop(task);
    Ok(json!({"copies": copies, "failed": failed, "cancelled": cancelled}))
}

pub fn edit_specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "photo.flatField",
                "Flat-Field Correction…",
                ["Library"],
                None,
                "{reference?: id (default: the active photo), ids?: the photos to correct (default: the selection without the reference)}: fit a smooth per-colour gain map from the reference (a raw shot through a diffuser with the same lens and aperture) and write each target as `<name>-FlatField.dng` next to it with that GainMap in OpcodeList2, imported and stacked on its original; refuses targets of another sensor size or CFA layout; shows a cancellable activity row → {copies: [{path, id, original}], failed: [{id, error}], cancelled}",
                has_selection,
                flat_field
            )
        },
        cmd!(
            "photo.duplicate",
            "Duplicate",
            ["Photo", "File"],
            None,
            "{ids?}: copy each photo's file next to it (`-copy`) and add it with the same settings, metadata and albums (a real file, unlike a virtual copy) → {ids}",
            has_selection,
            duplicate
        ),
        cmd!(
            "photo.reload",
            "Reload from Disk",
            ["Photo", "File"],
            None,
            "{ids?}: re-read photos whose files changed on disk (e.g. saved by an external editor), or raws shown from their embedded preview that can be decoded now; camera fields the catalog lacks are filled in, and so is the lens data the file carries (an untouched photo then gets its lens correction turned on as import would; an edited one keeps its settings); in the app the files are read in the background (activity row `reload`, cancellable) and the reply is {background: true, count} unless `wait: true` → {reloaded}",
            has_selection,
            reload
        ),
        cmd!(
            "photo.editExternal",
            "Edit Copy for External Editor",
            [],
            None,
            "{ids?, colorSpace?: adobeRgb (default) | proPhoto | displayP3 | srgb, format?: tiff (default) | psd, bitDepth?: 16 (default) | 8, dir?}: render each selected photo (or `ids`, at most 50) with its edits as `<name>-Edit.tif` (or `.psd`, flat, which PhotoCraft saves back to in place) next to it, add each stacked on its original and select the copies → {path, id, original (the first copy), copies: [{path, id, original}]} (the app then opens them in the external editor)",
            has_active,
            edit_external
        ),
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "photo.renderToDng",
                "Render to DNG",
                ["Photo", "File"],
                None,
                "{ids?, colorSpace?: proPhoto (default) | rec2020 | adobeRgb | displayP3 | srgb, dir?}: render each selected photo (or `ids`, at most 500) with its edits as a linear 16-bit float DNG `<name>-Rendered.dng` next to it (a demo photo into `dir`), add each stacked on its original and select them; a batch shows in the activity stack and can be cancelled between photos → {copies: [{path, id, original}], failed: [{id, error}], cancelled}",
                has_selection,
                render_to_dng
            )
        },
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "photo.updateDngPreview",
                "Update DNG Preview & Metadata",
                ["Metadata"],
                None,
                "{ids?, longEdge?: 256..4096 (1024), quality?: 1..100 (85)}: for each selected DNG (or `ids`, at most 500) rewrite the file atomically with a JPEG preview of the current edit (an sRGB preview IFD in SubIFDs; a preview of this app's from before is replaced) and the photo's current XMP embedded (merged into the packet already there); the raw data stays byte for byte. Refused with the reason, file untouched: DNGs written by Adobe software, BigTIFF, unreadable or unfamiliar structures, an embedded XMP that cannot be read; non-DNG photos are skipped. Shows in the activity stack (cancel between photos) → {updated: [{id, path, bytes, preview: [w, h], replacedPreviews}], skipped: [{id, name, reason}], failed: [{id, name, error}], cancelled}",
                has_dng_selection,
                update_dng_preview
            )
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcc_catalog::{Photo, PhotoId, Source};

    #[test]
    fn reload_fills_what_is_missing_and_keeps_what_is_set() {
        // reload replaces a file date guess but not a stated time
        // Reload replaces the file-date guess with the capture time the file states now, and keeps a stated one.
        {
            let mut ph = Photo::new(PhotoId(1), Source::File { path: "/x.jpg".into() }, "x.jpg", "jpeg", 40, 30, "2026-10-06T00:00:00");
            ph.captured = Some("2026-01-01T10:00:00".into());
            ph.captured_fallback = ph.captured.clone();
            let mut info = crate::media::ProbeInfo { captured: Some("2025-05-05T08:30:00".into()), ..Default::default() };
            let ops = fill_missing_meta(PhotoId(1), &ph, &info);
            assert!(matches!(ops.as_slice(), [Op::SetCaptured { captured: Some(c), .. }] if c == "2025-05-05T08:30:00"), "{ops:?}");
            // a time that was stated (or edited by hand) stays
            ph.captured_fallback = None;
            assert!(fill_missing_meta(PhotoId(1), &ph, &info).is_empty());
            // no stated time in the file now: the guess stays
            ph.captured_fallback = ph.captured.clone();
            info.captured = None;
            assert!(fill_missing_meta(PhotoId(1), &ph, &info).is_empty());
        }
        // reload backfills camera parts and rederives label
        // A catalog from before Make and Model were kept separately gets its parts backfilled and its
        // label re-derived ("Canon Canon IXUS 185" → "Canon IXUS 185") on reload (upstream #1618).
        {
            let mut ph = Photo::new(PhotoId(1), Source::File { path: "/x.jpg".into() }, "x.jpg", "jpeg", 4000, 3000, "2026-10-06T00:00:00");
            ph.meta.camera = "Canon Canon IXUS 185".into();
            let mut info = crate::media::ProbeInfo::default();
            info.meta.camera = "Canon IXUS 185".into();
            info.meta.camera_make = "Canon".into();
            info.meta.camera_model = "Canon IXUS 185".into();
            let ops = fill_missing_meta(PhotoId(1), &ph, &info);
            let Some(Op::SetMeta { meta, .. }) = ops.first() else { panic!("{ops:?}") };
            assert_eq!((meta.camera_make.as_str(), meta.camera_model.as_str(), meta.camera.as_str()), ("Canon", "Canon IXUS 185", "Canon IXUS 185"));
            // nothing left missing: nothing to do
            ph.meta = (**meta).clone();
            assert!(fill_missing_meta(PhotoId(1), &ph, &info).is_empty());
        }
        // reload fills only missing camera fields
        // Reload fills camera fields a photo lacks (a CR3 imported before CR3 metadata was read) and keeps
        // everything already set.
        {
            let mut ph = Photo::new(PhotoId(1), Source::File { path: "/x.cr3".into() }, "x.cr3", "CR3", 1620, 1080, "2026-10-06T00:00:00");
            ph.meta.title = "Mine".into();
            ph.meta.iso = Some(800);
            let mut info = crate::media::ProbeInfo { captured: Some("2026-10-04T08:16:11".into()), ..Default::default() };
            info.meta.camera = "Canon EOS R6 Mark III".into();
            info.meta.lens = "RF24-105mm".into();
            info.meta.iso = Some(400);
            info.meta.aperture = Some(6.3);
            let ops = fill_missing_meta(PhotoId(1), &ph, &info);
            let Some(Op::SetMeta { meta, .. }) = ops.first() else { panic!("{ops:?}") };
            assert_eq!((meta.camera.as_str(), meta.lens.as_str(), meta.aperture), ("Canon EOS R6 Mark III", "RF24-105mm", Some(6.3)));
            assert_eq!((meta.title.as_str(), meta.iso), ("Mine", Some(800)), "set fields are kept");
            assert!(matches!(ops.get(1), Some(Op::SetCaptured { captured: Some(_), .. })));
            // nothing missing → nothing to do
            ph.meta = (**meta).clone();
            ph.captured = Some("2026-10-04T08:16:11".into());
            assert!(fill_missing_meta(PhotoId(1), &ph, &info).is_empty());
        }
        // reload fills a missing focus point
        // Reload from Disk gives a photo imported before focus points were read its camera focus point,
        // and never replaces one it has.
        {
            let fp = |x: f64| lcc_meta::FocusPoint {
                rect: lcc_meta::Rect { x0: x, y0: 0.4, x1: x + 0.02, y1: 0.42 },
                mode: Some("AF-C".into()),
                area: None,
                tracking: None,
            };
            let mut ph = Photo::new(PhotoId(1), Source::File { path: "/x.arw".into() }, "x.arw", "ARW", 8640, 5760, "2026-10-06T00:00:00");
            ph.captured = Some("2026-10-04T08:16:11".into());
            let mut info = crate::media::ProbeInfo { captured: ph.captured.clone(), ..Default::default() };
            info.meta.focus = Some(fp(0.3));
            let ops = fill_missing_meta(PhotoId(1), &ph, &info);
            let Some(Op::SetMeta { meta, .. }) = ops.first() else { panic!("{ops:?}") };
            assert_eq!(meta.focus, Some(fp(0.3)));
            ph.meta.focus = Some(fp(0.1));
            assert!(fill_missing_meta(PhotoId(1), &ph, &info).is_empty(), "a known focus point is kept");
        }
    }

    fn lens() -> lcc_develop::EmbeddedLens {
        lcc_develop::EmbeddedLens {
            warp: Some(lcc_develop::EmbeddedWarp { planes: [[1.0, -0.1, 0.03, 0.0, 0.0, 0.0]; 3], ..Default::default() }),
            vignette: None,
            stages: Default::default(),
        }
    }
    fn old_rw2() -> lcc_catalog::Photo {
        // as an older build catalogued it: no lens data, correction off
        lcc_catalog::Photo::new(lcc_catalog::PhotoId(1), Source::File { path: "/x.rw2".into() }, "x.rw2", "RW2", 4592, 3448, "2026-10-06T00:00:00")
    }

    #[test]
    fn reload_adds_lens_data_and_respects_the_users_settings() {
        // reload adds lens data and turns the correction on for an untouched photo
        {
            let ph = old_rw2();
            let info = crate::media::ProbeInfo { embedded_lens: Some(lens()), ..Default::default() };
            let ops = lens_ops(ph.id, &ph, &info);
            assert!(matches!(ops.first(), Some(Op::SetEmbeddedLens { lens: Some(_), .. })), "{ops:?}");
            let Some(Op::SetDevelop { settings, .. }) = ops.get(1) else { panic!("{ops:?}") };
            assert!(settings.optics.lens_profile);
            // once applied, the photo is still "unedited" and has what import would have given it
            let mut c = lcc_catalog::Catalog::default();
            c.apply(Op::AddPhoto { photo: Box::new(ph) }).unwrap();
            for op in ops {
                c.apply(op).unwrap();
            }
            let after = c.photo(lcc_catalog::PhotoId(1)).unwrap();
            assert_eq!(after.embedded_lens.as_deref().copied(), Some(lens()));
            assert!(!after.is_edited());
            assert_eq!(*after.develop, after.camera_defaults());
            assert!(lens_ops(after.id, after, &info).is_empty(), "nothing more to do the second time");
        }
        // reload adds lens data but keeps an edited photos settings
        {
            let mut ph = old_rw2();
            let mut d = (*ph.develop).clone();
            d.light.exposure = 0.7;
            ph.develop = std::sync::Arc::new(d);
            assert!(ph.is_edited());
            let info = crate::media::ProbeInfo { embedded_lens: Some(lens()), ..Default::default() };
            let ops = lens_ops(ph.id, &ph, &info);
            assert!(matches!(ops.as_slice(), [Op::SetEmbeddedLens { lens: Some(_), .. }]), "{ops:?}");
        }
        // reload leaves a photo whose user switched the correction off alone
        {
            let mut ph = old_rw2();
            ph.embedded_lens = Some(Box::new(lens()));
            let mut d = ph.camera_defaults();
            d.optics.lens_profile = false;
            ph.develop = std::sync::Arc::new(d);
            let info = crate::media::ProbeInfo { embedded_lens: Some(lens()), ..Default::default() };
            assert!(lens_ops(ph.id, &ph, &info).is_empty());
        }
    }

    /// End to end on a public Panasonic raw (skipped without the corpus): a catalog from before the lens data was read
    /// gets it on Reload, the render changes to the corrected one, and Undo takes it all back.
    #[test]
    fn corpus_reload_corrects_an_rw2_catalogued_without_lens_data() {
        let root =
            lcc_brand::env_os("CORPUS").map(std::path::PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
        let f = root.join("raw/rw2-panasonic-gx80.rw2");
        if !f.exists() {
            lcc_testkit::skip!(Corpus, "{} absent", f.display());
            return;
        }
        let mut s = Session::new().with_fs();
        s.execute("library.import", &json!({"paths": [f.to_string_lossy()]})).unwrap();
        let id = s.active().unwrap();
        let ph = s.catalog.photo(id).unwrap().clone();
        assert!(ph.embedded_lens.is_some() && ph.develop.optics.lens_profile, "import reads it");
        // back to how 0.4.0 catalogued it
        let mut old = (*ph.develop).clone();
        old.optics.lens_profile = false;
        s.catalog.apply(Op::SetEmbeddedLens { id, lens: None }).unwrap();
        s.catalog.apply(Op::SetDevelop { id, settings: std::sync::Arc::new(old), label: "x".into(), edited: None }).unwrap();
        s.media.forget(id);
        let before = s.render_now(id, 160, 120).unwrap().image.data;
        let r = s.execute("photo.reload", &json!({"ids": [id.0]})).unwrap();
        assert_eq!(r["reloaded"], json!([id.0]));
        let now = s.catalog.photo(id).unwrap().clone();
        assert_eq!(now.embedded_lens, ph.embedded_lens);
        assert!(now.develop.optics.lens_profile && !now.is_edited());
        assert_ne!(s.render_now(id, 160, 120).unwrap().image.data, before, "the corrected render differs");
        s.execute("edit.undo", &json!({})).unwrap();
        let undone = s.catalog.photo(id).unwrap();
        assert!(undone.embedded_lens.is_none() && !undone.develop.optics.lens_profile, "one undo step");
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("lc-render-dng-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn flat_field_writes_stacked_corrected_dngs_and_refuses_other_sensors() {
        let dir = scratch("flatfield");
        let mosaic = |w: usize, h: usize, level: f64| -> Vec<u16> {
            (0..w * h)
                .map(|i| {
                    let (dx, dy) = ((i % w) as f64 / w as f64 - 0.5, (i / w) as f64 / h as f64 - 0.5);
                    (256.0 + level * (1.0 - (dx * dx + dy * dy)) * 15000.0) as u16
                })
                .collect()
        };
        let write = |name: &str, w: usize, h: usize, level: f64| {
            let p = dir.join(name);
            std::fs::write(&p, crate::tests_xmp::synthetic_dng_of(w, h, "RGGB", mosaic(w, h, level), None, Default::default())).unwrap();
            p.to_string_lossy().to_string()
        };
        let (flat, target, small) = (write("flat.dng", 128, 96, 0.7), write("t.dng", 128, 96, 0.3), write("small.dng", 64, 48, 0.3));
        let mut s = Session::new();
        let r = s.execute("library.import", &json!({"paths": [flat, target, small]})).unwrap();
        let ids: Vec<u64> = r["imported"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
        assert_eq!(ids.len(), 3, "{r}");
        let r = s.execute("photo.flatField", &json!({"reference": ids[0], "ids": ids})).unwrap();
        assert_eq!(r["cancelled"], false);
        let copies = r["copies"].as_array().unwrap();
        assert_eq!(copies.len(), 1, "{r}");
        assert_eq!(r["failed"][0]["id"].as_u64(), Some(ids[2]), "the other sensor size is refused: {r}");
        let path = copies[0]["path"].as_str().unwrap();
        assert!(path.ends_with("t-FlatField.dng"), "{path}");
        let n = lcc_raw::decode(&std::fs::read(path).unwrap()).unwrap().normalized().unwrap();
        let (lo, hi) = n.data.iter().fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
        assert!((hi - lo) / hi < 0.08, "flattened: {lo}..{hi}");
        let new = PhotoId(copies[0]["id"].as_u64().unwrap());
        assert!(s.catalog.stack_of(new).unwrap().photos.contains(&PhotoId(ids[1])), "stacked on its original");
        assert!(s.activity.list().is_empty());
        assert!(s.execute("photo.flatField", &json!({"reference": ids[0], "ids": [ids[0]]})).is_err(), "no targets");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One test for the plain run (each photo is rendered at full size, so the renders are the cost): explicit ids
    /// work without a selection, the copy is a linear DNG stacked on its original and selected, a second run never
    /// overwrites, and a photo edited in HDR renders too.
    #[test]
    fn render_to_dng_adds_stacked_linear_dngs() {
        let dir = scratch("stack");
        let mut s = Session::with_demo();
        let orig = s.visible()[0];
        // explicit ids need no selection
        s.execute("library.select", &json!({"ids": []})).unwrap();
        assert!(s.selection.ids.is_empty(), "selection was cleared");
        let r = s.execute("photo.renderToDng", &json!({"ids": [orig.0], "dir": dir.to_string_lossy()})).unwrap();
        assert_eq!(r["cancelled"], false);
        let copies = r["copies"].as_array().unwrap();
        assert_eq!(copies.len(), 1, "the explicit target was rendered: {r}");
        let c = &copies[0];
        let path = c["path"].as_str().unwrap();
        assert!(path.ends_with("-Rendered.dng"), "{path}");
        let raw = lcc_raw::decode(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!((raw.cpp, raw.cfa.is_none()), (3, true));
        let new = PhotoId(c["id"].as_u64().unwrap());
        assert_eq!(c["original"].as_u64(), Some(orig.0));
        let st = s.catalog.stack_of(new).unwrap();
        assert_eq!((st.photos.first(), st.photos.contains(&orig)), (Some(&new), true), "stacked on top of its original");
        assert_eq!(s.selection.ids, vec![new], "the new DNG is selected");
        assert!(s.activity.list().is_empty(), "the activity row ends with the command");
        // a second run never overwrites, and HDR editing on the photo is no obstacle
        s.execute("develop.hdr", &json!({"id": orig.0, "enabled": true})).unwrap();
        let again = s.execute("photo.renderToDng", &json!({"ids": [orig.0], "dir": dir.to_string_lossy()})).unwrap();
        let second = again["copies"][0]["path"].as_str().unwrap();
        assert_ne!(second, path);
        assert!(std::path::Path::new(second).exists() && std::path::Path::new(path).exists(), "both DNGs are on disk");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_to_dng_rejects_bad_params_and_cancels_between_photos() {
        let mut s = Session::with_demo();
        let id = s.visible()[0];
        assert!(s.execute("photo.renderToDng", &json!({"ids": [id.0]})).is_err(), "a demo photo needs `dir`");
        assert!(s.execute("photo.renderToDng", &json!({"ids": [id.0], "dir": "/x", "colorSpace": "cmyk"})).is_err());
        let dir = scratch("cancel");
        let ids: Vec<u64> = s.visible().iter().take(6).map(|i| i.0).collect();
        let activity = s.activity.clone();
        let stop = std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            while t0.elapsed().as_secs() < 30 {
                if let Some(t) = activity.list().into_iter().find(|t| t.kind == "renderDng" && t.cancellable) {
                    activity.cancel(t.id).unwrap();
                    return true;
                }
                std::thread::yield_now();
            }
            false
        });
        let r = s.execute("photo.renderToDng", &json!({"ids": ids, "dir": dir.to_string_lossy()})).unwrap();
        assert!(stop.join().unwrap(), "the batch showed a cancellable activity row");
        assert_eq!(r["cancelled"], true, "{r}");
        assert!(r["copies"].as_array().unwrap().len() < 6);
        assert!(s.activity.list().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Reload from Disk repairs a shutter speed an older import rounded (0.4 s stored as `1/2`, with no exact
    /// time): the photo takes the file's exact time and today's text. A shutter speed set by hand that is not what
    /// an import wrote, and one that already has its exact time, are kept (upstream #1692).
    #[test]
    fn reload_repairs_a_shutter_speed_an_older_import_rounded() {
        let mut ph = Photo::new(PhotoId(1), Source::File { path: "/x.nef".into() }, "x.nef", "NEF", 6000, 4000, "2026-10-06T00:00:00");
        ph.captured = Some("2026-10-04T08:16:11".into());
        let mut info = crate::media::ProbeInfo { captured: ph.captured.clone(), ..Default::default() };
        for (t, legacy, shown) in [
            (0.4, "1/2", "0.4"),
            (2.5, "2", "2.5\u{2033}"),
            (1.0 / 2.8, "1/3", "0.3571"),
            (1.0 / 2.8, "0.4", "0.3571"),
            (1.0 / 250.0, "1/250", "1/250"),
        ] {
            info.meta.exposure_time = Some(t);
            info.meta.shutter = lcc_meta::exposure_display_of(t).unwrap();
            ph.meta.shutter = legacy.into();
            ph.meta.exposure_time = None;
            let ops = fill_missing_meta(PhotoId(1), &ph, &info);
            let Some(Op::SetMeta { meta, .. }) = ops.first() else { panic!("{t}: {ops:?}") };
            assert_eq!((meta.shutter.as_str(), meta.exposure_time), (shown, Some(t)), "{t}");
            assert_eq!(meta.shutter_seconds(), Some(t), "{t}");
            // repaired → nothing more to do
            ph.meta = (**meta).clone();
            assert!(fill_missing_meta(PhotoId(1), &ph, &info).is_empty(), "{t}");
        }
        // typed by hand, not what any import wrote for this file
        info.meta.exposure_time = Some(0.4);
        info.meta.shutter = "0.4".into();
        ph.meta.shutter = "1/8".into();
        ph.meta.exposure_time = None;
        assert!(fill_missing_meta(PhotoId(1), &ph, &info).is_empty(), "a hand-set shutter speed is kept");
        // already has an exact time (the text was edited afterwards): kept
        ph.meta.shutter = "1/2".into();
        ph.meta.exposure_time = Some(0.4);
        assert!(fill_missing_meta(PhotoId(1), &ph, &info).is_empty(), "a photo with an exact time is left alone");
    }

    #[test]
    fn reload_repairs_legacy_shutter_on_copies_without_replacing_edits() {
        let mut cat = lcc_catalog::Catalog::new();
        let mut source = Photo::new(PhotoId(1), Source::File { path: "/x.nef".into() }, "x.nef", "NEF", 6000, 4000, "");
        source.meta.shutter = "1/2".into();
        cat.apply(Op::AddPhoto { photo: Box::new(source.clone()) }).unwrap();
        for (id, text, exact) in [(2, "1/2", None), (3, "0.4", None), (4, "1/8", None), (5, "1/2", Some(0.4))] {
            let mut copy = source.clone();
            copy.id = PhotoId(id);
            copy.copy_of = Some(source.id);
            copy.meta.shutter = text.into();
            copy.meta.exposure_time = exact;
            copy.meta.title = "My copy".into();
            cat.apply(Op::AddPhoto { photo: Box::new(copy) }).unwrap();
        }
        let mut info = crate::media::ProbeInfo::default();
        info.meta.shutter = "0.4".into();
        info.meta.exposure_time = Some(0.4);
        info.meta.title = "File title".into();
        let (mut ops, mut changed) = (Vec::new(), Vec::new());
        reload_ops(&cat, source.id, &info, &mut ops, &mut changed);
        for op in ops {
            cat.apply(op).unwrap();
        }
        for id in [1, 2, 3] {
            let m = &cat.photo(PhotoId(id)).unwrap().meta;
            assert_eq!((m.shutter.as_str(), m.exposure_time, m.shutter_seconds()), ("0.4", Some(0.4), Some(0.4)), "{id}");
        }
        for (id, text, exact) in [(4, "1/8", None), (5, "1/2", Some(0.4))] {
            let m = &cat.photo(PhotoId(id)).unwrap().meta;
            assert_eq!((m.shutter.as_str(), m.exposure_time), (text, exact), "{id}");
        }
        for id in [2, 3, 4, 5] {
            assert_eq!(cat.photo(PhotoId(id)).unwrap().meta.title, "My copy", "{id}");
        }
    }
}
