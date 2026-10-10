//! Edit In (P4.4): external editor presets, Edit In with Lightroom's copy options, layered PSD
//! round trips (PhotoCraft) and Open as Layers.
//!
//! Upstream already renders a 16-bit TIFF edit copy (`photo.editExternal`), stacks it and reloads
//! it when the app regains focus (`photo.reload`); this module adds what Lightroom's Edit In
//! dialog offers on top:
//!
//! - **presets** (`editIn.presets` / `editIn.savePreset` / `editIn.deletePreset`): application,
//!   arguments, file format (TIFF or layered PSD), colour space, bit depth, mode, naming, stacking;
//!   kept in `editors.json` in the settings folder. Built in: the system's TIFF editor and
//!   PhotoCraft (layered PSD).
//! - **`photo.editIn`**: *Edit a Copy with Lightroom Adjustments* (`copyWithAdjustments`, rendered
//!   with the edits), *Edit a Copy* (`copy`, the original file's bytes) or *Edit Original*
//!   (`original`), the last two for files an editor can open (not raws). The new file is added to
//!   the library and stacked on the original; its metadata is carried over (TIFF: the export
//!   metadata; PSD: an XMP resource). The result names the file and the application to open it
//!   with (`openWith`); the desktop app opens it and reloads the photo when it regains focus,
//!   `launch: true` starts the application from the engine (CLI, MCP).
//! - **`photo.openAsLayers`**: one layered PSD from the selection, each photo rendered with its
//!   edits as a layer (PSB past 30 000 px), added to the library and stacked on the active photo.
//! - **PhotoCraft's control channel**: a preset with `control: {port, tokenFile, root}` asks a
//!   PhotoCraft running with `--control` to open the file (`app.open`, relative to its automation
//!   read root) instead of starting a new process.
//!
//! Layered PSDs are written with `dac-psd` (PhotoCraft's PSD writer, ported).
//!
//! Sources: own design; behaviour from Lightroom's documented Edit In options; PSD layout from
//! Adobe's public file format specification (via dac-psd).

use std::path::{Path, PathBuf};

use dac_catalog::{PhotoId, Source};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, has_active, has_selection, str_param};
use crate::{Result, Session};

/// Files an image editor opens as they are (Edit a Copy / Edit Original).
const EDITABLE: &[&str] = &["jpg", "jpeg", "tif", "tiff", "png", "psd", "psb", "webp"];
/// Most photos in one Open as Layers.
const MAX_LAYERS: usize = 64;
/// Most pixels (canvas × layers) one Open as Layers holds in memory at 16 bits.
const MAX_LAYER_PIXELS: u64 = 800_000_000;
/// PSD's (version 1) largest side; larger documents are PSB.
const PSD_MAX_SIDE: u32 = 30_000;

/// How Edit In hands the photo over.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    /// Render a copy with the edits (the only choice for raws).
    #[default]
    CopyWithAdjustments,
    /// Copy the original file, without the edits.
    Copy,
    /// The original file itself.
    Original,
}

/// File format of a rendered copy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Format {
    #[default]
    Tiff,
    /// Layered Photoshop document (dac-psd).
    Psd,
}

/// A running PhotoCraft to send files to over its control channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Control {
    pub port: u16,
    /// The file holding PhotoCraft's control token (`--control-token-file`).
    pub token_file: String,
    /// PhotoCraft's automation read root: the file must be inside it.
    pub root: String,
}

/// An external editor preset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EditorPreset {
    pub name: String,
    /// Application: a program path or name (an app name on macOS); empty = the system's default.
    pub app: String,
    /// Arguments before the file; one containing `{file}` takes the path instead of it going last.
    pub args: Vec<String>,
    pub format: Format,
    /// `srgb`, `adobeRgb`, `displayP3` or `proPhoto`.
    pub color_space: String,
    /// 8 or 16.
    pub bit_depth: u8,
    pub mode: Mode,
    /// File name template (`{name}` = the original's name without extension).
    pub naming: String,
    /// Stack the new file on the original.
    pub stack: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control: Option<Control>,
}

impl Default for EditorPreset {
    fn default() -> Self {
        EditorPreset {
            name: String::new(),
            app: String::new(),
            args: Vec::new(),
            format: Format::Tiff,
            color_space: "adobeRgb".into(),
            bit_depth: 16,
            mode: Mode::CopyWithAdjustments,
            naming: "{name}-Edit".into(),
            stack: true,
            control: None,
        }
    }
}

