use super::render::{RenderJob, RenderResult};
use super::source::{PreviewLoader, SourceLevel};
use super::{Arc, Deserialize, Hash128, Hasher128, Histogram, Photo, PhotoId, PreviewCache, Rendered, Rgba8, Serialize, Source, SourceInfo};

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
    /// Where a decoded [`Self::embedded`] preview is kept in memory, so stepping back to a photo
    /// shows its camera JPEG again without reading and decoding it (culling goes back and forth):
    /// the cache (memory only, never its disk) and the key for this photo and edge.
    pub embedded_cache: Option<(Arc<PreviewCache>, Hash128)>,
    /// Whether [`Self::embedded_cache`] keeps the decoded preview on disk too: a grid thumbnail,
    /// so a folder seen before fills in from small files at once instead of every raw being read
    /// and its camera JPEG decoded again (the loupe's large stand-ins stay in memory).
    pub embedded_on_disk: bool,
    /// Import tiles need this many pixels when a render can provide them. `None` accepts any
    /// camera JPEG immediately (ordinary transient stand-ins and preview-only imports).
    /// An undersized JPEG remains the fallback if the render fails or is unavailable.
    pub minimum_embedded_edge: Option<usize>,
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
            frame: None,
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
            frame: None,
        };
        let rendered = |image: Rgba8| {
            let histogram = Histogram::of_srgb8(&image);
            Ok(Rendered { image, histogram, deep: None })
        };
        for (cache, k) in &self.cached {
            if let Some(img) = cache.get(*k) {
                return done(rendered(Arc::unwrap_or_clone(img)), Some(QuickSource::Cached));
            }
        }
        let mut embedded_fallback = None;
        if let Some((path, loader, edge)) = &self.embedded {
            let kept = self.embedded_cache.as_ref().and_then(|(cache, k)| if self.embedded_on_disk { cache.get(*k) } else { cache.get_memory(*k) });
            let image = match kept {
                Some(img) => Some(Arc::unwrap_or_clone(img)),
                None => {
                    let image = loader(path, *edge);
                    if let (Some(image), Some((cache, k))) = (&image, &self.embedded_cache) {
                        if self.embedded_on_disk {
                            cache.put_deferred(*k, Arc::new(image.clone()));
                        } else {
                            cache.put_memory(*k, Arc::new(image.clone()));
                        }
                    }
                    image
                }
            };
            if let Some(image) = image {
                if self.minimum_embedded_edge.is_none_or(|minimum| image.width.max(image.height) >= minimum) {
                    return done(rendered(image), Some(QuickSource::Embedded));
                }
                embedded_fallback = Some(image);
            }
        }
        if let Some(job) = self.small {
            let r = job.run();
            if r.rendered.is_ok() || embedded_fallback.is_none() {
                return RenderResult { request_id, key, quick: Some(QuickSource::Small), ..r };
            }
        }
        if let Some(image) = embedded_fallback {
            return done(rendered(image), Some(QuickSource::Embedded));
        }
        done(Err("no quick preview".into()), None)
    }
}

/// The thumbnail cache's key for the camera JPEG standing for the thumbnail of cache key `k`.
pub(super) fn embedded_thumb_key(k: Hash128) -> Hash128 {
    Hasher128::new().str("embedded-thumb").u64((k.0 >> 64) as u64).u64(k.0 as u64).finish()
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
