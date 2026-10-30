use super::render::{RenderJob, RenderResult};
use super::source::{PreviewLoader, SourceLevel};
use super::{Arc, Deserialize, Hash128, Histogram, Photo, PhotoId, PreviewCache, Rendered, Rgba8, Serialize, Source, SourceInfo};

/// Where a [`QuickJob`]'s stand-in image came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QuickSource {
    /// The photo's own develop render for its current settings (cached view or thumbnail render).
    Cached,
    /// The camera's embedded JPEG (unedited raws).
    Embedded,
    /// A thumbnail-level develop render made for the occasion.
    Small,
}

/// Something to show *now* for a photo while its real render is prepared. Tried in order: cached
/// renders, the embedded preview of an unedited raw, a thumbnail-level render.
#[derive(Clone)]
pub struct QuickJob {
    pub request_id: u64,
    pub photo: PhotoId,
    pub key: u64,
    /// Cached renders to try first, in order: (cache, key).
    pub cached: Vec<(Arc<PreviewCache>, Hash128)>,
    /// Embedded preview of an unedited raw: (path, loader, max edge).
    pub embedded: Option<(String, PreviewLoader, usize)>,
    /// Last resort: run this thumbnail job.
    pub small: Option<Box<RenderJob>>,
}

impl QuickJob {
    /// The stand-in, from the first source that has one. A panic inside is this job's failure
    /// (see [`RenderJob::run`]).
    pub fn run(self) -> RenderResult {
        let (request_id, photo, key) = (self.request_id, self.photo, self.key);
        crate::guard::catch("the render", || self.run_unguarded()).unwrap_or_else(|e| RenderResult {
            request_id,
            source_key: None,
            photo,
            level: SourceLevel::Thumb,
            key,
            rendered: Err(e),
            gpu_fallback: None,
            loaded: None,
            quick: None,
            display: None,
        })
    }

    fn run_unguarded(self) -> RenderResult {
        let (photo, key) = (self.photo, self.key);
        let request_id = self.request_id;
        let done = |rendered: Result<Rendered, String>, quick| RenderResult {
            request_id,
            source_key: None,
            photo,
            level: SourceLevel::Thumb,
            key,
            rendered,
            gpu_fallback: None,
            loaded: None,
            quick,
            display: None,
        };
        let rendered = |image: Rgba8| {
            let histogram = Histogram::of_srgb8(&image);
            Ok(Rendered { image, histogram, deep: None, alpha: None })
        };
        for (cache, k) in &self.cached {
            if let Some(img) = cache.get(*k) {
                return done(rendered(Arc::unwrap_or_clone(img)), Some(QuickSource::Cached));
            }
        }
        if let Some((path, loader, edge)) = &self.embedded
            && let Some(image) = loader(path, *edge)
        {
            return done(rendered(image), Some(QuickSource::Embedded));
        }
        if let Some(job) = self.small {
            let r = job.run();
            return RenderResult { request_id, key, quick: Some(QuickSource::Small), ..r };
        }
        done(Err("no quick preview".into()), None)
    }
}

/// What identifies a photo's pixels for caching: its content hash, else its source.
pub fn content_key(p: &Photo) -> String {
    match (&p.content_hash, &p.source) {
        (Some(h), _) => h.clone(),
        (None, Source::Demo { scene }) => format!("demo:{scene}"),
        (None, Source::File { path }) => format!("file:{path}:{}", p.file_size),
    }
}

pub fn source_info(p: &Photo) -> SourceInfo {
    // Procedural demo scenes are scene-referred HDR (like raw files): use the filmic tone map.
    // (They are drawn afresh at every size, so they have no original size.)
    if matches!(p.source, Source::Demo { .. }) {
        return SourceInfo { raw: true, ..Default::default() };
    }
    // the thumbnail and preview levels are smaller copies: noise reduction works in the original's pixels (#960)
    let native_long = Some(p.width.max(p.height) as usize).filter(|&n| n > 0);
    SourceInfo { native_long, ..header_info(p) }
}

fn header_info(p: &Photo) -> SourceInfo {
    // A raw shown from its embedded preview is a rendered (display-referred) JPEG: relative white
    // balance and the display tone curve, like any other rendered file.
    if p.develops_raw() {
        let relative = p.relative_wb();
        let (temp, tint) = if relative { (6500.0, 0.0) } else { p.as_shot_wb.unwrap_or((5500.0, 0.0)) };
        SourceInfo {
            raw: true,
            as_shot_temp: temp,
            as_shot_tint: tint,
            lens: p.embedded_lens,
            relative_wb: relative,
            // (a fitted-look raw edited in Kelvin: its camera's matrices give the scale, see `files::WbScaleOf`)
            legacy_relative_wb: p.fitted_look() && !relative,
            ..Default::default()
        }
    } else {
        SourceInfo::default()
    }
}