impl EditorPreset {
    fn validate(&self) -> std::result::Result<(), String> {
        dac_actions::validate_name(&self.name)?;
        space(&self.color_space)?;
        if !matches!(self.bit_depth, 8 | 16) {
            return Err(format!("bit depth {} (8 or 16)", self.bit_depth));
        }
        if self.naming.trim().is_empty() || self.naming.contains(['/', '\\']) || self.naming.len() > 200 {
            return Err("the file name template must be a plain name".into());
        }
        if self.args.len() > 32 || self.args.iter().any(|a| a.len() > 1000) {
            return Err("too many or too long arguments".into());
        }
        Ok(())
    }
}

/// Built-in presets: the system's editor for TIFFs, and PhotoCraft with layered PSDs.
pub fn builtin_presets() -> Vec<EditorPreset> {
    vec![
        EditorPreset { name: "External Editor".into(), ..Default::default() },
        EditorPreset {
            name: "PhotoCraft".into(),
            app: "photocraft".into(),
            format: Format::Psd,
            color_space: "proPhoto".into(),
            ..Default::default()
        },
    ]
}

fn space(s: &str) -> std::result::Result<dac_pipeline::OutputSpace, String> {
    Ok(match s {
        "srgb" | "sRGB" => dac_pipeline::OutputSpace::Srgb,
        "adobeRgb" => dac_pipeline::OutputSpace::AdobeRgb,
        "displayP3" => dac_pipeline::OutputSpace::DisplayP3,
        "proPhoto" | "prophoto" => dac_pipeline::OutputSpace::ProPhoto,
        other => return Err(format!("unknown colour space `{other}` (srgb, adobeRgb, displayP3, proPhoto)")),
    })
}

/// The user's presets (read from `editors.json` on first use; a damaged file is an error and is
/// not written over).
fn user_presets(s: &mut Session, cmd: &str) -> Result<Vec<EditorPreset>> {
    if let Some(v) = &s.workflow.editors {
        return Ok(v.clone());
    }
    let list = match &s.workflow.editors_file {
        Some(f) => match std::fs::read(f) {
            Ok(b) if b.len() > 1 << 20 => return Err(bad(cmd, format!("{} is too large", f.display()))),
            Ok(b) => serde_json::from_slice::<Vec<EditorPreset>>(&b).map_err(|e| bad(cmd, format!("{} can't be read: {e}", f.display())))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(bad(cmd, format!("{} can't be read: {e}", f.display()))),
        },
        None => Vec::new(),
    };
    s.workflow.editors = Some(list.clone());
    Ok(list)
}

fn store_presets(s: &mut Session, cmd: &str, list: Vec<EditorPreset>) -> Result<()> {
    if let Some(f) = &s.workflow.editors_file {
        let bytes = serde_json::to_vec_pretty(&list).map_err(|e| bad(cmd, e.to_string()))?;
        if let Some(d) = f.parent() {
            std::fs::create_dir_all(d).map_err(|e| bad(cmd, format!("can't create {}: {e}", d.display())))?;
        }
        // temp file + rename; a failed write (full disk) removes the temp file
        dac_catalog::safe_file::write_atomic(f, &bytes).map_err(|e| bad(cmd, format!("can't write {}: {e}", f.display())))?;
    }
    s.workflow.editors = Some(list);
    Ok(())
}

fn presets(s: &mut Session, _: &Value) -> Result<Value> {
    let user = user_presets(s, "editIn.presets")?;
    Ok(json!({"builtin": builtin_presets(), "user": user}))
}

fn save_preset(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "editIn.savePreset";
    let mut pr: EditorPreset = serde_json::from_value(p.clone()).map_err(|e| bad(C, e.to_string()))?;
    pr.name = pr.name.trim().to_string();
    pr.validate().map_err(|e| bad(C, e))?;
    if builtin_presets().iter().any(|b| b.name == pr.name) {
        return Err(bad(C, format!("`{}` is a built-in preset: choose another name", pr.name)));
    }
    let mut list = user_presets(s, C)?;
    match list.iter().position(|x| x.name == pr.name) {
        Some(i) => {
            if let Some(x) = list.get_mut(i) {
                *x = pr.clone();
            }
        }
        None if list.len() >= 100 => return Err(bad(C, "at most 100 presets")),
        None => list.push(pr.clone()),
    }
    store_presets(s, C, list)?;
    serde_json::to_value(pr).map_err(|e| bad(C, e.to_string()))
}

fn delete_preset(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "editIn.deletePreset";
    let name = str_param(p, "name").map(str::trim).ok_or_else(|| bad(C, "missing `name`"))?.to_string();
    let mut list = user_presets(s, C)?;
    let n = list.len();
    list.retain(|x| x.name != name);
    if list.len() == n {
        return Err(bad(C, format!("no preset `{name}`")));
    }
    store_presets(s, C, list)?;
    Ok(json!({"deleted": name}))
}

