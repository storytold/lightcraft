//! An in-process [`Session`] that answers the control-channel methods the MCP tools use, so the
//! MCP server (and the CLI) work without a window.

use std::path::Path;

use dac_engine::Session;
use dac_engine::catalog::PhotoId;
use dac_raster::Rgba8;
use serde_json::{Value, json};

use crate::backend::{Backend, ProgressHook};

/// File extensions recognised as photos when expanding folders.
pub const PHOTO_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "tif", "tiff", "webp", "dng", "cr2", "cr3", "nef", "arw", "raf", "orf", "rw2", "rwl", "raw", "pef", "srw", "psd", "jxl",
    "gif", "bmp", "avif", // containers LightCraft cannot decode but imports as preview only (their embedded JPEG)
    "iiq", "crw", "mrw", "x3f", "kdc", "mos", "erf", "3fr", "fff",
];

/// Headless backend: a [`Session`] with filesystem hooks.
pub struct Headless {
    pub session: Session,
    progress: Option<ProgressHook>,
}

impl Drop for Headless {
    /// Snapshot a persistent library on exit (a no-op for in-memory sessions).
    fn drop(&mut self) {
        let _ = self.session.close_library();
    }
}

impl Default for Headless {
    fn default() -> Self {
        Self::new(Session::new().with_fs().with_default_denoise_models().with_default_face_models())
    }
}

impl Headless {
    pub fn new(session: Session) -> Self {
        Self { session, progress: None }
    }

    /// A headless session with the procedurally generated demo library.
    pub fn demo() -> Self {
        Self::new(Session::with_demo().with_fs().with_default_denoise_models().with_default_face_models())
    }

    fn photo_or_active(&self, p: &Value) -> Result<PhotoId, String> {
        p.get("id").and_then(Value::as_u64).map(PhotoId).or(self.session.active()).ok_or_else(|| "no photo (import one or pass `id`)".to_string())
    }

    /// Render `id` (or the active photo) so its long edge is at most `size` pixels.
    pub fn render(&mut self, p: &Value, default_size: u64) -> Result<Rgba8, String> {
        let id = self.photo_or_active(p)?;
        let size = p.get("size").or(p.get("longEdge")).and_then(Value::as_u64).unwrap_or(default_size).clamp(16, 16384) as usize;
        Ok(self.session.render_now(id, size, size)?.image)
    }

    /// The UI command `app.export`, emulated with the same parameters as the desktop app
    /// (see `dac_engine::export::ExportOptions::from_json`, plus `ids`, `dir`, `path`).
    /// With `path` and no `format`, the format follows the path's extension.
    fn export(&mut self, p: &Value) -> Result<Value, String> {
        use dac_engine::export::{Destination, ExportFormat, ExportOptions, Resize, prepare_batch, run_batch};
        let p = &self.session.export_params(p)?;
        let mut opts = ExportOptions::from_params(p).map_err(|e| e.to_string())?;
        if !ExportOptions::has_size_param(p) {
            opts.resize = Some(Resize::long_edge(3000));
        }
        let exact = p.get("path").and_then(Value::as_str);
        if let (Some(path), None) = (exact, p.get("format")) {
            let ext = Path::new(path).extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
            opts.format = ExportFormat::parse(&ext).unwrap_or(ExportFormat::Png);
        }
        let ids: Vec<_> = match p.get("ids").and_then(Value::as_array) {
            Some(a) => a.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect(),
            // like the desktop app: the selection (in grid order), else the given / active photo
            None if p.get("id").is_none() && self.session.selection.ids.len() > 1 => {
                let sel: std::collections::HashSet<_> = self.session.selection.ids.iter().copied().collect();
                self.session.visible_cloned().into_iter().filter(|id| sel.contains(id)).collect()
            }
            None => vec![self.photo_or_active(p)?],
        };
        let dir = p.get("dir").and_then(Value::as_str).unwrap_or("");
        let write = &mut dac_engine::export::write_file;
        let items = prepare_batch(&mut self.session, &ids, &opts)?;
        let total = items.len();
        let mut hook = self.progress.take();
        let mut cancelled = false;
        let files = run_batch(
            items,
            &opts,
            &Destination { dir: dir.to_string(), exact: exact.map(str::to_string) },
            write,
            &|path| Path::new(path).exists(),
            true,
            &mut |done, name| {
                cancelled = hook.as_mut().is_some_and(|h| !h(done, total, name));
                !cancelled
            },
        )?;
        if !cancelled && let Some(hook) = hook.as_mut() {
            cancelled = !hook(total, total, "");
        }
        if cancelled {
            return Err("cancelled".into());
        }
        // Single-photo exports also report path/width/height at the top level (back-compat).
        let mut out = files.first().cloned().unwrap_or_else(|| json!({}));
        out["files"] = json!(files);
        Ok(out)
    }
}

