//! Render to DNG (Lightroom Classic 15.5), split so the desktop app can run the heavy part off the
//! UI thread: [`prepare`] (on the session: checks the params and sets each photo's render up),
//! [`RenderDngJob::run`] (no session: renders, encodes and writes each DNG next to its original),
//! [`finish`] (on the session: adds each DNG to the library stacked on top of its original and
//! selects them). `photo.renderToDng` runs all three in a row; the desktop app hands `run` to a
//! worker with a row in the activity stack.

use std::path::Path;

use lcc_catalog::{PhotoId, Source};
use serde_json::{Value, json};

use crate::export::{Conflict, Destination, ExportFormat, ExportOptions, PreparedExport, run_batch};
use crate::{EngineError, Result, Session};

pub const COMMAND: &str = "photo.renderToDng";

/// Most photos one Render to DNG takes (each becomes a full-size float DNG and a library photo).
pub const MAX_PHOTOS: usize = 500;

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: COMMAND.into(), msg: msg.into() }
}

/// The renders of one Render to DNG, set up from the session ([`prepare`]).
pub struct RenderDngJob {
    /// Per photo: its id, the folder its DNG goes to, its prepared render.
    items: Vec<(PhotoId, String, PreparedExport)>,
    opts: ExportOptions,
}

/// What [`RenderDngJob::run`] did with one photo: the DNG's path, or why there is none.
#[derive(Clone, Debug)]
pub struct Rendered {
    pub original: PhotoId,
    pub result: std::result::Result<String, String>,
}

/// Check `p` (`ids?`, `colorSpace?`, `dir?`) and set up each target photo's render.
pub fn prepare(s: &mut Session, p: &Value) -> Result<RenderDngJob> {
    let mut ids = s.targets(p)?;
    ids.dedup();
    if ids.is_empty() {
        return Err(bad("no active photo"));
    }
    if ids.len() > MAX_PHOTOS {
        return Err(bad(format!("{} photos selected: render at most {MAX_PHOTOS} at a time", ids.len())));
    }
    use lcc_pipeline::OutputSpace as S;
    let space = match p.get("colorSpace").and_then(Value::as_str).unwrap_or("proPhoto") {
        "srgb" => S::Srgb,
        "displayP3" => S::DisplayP3,
        "adobeRgb" => S::AdobeRgb,
        "rec2020" => S::Rec2020,
        "prophoto" | "proPhoto" => S::ProPhoto,
        c => return Err(bad(format!("colorSpace {c:?}: use proPhoto, rec2020, adobeRgb, displayP3 or srgb"))),
    };
    let opts = ExportOptions {
        format: ExportFormat::RenderedDng,
        color_space: space,
        naming: "{name}-Rendered".into(),
        conflict: Conflict::Unique,
        ..Default::default()
    };
    let mut items = Vec::with_capacity(ids.len());
    for id in ids {
        let ph = s.catalog.photo(id).ok_or_else(|| bad(format!("no photo {}", id.0)))?;
        let dir = match &ph.source {
            Source::File { path } => Path::new(path).parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default(),
            Source::Demo { .. } => match p.get("dir").and_then(Value::as_str) {
                Some(d) => d.to_string(),
                None => return Err(bad("a generated demo photo has no folder: give `dir`")),
            },
        };
        let item = crate::export::prepare_export(s, id, &opts, 1).map_err(bad)?;
        items.push((id, dir, item));
    }
    Ok(RenderDngJob { items, opts })
}

impl RenderDngJob {
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Render and write each DNG (never over an existing file). `progress(done)` is asked before
    /// each photo; `false` stops there (cancelled). Returns the outcomes and whether it was cancelled.
    pub fn run(
        self,
        write: &mut dyn FnMut(&str, &[u8]) -> std::result::Result<(), String>,
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> (Vec<Rendered>, bool) {
        let mut out = Vec::with_capacity(self.items.len());
        for (i, (original, dir, item)) in self.items.into_iter().enumerate() {
            if !progress(i) {
                return (out, true);
            }
            let to = Destination { dir, exact: None };
            let files = run_batch(vec![item], &self.opts, &to, write, &|path| Path::new(path).exists(), true, &mut |_, _| true);
            let result = files.and_then(|files| {
                let f = files.first().ok_or("nothing was written")?;
                if let Some(e) = f.get("error").and_then(Value::as_str) {
                    return Err(e.to_string());
                }
                f.get("path")
                    .and_then(Value::as_str)
                    .filter(|p| p.ends_with(".dng"))
                    .map(str::to_string)
                    .ok_or_else(|| "nothing was written".to_string())
            });
            out.push(Rendered { original, result });
        }
        (out, false)
    }
}

/// Add each written DNG to the library stacked (expanded) on top of its original and select them →
/// `{copies: [{path, id, original}], failed: [{id, error}], cancelled}`. A single photo that failed
/// is an error.
pub fn finish(s: &mut Session, done: Vec<Rendered>, cancelled: bool) -> Result<Value> {
    let single = done.len() == 1 && !cancelled;
    let mut copies = Vec::new();
    let mut failed = Vec::new();
    for r in done {
        let added = r.result.and_then(|path| {
            let imported = s.execute("library.import", &json!({"paths": [path]})).map_err(|e| e.to_string())?;
            let new = imported["imported"].get(0).and_then(Value::as_u64).ok_or("the DNG could not be added")?;
            let _ = s.execute("stack.group", &json!({"ids": [new, r.original.0], "top": new, "collapsed": false}));
            Ok(json!({"path": path, "id": new, "original": r.original.0}))
        });
        match added {
            Ok(c) => copies.push(c),
            Err(e) if single => return Err(bad(e)),
            Err(e) => failed.push(json!({"id": r.original.0, "error": e})),
        }
    }
    let new: Vec<PhotoId> = copies.iter().filter_map(|c| c["id"].as_u64()).map(PhotoId).collect();
    if !new.is_empty() {
        s.selection = crate::Selection { active: new.first().copied(), ids: new };
    }
    Ok(json!({"copies": copies, "failed": failed, "cancelled": cancelled}))
}