/// The preset `p` names (default: the first built-in), with the fields `p` gives on top.
fn resolve(s: &mut Session, p: &Value, cmd: &str) -> Result<EditorPreset> {
    let base = match str_param(p, "preset").map(str::trim) {
        Some(n) => builtin_presets()
            .into_iter()
            .chain(user_presets(s, cmd)?)
            .find(|x| x.name == n)
            .ok_or_else(|| bad(cmd, format!("no Edit In preset `{n}` (editIn.presets lists them)")))?,
        None => builtin_presets().into_iter().next().unwrap_or_default(),
    };
    let mut v = serde_json::to_value(&base).map_err(|e| bad(cmd, e.to_string()))?;
    for k in ["app", "args", "format", "colorSpace", "bitDepth", "mode", "naming", "stack", "control"] {
        if let (Some(x), Some(o)) = (p.get(k), v.as_object_mut()) {
            o.insert(k.into(), x.clone());
        }
    }
    let pr: EditorPreset = serde_json::from_value(v).map_err(|e| bad(cmd, e.to_string()))?;
    let mut check = pr.clone();
    if check.name.is_empty() {
        check.name = "-".into();
    }
    check.validate().map_err(|e| bad(cmd, e))?;
    Ok(pr)
}

/// The folder a photo's copies go to: next to the file, or `dir` (required for demo photos).
fn out_dir(src: &Source, p: &Value, cmd: &str) -> Result<String> {
    if let Some(d) = str_param(p, "dir") {
        return Ok(d.to_string());
    }
    match src {
        Source::File { path } => Ok(Path::new(path).parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default()),
        Source::Demo { .. } => Err(bad(cmd, "a generated demo photo has no folder: give `dir`")),
    }
}

/// `dir/stem.ext`, or `dir/stem-2.ext`, … when taken.
fn unique_path(dir: &str, stem: &str, ext: &str) -> Result<PathBuf> {
    let d = Path::new(dir);
    for i in 1..10_000u32 {
        let name = if i == 1 { format!("{stem}.{ext}") } else { format!("{stem}-{i}.{ext}") };
        let p = d.join(name);
        if !p.exists() {
            return Ok(p);
        }
    }
    Err(bad("photo.editIn", format!("no free file name for {stem}.{ext} in {dir}")))
}

fn stem_of(file_name: &str) -> String {
    Path::new(file_name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "photo".into())
}

fn editable_path(s: &Session, id: PhotoId, cmd: &str) -> Result<String> {
    let ph = s.catalog.photo(id).ok_or_else(|| bad(cmd, "no photo"))?;
    let Source::File { path } = &ph.source else {
        return Err(bad(cmd, "a generated demo photo has no file to edit: use mode copyWithAdjustments"));
    };
    let ext = Path::new(path).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if !EDITABLE.contains(&ext.as_str()) {
        return Err(bad(cmd, format!("an editor can't open the {} file itself: use mode copyWithAdjustments", ph.format)));
    }
    Ok(path.clone())
}

/// Add `path` to the library (stacked on `on` when asked) and select it. → the new photo
fn add_and_stack(s: &mut Session, path: &Path, on: PhotoId, stack: bool, cmd: &str) -> Result<u64> {
    let r = s.execute("library.import", &json!({"paths": [path.to_string_lossy()]}))?;
    let new =
        r["imported"].get(0).and_then(Value::as_u64).ok_or_else(|| bad(cmd, format!("{} could not be added to the library", path.display())))?;
    // the round trip keeps the catalog's metadata (rating, label, title, keywords, camera…),
    // whatever the file format could carry
    if let Some(from) = s.catalog.photo(on).cloned() {
        let nid = PhotoId(new);
        let ops = vec![
            dac_catalog::Op::SetRating { id: nid, rating: from.rating },
            dac_catalog::Op::SetLabel { id: nid, label: from.label },
            dac_catalog::Op::SetMeta { id: nid, meta: Box::new(from.meta.clone()) },
        ];
        s.commit("Edit In", dac_catalog::Op::Batch { ops })?;
    }
    if stack {
        // the edit on top of the original, expanded so both show
        let _ = s.execute("stack.group", &json!({"ids": [new, on.0], "top": new, "collapsed": false}));
    }
    s.selection = crate::Selection::single(PhotoId(new));
    Ok(new)
}