impl Backend for Headless {
    fn set_progress(&mut self, hook: Option<ProgressHook>) {
        self.progress = hook;
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let p = if params.is_null() { json!({}) } else { params };
        match method {
            "engine.execute" | "ui.menu.invoke" | "command" => {
                let id = p.get("command").or(p.get("id")).and_then(Value::as_str).ok_or("missing `command`")?;
                let params = p.get("params").cloned().filter(|v| !v.is_null()).unwrap_or(json!({}));
                if id == "app.export" {
                    return self.export(&params);
                }
                self.session.execute(id, &params).map_err(|e| e.to_string())
            }
            "engine.commands" => {
                let mut v: Vec<Value> = self.session.commands().into_iter().map(|c| serde_json::to_value(c).unwrap_or_default()).collect();
                v.push(json!({"id": "app.export", "label": "Export Now", "menu": [], "shortcut": null,
                    "params": "{path?: output file (.jpg/.png/.tif/.webp/.avif/.dng) | dir?, ids?, preset?, format?: jpeg|png|tiff|webp|avif|dng|original, longEdge?|shortEdge?|width?|height?|megapixels?|percent? (default longEdge 3000; longEdge 0 = full size), dontEnlarge?, ppi?, quality?: 1..100, limitKb?, colorSpace?: srgb|displayP3|adobeRgb|proPhoto|rec2020, bitDepth?: 8|10|16|32, sharpen?: none|screen|matte|glossy, sharpenAmount?: low|standard|high, metadata?: all|allExceptCamera|copyright|none, removeLocation?, naming?, startNumber?, subfolder?, conflict?: unique|overwrite|skip, tiffCompression?: none|lzw|zip, dngCompression?: lossless|deflate|uncompressed, watermark?: text | {text?, vertical? (upright columns, right to left), size? (text height, 0.005..0.5 of the short edge; default 0.035), opacity? (0..1; 0.7), anchor?: topLeft|top|topRight|left|center|right|bottomLeft|bottom|bottomRight, inset? (margin, 0..0.4 of the short edge; 0.025), color? [r,g,b] sRGB, shadow?, image? (graphic drawn instead of the text), imageWidth? (0.01..1 of the photo's width; 0.2)}} — an unknown parameter, an out-of-range watermark size or a value of the wrong kind is an error, not a default",
                    "enabled": self.session.active().is_some()}));
                Ok(Value::Array(v))
            }
            "ui.render" => {
                let img = self.render(&p, 1600)?;
                match p.get("path").and_then(Value::as_str) {
                    Some(path) => {
                        self.session.check_write_target(path)?;
                        write_image(Path::new(path), &img, 92)?;
                        Ok(json!({"path": path, "width": img.width, "height": img.height}))
                    }
                    None => Ok(json!({"width": img.width, "height": img.height})),
                }
            }
            "app.export" => self.export(&p),
            m if m.starts_with("ui.") || m == "app.quit" => {
                Err(format!("`{m}` needs the desktop app: start `lightcraft --control 7980` and run the MCP server with `--connect`"))
            }
            other => Err(format!("unknown method `{other}`")),
        }
    }