/// Edit a Copy: the same bytes as the original, so import would call it a duplicate. Add it as
/// its own photo instead (metadata kept, edits not: the editor gets the unedited file), stacked
/// on the original and selected. → the new photo
fn add_copy(s: &mut Session, ph: &dac_catalog::Photo, dest: &Path, stack: bool) -> Result<u64> {
    let new = s.catalog.alloc_photo_id();
    let mut q = ph.clone();
    q.id = new;
    q.source = Source::File { path: dest.to_string_lossy().to_string() };
    q.file_name = dest.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
    q.content_hash = q.content_hash.map(|h| format!("{h}:dup{}", new.0));
    q.develop = q.import_look.clone().unwrap_or_default();
    (q.edited, q.copy_of, q.copy_name, q.sha1, q.xmp) = (None, None, None, None, None);
    q.history.clear();
    q.versions.clear();
    s.commit("Edit a Copy", dac_catalog::Op::AddPhoto { photo: Box::new(q) })?;
    if stack {
        let _ = s.execute("stack.group", &json!({"ids": [new.0, ph.id.0], "top": new.0, "collapsed": false}));
    }
    s.selection = crate::Selection::single(new);
    Ok(new.0)
}

/// A photo rendered with its edits: 16-bit RGB samples in `space`, and the profile.
struct Rendered16 {
    width: u32,
    height: u32,
    rgb: Vec<u16>,
    icc: Option<Vec<u8>>,
}

fn render16(s: &mut Session, id: PhotoId, space: dac_pipeline::OutputSpace, cmd: &str) -> Result<Rendered16> {
    let opts = crate::export::ExportOptions {
        format: crate::export::ExportFormat::Tiff,
        bit_depth: Some(16),
        color_space: space,
        tiff_compression: crate::export::TiffCompression::None,
        metadata: crate::export::MetadataPolicy::None,
        ..Default::default()
    };
    let e = crate::export::export_photo(s, id, &opts, 1).map_err(|e| bad(cmd, e))?;
    decode_tiff16(&e.bytes).map_err(|e| bad(cmd, format!("photo {}: {e}", id.0)))
}

/// Our own 16-bit RGB(A) TIFF back to samples (alpha dropped).
fn decode_tiff16(bytes: &[u8]) -> std::result::Result<Rendered16, String> {
    use tiff::decoder::{Decoder, DecodingResult, Limits};
    let mut d = Decoder::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?.with_limits(Limits::unlimited());
    let (w, h) = d.dimensions().map_err(|e| e.to_string())?;
    let spp = match d.colortype().map_err(|e| e.to_string())? {
        tiff::ColorType::RGB(16) => 3,
        tiff::ColorType::RGBA(16) => 4,
        other => return Err(format!("unexpected render layout {other:?}")),
    };
    let icc = d.get_tag_u8_vec(tiff::tags::Tag::Unknown(34675)).ok();
    let DecodingResult::U16(v) = d.read_image().map_err(|e| e.to_string())? else {
        return Err("unexpected render sample format".into());
    };
    let n = (w as usize).saturating_mul(h as usize);
    if v.len() < n.saturating_mul(spp) {
        return Err("render is short".into());
    }
    let rgb = if spp == 3 { v } else { v.chunks_exact(4).flat_map(|p| p.iter().take(3).copied()).collect() };
    Ok(Rendered16 { width: w, height: h, rgb, icc })
}

/// A layered PSD (PSB when a side passes 30 000 px): `layers` bottom to top, each centred on a
/// canvas as large as the largest, and the composite (layers are opaque: the topmost covering a
/// pixel shows; uncovered pixels are transparent).
fn layered_psd(layers: &[(String, Rendered16)], depth: u8, icc: Option<Vec<u8>>, xmp: Option<String>) -> std::result::Result<Vec<u8>, String> {
    let w = layers.iter().map(|(_, r)| r.width).max().unwrap_or(0);
    let h = layers.iter().map(|(_, r)| r.height).max().unwrap_or(0);
    if w == 0 || h == 0 {
        return Err("nothing to write".into());
    }
    let canvas = (w as usize).saturating_mul(h as usize);
    let mut comp = vec![0u16; canvas.saturating_mul(4)];
    // RLE: what every reader takes (ours reads no ZIP composites)
    let eight = depth == 8;
    let pixels = |v: Vec<u16>| {
        if eight {
            dac_psd::PixelData::Rgba8(v.into_iter().map(|x| ((u32::from(x) * 255 + 32_767) / 65_535) as u8).collect())
        } else {
            dac_psd::PixelData::Rgba16(v)
        }
    };
    let mut b = dac_psd::PsdBuilder::new(w, h).depth(if eight { 8 } else { 16 });
    if w > PSD_MAX_SIDE || h > PSD_MAX_SIDE {
        b = b.version(dac_psd::Version::Psb);
    }
    if let Some(icc) = icc {
        b = b.icc_profile(icc);
    }
    if let Some(x) = xmp {
        b = b.resource(dac_psd::ImageResource::new(dac_psd::resources::ids::XMP, x.into_bytes()));
    }
    for (name, r) in layers {
        let (lw, lh) = (r.width as usize, r.height as usize);
        let (left, top) = ((w - r.width) / 2, (h - r.height) / 2);
        let mut px = Vec::with_capacity(lw.saturating_mul(lh).saturating_mul(4));
        for (i, c) in r.rgb.chunks_exact(3).take(lw.saturating_mul(lh)).enumerate() {
            px.extend_from_slice(c);
            px.push(u16::MAX);
            let (x, y) = (left as usize + i % lw.max(1), top as usize + i / lw.max(1));
            if let Some(o) = comp.get_mut((y * w as usize + x) * 4..(y * w as usize + x) * 4 + 4) {
                o.copy_from_slice(&[c[0], c[1], c[2], u16::MAX]);
            }
        }
        let left = i32::try_from(left).map_err(|_| "too large")?;
        let top = i32::try_from(top).map_err(|_| "too large")?;
        b.push_layer(dac_psd::LayerSpec::new(name.clone(), left, top, r.width, r.height, pixels(px)));
    }
    b.composite(pixels(comp));
    b.to_bytes().map_err(|e| e.to_string())
}

fn xmp_of(s: &Session, id: PhotoId) -> Option<String> {
    let ph = s.catalog.photo(id)?;
    let o = crate::export::ExportOptions::default();
    crate::export::export_metadata(ph, &s.catalog, &o).map(|m| dac_meta::write_xmp(&m, None))
}

fn write_durable(path: &Path, bytes: &[u8], cmd: &str) -> Result<()> {
    crate::export::write_file_durable(&path.to_string_lossy(), bytes).map_err(|e| bad(cmd, e))
}

fn edit_in(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "photo.editIn";
    let pr = resolve(s, p, C)?;
    let id = match p.get("id").and_then(Value::as_u64) {
        Some(i) => PhotoId(i),
        None => s.active().ok_or_else(|| bad(C, "no active photo"))?,
    };
    let ph = s.catalog.photo(id).cloned().ok_or_else(|| bad(C, format!("no photo {}", id.0)))?;
    let stem = crate::rename::expand_tokens(&pr.naming, &ph, 1, 3).replace("{name}", &stem_of(&ph.file_name));
    let stem = if stem.trim().is_empty() || stem.contains(['/', '\\']) { format!("{}-Edit", stem_of(&ph.file_name)) } else { stem };
    let (path, new) = match pr.mode {
        Mode::Original => (PathBuf::from(editable_path(s, id, C)?), None),
        Mode::Copy => {
            let src = editable_path(s, id, C)?;
            let ext = Path::new(&src).extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
            let dest = unique_path(&out_dir(&ph.source, p, C)?, &stem, &ext)?;
            let bytes = std::fs::read(&src).map_err(|e| bad(C, format!("can't read {src}: {e}")))?;
            write_durable(&dest, &bytes, C)?;
            let n = add_copy(s, &ph, &dest, pr.stack)?;
            (dest, Some(n))
        }
        Mode::CopyWithAdjustments => {
            let space = space(&pr.color_space).map_err(|e| bad(C, e))?;
            let dir = out_dir(&ph.source, p, C)?;
            let dest = match pr.format {
                Format::Tiff => {
                    let opts = crate::export::ExportOptions {
                        format: crate::export::ExportFormat::Tiff,
                        bit_depth: Some(pr.bit_depth),
                        color_space: space,
                        ..Default::default()
                    };
                    let e = crate::export::export_photo(s, id, &opts, 1).map_err(|e| bad(C, e))?;
                    let dest = unique_path(&dir, &stem, "tif")?;
                    write_durable(&dest, &e.bytes, C)?;
                    dest
                }
                Format::Psd => {
                    let r = render16(s, id, space, C)?;
                    let icc = r.icc.clone();
                    let bytes = layered_psd(&[(stem_of(&ph.file_name), r)], pr.bit_depth, icc, xmp_of(s, id)).map_err(|e| bad(C, e))?;
                    let dest = unique_path(&dir, &stem, "psd")?;
                    write_durable(&dest, &bytes, C)?;
                    dest
                }
            };
            let n = add_and_stack(s, &dest, id, pr.stack, C)?;
            (dest, Some(n))
        }
    };
    let opened = hand_over(&pr, &path, p, C)?;
    let edited = new.unwrap_or(id.0);
    Ok(json!({
        "path": path.to_string_lossy(),
        "id": edited,
        "original": id.0,
        "mode": pr.mode,
        "openWith": {"path": path.to_string_lossy(), "app": pr.app, "args": pr.args, "reload": [edited]},
        "opened": opened,
    }))
}