    fn has_ui(&self) -> bool {
        false
    }

    fn describe(&self) -> String {
        "headless".into()
    }
}

/// Expand folders (recursively, sorted; bounded, see `dac_engine::walk`) into photo files
/// and make paths absolute.
pub fn expand_paths(paths: &[String]) -> Vec<String> {
    let absolute = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().to_string();
    let photo = |p: &Path| p.extension().is_some_and(|e| PHOTO_EXTENSIONS.contains(&e.to_string_lossy().to_lowercase().as_str()));
    let mut out = Vec::new();
    for p in paths {
        let path = Path::new(p);
        if path.is_dir() {
            let w = dac_engine::walk::files_in(path, None, dac_engine::walk::Limits::default(), photo);
            out.extend(w.files.iter().map(|f| absolute(f)));
        } else {
            // Explicit files are kept even with unknown extensions (the probe decides).
            out.push(absolute(path));
        }
    }
    out
}

/// Encode by extension: `.png` (default), `.jpg`/`.jpeg` (quality), `.tif`/`.tiff`, `.webp` (lossless),
/// `.avif` — via the shared export encoder (embeds an sRGB profile).
pub fn encode_image(ext: &str, img: &Rgba8, quality: u8) -> Result<Vec<u8>, String> {
    use dac_engine::export::{ExportFormat, ExportOptions};
    let format = ExportFormat::parse(ext).unwrap_or(ExportFormat::Png);
    dac_engine::export::encode_image(img, &ExportOptions { format, quality: quality.clamp(1, 100), ..Default::default() })
}

/// Encode by the path's extension and write the file.
pub fn write_image(path: &Path, img: &Rgba8, quality: u8) -> Result<(), String> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
    let bytes = encode_image(&ext, img, quality)?;
    dac_engine::export::write_file(&path.to_string_lossy(), &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `app.export` without `ids` exports the selection (as the desktop app does), else the active photo.
    #[test]
    fn export_follows_the_selection() {
        let dir = std::env::temp_dir().join(format!("lc-mcp-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut h = Headless::demo();
        let ids: Vec<u64> = h.session.visible_cloned().iter().take(3).map(|p| p.0).collect();
        h.call("engine.execute", json!({"command": "library.select", "params": {"ids": ids}})).unwrap();
        let r = h
            .call("engine.execute", json!({"command": "app.export", "params": {"dir": dir.to_string_lossy(), "longEdge": 64, "format": "png"}}))
            .unwrap();
        assert_eq!(r["files"].as_array().map(Vec::len), Some(3), "{r}");
        // one photo selected: that one
        h.call("engine.execute", json!({"command": "library.select", "params": {"ids": [ids[0]]}})).unwrap();
        let r = h
            .call("engine.execute", json!({"command": "app.export", "params": {"dir": dir.to_string_lossy(), "longEdge": 64, "format": "png"}}))
            .unwrap();
        assert_eq!(r["files"].as_array().map(Vec::len), Some(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The export file-name template takes the Rename Photos tokens.
    #[test]
    fn export_naming_tokens() {
        let dir = std::env::temp_dir().join(format!("lc-mcp-export-naming-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut h = Headless::demo();
        let ids: Vec<u64> = h.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
        for (id, title) in ids.iter().zip(["Harbour", "Hills"]) {
            h.call("engine.execute", json!({"command": "photo.setMeta", "params": {"ids": [id], "title": title}})).unwrap();
            h.call("engine.execute", json!({"command": "photo.rate", "params": {"ids": [id], "rating": 4}})).unwrap();
        }
        let params = json!({"dir": dir.to_string_lossy(), "ids": ids, "longEdge": 32, "format": "png", "naming": "{title}_{rating}star_{seq:2}", "startNumber": 5});
        let r = h.call("engine.execute", json!({"command": "app.export", "params": params})).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        names.sort();
        assert_eq!(names, ["Harbour_4star_05.png", "Hills_4star_06.png"], "{r}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