fn open_as_layers(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "photo.openAsLayers";
    let pr = resolve(s, &json!({"preset": str_param(p, "preset").unwrap_or("PhotoCraft")}), C)?;
    let ids: Vec<PhotoId> = s.targets(p).into_iter().filter(|i| s.catalog.photo(*i).is_some()).collect();
    if ids.len() < 2 {
        return Err(bad(C, "select at least two photos"));
    }
    if ids.len() > MAX_LAYERS {
        return Err(bad(C, format!("at most {MAX_LAYERS} photos")));
    }
    let space = space(str_param(p, "colorSpace").unwrap_or(&pr.color_space)).map_err(|e| bad(C, e))?;
    let depth = match p.get("bitDepth").and_then(Value::as_u64) {
        None => pr.bit_depth,
        Some(8) => 8,
        Some(16) => 16,
        Some(d) => return Err(bad(C, format!("bit depth {d} (8 or 16)"))),
    };
    // memory: the canvas is as large as the largest photo, one copy per layer
    let mut long = (0f64, 0f64);
    for id in &ids {
        if let Some(ph) = s.catalog.photo(*id) {
            let (w, h) = crate::export::output_size(ph, &crate::export::ExportOptions::default());
            long = (long.0.max(w as f64), long.1.max(h as f64));
        }
    }
    let total = long.0 * long.1 * ids.len() as f64;
    if total > MAX_LAYER_PIXELS as f64 {
        return Err(bad(C, format!("{} photos of up to {:.0}×{:.0} px are too large to open as layers at once", ids.len(), long.0, long.1)));
    }
    let first = ids.first().copied().ok_or_else(|| bad(C, "no photos"))?;
    let on = s.active().filter(|a| ids.contains(a)).unwrap_or(first);
    let ph = s.catalog.photo(on).cloned().ok_or_else(|| bad(C, "no photo"))?;
    let dir = out_dir(&ph.source, p, C)?;
    // the first selected photo ends up on top
    let mut layers = Vec::with_capacity(ids.len());
    let mut icc = None;
    for id in ids.iter().rev() {
        let name = s.catalog.photo(*id).map(|x| stem_of(&x.file_name)).unwrap_or_default();
        let r = render16(s, *id, space, C)?;
        icc = icc.or_else(|| r.icc.clone());
        layers.push((name, r));
    }
    let bytes = layered_psd(&layers, depth, icc, xmp_of(s, on)).map_err(|e| bad(C, e))?;
    drop(layers);
    let stem = str_param(p, "name").map(str::to_string).unwrap_or_else(|| format!("{}-Layers", stem_of(&ph.file_name)));
    if stem.contains(['/', '\\']) || stem.trim().is_empty() {
        return Err(bad(C, "`name` must be a plain file name"));
    }
    let dest = unique_path(&dir, &stem, "psd")?;
    write_durable(&dest, &bytes, C)?;
    let new = add_and_stack(s, &dest, on, bool_or(p, "stack", true), C)?;
    let opened = hand_over(&pr, &dest, p, C)?;
    Ok(json!({
        "path": dest.to_string_lossy(),
        "id": new,
        "layers": ids.len(),
        "openWith": {"path": dest.to_string_lossy(), "app": pr.app, "args": pr.args, "reload": [new]},
        "opened": opened,
    }))
}

/// Open the file in the editor when asked (`launch: true`): over PhotoCraft's control channel when
/// the preset has one, else by starting the application. → how it was opened (`null` = not)
fn hand_over(pr: &EditorPreset, path: &Path, p: &Value, cmd: &str) -> Result<Value> {
    if !bool_or(p, "launch", false) {
        return Ok(Value::Null);
    }
    if let Some(c) = &pr.control {
        return control_open(c, path)
            .map(|r| json!({"control": c.port, "result": r}))
            .map_err(|e| bad(cmd, format!("PhotoCraft's control channel: {e}")));
    }
    launch(&pr.app, &pr.args, path).map(|_| json!("launched")).map_err(|e| bad(cmd, format!("couldn't start the editor: {e}")))
}

/// The command line that opens `path` in `app` with `args`.
pub fn command_line(app: &str, args: &[String], path: &Path) -> Vec<String> {
    let file = path.to_string_lossy().to_string();
    let mut v: Vec<String> = if app.is_empty() {
        if cfg!(target_os = "macos") {
            vec!["open".into()]
        } else if cfg!(windows) {
            vec!["cmd".into(), "/C".into(), "start".into(), String::new()]
        } else {
            vec!["xdg-open".into()]
        }
    } else if cfg!(target_os = "macos") && !app.contains('/') {
        vec!["open".into(), "-a".into(), app.into(), "--args".into()]
    } else {
        vec![app.into()]
    };
    let mut placed = false;
    for a in args {
        if a.contains("{file}") {
            placed = true;
            v.push(a.replace("{file}", &file));
        } else {
            v.push(a.clone());
        }
    }
    if !placed {
        // `open -a App --args` passes later words to the app, not as files to open
        if v.get(3).is_some_and(|x| x == "--args") && args.is_empty() {
            v.truncate(3);
        }
        v.push(file);
    }
    v
}

#[cfg(not(target_arch = "wasm32"))]
fn launch(app: &str, args: &[String], path: &Path) -> std::result::Result<(), String> {
    let line = command_line(app, args, path);
    let (prog, rest) = line.split_first().ok_or("no program")?;
    std::process::Command::new(prog).args(rest).spawn().map(|_| ()).map_err(|e| format!("{prog}: {e}"))
}

#[cfg(target_arch = "wasm32")]
fn launch(_: &str, _: &[String], _: &Path) -> std::result::Result<(), String> {
    Err("not available in the browser".into())
}

/// Ask a PhotoCraft running with `--control` to open `path` (`auth`, then `app.open` with the path
/// relative to its automation read root). → its reply's result
#[cfg(not(target_arch = "wasm32"))]
pub fn control_open(c: &Control, path: &Path) -> std::result::Result<Value, String> {
    use std::io::{BufRead, BufReader, Write};
    let rel = path.strip_prefix(&c.root).map_err(|_| format!("{} is not inside PhotoCraft's read root {}", path.display(), c.root))?;
    let token = std::fs::read_to_string(&c.token_file).map_err(|e| format!("can't read the token file {}: {e}", c.token_file))?;
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], c.port));
    let t = std::time::Duration::from_secs(10);
    let stream = std::net::TcpStream::connect_timeout(&addr, t).map_err(|e| format!("no PhotoCraft on port {}: {e}", c.port))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(60))).map_err(|e| e.to_string())?;
    let mut w = stream.try_clone().map_err(|e| e.to_string())?;
    let mut r = BufReader::new(std::io::Read::take(stream, 1 << 20));
    let mut call = |id: &str, method: &str, params: Value| -> std::result::Result<Value, String> {
        let line = json!({"id": id, "method": method, "params": params}).to_string();
        w.write_all(line.as_bytes()).and_then(|_| w.write_all(b"\n")).map_err(|e| e.to_string())?;
        let mut reply = String::new();
        r.read_line(&mut reply).map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_str(reply.trim()).map_err(|e| format!("bad reply ({e})"))?;
        if v["ok"].as_bool() == Some(true) { Ok(v["result"].clone()) } else { Err(v["error"].as_str().unwrap_or("refused").to_string()) }
    };
    call("auth", "auth", json!({"token": token.trim()}))?;
    call("open", "app.open", json!({"path": rel.to_string_lossy()}))
}

#[cfg(target_arch = "wasm32")]
pub fn control_open(_: &Control, _: &Path) -> std::result::Result<Value, String> {
    Err("not available in the browser".into())
}

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "editIn.presets", "Edit In Presets", [], None, "{} — external editor presets → {builtin: [preset], user: [preset]}; a preset is {name, app, args, format: tiff|psd, colorSpace, bitDepth: 8|16, mode: copyWithAdjustments|copy|original, naming, stack, control?: {port, tokenFile, root}}", always, presets),
        cmd!(
            "editIn.savePreset",
            "Save Edit In Preset",
            [],
            None,
            "{name, app?, args?: [..., \"{file}\"?], format?: tiff|psd, colorSpace?: srgb|adobeRgb|displayP3|proPhoto, bitDepth?: 8|16, mode?: copyWithAdjustments|copy|original, naming?: \"{name}-Edit\", stack?: true, control?: {port, tokenFile, root}} — add or replace an external editor preset (kept in editors.json in the settings folder) → the preset",
            always,
            save_preset
        ),
        cmd!(
            "editIn.deletePreset",
            "Delete Edit In Preset",
            [],
            None,
            "{name} — delete a saved external editor preset → {deleted}",
            always,
            delete_preset
        ),
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "photo.editIn",
                "Edit In…",
                ["Photo", "Edit In"],
                None,
                "{id?, preset?: name (default \"External Editor\"), mode?, format?, colorSpace?, bitDepth?, app?, args?, naming?, stack?, dir?, launch?: false} — Edit In with Lightroom's options: copyWithAdjustments renders the photo with its edits (TIFF, or a layered PSD), copy copies the original file, original edits the file itself (copy/original: JPEG, TIFF, PNG, PSD, WebP only). A new file is added to the library, stacked on the original and selected; launch opens it in the preset's application (or over PhotoCraft's control channel) → {path, id, original, mode, openWith: {path, app, args, reload}, opened}",
                has_active,
                edit_in
            )
        },
        cmd!(
            "photo.openAsLayers",
            "Open as Layers in PhotoCraft…",
            ["Photo", "Edit In"],
            None,
            "{ids?, preset?: \"PhotoCraft\", colorSpace?, bitDepth?: 8|16, name?, dir?, stack?: true, launch?: false} — one layered PSD (16-bit unless the preset or bitDepth says 8) from the selected photos (2–64), each rendered with its edits as a layer named after it (the first selected on top), added to the library and stacked on the active photo → {path, id, layers, openWith, opened}",
            has_selection,
            open_as_layers
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_places_the_file() {
        let p = Path::new("/x/a b.psd");
        let l = command_line("/opt/pc/photocraft", &["--new-window".into()], p);
        assert_eq!(l, vec!["/opt/pc/photocraft", "--new-window", "/x/a b.psd"]);
        let l = command_line("/usr/bin/ed", &["--open={file}".into(), "-v".into()], p);
        assert_eq!(l, vec!["/usr/bin/ed", "--open=/x/a b.psd", "-v"]);
    }

    #[test]
    fn layered_psd_has_named_layers_and_a_composite() {
        let a = Rendered16 { width: 4, height: 2, rgb: vec![1000; 4 * 2 * 3], icc: None };
        let b = Rendered16 { width: 2, height: 2, rgb: vec![60000; 2 * 2 * 3], icc: None };
        let bytes = layered_psd(&[("bottom".into(), a), ("top".into(), b)], 16, None, Some("<x:xmpmeta/>".into())).unwrap();
        let f = dac_psd::PsdFile::from_bytes(&bytes).unwrap();
        assert_eq!((f.header.width, f.header.height, f.header.depth), (4, 2, 16));
        assert_eq!(f.layer(0).unwrap().name(), "bottom");
        assert_eq!(f.layer(1).unwrap().name(), "top");
        // our own PSD reader sees the composite: the top layer centred over the bottom one
        let d = dac_codecs::decode(&bytes, Default::default()).unwrap();
        assert_eq!((d.width, d.height), (4, 2));
        assert_eq!(d.xmp.as_deref(), Some("<x:xmpmeta/>"));
        let px = d.to_srgb8();
        assert!(px.data[0][0] < px.data[1][0], "left column is the dark bottom layer, centre the bright top one");
    }

    #[test]
    fn hostile_presets_are_refused() {
        let mut p = EditorPreset { name: "x".into(), ..Default::default() };
        p.bit_depth = 12;
        assert!(p.validate().is_err());
        let p = EditorPreset { name: "x".into(), naming: "../evil".into(), ..Default::default() };
        assert!(p.validate().is_err());
        let p = EditorPreset { name: "x".into(), color_space: "lab".into(), ..Default::default() };
        assert!(p.validate().is_err());
        assert!(decode_tiff16(b"II*\0garbage").is_err());
    }

    /// P6.2: a hand-edited or damaged `editors.json` never panics: every entry either fails to
    /// parse or is checked like a preset saved through `editIn.savePreset`.
    #[test]
    fn hostile_editors_json_never_panics() {
        let values = [r#""""#, r#""../x""#, r#""{file}{file}""#, r#""\u0000""#, "1e308", "-1", "null", "[]", "{}", r#""a\nb""#, "255", "65536"];
        let keys = ["name", "app", "args", "format", "colorSpace", "bitDepth", "mode", "naming", "stack", "control"];
        let extras = ["", r#","args":["{file}","-x"]"#, r#","control":{"port":99999,"tokenFile":"","root":"/"}"#];
        let mut n = 0;
        for (i, k) in keys.iter().enumerate() {
            for v in values {
                for extra in extras {
                    let doc = format!(r#"[{{"name":"p{i}","{k}":{v}{extra}}}]"#);
                    let Ok(list) = serde_json::from_str::<Vec<EditorPreset>>(&doc) else { continue };
                    for p in list {
                        n += 1;
                        if p.validate().is_ok() {
                            let _ = command_line(&p.app, &p.args, std::path::Path::new("/tmp/x y.tif"));
                        }
                    }
                }
            }
        }
        assert!(n > 20, "{n}");
    }
}
