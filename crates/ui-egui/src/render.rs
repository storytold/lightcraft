//! Background rendering: engine [`RenderJob`]s run on a [`JobPool`]; results become textures.
//!
//! Each visible thing that needs pixels is a *slot* (a grid thumbnail, the loupe, the "before"
//! image…). Requests are deduplicated per slot (a newer request replaces a queued older one) and
//! prioritised (loupe first, then on-screen thumbnails, then prefetch); thumbnails scrolled far
//! out of view are dropped from the queue. Thumbnail jobs hit the engine's preview cache (memory +
//! disk) before rendering. On wasm the jobs run inline, one per frame, unless the host installs a
//! [`RenderOffload`] (the browser build's Web Workers): then queued jobs are handed to it as it has
//! room, and its results are collected every frame.
//!
//! Stand-ins ([`QuickJob`]s, once per photo and settings): [`Slot::Preview`] holds what the loupe
//! shows until [`Slot::Main`] has the photo's render (cached view render, embedded camera JPEG or
//! a thumbnail render); [`Slot::ThumbQuick`] holds a raw's embedded preview in the grid until its
//! rendered thumbnail arrives (a cached thumbnail found by the quick job becomes the
//! [`Slot::Thumb`] texture directly).

use std::collections::HashMap;

use lcc_catalog::PhotoId;
use lcc_engine::Session;
use lcc_engine::media::{QuickJob, QuickSource, RenderJob, RenderResult};
use lcc_engine::pipeline::StageCache;
use lcc_preview::JobPool;
use lcc_raster::Histogram;
use std::sync::Arc;

/// The histogram a texture keeps: the loupe's (`Main`, and the `Preview` standing in for it) with its waveform
/// planes, which only the Histogram panel draws, every other slot without them (they are most of a histogram's
/// size, and thumbnails are many).
fn keep_waveform(slot: Slot, h: lcc_raster::Histogram) -> lcc_raster::Histogram {
    if matches!(slot, Slot::Main | Slot::Preview) { h } else { h.without_waveform() }
}

/// How many loupe renders [`Renderer::loupe_log`] keeps.
pub const LOUPE_LOG_LEN: usize = 512;

/// A whole-frame loupe render that replaced what was on screen.
#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoupeRender {
    /// When it reached the screen (ms, wall clock: [`crate::now_ms`]).
    pub at_ms: f64,
    /// Pixels of the texture.
    pub width: usize,
    pub height: usize,
    /// A drag's draft (else a full-quality render).
    pub draft: bool,
    /// Time on the worker.
    pub render_ms: f64,
    /// From the request to the screen.
    pub latency_ms: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    Thumb(PhotoId),
    /// A thumbnail's stand-in (embedded preview of an unedited raw).
    ThumbQuick(PhotoId),
    Main,
    /// A zoomed view while a slider drags: the window of the frame that is on screen, rendered in
    /// one draft job with its own stage cache (each tile apart would redo every stage per tile) and
    /// drawn over `Main` until the tiles of the final look arrive. By pane ([`PANE_AFTER`]…).
    Window(u8),
    /// The loupe's stand-in until `Main` has the photo.
    Preview,
    Before,
    Compare(u8),
    /// Background preparation of a neighbouring photo (no texture: its decoded source and view
    /// render are cached by the engine). 0 = the nearest ahead, 1 = the nearest behind; 2 = the
    /// open photo's full-resolution source, for a zoom to 1:1; 3 and up = the photos prepared
    /// further each way (Settings ▸ Performance ▸ Prepare photos in the background).
    Prefetch(u8),
    /// The loupe's stand-in for a photo the user is likely to step to next (the camera JPEG of a
    /// raw as shot, or a cached or latest render of an edited one), made ahead in the direction of
    /// travel so a step shows it at once: [`Renderer::request_quick`] takes the texture over into
    /// [`Slot::Preview`].
    QuickAhead(u8),
    /// The loupe zoomed in on a raw as shot: its camera JPEG at full size, drawn under the window
    /// render until that arrives (a focus check at 100 % without waiting for the raw).
    QuickFull,
    /// A variant thumbnail (profile / preset browsers), by its job key ([`Session::variant_job`]).
    /// Kept in a bounded LRU ([`VARIANT_TEXTURES`]).
    Variant(u64),
    /// The loupe while hovering a preset or profile: the photo with the hovered look (nothing is
    /// committed).
    Hover,
    /// An import candidate's thumbnail in the import review dialog (by index).
    Import(u32),
    /// A duplicate group member's thumbnail in the Find Duplicates window (by index).
    Duplicates(u32),
    /// The second window's view of the active photo.
    Second,
    /// A tile of a zoomed view (see [`crate::region::TileCache`]): the pane it is shown in
    /// ([`PANE_AFTER`]…) and its place in the grid. Its result goes to [`Renderer::tiles`], not
    /// [`Renderer::textures`].
    Tile {
        pane: u8,
        col: u16,
        row: u16,
    },
    /// A slideshow slide (the one up, the one fading out, the next ones prepared in advance; see
    /// [`crate::panels::slideshow`]): a photo's, or a title screen.
    Slide(lcc_engine::slideshow::Item),
    /// The small composed slide in Settings ▸ Slideshow (the active photo with the layout being
    /// edited).
    SlidePreview,
    /// A slider drag's live HDR histogram: a small full-quality render of an HDR edit, beside the
    /// drafts (which have no HDR histogram), with no texture ([`Renderer::live_hdr_histogram`]).
    HdrHistogram,
    /// The Histogram panel's own small render of its photo, when no view of it carries what the
    /// panel's mode draws (a grid thumbnail has no waveform, a cached one no histogram), with no
    /// texture ([`Renderer::panel_histogram`]).
    PanelHistogram,
    /// An HDR render (linear float, highlights above SDR white) of an HDR edit, for an HDR display
    /// ([`crate::HdrOutput`]), for one view ([`HDR_LOUPE`]…): its result waits in
    /// [`Renderer::hdr_view_result`] for the upload.
    HdrView(u8),
}

/// The views an HDR edit is shown in HDR in ([`Slot::HdrView`], [`crate::HdrOutput::upload`]):
/// the loupe's whole frame, its Before side, the second window, the zoomed window of a pane
/// (`HDR_ZOOMED + pane`, see [`PANE_AFTER`]…) and the Compare / Reference photos
/// (`HDR_COMPARE + i`).
pub const HDR_LOUPE: u8 = 0;
pub const HDR_BEFORE: u8 = 1;
pub const HDR_SECOND: u8 = 2;
pub const HDR_COMPARE: u8 = 8;
pub const HDR_ZOOMED: u8 = 32;
/// The first [`crate::HdrOutput::upload`] slot of the zoomed panes' recent windows
/// ([`HDR_WINDOW_RING`] each, after the views' own slots): a window kept for going back to it
/// has a texture of its own, so a new window never overwrites one still drawn.
const HDR_RING_SLOTS: u8 = 64;
/// The view whose frame upload slot `slot` ([`Renderer::hdr_upload_slot`]) holds.
pub fn hdr_slot_view(slot: u8) -> u8 {
    match slot.checked_sub(HDR_RING_SLOTS) {
        Some(o) => HDR_ZOOMED.saturating_add(o / HDR_WINDOW_RING),
        None => slot,
    }
}
/// Upload slots per zoomed pane: the window shown, the ones kept ([`HDR_WINDOWS_KEPT`]) and
/// room for the next.
const HDR_WINDOW_RING: u8 = 8;
/// Recent HDR windows kept per zoomed pane besides the one shown (zoom out and back in, or pan
/// back: shown at once), within [`HDR_WINDOWS_BYTES`] together.
const HDR_WINDOWS_KEPT: usize = 4;
const HDR_WINDOWS_BYTES: usize = 256 << 20;

/// The most thumbnail decode failures remembered ([`Renderer::thumb_failures`]): a library of
/// nothing but unreadable files cannot grow the map forever; once full, it is cleared and the
/// next visit of a cell retries (and re-records) its file once.
const THUMB_FAILURES_MAX: usize = 16_384;
/// Zoomed panes that can show at once (Before / After, the two Compare panes).
const HDR_ZOOMED_PANES: u64 = 2;
/// The kept windows may take no more than this share of the GPU's memory (1 / n): the rest is
/// for the whole-frame textures, the GPU render pipeline and everything else on screen.
const HDR_CACHE_SHARE: u64 = 4;

/// Whether the GPU has room for the zoomed panes' recent HDR windows ([`Renderer::set_hdr_cache`])
/// on a screen of a given size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HdrCacheFit {
    /// GPU memory the kept windows can take, in bytes.
    pub need: u64,
    /// GPU memory there is (`None`: unknown).
    pub have: Option<u64>,
}

impl HdrCacheFit {
    /// The kept windows fit in their share of the GPU's memory (assumed when it can't be read).
    pub fn fits(&self) -> bool {
        self.have.is_none_or(|have| self.need <= have / HDR_CACHE_SHARE)
    }
}

/// [`HdrCacheFit`] for a screen of `screen_px` physical pixels and a GPU with `have` bytes: each
/// zoomed pane keeps [`HDR_WINDOWS_KEPT`] windows the size of the screen with the margin a zoomed
/// view renders around it, as `Rgba16Float`, up to [`HDR_WINDOWS_BYTES`].
pub fn hdr_cache_fit(screen_px: [f32; 2], have: Option<u64>) -> HdrCacheFit {
    use crate::region::{MAX_SPAN, SNAP};
    let side = |v: f32| {
        let px = if v.is_finite() { v.clamp(0.0, 1e6) as usize } else { 0 };
        px.saturating_add(2 * SNAP).div_ceil(SNAP).saturating_mul(SNAP).min(MAX_SPAN)
    };
    let window = (side(screen_px[0]) as u64).saturating_mul(side(screen_px[1]) as u64).saturating_mul(8);
    let per_pane = window.saturating_mul(HDR_WINDOWS_KEPT as u64).min(HDR_WINDOWS_BYTES as u64);
    HdrCacheFit { need: per_pane.saturating_mul(HDR_ZOOMED_PANES), have }
}

/// A view's HDR texture ([`Slot::HdrView`]), drawn in place of (or over) its 8-bit render.
#[derive(Clone, Copy, Debug)]
pub struct HdrView {
    pub photo: PhotoId,
    /// The [`Slot::HdrView`] job key it was rendered for.
    pub key: u64,
    pub tex: egui::TextureId,
    pub size: [usize; 2],
    /// The part of the frame it shows (`None`: all of it), where a zoomed view draws it.
    pub window: Option<lcc_pipeline::PixelWindow>,
    /// The [`crate::HdrOutput::upload`] slot its texture lives in.
    pub slot: u8,
}

/// What a view's HDR frame covers: a frame held while its update renders must show the same
/// part of the photo in the same shape (a crop, rotation or flip drops it).
#[derive(Clone, Copy, PartialEq)]
struct HdrGeometry {
    photo: PhotoId,
    full: Option<(usize, usize)>,
    window: Option<lcc_pipeline::PixelWindow>,
    /// The crop applied (`None`: the crop isn't, as in the crop tool).
    crop: Option<lcc_develop::Crop>,
    orientation: lcc_geom::Orientation,
}

/// The panes a zoomed view's tiles and drag windows belong to: the loupe (its After and Before
/// sides) and the Compare / Reference views' two photos.
pub const PANE_AFTER: u8 = 0;
pub const PANE_BEFORE: u8 = 1;
/// The first Compare / Reference pane (`PANE_COMPARE + i` for [`Slot::Compare`]`(i)`).
pub const PANE_COMPARE: u8 = 2;

/// Where a [`Slot::Window`] render sits: the photo, the zoomed frame, the window of it rendered,
/// and the develop settings' hash it was asked for with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowGeom {
    pub photo: PhotoId,
    pub full: (usize, usize),
    pub window: lcc_engine::pipeline::PixelWindow,
    pub look: u64,
    /// Rendered at full quality (a high quality drag's update, or the window after a drag), not
    /// a draft: at rest it stands for the tiles it covers.
    pub full_quality: bool,
}

/// Full-quality whole-frame loupe renders ([`Slot::Main`]) kept after the view moved on, newest
/// last: the crop tool's whole photo and the cropped view of the same edit (pressing C changes
/// nothing in the edit, so C on and off shows the kept render at once instead of rendering again).
pub const KEPT_VIEWS: usize = 2;

pub struct Tex {
    pub key: u64,
    pub photo: PhotoId,
    pub tex: egui::TextureHandle,
    pub size: [usize; 2],
    pub histogram: Option<Histogram>,
    pub ms: f64,
    /// Set for stand-ins: where the image came from.
    pub quick: Option<QuickSource>,
    /// CPU copy of the pixels (only with [`Renderer::keep_pixels`]; for headless screenshots).
    pub pixels: Option<std::sync::Arc<egui::ColorImage>>,
    /// What went wrong without failing the render (e.g. a plug-in filter skipped).
    pub warnings: Vec<String>,
    /// Showing a render that stayed on the GPU ([`ScreenTextures`]): drawn by this id instead of `tex`.
    pub native: Option<egui::TextureId>,
    /// Where the picture lies in its photo and the look it has ([`lcc_engine::placement`]): the
    /// loupe draws it cut to the photo's current crop.
    pub placement: Option<lcc_engine::placement::Placement>,
}

impl Tex {
    /// The texture to draw.
    pub fn id(&self) -> egui::TextureId {
        self.native.unwrap_or_else(|| egui::TextureHandle::id(&self.tex))
    }
}

/// Draws renders that stayed on the GPU ([`lcc_engine::media::ScreenFrame`]): the window's
/// renderer registers their textures. Installed by a host whose GPU renderer runs on the window's
/// device (the desktop app); without one, every render comes back as pixels.
pub trait ScreenTextures {
    /// Show `frame` under `id` (one this returned before, updated in place) or a new id; `None`
    /// when it can't be shown.
    fn show(&mut self, frame: &lcc_engine::media::ScreenFrame, id: Option<egui::TextureId>) -> Option<egui::TextureId>;
    /// Release `id` (and the texture it shows).
    fn free(&mut self, id: egui::TextureId);
    /// The pixels `frame` holds, read back from the GPU: only for a headless screenshot of a view
    /// that shows it ([`Renderer::cpu_textures`]). `None` when they can't be read (the view is
    /// then left out of the picture).
    fn read(&self, frame: &lcc_engine::media::ScreenFrame) -> Option<egui::ColorImage> {
        let _ = frame;
        None
    }
}

/// Runs render jobs outside this thread's job pool (the browser build: Web Workers, each with its
/// own wasm instance). The [`Renderer`] keeps the queue (priorities, per-slot de-duplication) and
/// hands jobs over one at a time.
pub trait RenderOffload {
    /// Start `job` if an executor is free; otherwise give it back (`Some`: it stays queued).
    fn try_start(&mut self, slot: Slot, job: RenderJob) -> Option<RenderJob>;
    /// Finished jobs since the last call: (slot, result, run time in ms).
    fn finished(&mut self) -> Vec<(Slot, RenderResult, f64)>;
}

/// A job's result as the pool hands it back: the render and its texture pixels, converted on the
/// worker (`None` for prefetches and failures). A 10 MP zoom window's conversion took a few ms of
/// the UI thread on every slider step.
struct Finished {
    result: RenderResult,
    color: Option<Arc<egui::ColorImage>>,
    /// An HDR view's float samples as its texture's texels, and the headroom they were made for.
    texels: Option<(Vec<u16>, f32)>,
}

/// A finished [`Slot::HdrView`] render waiting for its upload.
pub struct HdrResult {
    pub photo: PhotoId,
    pub key: u64,
    /// Its size in pixels.
    pub size: [usize; 2],
    /// Its linear float samples; `None` when the GPU made its texels instead
    /// ([`lcc_engine::media::RenderJob::hdr_texels`]).
    pub deep: Option<lcc_pipeline::DeepImage>,
    /// The part of the frame it shows ([`HdrView::window`]).
    pub window: Option<lcc_pipeline::PixelWindow>,
    /// Its texels, made on the render worker ([`crate::HdrOutput::upload_texels`]).
    pub texels: Option<(Vec<u16>, f32)>,
}

/// Immutable identity captured when a request is submitted, not when its result is collected.
struct Requested {
    id: u64,
    source: Option<lcc_catalog::Source>,
}

struct Queued {
    slot: Slot,
    priority: u32,
    seq: u64,
    job: RenderJob,
}

/// The texture of a render: its pixels as egui wants them. Renders are opaque, so the common case
/// is a plain copy; only a pixel with alpha pays for premultiplying. (The generic conversion, on
/// the UI thread, was most of the time a 5 MP zoom window took to show.)
pub(crate) fn color_image(img: &lcc_raster::Rgba8) -> egui::ColorImage {
    let pixels = img
        .data
        .iter()
        .map(|p| if p[3] == 255 { egui::Color32::from_rgb(p[0], p[1], p[2]) } else { egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]) })
        .collect();
    egui::ColorImage { size: [img.width, img.height], source_size: egui::vec2(img.width as f32, img.height as f32), pixels }
}

/// How far over the budget the open photo's window and before caches may run before they are cut.
const HARD_FACTOR: usize = 4;

/// Which stage caches to clear, in order, so that the rest fit `budget` bytes.
///
/// What nothing is looking at goes first, the cheapest to rebuild and least reused first (hover,
/// compare, second window, background work). The open photo's own views are what a slider drag
/// reuses on every tick, and a cache cleared every frame is rebuilt every frame (a drag on a
/// 2800 px canvas got 3.6x slower), so they stay — counted and reported — up to
/// [`HARD_FACTOR`] times the budget; past that (a runaway: 8:1 on a huge canvas, both sides of
/// Before/After) the before side's and the zoom window's go, the before side first. The
/// whole-frame view's cache is never named. Empty caches are never named.
///
/// An HDR view's cache ([`Slot::HdrView`]) lives as long as its 8-bit view's (a drag reuses it
/// as much), but the HDR caches together hold no more than `budget` on top: past it they go,
/// the idle ones first and the loupe's last.
pub(crate) fn stage_trim_order(sizes: &[(Slot, usize)], budget: usize) -> Vec<Slot> {
    let view_slot = |s: Slot| match s {
        Slot::HdrView(HDR_LOUPE) => Slot::Main,
        Slot::HdrView(HDR_BEFORE) => Slot::Before,
        Slot::HdrView(HDR_SECOND) => Slot::Second,
        Slot::HdrView(v) if v >= HDR_ZOOMED => Slot::Window(v - HDR_ZOOMED),
        Slot::HdrView(v) if v >= HDR_COMPARE => Slot::Compare(v - HDR_COMPARE),
        _ => s,
    };
    let idle_rank = |s: Slot| match view_slot(s) {
        Slot::Hover => 0,
        Slot::Compare(_) => 2,
        Slot::Second => 3,
        _ => 5,
    };
    let view_rank = |s: Slot| match view_slot(s) {
        Slot::Window(PANE_BEFORE) => Some(0),
        Slot::Before => Some(1),
        Slot::Window(_) => Some(2),
        _ => None,
    };
    let is_view = |s: Slot| matches!(view_slot(s), Slot::Main) || view_rank(s).is_some();
    let mut held: usize = sizes.iter().map(|(_, b)| *b).fold(0, usize::saturating_add);
    let mut out = Vec::new();
    let mut idle: Vec<(Slot, usize)> = sizes.iter().copied().filter(|(s, b)| *b > 0 && !is_view(*s)).collect();
    idle.sort_by_key(|(s, _)| idle_rank(*s));
    for (slot, bytes) in idle {
        if held <= budget {
            break;
        }
        held = held.saturating_sub(bytes);
        out.push(slot);
    }
    let hard = budget.saturating_mul(HARD_FACTOR);
    let mut views: Vec<(Slot, usize, u8)> = sizes.iter().filter_map(|(s, b)| view_rank(*s).filter(|_| *b > 0).map(|r| (*s, *b, r))).collect();
    views.sort_by_key(|(_, _, r)| *r);
    for (slot, bytes, _) in views {
        if held <= hard {
            break;
        }
        held = held.saturating_sub(bytes);
        out.push(slot);
    }
    let hdr_rank = |s: Slot| match s {
        Slot::HdrView(HDR_LOUPE) => 5,
        Slot::HdrView(v) if v == HDR_ZOOMED + PANE_AFTER => 4,
        Slot::HdrView(HDR_BEFORE) => 3,
        Slot::HdrView(v) if v == HDR_ZOOMED + PANE_BEFORE => 2,
        Slot::HdrView(HDR_SECOND) => 1,
        _ => 0,
    };
    let mut hdr: Vec<(Slot, usize)> = sizes.iter().copied().filter(|(s, b)| *b > 0 && matches!(s, Slot::HdrView(_)) && !out.contains(s)).collect();
    let mut hdr_held = hdr.iter().map(|(_, b)| *b).fold(0, usize::saturating_add);
    hdr.sort_by_key(|(s, _)| hdr_rank(*s));
    for (slot, bytes) in hdr {
        if hdr_held <= budget {
            break;
        }
        hdr_held = hdr_held.saturating_sub(bytes);
        out.push(slot);
    }
    out
}

/// The share of the memory budget the per-view stage caches (the loupe's, the zoom window's, the
/// before / hover renders') may hold together before the idle ones are cleared (see
/// [`stage_trim_order`] for the open photo's own): they are the decoded-image sized buffers that the
/// engine's own caches do not count.
const STAGE_BUDGET_SHARE: usize = 4;

/// Variant thumbnails kept as textures (LRU), when nothing asks for more.
pub const VARIANT_TEXTURES: usize = 96;
/// The most a view can raise that to ([`Renderer::want_variants`]): a screenful of small faces is a few hundred pictures of
/// a few tens of kilobytes each.
pub const MAX_VARIANT_TEXTURES: usize = 800;
/// Priority of variant thumbnail jobs: below on-screen grid thumbnails and the loupe.
const VARIANT_PRIORITY: u32 = 6;

/// Whether a render of `s` solves a content-aware fill (a Fill spot).
fn has_fill(s: &lcc_develop::DevelopSettings) -> bool {
    s.spots.iter().any(|sp| matches!(sp.mode, lcc_develop::SpotMode::Fill))
}

pub struct Renderer {
    #[cfg(test)]
    pub(crate) thumb_jobs_built: usize,
    requests: HashMap<Slot, Requested>,
    #[cfg(test)]
    failure_messages: Vec<String>,
    /// Request id of the pixels each interactive view (loupe, before, hover) shows: a draft that
    /// finishes after a newer request was made still replaces older pixels, so a long slider drag
    /// keeps updating the loupe instead of waiting for the drag to pause.
    shown_ids: HashMap<Slot, u64>,
    /// Requests made before this id belong to a library or preview cache that is gone.
    epoch: u64,
    preview_generation: Option<(std::sync::Weak<lcc_preview::PreviewCache>, u64)>,
    /// Last thumbnail inputs; weak identities do not retain photo histories or force deep clones.
    /// (photo, size bucket, job key, whether the request went through an embedded stand-in)
    thumb_inputs: HashMap<PhotoId, (std::sync::Weak<lcc_catalog::Photo>, usize, u64, bool)>,
    /// Photos whose grid thumbnail is the camera's embedded preview itself (no render behind it).
    embedded_thumbs: HashMap<PhotoId, (std::sync::Weak<lcc_catalog::Photo>, usize, u64)>,
    pool: JobPool<Slot, Finished>,
    /// Jobs run elsewhere (see [`RenderOffload`]); `queue` holds the ones not started yet.
    offload: Option<Box<dyn RenderOffload>>,
    queue: Vec<Queued>,
    seq: u64,
    /// Slot → (key, priority) of the request in flight (queued or running).
    pending: HashMap<Slot, (u64, u32)>,
    /// Pending requests whose settings hold a content-aware fill (slot → key): solving one can take
    /// seconds, so while any is out the activity stack says so ([`Self::fill_row`]).
    fill_jobs: HashMap<Slot, u64>,
    /// The "Filling spots" row, while [`Self::fill_jobs`] isn't empty.
    fill_row: Option<lcc_engine::activity::TaskGuard>,
    pub textures: HashMap<Slot, Tex>,
    /// Recent full-quality whole-frame renders ([`KEPT_VIEWS`], least recently shown first).
    kept_views: std::collections::VecDeque<Tex>,
    /// Request ids of the loupe's ([`Slot::Main`]) draft requests still out.
    main_drafts: std::collections::HashSet<u64>,
    /// When each `Main` request still out was made (request id → ms, [`crate::now_ms`]), for
    /// [`Self::loupe_log`].
    main_asked: HashMap<u64, f64>,
    /// The loupe's last full-quality request per view ([`Slot::Main`], [`Slot::Window`]): its photo
    /// and the flag that tells it a draft replaced it ([`lcc_engine::media::RenderJob::superseded`]).
    full_requests: HashMap<Slot, (PhotoId, Arc<std::sync::atomic::AtomicBool>)>,
    /// The latest whole-frame loupe renders that reached the screen, oldest first (`ui.inspect` →
    /// `perf.loupeRenders`): how sharp the view was while a slider dragged and how soon after.
    pub loupe_log: std::collections::VecDeque<LoupeRender>,
    /// Render time of the loupe's last full-quality (not draft) whole-frame render.
    pub last_full_main_ms: f64,
    /// Keys of the full-quality (not draft) whole-frame renders among the textures and `kept_views`.
    full_views: std::collections::HashSet<u64>,
    /// The last [`Slot::HdrHistogram`] result: (photo, job key, histogram).
    pub live_hdr_histogram: Option<(PhotoId, u64, Histogram)>,
    /// The last [`Slot::PanelHistogram`] result: (photo, job key, histogram with its waveform).
    pub panel_histogram: Option<(PhotoId, u64, Histogram)>,
    /// The [`Slot::HdrView`] renders not yet uploaded: view → (photo, job key, linear float image).
    pub hdr_view_result: HashMap<u8, HdrResult>,
    /// The headroom HDR views are encoded for on the worker ([`Self::set_hdr_headroom`]).
    hdr_headroom: Option<f32>,
    /// Each view's uploaded HDR texture.
    pub hdr_view: HashMap<u8, HdrView>,
    /// A zoomed view's recent HDR windows besides the one shown, newest first.
    hdr_kept: HashMap<u8, Vec<HdrView>>,
    /// How many recent windows a zoomed view keeps ([`Self::set_hdr_cache`]).
    hdr_windows_kept: usize,
    /// The zoomed panes' upload slots that hold a texture ([`Self::hdr_unused_slots`]).
    hdr_ring_textures: std::collections::BTreeSet<u8>,
    hdr_geometry: HashMap<u8, HdrGeometry>,
    /// How long each view's latest HDR render took (ms): what a high quality drag times when the
    /// HDR texture is what shows it.
    pub hdr_view_ms: HashMap<u8, f64>,
    pub last_main_ms: f64,
    /// Jobs finished since start (for inspect/perf).
    pub completed: u64,
    /// Loupe ([`Slot::Main`]) renders asked for since start (a kept view shown again is none).
    pub main_requested: u64,
    /// Finished jobs thrown away because a newer request for their slot had replaced them.
    pub discarded: u64,
    /// Keep a CPU copy of every texture so the UI can be rasterized headlessly (set when the app
    /// is driven by the control channel).
    pub keep_pixels: bool,
    /// Per-view intermediate results (loupe and "before"), so slider drags redo only what changed.
    stages: HashMap<Slot, Arc<StageCache>>,
    /// Slot → key of the last quick job requested for it (each is tried once).
    quick_tried: HashMap<Slot, u64>,
    /// Jobs that failed (unreadable or missing file…), by slot and job key, with the error and
    /// the photo: not requested again until the key changes (an edit, another size) — the grid
    /// asks every frame.
    failed: HashMap<Slot, (u64, String, u64, PhotoId)>,
    /// Thumbnail decode failures, by (photo, quick?) and job key, with the error (issue #448):
    /// an unsupported or malformed file is not decoded again on every visit. `failed` (above)
    /// stops counting once the catalog's revision moves on — which background thumbnail writes
    /// do all the time — so without this the same broken file was re-read, re-decoded and
    /// re-logged at every cell it appears in. A changed file (or edit) renders under another job
    /// key, which this never held, and so is tried again.
    thumb_failures: HashMap<(PhotoId, bool), (u64, String)>,
    /// Photos whose grid cell showed a failure or the photo's kept thumbnail while their original
    /// was known to be missing ([`Self::note_missing_original`]): the cell asks again once the
    /// original is back.
    missing_originals: std::collections::HashSet<PhotoId>,
    /// The availability generation ([`lcc_engine::availability::Availability::gone_generation`]) at
    /// which each photo's cell last asked again for an original found back
    /// ([`Self::retry_found_original`]): once per generation, so a file that keeps failing is not
    /// read on every frame.
    found_back: HashMap<PhotoId, u64>,
    /// The catalog revision seen at the last poll: a failure is retried once the catalog changes
    /// (a relinked or re-imported file).
    catalog_rev: u64,
    /// Prefetch slot → key of the last job submitted (each runs once).
    prefetched: HashMap<Slot, u64>,
    /// A neighbouring photo's finished prefetch, by prefetch slot: the very render the loupe asks
    /// for once that photo is shown, adopted then instead of rendered again ([`Self::prefetch`]).
    ready_views: HashMap<Slot, (RenderResult, Option<Arc<egui::ColorImage>>, f64)>,
    /// Adopted prefetches, collected as finished loupe renders by the next [`Self::collect`].
    adopted: Vec<(Slot, RenderResult, Option<Arc<egui::ColorImage>>, f64)>,
    /// Finished stand-ins made ahead ([`Slot::QuickAhead`]) held for a later frame: at most one big
    /// picture made ahead is uploaded per frame ([`BIG_UPLOAD_BYTES`]), so that a set of 1:1
    /// previews landing together never stalls the frame of a drag or a zoom.
    held: Vec<(Slot, RenderResult, Option<Arc<egui::ColorImage>>, f64)>,
    /// The frame a big picture made ahead was last uploaded in.
    big_upload_frame: Option<u64>,
    /// [`BIG_UPLOAD_BYTES`] (smaller in tests, whose context takes no texture over 2048 px).
    big_upload_bytes: usize,
    /// Variant key → frame it was last asked for (LRU of [`Slot::Variant`] textures).
    variant_used: HashMap<u64, u64>,
    /// How many variants the last frame's views showed at once (see [`Self::want_variants`]).
    variant_want: usize,
    /// Frames polled so far.
    frame: u64,
    /// Slideshow slides may be held ([`Self::release_slides`] has something to do).
    slides_live: bool,
    /// Stage caches cleared to stay in budget (see [`stage_trim_order`]).
    stages_trimmed: u64,
    /// A budget other than the memory budget's share (tests).
    #[cfg(test)]
    pub(crate) stage_budget_override: Option<usize>,
    /// Tiles of zoomed views, by what they show (see [`crate::region`]).
    pub tiles: crate::region::TileCache<Tex>,
    /// Tile slot → (job key, what it shows, window rendered, tile within it) of its request.
    tile_meta: HashMap<Slot, (u64, crate::region::TileKey, lcc_engine::pipeline::PixelWindow, lcc_engine::pipeline::PixelWindow)>,
    /// Request id → where a [`Slot::Window`] request's pixels go (a late draft is still shown, so
    /// each request keeps its own; bounded).
    window_req: HashMap<u64, WindowGeom>,
    /// Where each [`Slot::Window`] texture's pixels go.
    window_shown: HashMap<Slot, WindowGeom>,
    /// The monitor profile textures are made for (`app.displayProfile`; `None`: sRGB).
    display: Option<Arc<lcc_engine::display::Display>>,
    /// Since when nothing has been pending, and whether the GPU pool was trimmed since.
    #[cfg(not(target_arch = "wasm32"))]
    idle: Option<(std::time::Instant, bool)>,
    /// Bytes still pooled after the last idle trim ([`Self::trim_when_idle`]).
    #[cfg(not(target_arch = "wasm32"))]
    pooled_after_trim: u64,
    /// Surface is extended linear sRGB (macOS EDR / Windows scRGB): skip monitor profile
    /// conversion, let the compositor do it (P2-3).
    surface_is_extended_linear: bool,
    /// Draws GPU-resident renders; with it, interactive drafts of views stay on the GPU.
    screen: Option<Box<dyn ScreenTextures>>,
    /// The texture id each view slot shows GPU-resident renders under (one per slot, reused).
    natives: HashMap<Slot, egui::TextureId>,
    /// The GPU-resident render each of those shows, kept with [`Self::keep_pixels`]: a headless
    /// screenshot reads it back ([`Self::cpu_textures`]) instead of finding no pixels.
    native_frames: HashMap<Slot, lcc_engine::media::ScreenFrame>,
}

/// How many [`Slot::QuickAhead`] stand-ins the loupe keeps ready.
pub const AHEAD_SLOTS: u8 = 6;

/// A picture made ahead bigger than this (a 1:1 preview of a 45 MP photo is 180 MB) waits for a
/// frame without another such upload: each takes the UI thread some 50 ms to copy to the GPU.
pub const BIG_UPLOAD_BYTES: usize = 32 << 20;

/// The [`Slot::Prefetch`] of the open photo's full-resolution source, decoded ahead of a zoom to 1:1.
pub const FULL_SOURCE_PREFETCH: u8 = 2;

/// After this long without renders the GPU renderer's pool of recycled buffers is freed.
#[cfg(not(target_arch = "wasm32"))]
const IDLE_TRIM: std::time::Duration = std::time::Duration::from_secs(3);

/// What [`Renderer::trim_when_idle`] reads and does on the GPU: batch renders running, bytes of
/// recycled buffers pooled (both cards), and the trim itself.
#[cfg(not(target_arch = "wasm32"))]
struct IdleGpu<'a> {
    batch_jobs: &'a dyn Fn() -> usize,
    pooled: &'a dyn Fn() -> u64,
    trim: &'a dyn Fn(),
}

impl Slot {
    /// An interactive view of the open photo: slider drags redo only the stages they feed, and a
    /// draft that finishes late still replaces older pixels.
    pub fn is_view(self) -> bool {
        matches!(self, Slot::Main | Slot::Window(_) | Slot::Before | Slot::Hover | Slot::HdrHistogram | Slot::HdrView(_))
    }
}

impl Default for Renderer {
    fn default() -> Self {
        let threads = if cfg!(target_arch = "wasm32") { 0 } else { JobPool::<Slot, Finished>::default_threads().min(6) };
        Self::with_worker_threads(threads)
    }
}

/// Workers kept for the stand-ins ([`Slot::is_stand_in`]) on top of the others: the cheapest
/// pictures (a raw's embedded JPEG, a cached thumbnail or render) never wait for a worker while the
/// others render raws for seconds ("Rendering…" in the loupe and empty grid cells while every
/// worker decoded a 45 MP CR3).
const STAND_IN_WORKERS: usize = 2;

/// Renders that go to the second graphics card when there is one (`lcc_gpu::prefer_helper`): the
/// full-quality tiles of a 1:1 view. They come back as pixels for the screen either way, and on the
/// window's card their work held the window's frames up: presenting waited behind it, 100–1,100 ms
/// between frames while sliders dragged at 100 % (CF-ZOOM-FREEZE 2026-11-10; with the tiles on the
/// second card the slowest frames were 22–145 ms). The drag's drafts stay on the window's card.
/// With one card this changes nothing.
pub(crate) fn on_second_card(slot: Slot) -> bool {
    matches!(slot, Slot::Tile { .. })
}

/// Jobs below this priority are background work (the real thumbnails rendered behind the
/// stand-ins, [`crate::panels::grid::BACKGROUND_THUMB_PRIORITY`]): at most [`BACKGROUND_AT_ONCE`]
/// of them run at once, so a worker is always free for the photo on screen.
const BACKGROUND_BELOW: u32 = 4;
const BACKGROUND_AT_ONCE: usize = 2;

impl Slot {
    /// A stand-in picture: cheap to make (a raw's embedded JPEG, a cached picture), and what a view
    /// shows first, before any render ([`STAND_IN_WORKERS`]).
    pub fn is_stand_in(&self) -> bool {
        matches!(self, Slot::ThumbQuick(_) | Slot::Preview | Slot::QuickAhead(_))
    }
}

impl Renderer {
    fn pool(threads: usize) -> JobPool<Slot, Finished> {
        let pool = if threads == 0 { JobPool::new(0) } else { JobPool::with_fast_lane(threads, STAND_IN_WORKERS, Slot::is_stand_in) };
        pool.cap_background(BACKGROUND_BELOW, BACKGROUND_AT_ONCE);
        pool
    }

    /// Explicit worker count for hosts and bounded validation runs (one to six on native).
    /// Browser hosts continue to use inline jobs or their installed worker offload.
    pub fn with_worker_threads(threads: usize) -> Self {
        let threads = if cfg!(target_arch = "wasm32") { 0 } else { threads.clamp(1, 6) };
        Renderer {
            #[cfg(test)]
            thumb_jobs_built: 0,
            requests: HashMap::new(),
            #[cfg(test)]
            failure_messages: Vec::new(),
            shown_ids: HashMap::new(),
            epoch: 0,
            preview_generation: None,
            pool: Self::pool(threads),
            thumb_inputs: HashMap::new(),
            embedded_thumbs: HashMap::new(),
            offload: None,
            queue: Vec::new(),
            seq: 0,
            pending: HashMap::new(),
            fill_jobs: HashMap::new(),
            fill_row: None,
            textures: HashMap::new(),
            kept_views: std::collections::VecDeque::new(),
            main_drafts: std::collections::HashSet::new(),
            main_asked: HashMap::new(),
            full_requests: HashMap::new(),
            loupe_log: std::collections::VecDeque::new(),
            last_full_main_ms: 0.0,
            full_views: std::collections::HashSet::new(),
            live_hdr_histogram: None,
            panel_histogram: None,
            hdr_view_result: HashMap::new(),
            hdr_headroom: None,
            hdr_view: HashMap::new(),
            hdr_kept: HashMap::new(),
            hdr_windows_kept: HDR_WINDOWS_KEPT,
            hdr_ring_textures: std::collections::BTreeSet::new(),
            hdr_geometry: HashMap::new(),
            hdr_view_ms: HashMap::new(),
            last_main_ms: 0.0,
            completed: 0,
            main_requested: 0,
            discarded: 0,
            keep_pixels: false,
            stages: HashMap::new(),
            tiles: Default::default(),
            tile_meta: HashMap::new(),
            window_req: HashMap::new(),
            window_shown: HashMap::new(),
            quick_tried: HashMap::new(),
            failed: HashMap::new(),
            thumb_failures: HashMap::new(),
            missing_originals: std::collections::HashSet::new(),
            found_back: HashMap::new(),
            catalog_rev: 0,
            prefetched: HashMap::new(),
            ready_views: HashMap::new(),
            adopted: Vec::new(),
            held: Vec::new(),
            big_upload_frame: None,
            big_upload_bytes: BIG_UPLOAD_BYTES,
            variant_used: HashMap::new(),
            variant_want: 0,
            frame: 0,
            slides_live: false,
            stages_trimmed: 0,
            #[cfg(test)]
            stage_budget_override: None,
            display: None,
            #[cfg(not(target_arch = "wasm32"))]
            idle: None,
            #[cfg(not(target_arch = "wasm32"))]
            pooled_after_trim: 0,
            surface_is_extended_linear: false,
            screen: None,
            natives: HashMap::new(),
            native_frames: HashMap::new(),
        }
    }

    /// Set whether the surface is extended linear sRGB (macOS EDR / Windows scRGB). When true,
    /// skip monitor profile conversion and let the compositor do it (P2-3).
    pub fn set_surface_extended_linear(&mut self, extended: bool) {
        if self.surface_is_extended_linear != extended {
            self.surface_is_extended_linear = extended;
            // Forget all textures so they are re-rendered without the display profile.
            self.forget_all();
        }
    }
}

impl Renderer {
    /// Would asking for this thumbnail again (at `priority`) change nothing?
    /// The size bucket of the thumbnail this photo has or is getting, while the photo is unchanged.
    pub fn thumb_bucket_kept(&self, photo: &Arc<lcc_catalog::Photo>) -> Option<usize> {
        self.thumb_inputs.get(&photo.id).filter(|(old, ..)| old.as_ptr() == Arc::as_ptr(photo)).map(|(_, size, ..)| *size)
    }

    pub fn thumb_current(&self, photo: &Arc<lcc_catalog::Photo>, bucket: usize, priority: u32) -> bool {
        let Some(&(ref old, size, key, quick)) = self.thumb_inputs.get(&photo.id) else { return false };
        // a larger thumbnail serves a smaller request: two views of the same photo asking for different sizes (the
        // Slideshow and Print modules' filmstrip and their own pictures) would otherwise each replace the other's
        // and render it again on every frame
        if old.as_ptr() != Arc::as_ptr(photo) || size < bucket {
            return false;
        }
        let slot = Slot::Thumb(photo.id);
        if quick && !self.textures.contains_key(&slot) {
            // Stand-in path (`grid::request_thumb`): the embedded preview is still on its way, or
            // the real thumbnail is requested at the background priority whatever the caller's.
            let quick_slot = Slot::ThumbQuick(photo.id);
            if let Some(&(k, p)) = self.pending.get(&quick_slot)
                && k == key
            {
                return p == crate::panels::grid::stand_in_priority(priority) || !self.is_queued(quick_slot);
            }
            return !self.request_needed(slot, key, crate::panels::grid::BACKGROUND_THUMB_PRIORITY);
        }
        !self.request_needed(slot, key, priority)
    }

    /// Does a larger thumbnail of this photo, as it is now, already serve a request for `bucket`? It
    /// is shown, or rendering, at the larger size. Two views asking for one photo at different sizes
    /// (the slideshow or print preview and the navigator, say) otherwise replaced each other's request
    /// for the one thumbnail slot every frame: neither render was ever kept, and the photo rendered
    /// non-stop while the module was open. The larger size wins and the smaller view scales it down.
    pub fn thumb_covers(&self, photo: &Arc<lcc_catalog::Photo>, bucket: usize) -> bool {
        let Some(&(ref old, size, key, _)) = self.thumb_inputs.get(&photo.id) else { return false };
        old.as_ptr() == Arc::as_ptr(photo) && size > bucket && self.wanted(Slot::Thumb(photo.id)) == Some(key)
    }

    /// Remember the inputs of the thumbnail just requested (`quick`: through a stand-in).
    /// Whether the camera's embedded preview *is* this photo's thumbnail: its quick render arrived
    /// (so `Slot::ThumbQuick` holds the picture) and no edit has moved the photo on to a settings
    /// hash of its own. The grid stops here instead of decoding the raw behind a thumbnail the
    /// embedded JPEG already covers — see `grid::skips_the_real_render`.
    pub fn thumb_quick_is_final(&self, photo: &Arc<lcc_catalog::Photo>, bucket: usize, priority: u32) -> bool {
        let Some((old, size, key)) = self.embedded_thumbs.get(&photo.id) else { return false };
        let slot = Slot::ThumbQuick(photo.id);
        // a queued stand-in at another priority isn't settled yet: the grid moves it (both ways)
        let requeue =
            self.pending.get(&slot).is_some_and(|&(k, p)| k == *key && p != crate::panels::grid::stand_in_priority(priority) && self.is_queued(slot));
        old.as_ptr() == Arc::as_ptr(photo) && *size >= bucket && !requeue && !self.request_needed(slot, *key, priority)
    }

    /// Whether this photo's embedded-preview thumbnail is pending at the requested priority, or
    /// already running. A queued priority change must reach the grid's stand-in request.
    pub fn thumb_quick_pending(&self, photo: &Arc<lcc_catalog::Photo>, bucket: usize, priority: u32) -> bool {
        let Some((old, size, key, quick)) = self.thumb_inputs.get(&photo.id) else { return false };
        let slot = Slot::ThumbQuick(photo.id);
        old.as_ptr() == Arc::as_ptr(photo)
            && *size == bucket
            && *quick
            && self.pending.get(&slot).is_some_and(|&(k, p)| {
                // a queued stand-in follows the grid's priority both ways (scrolling demotes it too);
                // `request_quick` only reprioritizes it, so this never restarts work
                k == *key && (p == crate::panels::grid::stand_in_priority(priority) || !self.is_queued(slot))
            })
    }

    /// Remember that the camera's embedded preview is this photo's final thumbnail, so the grid's
    /// per-frame requests stop at it (the quick render's key, which [`Self::thumb_quick_is_final`]
    /// compares against).
    pub fn remember_embedded_thumb(&mut self, photo: &Arc<lcc_catalog::Photo>, bucket: usize, key: u64) {
        let slot = Slot::Thumb(photo.id);
        if self.pending.get(&slot).is_some_and(|(pending_key, _)| *pending_key != key) {
            // An edit can still be rendering when Undo restores this embedded-final thumbnail.
            // Retire its full-render identity before it can replace the correct stand-in.
            self.pending.remove(&slot);
            self.requests.remove(&slot);
            self.pool.reprioritize(|s, p| if *s == slot { None } else { Some(p) });
            self.queue.retain(|q| q.slot != slot);
        }
        self.remember_thumb(photo, bucket, key, true);
        self.embedded_thumbs.insert(photo.id, (Arc::downgrade(photo), bucket, key));
    }

    /// The original of photo `id`, whose grid cell shows a failure or the photo's kept thumbnail, is known to be
    /// missing: once it is back, [`Self::retry_found_original`] lets the cell ask again.
    pub fn note_missing_original(&mut self, id: PhotoId) {
        if self.missing_originals.len() >= THUMB_FAILURES_MAX && !self.missing_originals.contains(&id) {
            self.missing_originals.clear();
        }
        self.missing_originals.insert(id);
    }

    /// The original of photo `id` is there (`generation`: the availability's gone generation): if its grid cell
    /// shows the photo's kept thumbnail (`kept`), or a failure from while the original was missing, it asks again,
    /// once per generation. Both stand for the thumbnail's key, so nothing else would until a restart, a relink or
    /// an edit (issue #448). The kept picture stays on screen as a stand-in until the render replaces it.
    pub fn retry_found_original(&mut self, id: PhotoId, kept: bool, generation: u64) {
        if (!kept && !self.missing_originals.contains(&id)) || self.found_back.get(&id) == Some(&generation) {
            return;
        }
        self.missing_originals.remove(&id);
        if self.found_back.len() >= THUMB_FAILURES_MAX && !self.found_back.contains_key(&id) {
            self.found_back.clear();
        }
        self.found_back.insert(id, generation);
        for slot in [Slot::Thumb(id), Slot::ThumbQuick(id)] {
            self.failed.remove(&slot);
            self.quick_tried.remove(&slot);
            self.thumb_failures.remove(&(id, matches!(slot, Slot::ThumbQuick(_))));
        }
        self.thumb_inputs.remove(&id);
        self.embedded_thumbs.remove(&id);
        // (a kept picture shown as the thumbnail itself becomes its stand-in: the grid then asks for the real one)
        if self.textures.get(&Slot::Thumb(id)).is_some_and(|t| t.quick == Some(QuickSource::Kept)) {
            self.release(Slot::ThumbQuick(id));
            if let Some(t) = self.textures.remove(&Slot::Thumb(id)) {
                self.textures.insert(Slot::ThumbQuick(id), t);
            }
        }
    }

    pub fn remember_thumb(&mut self, photo: &Arc<lcc_catalog::Photo>, bucket: usize, key: u64, quick: bool) {
        if self.thumb_inputs.len() >= 1024 && !self.thumb_inputs.contains_key(&photo.id) {
            self.thumb_inputs.retain(|_, (p, _, _, _)| p.strong_count() > 0);
            if self.thumb_inputs.len() >= 1024
                && let Some(id) = self.thumb_inputs.keys().next().copied()
            {
                self.thumb_inputs.remove(&id);
            }
        }
        self.thumb_inputs.insert(photo.id, (Arc::downgrade(photo), bucket, key, quick));
    }

    /// Views that render straight into the display profile's primaries (wide gamut); everything
    /// else is rendered for sRGB and converted ([`lcc_engine::display::present`]).
    fn renders_for_display(slot: Slot) -> bool {
        matches!(slot, Slot::Main | Slot::Before | Slot::Hover | Slot::Compare(_) | Slot::Second | Slot::Slide(_))
    }

    /// `job` as [`Self::request`] would run it for `slot` (its key included): views render for
    /// the display profile in use.
    pub fn job_for(&self, slot: Slot, job: RenderJob) -> RenderJob {
        // When the surface is extended linear sRGB (macOS EDR / Windows scRGB), skip monitor
        // profile conversion: the compositor owns it (P2-3).
        if !Self::renders_for_display(slot) {
            return job;
        }
        let job = if self.surface_is_extended_linear { job } else { job.with_display(self.display.clone()) };
        // a drag's drafts are only drawn: they may stay on the GPU when the window can show them
        if self.screen.is_some() && job.request.quality == lcc_engine::pipeline::Quality::Draft { job.on_screen() } else { job }
    }

    /// Show GPU-resident renders through `screen` from now on (the desktop app, once its GPU
    /// renderer runs on the window's device).
    pub fn set_screen_textures(&mut self, screen: Box<dyn ScreenTextures>) {
        self.screen = Some(screen);
    }

    /// Whether GPU-resident renders can be shown.
    pub fn has_screen_textures(&self) -> bool {
        self.screen.is_some()
    }

    /// Release every GPU-resident texture shown (the textures that drew them fall back to their
    /// pixels, or nothing, until the next render).
    fn free_natives(&mut self) {
        if let Some(screen) = self.screen.as_mut() {
            for (_, id) in self.natives.drain() {
                screen.free(id);
            }
        }
        self.natives.clear();
        self.native_frames.clear();
        for t in self.textures.values_mut() {
            t.native = None;
        }
    }

    /// Replace every view drawn straight from the GPU with its pixels, read back once (a screenshot:
    /// capturing native textures stopped the window presenting on Linux / X11). A view whose pixels
    /// can't be read keeps its GPU texture. GPU-resident drafts go on afterwards.
    pub fn views_to_pixels(&mut self) {
        let Some(screen) = self.screen.as_ref() else { return };
        let mut done = Vec::new();
        for (slot, t) in self.textures.iter_mut() {
            let (Some(_), Some(frame)) = (t.native, self.native_frames.get(slot)) else { continue };
            let Some(img) = screen.read(frame) else { continue };
            let img = Arc::new(img);
            t.tex.set(img.clone(), egui::TextureOptions::LINEAR);
            t.pixels = Some(img);
            t.native = None;
            done.push(*slot);
        }
        for slot in done {
            self.native_frames.remove(&slot);
            if let (Some(id), Some(screen)) = (self.natives.remove(&slot), self.screen.as_mut()) {
                screen.free(id);
            }
        }
    }

    /// Show the GPU-resident render `frame` (histogram `histogram`) in `slot`. `false` when it
    /// can't be shown: GPU-resident renders are then turned off and the view renders pixels again.
    fn show_frame(
        &mut self,
        ctx: &egui::Context,
        slot: Slot,
        r: &RenderResult,
        frame: &lcc_engine::media::ScreenFrame,
        histogram: Histogram,
        ms: f64,
    ) -> bool {
        let shown = self.screen.as_mut().and_then(|s| s.show(frame, self.natives.get(&slot).copied()));
        let Some(id) = shown else {
            log::warn!("render {slot:?}: a GPU-resident render can't be shown; rendering pixels");
            self.free_natives();
            self.screen = None;
            return false;
        };
        self.natives.insert(slot, id);
        if self.keep_pixels {
            self.native_frames.insert(slot, frame.clone());
        }
        let (size, histogram) = ([frame.width, frame.height], keep_waveform(slot, histogram));
        // a full-quality whole frame shown so far is kept for a return to it (Crop on and off), as
        // when pixels replace it
        if slot == Slot::Main
            && self.textures.get(&slot).is_some_and(|t| t.key != r.key && t.native.is_none() && self.full_views.contains(&t.key))
            && let Some(t) = self.textures.remove(&slot)
        {
            self.keep_view(t);
        }
        match self.textures.get_mut(&slot) {
            Some(t) => {
                t.native = Some(id);
                t.pixels = None;
                t.key = r.key;
                t.photo = r.photo;
                t.size = size;
                t.histogram = Some(histogram);
                t.warnings = Vec::new();
                t.ms = ms;
                t.quick = None;
                t.placement = r.placement;
            }
            None => {
                // (`tex` is never drawn while `native` is set)
                let blank = egui::ColorImage::filled([1, 1], egui::Color32::BLACK);
                let tex = ctx.load_texture(format!("{slot:?}"), blank, egui::TextureOptions::LINEAR);
                let t = Tex {
                    key: r.key,
                    photo: r.photo,
                    tex,
                    size,
                    histogram: Some(histogram),
                    ms,
                    quick: None,
                    pixels: None,
                    warnings: Vec::new(),
                    native: Some(id),
                    placement: r.placement,
                };
                self.textures.insert(slot, t);
            }
        }
        true
    }

    /// The display profile in use (id).
    pub fn display_id(&self) -> Option<u64> {
        self.display.as_ref().map(|d| d.id())
    }

    /// The display profile textures are made for (`None`: sRGB).
    pub fn display(&self) -> Option<&Arc<lcc_engine::display::Display>> {
        self.display.as_ref()
    }

    /// Show previews through `display` from now on (`app.displayProfile`): when it changes, every
    /// texture is made again for the new one.
    pub fn set_display(&mut self, display: Option<Arc<lcc_engine::display::Display>>) {
        if display.as_ref().map(|d| d.id()) != self.display_id() {
            self.forget_all();
        }
        self.display = display;
    }

    /// Run `job` for `slot` and make its image ready for `display` (on the worker thread). An
    /// HDR view's result is uploaded from its float samples, so it gets no 8-bit texture pixels,
    /// but those samples as texels for `headroom` (a few ms a frame the UI thread then doesn't spend).
    fn run_for(
        slot: Slot,
        job: impl FnOnce() -> RenderResult,
        display: &Option<Arc<lcc_engine::display::Display>>,
        headroom: Option<f32>,
    ) -> Finished {
        let mut r = job();
        if let Some(d) = display {
            lcc_engine::display::present(&mut r, d);
        }
        let hdr = matches!(slot, Slot::HdrView(_));
        let color = r.rendered.as_ref().ok().filter(|_| !hdr).map(|x| Arc::new(color_image(&x.image)));
        // (texels the GPU made, else encoded here from the floats)
        let gpu_texels = r.hdr_texels.take().filter(|_| hdr);
        let texels = gpu_texels.or_else(|| {
            let h = headroom.filter(|_| hdr)?;
            let deep = r.rendered.as_ref().ok()?.deep.as_ref()?;
            let lcc_pipeline::DeepSamples::F32(rgb) = &deep.samples else { return None };
            lcc_engine::hdr_texture::encode_rgba16f(rgb, deep.width, deep.height, h).ok().map(|t| (t, h))
        });
        Finished { result: r, color, texels }
    }

    /// Encode HDR views for `headroom` on the render worker from now on (`None`: on upload).
    pub fn set_hdr_headroom(&mut self, headroom: Option<f32>) {
        self.hdr_headroom = headroom;
    }

    /// Whether zoomed views keep their recent HDR windows to show again (the setting, where the
    /// GPU has room: [`hdr_cache_fit`]). Off, the ones kept go, and each pane holds only the window
    /// shown and the one on its way.
    pub fn set_hdr_cache(&mut self, on: bool) {
        self.hdr_windows_kept = if on { HDR_WINDOWS_KEPT } else { 0 };
        if !on {
            self.hdr_kept.clear();
        }
    }

    /// How many recent HDR windows a zoomed view keeps now.
    pub fn hdr_windows_kept(&self) -> usize {
        self.hdr_windows_kept
    }

    /// The zoomed panes' upload slots whose texture nothing shows or keeps any more: the host
    /// frees them ([`crate::HdrOutput::release`]), and they are forgotten here.
    pub fn hdr_unused_slots(&mut self) -> Vec<u8> {
        let used: std::collections::BTreeSet<u8> = self.hdr_view.values().chain(self.hdr_kept.values().flatten()).map(|v| v.slot).collect();
        let unused: Vec<u8> = self.hdr_ring_textures.difference(&used).copied().collect();
        for s in &unused {
            self.hdr_ring_textures.remove(s);
        }
        unused
    }

    fn request_needed(&self, slot: Slot, key: u64, priority: u32) -> bool {
        // A matching old texture must not mask a different newer request (e.g. undo).
        if let Some(&(k, p)) = self.pending.get(&slot) {
            // a queued job moves up for a more urgent ask, never down: two views asking for the same thumbnail at
            // different priorities (the Slideshow and Print modules and their filmstrip) took it in turns every frame
            return k != key || (p < priority && self.is_queued(slot));
        }
        if self.textures.get(&slot).is_some_and(|t| t.key == key) || self.failed.get(&slot).is_some_and(|f| f.0 == key && f.2 == self.catalog_rev) {
            return false;
        }
        // A thumbnail whose decode already failed for this very key (issue #448) is not asked
        // for again: the failure survives the slot's eviction and the catalog revision moving
        // on, which would otherwise re-request the same broken file every visit.
        if let Slot::Thumb(id) | Slot::ThumbQuick(id) = slot
            && self.thumb_failures.get(&(id, matches!(slot, Slot::ThumbQuick(_)))).is_some_and(|(k, _)| *k == key)
        {
            return false;
        }
        true
    }
    /// Show the kept whole-frame render made for `key` of `photo` ([`KEPT_VIEWS`]) in
    /// [`Slot::Main`], keeping the one shown there now. Whether there was one. (Not for HDR
    /// views: their HDR texture is not kept with the picture.)
    fn take_kept_view(&mut self, key: u64, photo: PhotoId) -> bool {
        if !self.hdr_view.is_empty() {
            return false;
        }
        let Some(i) = self.kept_views.iter().position(|t| t.key == key && t.photo == photo) else { return false };
        let Some(found) = self.kept_views.remove(i) else { return false };
        self.pending.remove(&Slot::Main);
        self.requests.remove(&Slot::Main);
        self.failed.remove(&Slot::Main);
        // (a render still out for the view is older than this one now)
        self.shown_ids.insert(Slot::Main, lcc_preview::next_tick());
        if let Some(old) = self.textures.insert(Slot::Main, found) {
            self.keep_view(old);
        }
        true
    }

    /// Keep `t`, a whole-frame render leaving [`Slot::Main`], if it is a full-quality one.
    fn keep_view(&mut self, t: Tex) {
        if !self.full_views.contains(&t.key) || !self.hdr_view.is_empty() {
            self.full_views.remove(&t.key);
            return;
        }
        self.kept_views.retain(|k| k.key != t.key);
        self.kept_views.push_back(t);
        while self.kept_views.len() > KEPT_VIEWS {
            if let Some(gone) = self.kept_views.pop_front() {
                self.full_views.remove(&gone.key);
            }
        }
    }

    /// Free a view slot's texture and stage cache (the view is no longer shown).
    pub fn release(&mut self, slot: Slot) {
        self.textures.remove(&slot);
        self.native_frames.remove(&slot);
        if let Some(id) = self.natives.remove(&slot)
            && let Some(screen) = self.screen.as_mut()
        {
            screen.free(id);
        }
        if slot == Slot::Main {
            self.kept_views.clear();
            self.full_views.clear();
        }
        self.stages.remove(&slot);
        self.window_shown.remove(&slot);
    }

    /// Run jobs through `offload` from now on (instead of the job pool / inline).
    pub fn set_offload(&mut self, offload: Box<dyn RenderOffload>) {
        self.offload = Some(offload);
    }

    fn is_queued(&self, slot: Slot) -> bool {
        self.queue.iter().any(|q| q.slot == slot) || self.pool.is_queued(slot)
    }

    /// Is the slot's current texture (or pending request) already for `key`?
    /// Key of what `slot` shows or is rendering next (the pending request wins).
    pub fn wanted(&self, slot: Slot) -> Option<u64> {
        self.pending.get(&slot).map(|p| p.0).or_else(|| self.textures.get(&slot).map(|t| t.key))
    }

    pub fn is_current(&self, slot: Slot, key: u64) -> bool {
        self.wanted(slot) == Some(key)
    }

    /// Retained HDR frames must cover the same region. Retire older requests on navigation.
    pub fn prepare_hdr_view(&mut self, view: u8, job: &RenderJob) {
        let r = &job.request;
        let geometry = HdrGeometry {
            photo: job.photo,
            // Whole-frame drafts can change resolution without changing their coverage.
            full: r.window.map(|_| (r.max_w, r.max_h)),
            window: r.window,
            crop: r.apply_crop.then_some(job.settings.crop),
            orientation: job.settings.orientation,
        };
        let old = self.hdr_geometry.get(&view).copied();
        if old != Some(geometry) {
            self.hdr_geometry.insert(view, geometry);
            // another part of the same frame (a pan): the frames there are stay, each drawn where
            // it belongs, until the new part's arrives; anything else they no longer show
            if old.is_none_or(|o| HdrGeometry { window: geometry.window, ..o } != geometry || o.window.is_none() || geometry.window.is_none()) {
                self.hdr_view.remove(&view);
                self.hdr_kept.remove(&view);
            }
            self.hdr_view_result.remove(&view);
            let slot = Slot::HdrView(view);
            self.requests.remove(&slot);
            self.pending.remove(&slot);
            self.shown_ids.insert(slot, lcc_preview::next_tick());
        }
    }

    /// View `view`'s HDR frame of `photo` to draw for the render `key`: that render's, or an
    /// older one while `key` is still rendering. Once nothing renders `key` (it failed, or was
    /// retired with no replacement) an older frame is stale and the view shows its 8-bit render.
    pub fn hdr_frame(&self, view: u8, photo: PhotoId, key: u64) -> Option<HdrView> {
        let held = self.hdr_view.get(&view).filter(|v| v.photo == photo)?;
        (held.key == key || self.pending.get(&Slot::HdrView(view)).is_some_and(|p| p.0 == key)).then_some(*held)
    }

    /// Show view `view`'s kept HDR frame for `key` again, if there is one (a zoomed view back at a
    /// place it showed): it becomes the frame shown, the one shown is kept, and nothing renders.
    pub fn reuse_hdr(&mut self, view: u8, key: u64) -> bool {
        let Some(kept) = self.hdr_kept.get_mut(&view) else { return false };
        let Some(i) = kept.iter().position(|v| v.key == key) else { return false };
        let frame = kept.remove(i);
        if let Some(shown) = self.hdr_view.insert(view, frame) {
            kept.insert(0, shown);
        }
        let slot = Slot::HdrView(view);
        self.requests.remove(&slot);
        self.pending.remove(&slot);
        self.hdr_view_result.remove(&view);
        self.shown_ids.insert(slot, lcc_preview::next_tick());
        true
    }

    /// The upload slot for view `view`'s next HDR frame, of `window`: the view's own, or for a
    /// zoomed pane the shown frame's when that is of the same window (a newer look of it: the
    /// older one is worth nothing kept), else one of its ring that no frame shown or kept uses
    /// (the oldest kept one's when all are).
    pub fn hdr_upload_slot(&mut self, view: u8, window: Option<lcc_pipeline::PixelWindow>) -> u8 {
        let Some(base) = view.checked_sub(HDR_ZOOMED).and_then(|pane| pane.checked_mul(HDR_WINDOW_RING)).and_then(|o| HDR_RING_SLOTS.checked_add(o))
        else {
            return view;
        };
        let Some(last) = base.checked_add(HDR_WINDOW_RING - 1) else { return view };
        let slot = self.ring_slot(view, window, base, last);
        if slot >= HDR_RING_SLOTS {
            self.hdr_ring_textures.insert(slot);
        }
        slot
    }

    fn ring_slot(&mut self, view: u8, window: Option<lcc_pipeline::PixelWindow>, base: u8, last: u8) -> u8 {
        if let Some(shown) = self.hdr_view.get(&view).filter(|v| v.window == window && (base..=last).contains(&v.slot)) {
            return shown.slot;
        }
        let used = |r: &Self, s: u8| {
            r.hdr_view.get(&view).is_some_and(|v| v.slot == s) || r.hdr_kept.get(&view).is_some_and(|k| k.iter().any(|v| v.slot == s))
        };
        if let Some(free) = (base..=last).find(|s| !used(self, *s)) {
            return free;
        }
        self.hdr_kept.get_mut(&view).and_then(|k| k.pop()).map_or(view, |v| v.slot)
    }

    /// View `view`'s new HDR frame, uploaded: it is shown from now on, and a zoomed pane keeps
    /// the one it replaces (of the same photo) for going back to it.
    pub fn adopt_hdr(&mut self, view: u8, frame: HdrView) {
        let Some(shown) = self.hdr_view.insert(view, frame) else { return };
        let limit = self.hdr_windows_kept;
        if view < HDR_ZOOMED || limit == 0 || shown.photo != frame.photo || shown.slot == frame.slot || shown.key == frame.key {
            return;
        }
        let kept = self.hdr_kept.entry(view).or_default();
        kept.retain(|v| v.key != shown.key && v.slot != frame.slot && v.window != frame.window);
        kept.insert(0, shown);
        let mut bytes = frame.size[0].saturating_mul(frame.size[1]).saturating_mul(8);
        kept.retain(|v| {
            bytes = bytes.saturating_add(v.size[0].saturating_mul(v.size[1]).saturating_mul(8));
            bytes <= HDR_WINDOWS_BYTES
        });
        kept.truncate(limit);
    }

    /// Request a render for `slot` (no-op if already current or pending at the same priority).
    pub fn request(&mut self, slot: Slot, job: RenderJob, priority: u32) {
        if matches!(slot, Slot::Slide(_)) {
            self.slides_live = true;
        }
        let mut job = self.job_for(slot, job);
        if slot == Slot::Main && self.textures.get(&slot).is_none_or(|t| t.key != job.key) && self.take_kept_view(job.key, job.photo) {
            return;
        }
        if !self.request_needed(slot, job.key, priority) {
            return;
        }
        self.pending.insert(slot, (job.key, priority));
        if has_fill(&job.settings) {
            self.fill_jobs.insert(slot, job.key);
        } else {
            self.fill_jobs.remove(&slot);
        }
        job.request_id = lcc_preview::next_tick();
        if slot == Slot::Main {
            self.main_requested += 1;
            if job.request.quality == lcc_pipeline::Quality::Draft {
                self.main_drafts.insert(job.request_id);
            }
            // (requests a drag superseded and that never came back: dropped, the map stays small)
            if self.main_asked.len() > 256
                && let Some(o) = self.main_asked.keys().copied().min()
            {
                self.main_asked.remove(&o);
            }
            self.main_asked.insert(job.request_id, crate::now_ms());
        }
        // The loupe's drafts never wait for its full render (on a photo just landed on, a slider
        // did nothing for 2–5 s while the first full render decoded and rendered): a draft starts
        // beside it, and the full render it replaces stops once its source is decoded (the draft
        // takes that decode over instead of decoding again). So does a full render of a photo the
        // view has left.
        let draft = job.request.quality == lcc_pipeline::Quality::Draft;
        let view = matches!(slot, Slot::Main | Slot::Window(_));
        let overtakes = draft && view;
        if view {
            if let Some((photo, flag)) = self.full_requests.get(&slot)
                && (draft || *photo != job.photo)
            {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            if !draft {
                let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
                job.superseded = Some(flag.clone());
                self.full_requests.insert(slot, (job.photo, flag));
            }
        }
        self.requests.insert(slot, Requested { id: job.request_id, source: Some(job.origin.clone()) });
        // the loupe stepping to a neighbour that was prepared: its render is there already
        // (its stages are not kept: a neighbour's would hold ~100 MB of device memory for the
        // first edit after a step only)
        if slot == Slot::Main
            && let Some(from) = self.ready_views.iter().find(|(_, (r, _, _))| r.key == job.key && r.photo == job.photo).map(|(s, _)| *s)
            && let Some((mut r, color, ms)) = self.ready_views.remove(&from)
        {
            r.request_id = job.request_id;
            self.prefetched.remove(&from);
            self.adopted.push((slot, r, color, ms));
            return;
        }
        // (an offload keeps its own per-view stage caches; the flag tells it to)
        let job = if slot.is_view() { job.with_stages(self.stages.entry(slot).or_default().clone()) } else { job };
        if self.offload.is_some() {
            self.seq += 1;
            self.queue.retain(|q| q.slot != slot);
            self.queue.push(Queued { slot, priority, seq: self.seq, job });
            self.dispatch(); // an idle worker starts now rather than next frame
            return;
        }
        let key = job.key;
        let background = matches!(slot, Slot::Thumb(_) | Slot::ThumbQuick(_) | Slot::Prefetch(_));
        let display = self.display.clone();
        let headroom = self.hdr_headroom;
        let run: lcc_preview::pool::Job<Finished> = Box::new(move || {
            // the neighbours prepared ahead take turns at the GPU behind the view's own renders
            // and keep one short submission queued at a time, so the window's frames slot in
            // between (`lcc_gpu::as_batch`); a thumbnail on screen does not wait for an
            // export's turn
            let prefetch = matches!(slot, Slot::Prefetch(_));
            let run = || match (background, prefetch) {
                (true, true) => lcc_engine::memory::in_background(|| lcc_engine::gpu::as_batch(|| job.run())),
                (true, false) => lcc_engine::memory::in_background(|| job.run()),
                _ if on_second_card(slot) => lcc_engine::gpu::prefer_helper(|| job.run()),
                _ => job.run(),
            };
            Self::run_for(slot, run, &display, headroom)
        });
        if overtakes {
            self.pool.submit_overtaking(slot, key, priority, run);
        } else {
            self.pool.submit(slot, key, priority, run);
        }
    }

    /// Request a stand-in for `slot` (once per job key). The loupe's [`Slot::Preview`] takes over
    /// a stand-in already made ahead for the same photo ([`Slot::QuickAhead`]) instead of making it
    /// again: shown in this frame when it is ready, or as soon as that job finishes.
    pub fn request_quick(&mut self, slot: Slot, mut job: QuickJob, priority: u32) {
        if self.quick_tried.get(&slot) == Some(&job.key) {
            if self.pending.get(&slot).is_some_and(|p| p.0 == job.key && p.1 != priority) {
                // Scrolling changes a stand-in's priority too. Keep the original closure and
                // request identity; running or completed work must never be started again.
                let mut queued = false;
                self.pool.reprioritize(|s, p| {
                    if *s == slot {
                        queued = true;
                        Some(priority)
                    } else {
                        Some(p)
                    }
                });
                if queued && let Some(p) = self.pending.get_mut(&slot) {
                    p.1 = priority;
                }
            }
            return;
        }
        // A stand-in whose decode already failed for this key (issue #448) is not tried again
        // after `quick_tried` lost the attempt (the cell was scrolled away and back).
        if let Slot::ThumbQuick(id) = slot
            && self.thumb_failures.get(&(id, true)).is_some_and(|(k, _)| *k == job.key)
        {
            return;
        }
        self.quick_tried.insert(slot, job.key);
        if slot == Slot::Preview && self.take_ahead(job.photo, job.key) {
            self.pending.remove(&slot);
            self.requests.remove(&slot);
            return;
        }
        self.pending.insert(slot, (job.key, priority));
        job.request_id = lcc_preview::next_tick();
        let source = job
            .small
            .as_ref()
            .map(|job| job.origin.clone())
            .or_else(|| job.embedded.as_ref().map(|(path, _, _)| lcc_catalog::Source::File { path: path.clone() }));
        self.requests.insert(slot, Requested { id: job.request_id, source });
        let key = job.key;
        let display = self.display.clone();
        let background = matches!(slot, Slot::QuickAhead(_));
        self.pool.submit(
            slot,
            key,
            priority,
            Box::new(move || {
                let run = || if background { lcc_engine::memory::in_background(|| job.run()) } else { job.run() };
                Self::run_for(slot, run, &display, None)
            }),
        );
    }

    /// Whether a [`Slot::QuickAhead`] stand-in of `photo` for `key` is made (or being made).
    pub fn made_ahead(&self, photo: PhotoId, key: u64) -> bool {
        self.textures.iter().any(|(s, t)| matches!(s, Slot::QuickAhead(_)) && t.photo == photo && t.key == key)
            || self.quick_tried.iter().any(|(s, k)| matches!(s, Slot::QuickAhead(_)) && *k == key)
    }

    /// Move a [`Slot::QuickAhead`] texture of `photo` made for `key` into [`Slot::Preview`].
    fn take_ahead(&mut self, photo: PhotoId, key: u64) -> bool {
        let Some(slot) = self.textures.iter().find(|(s, t)| matches!(s, Slot::QuickAhead(_)) && t.photo == photo && t.key == key).map(|(s, _)| *s)
        else {
            return false;
        };
        let Some(tex) = self.textures.remove(&slot) else { return false };
        self.quick_tried.remove(&slot);
        self.textures.insert(Slot::Preview, tex);
        true
    }

    /// Make the loupe's stand-ins for the photos it is likely to be stepped to next (see
    /// [`Slot::QuickAhead`]), most likely first: `jobs[0]` gets `priority`, each next one less.
    /// A slot keeps a photo for as long as the photo stays wanted (made or being made, it is not
    /// made again when the user steps and the photos shift places); slots holding photos not
    /// wanted any more are freed for the new ones, and their queued jobs dropped.
    pub fn look_ahead(&mut self, jobs: Vec<QuickJob>, priority: u32) {
        let slots: Vec<Slot> = (0..AHEAD_SLOTS).map(Slot::QuickAhead).collect();
        let held = |r: &Self, s: Slot| r.textures.get(&s).map(|t| t.key).or_else(|| r.quick_tried.get(&s).copied());
        let wanted: std::collections::HashSet<u64> = jobs.iter().map(|j| j.key).collect();
        let mut free = Vec::new();
        for &s in &slots {
            if held(self, s).is_none_or(|k| !wanted.contains(&k)) {
                self.textures.remove(&s);
                self.quick_tried.remove(&s);
                self.drop_queued(s);
                free.push(s);
            }
        }
        let have: std::collections::HashSet<u64> = slots.iter().filter_map(|&s| held(self, s)).collect();
        let mut free = free.into_iter();
        for (rank, job) in jobs.into_iter().enumerate() {
            if have.contains(&job.key) {
                continue;
            }
            let Some(slot) = free.next() else { break };
            self.request_quick(slot, job, priority.saturating_sub(rank as u32));
        }
    }

    /// Drop `slot`'s job if it is still queued (a running one finishes). Returns whether one was.
    pub fn drop_queued(&mut self, slot: Slot) -> bool {
        let dropped = self.pool.reprioritize(|s, p| if *s == slot { None } else { Some(p) });
        let before = self.queue.len();
        self.queue.retain(|q| q.slot != slot);
        let any = !dropped.is_empty() || self.queue.len() != before;
        if any {
            self.pending.remove(&slot);
            self.requests.remove(&slot);
        }
        any
    }

    /// Forget everything about `slot`: its texture, its queued job, and that its stand-in was asked
    /// for (so asking again makes it again).
    pub fn forget(&mut self, slot: Slot) {
        self.drop_queued(slot);
        self.pending.remove(&slot);
        self.requests.remove(&slot);
        self.quick_tried.remove(&slot);
        self.release(slot);
    }

    /// Render a slideshow title screen into `slot` (once per screen key, like [`Self::request`]):
    /// a slide with no photo, composed on a worker (on wasm: the job pool's inline runs, not the
    /// offload, which only knows photo renders).
    pub fn request_title(&mut self, slot: Slot, compose: Arc<lcc_engine::slide_title::TitleCompose>, priority: u32) {
        if matches!(slot, Slot::Slide(_)) {
            self.slides_live = true;
        }
        let key = compose.key();
        if !self.request_needed(slot, key, priority) {
            return;
        }
        self.pending.insert(slot, (key, priority));
        let request_id = lcc_preview::next_tick();
        self.requests.insert(slot, Requested { id: request_id, source: None });
        let display = self.display.clone();
        self.pool.submit(slot, key, priority, Box::new(move || Self::run_for(slot, || compose.result(request_id), &display, None)));
    }

    /// Prepare a photo in the background (a [`Slot::Prefetch`] job runs once per key; a newer one
    /// replaces it while queued). Memory stays bounded by the engine's source caches.
    pub fn prefetch(&mut self, slot: Slot, job: RenderJob, priority: u32) {
        // (made for the display as the loupe's would be, so its key is the one the loupe asks for)
        let mut job = self.job_for(Slot::Main, job);
        if self.prefetched.get(&slot) == Some(&job.key) {
            return;
        }
        self.prefetched.insert(slot, job.key);
        self.ready_views.remove(&slot);
        self.pending.insert(slot, (job.key, priority));
        job.request_id = lcc_preview::next_tick();
        self.requests.insert(slot, Requested { id: job.request_id, source: Some(job.origin.clone()) });
        let key = job.key;
        let display = self.display.clone();
        self.pool.submit(
            slot,
            key,
            priority,
            // (behind the view's own renders at the GPU, one short submission queued at a time)
            Box::new(move || Self::run_for(slot, || lcc_engine::memory::in_background(|| lcc_engine::gpu::as_batch(|| job.run())), &display, None)),
        );
    }

    /// Prepared-ahead jobs of the photos around the one on screen ([`Slot::Prefetch`]) queued or
    /// running. Not the open photo's full-resolution source ([`FULL_SOURCE_PREFETCH`]): a slow
    /// decode for a zoom that may never come, which must neither take a neighbour's turn nor hold
    /// the background preparation back (it held it for 20 s while a wedding folder was read).
    pub fn prefetches_pending(&self) -> usize {
        self.pending.keys().filter(|s| matches!(s, Slot::Prefetch(n) if *n != FULL_SOURCE_PREFETCH)).count()
    }

    /// Slots of photos prepared around the one on screen asked for since the renderer last forgot
    /// everything (not the open photo's full-resolution source, [`Slot::Prefetch`] 2).
    pub fn prefetch_slots_asked(&self) -> usize {
        self.prefetch_keys().len()
    }

    /// The jobs asked for in the slots of photos prepared around the one on screen (queued,
    /// running or made).
    pub fn prefetch_keys(&self) -> Vec<u64> {
        self.prefetched.iter().filter(|(s, _)| matches!(s, Slot::Prefetch(n) if *n != FULL_SOURCE_PREFETCH)).map(|(_, k)| *k).collect()
    }

    /// Whether `slot` was last asked for the job of `key` (queued, running or done).
    pub fn prefetch_asked(&self, slot: Slot, key: u64) -> bool {
        self.prefetched.get(&slot) == Some(&key)
    }

    /// Drop the slideshow slides other than `keep`: their textures and their requests that have
    /// not started (a slide that is no longer near is not worth rendering).
    pub fn release_slides(&mut self, keep: &[lcc_engine::slideshow::Item]) {
        if !self.slides_live {
            return;
        }
        let gone = |s: &Slot| matches!(s, Slot::Slide(id) if !keep.contains(id));
        self.textures.retain(|s, _| !gone(s));
        let _ = self.pool.reprioritize(|s, p| if gone(s) { None } else { Some(p) });
        self.queue.retain(|q| !gone(&q.slot));
        // (a render already running finishes unclaimed: its result is dropped)
        self.pending.retain(|s, _| !gone(s));
        self.requests.retain(|s, _| !gone(s));
        self.failed.retain(|s, _| !gone(s));
        self.slides_live = !keep.is_empty();
    }

    /// Is a request for `slot` queued or running?
    /// Drop every texture and cached stage (another library was opened: photo ids changed meaning).
    pub fn forget_all(&mut self) {
        self.requests.clear();
        self.shown_ids.clear();
        self.epoch = lcc_preview::next_tick();
        self.preview_generation = None;
        self.pool.reprioritize(|_, _| None);
        self.thumb_inputs.clear();
        self.free_natives();
        self.textures.clear();
        self.kept_views.clear();
        self.full_views.clear();
        self.main_drafts.clear();
        self.stages.clear();
        self.quick_tried.clear();
        self.failed.clear();
        self.thumb_failures.clear();
        self.missing_originals.clear();
        self.found_back.clear();
        self.prefetched.clear();
        self.ready_views.clear();
        self.adopted.clear();
        self.held.clear();
        self.pending.clear();
        self.queue.clear();
        self.tiles.clear();
        self.tile_meta.clear();
        self.window_req.clear();
        self.window_shown.clear();
        self.hdr_view.clear();
        self.hdr_kept.clear();
        self.hdr_view_result.clear();
        self.hdr_geometry.clear();
        self.hdr_view_ms.clear();
    }

    /// Ask for the drag window of `pane` ([`Slot::Window`]): `job` renders `geom.window` of the
    /// frame, with the pane's own stage cache, so a drag redoes only the stages it feeds.
    pub fn request_window(&mut self, pane: u8, job: RenderJob, geom: WindowGeom, priority: u32) {
        let slot = Slot::Window(pane);
        self.request(slot, job, priority);
        if let Some(id) = self.requests.get(&slot).map(|r| r.id) {
            if self.window_req.len() >= 32 {
                // the oldest requests go: their results are superseded by now
                let mut ids: Vec<u64> = self.window_req.keys().copied().collect();
                ids.sort_unstable();
                for old in ids.into_iter().take(16) {
                    self.window_req.remove(&old);
                }
            }
            self.window_req.insert(id, geom);
        }
    }

    /// The drag window `pane` shows, and where its pixels go.
    pub fn window_texture(&self, pane: u8) -> Option<(&Tex, WindowGeom)> {
        let slot = Slot::Window(pane);
        Some((self.textures.get(&slot)?, *self.window_shown.get(&slot)?))
    }

    /// Ask for tile `key`: `job` renders `window` of the frame, of which `tile` is kept. Nothing
    /// happens when the tile is held or already asked for.
    pub fn request_tile(
        &mut self,
        key: crate::region::TileKey,
        job: RenderJob,
        window: lcc_engine::pipeline::PixelWindow,
        tile: lcc_engine::pipeline::PixelWindow,
        priority: u32,
    ) {
        if self.tiles.has_render(&key) {
            return;
        }
        let slot = Slot::Tile { pane: key.pane, col: key.tile.col, row: key.tile.row };
        self.tile_meta.insert(slot, (job.key, key, window, tile));
        self.request(slot, job, priority);
    }

    /// Show `img` for tile `key` until its render arrives (a part of a 1:1 preview).
    pub fn tile_placeholder(&mut self, ctx: &egui::Context, key: crate::region::TileKey, img: &lcc_raster::Rgba8) {
        if self.tiles.contains(&key) {
            return;
        }
        let tex = self.tile_tex(ctx, key, img, 0.0);
        self.tiles.insert(key, tex, img.width.saturating_mul(img.height).saturating_mul(4), true);
    }

    fn tile_tex(&self, ctx: &egui::Context, key: crate::region::TileKey, img: &lcc_raster::Rgba8, ms: f64) -> Tex {
        let color = std::sync::Arc::new(color_image(img));
        let pixels = self.keep_pixels.then(|| color.clone());
        let tex = ctx.load_texture(format!("tile {:?} {:?}", key.tile, key.pane), color, egui::TextureOptions::LINEAR);
        Tex {
            key: key.look,
            photo: key.photo,
            tex,
            size: [img.width, img.height],
            histogram: None,
            ms,
            quick: None,
            pixels,
            warnings: Vec::new(),
            native: None,
            placement: None,
        }
    }

    /// Drop queued tile renders `keep` says no to (tiles scrolled away). Running ones finish and
    /// are kept: their pixels are good.
    pub fn cancel_tiles(&mut self, keep: impl Fn(Slot) -> bool) {
        let drop_it = |s: &Slot| matches!(s, Slot::Tile { .. }) && !keep(*s);
        let mut dropped = self.pool.reprioritize(|s, p| if drop_it(s) { None } else { Some(p) });
        self.queue.retain(|q| {
            if drop_it(&q.slot) {
                dropped.push(q.slot);
                false
            } else {
                true
            }
        });
        for s in dropped {
            self.pending.remove(&s);
            self.requests.remove(&s);
            self.tile_meta.remove(&s);
        }
    }

    /// Tile renders queued or running.
    pub fn tiles_pending(&self) -> usize {
        self.pending.keys().filter(|s| matches!(s, Slot::Tile { .. })).count()
    }

    pub fn is_pending(&self, slot: Slot) -> bool {
        self.pending.contains_key(&slot)
    }

    /// The app is closing: drop the jobs that have not started and wait up to `timeout` for the
    /// ones running on the worker threads. Returns whether all of them have ended. Nothing is
    /// rendered afterwards.
    pub fn shutdown(&mut self, timeout: std::time::Duration) -> bool {
        self.queue.clear();
        self.pool.shutdown(timeout)
    }

    /// Worker threads that are still alive (0 before the first render).
    pub fn live_workers(&self) -> usize {
        self.pool.live_workers()
    }

    /// The texture to show for a grid/filmstrip thumbnail: the rendered one, else its stand-in.
    pub fn thumb(&self, id: PhotoId) -> Option<&Tex> {
        self.textures.get(&Slot::Thumb(id)).or_else(|| self.textures.get(&Slot::ThumbQuick(id)))
    }

    pub fn queued(&self) -> usize {
        self.pool.queued() + self.queue.len()
    }

    /// Hand queued jobs to the offload (best first) while it has room.
    fn dispatch(&mut self) {
        let Some(off) = self.offload.as_mut() else { return };
        while let Some(i) = self.queue.iter().enumerate().max_by_key(|(_, q)| (q.priority, q.seq)).map(|(i, _)| i) {
            let q = self.queue.swap_remove(i);
            if let Some(job) = off.try_start(q.slot, q.job) {
                self.queue.push(Queued { job, ..q });
                break;
            }
        }
    }

    /// Requests queued or running.
    pub fn in_flight(&self) -> usize {
        self.pending.len()
    }

    /// Why the last render for `slot` failed (until a render with another key succeeds).
    pub fn failure(&self, slot: Slot) -> Option<&str> {
        self.failed.get(&slot).map(|f| f.1.as_str())
    }

    /// The job key of `slot`'s last failed render.
    pub fn failed_key(&self, slot: Slot) -> Option<u64> {
        self.failed.get(&slot).map(|f| f.0)
    }

    /// Test seam: a failure left by an earlier request of `slot` for `photo` (a key no later job has).
    #[cfg(test)]
    pub(crate) fn record_failure_for_test(&mut self, slot: Slot, photo: PhotoId) {
        self.failed.insert(slot, (0, "an earlier failure".into(), 0, photo));
    }

    /// Why a photo has no grid thumbnail and won't get one: the thumbnail's own render failed, or the embedded
    /// preview failed and nothing else is on its way (an unedited raw asks for no other: see `request_thumb`).
    /// A new request of either kind (another size or look, a relinked file) is on its way too; a retry of
    /// the very request that failed keeps the failure until it answers.
    pub fn thumb_failure(&self, id: PhotoId) -> Option<&str> {
        let (full, quick) = (Slot::Thumb(id), Slot::ThumbQuick(id));
        if self.is_pending(full) && !self.retrying(full) {
            return None;
        }
        // a full-render failure that stands, else the embedded preview's, unless a new request of it is on its way
        if let Some(e) = self.failure(full) {
            return Some(e);
        }
        if let Some((_, e)) = self.thumb_failures.get(&(id, false)) {
            return Some(e);
        }
        if self.is_pending(quick) && !self.retrying(quick) {
            return None;
        }
        self.failure(quick).or_else(|| self.thumb_failures.get(&(id, true)).map(|(_, e)| e.as_str()))
    }

    /// Why the last render of `photo` for `slot` failed: a view slot shows one photo after
    /// another, and another photo's error isn't this one's.
    pub fn failure_of(&self, slot: Slot, photo: PhotoId) -> Option<&str> {
        self.failed.get(&slot).filter(|f| f.3 == photo).map(|f| f.1.as_str())
    }

    /// Whether `slot`'s pending render asks again for the key that last failed (a library change
    /// retries failures): what is on screen stays what the failure left until it answers.
    pub fn retrying(&self, slot: Slot) -> bool {
        self.pending.get(&slot).zip(self.failed.get(&slot)).is_some_and(|(p, f)| p.0 == f.0)
    }

    /// The slots with a render pending (diagnostics: `ui.inspect`).
    pub fn pending_slots(&self) -> Vec<String> {
        self.pending.keys().map(|s| format!("{s:?}")).collect()
    }

    /// Thumbnail textures currently loaded.
    /// A variant thumbnail ([`Slot::Variant`]): its texture once rendered; until then the job is
    /// queued at low priority (below on-screen grid thumbnails). Call it every frame the variant
    /// is visible: variants not asked for during the last frames leave the queue, and textures
    /// beyond [`VARIANT_TEXTURES`] are evicted least recently used first.
    pub fn variant(&mut self, job: RenderJob) -> Option<&Tex> {
        let key = job.key;
        let slot = Slot::Variant(key);
        self.variant_used.insert(key, self.frame);
        if !self.textures.contains_key(&slot) {
            self.request(slot, job, VARIANT_PRIORITY);
        }
        self.textures.get(&slot)
    }

    /// A view that shows `n` variants at once (the People view's faces) says so each frame, so the cache holds them all:
    /// with a fixed budget a screen of more than [`VARIANT_TEXTURES`] pictures evicts, and re-requests, the same few every
    /// frame, and those tiles stay blank. The budget is the most asked for in the last frame, a quarter more, up to
    /// [`MAX_VARIANT_TEXTURES`].
    pub fn want_variants(&mut self, n: usize) {
        self.variant_want = self.variant_want.max(n);
    }

    /// How many variant textures are kept now.
    fn variant_budget(&self) -> usize {
        VARIANT_TEXTURES.max(self.variant_want + self.variant_want / 4).min(MAX_VARIANT_TEXTURES)
    }

    /// Variant thumbnail textures currently loaded.
    pub fn variant_textures(&self) -> usize {
        self.textures.keys().filter(|s| matches!(s, Slot::Variant(_))).count()
    }

    /// Drop variant jobs nobody asked for in the last frames, and the least recently used
    /// variant textures beyond the budget.
    fn evict_variants(&mut self) {
        let now = self.frame;
        let stale = |used: Option<&u64>| used.is_none_or(|f| now.saturating_sub(*f) > 2);
        let used = &self.variant_used;
        let dropped = self.pool.reprioritize(|s, p| match s {
            Slot::Variant(k) if stale(used.get(k)) => None,
            _ => Some(p),
        });
        for s in dropped {
            self.pending.remove(&s);
            self.requests.remove(&s);
        }
        let requests = &mut self.requests;
        let pending = &mut self.pending;
        self.queue.retain(|q| match q.slot {
            Slot::Variant(k) if stale(used.get(&k)) => {
                pending.remove(&q.slot);
                requests.remove(&q.slot);
                false
            }
            _ => true,
        });
        let budget = self.variant_budget();
        self.variant_want = 0;
        let n = self.variant_textures();
        if n > budget {
            let mut have: Vec<(u64, u64)> = self
                .textures
                .keys()
                .filter_map(|s| if let Slot::Variant(k) = s { Some((self.variant_used.get(k).copied().unwrap_or(0), *k)) } else { None })
                .collect();
            have.sort_unstable();
            for (_, k) in have.into_iter().take(n - budget) {
                self.textures.remove(&Slot::Variant(k));
                self.variant_used.remove(&k);
            }
        }
        if self.variant_used.len() > 4 * budget {
            let textures = &self.textures;
            let pending = &self.pending;
            self.variant_used.retain(|k, _| textures.contains_key(&Slot::Variant(*k)) || pending.contains_key(&Slot::Variant(*k)));
        }
    }

    /// Frames polled so far (one a frame).
    pub fn frame(&self) -> u64 {
        self.frame
    }

    pub fn thumb_textures(&self) -> usize {
        self.textures.keys().filter(|s| matches!(s, Slot::Thumb(_) | Slot::ThumbQuick(_))).count()
    }

    /// Collect finished jobs into textures. Returns true if anything changed.
    pub fn poll(&mut self, ctx: &egui::Context, session: &mut Session) -> bool {
        let cache = &session.media.rendered;
        let generation = cache.generation();
        if self.preview_generation.as_ref().is_some_and(|(old, g)| old.as_ptr() != Arc::as_ptr(cache) || *g != generation) {
            self.forget_all();
        }
        self.preview_generation = Some((Arc::downgrade(cache), generation));
        self.catalog_rev = session.catalog.revision;
        self.frame += 1;
        self.trim_stages();
        // wasm: run one job per frame on this thread, timed with the host clock
        #[cfg(target_arch = "wasm32")]
        let inline_ms = {
            let t0 = crate::now_ms();
            self.pool.run_inline(1);
            crate::now_ms() - t0
        };
        #[cfg(not(target_arch = "wasm32"))]
        let inline_ms = 0.0;
        let changed = self.collect(ctx, session, inline_ms);
        self.track_fills(session);
        self.evict_variants();
        #[cfg(not(target_arch = "wasm32"))]
        self.trim_when_idle(ctx);
        if !self.pending.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        changed
    }

    /// Hold the "Filling spots" activity row while a render that solves a content-aware fill is
    /// out (a finished patch outline kept the machine busy with no word of why). Solved fills are
    /// cached, so the row stays only for the first render of a new fill.
    fn track_fills(&mut self, session: &Session) {
        let pending = &self.pending;
        self.fill_jobs.retain(|s, k| pending.get(s).is_some_and(|p| p.0 == *k));
        if self.fill_jobs.is_empty() {
            self.fill_row = None;
        } else if self.fill_row.is_none() {
            self.fill_row = Some(session.activity.start("fill", "Filling spots", lcc_engine::activity::Cancel::No));
        }
    }

    /// Whether the "Filling spots" row is up (tests).
    #[cfg(test)]
    pub(crate) fn filling(&self) -> bool {
        self.fill_row.is_some()
    }

    /// Ask for the next frame while renders are pending, for requests made after this frame's
    /// [`Self::poll`] (which asks the same for the ones it saw): the refine of a still photo in an
    /// open interaction would otherwise sit finished until the next input.
    pub fn keep_polling(&self, ctx: &egui::Context) {
        if !self.pending.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    /// In a slider drag, give the loupe's draft (just requested) until `budget` to finish, so that it
    /// is drawn in this frame rather than picked up at the start of the next one. Only when the last
    /// loupe render took well under the budget (a GPU draft takes a few milliseconds; a CPU render
    /// that would only make the frame late is not waited for).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn wait_for_draft(&mut self, ctx: &egui::Context, session: &mut Session, budget: std::time::Duration) {
        if !self.waits_for_draft(budget) {
            return;
        }
        let Some(deadline) = std::time::Instant::now().checked_add(budget) else { return };
        while self.is_pending(Slot::Main) {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() || !self.pool.wait(left) {
                break;
            }
            self.collect(ctx, session, 0.0);
        }
    }

    /// Whether [`Self::wait_for_draft`] waits at all with `budget`: only when the last loupe render
    /// took well under it.
    #[cfg(not(target_arch = "wasm32"))]
    fn waits_for_draft(&self, budget: std::time::Duration) -> bool {
        self.offload.is_none() && self.last_main_ms * 1.5 < budget.as_secs_f64() * 1e3
    }

    /// Turn finished jobs into textures. Returns true if anything changed.
    fn collect(&mut self, ctx: &egui::Context, session: &mut Session, inline_ms: f64) -> bool {
        let mut finished: Vec<_> = std::mem::take(&mut self.adopted).into_iter().map(|(slot, r, color, ms)| (slot, r, color, None, ms)).collect();
        finished.extend(std::mem::take(&mut self.held).into_iter().map(|(slot, r, color, ms)| (slot, r, color, None, ms)));
        while let Some(done) = self.pool.try_recv() {
            finished.push((done.slot, done.result.result, done.result.color, done.result.texels, if done.ms > 0.0 { done.ms } else { inline_ms }));
        }
        if let Some(off) = self.offload.as_mut() {
            finished.extend(off.finished().into_iter().map(|(slot, r, ms)| (slot, r, None, None, ms)));
        }
        self.dispatch();
        let mut changed = false;
        for (mut slot, mut r, mut color, texels, ms) in finished {
            // (not when the loupe waits for that very photo: it is shown at once)
            if matches!(slot, Slot::QuickAhead(_))
                && r.rendered.as_ref().is_ok_and(|x| x.image.width.saturating_mul(x.image.height).saturating_mul(4) > self.big_upload_bytes)
                && !self.pending.get(&Slot::Preview).is_some_and(|p| p.0 == r.key)
            {
                if self.big_upload_frame == Some(self.frame) {
                    self.held.push((slot, r, color, ms));
                    continue;
                }
                self.big_upload_frame = Some(self.frame);
            }
            self.completed += 1;
            let main_draft = slot == Slot::Main && self.main_drafts.remove(&r.request_id);
            // made for another display profile than the one in use: obsolete
            if r.display.is_some_and(|id| Some(id) != self.display_id()) {
                continue;
            }
            // Ignore obsolete pixels, failures and decoded sources, even for the same render key;
            // an interactive view still takes a superseded draft that is newer than what it
            // shows, and the source it decoded: a drag supersedes every request before it
            // finishes, and a source thrown away with them is decoded again for every tick.
            let source = if self.requests.get(&slot).map(|request| request.id) != Some(r.request_id) {
                let newer_draft = slot.is_view()
                    && self.requests.contains_key(&slot)
                    && r.request_id >= self.epoch
                    && self.shown_ids.get(&slot).is_none_or(|shown| r.request_id > *shown)
                    && r.rendered.is_ok();
                if !newer_draft {
                    self.discarded += 1;
                    // (the source a view's render decoded before a draft replaced it is still the
                    // photo's: kept, or the next render decodes it again)
                    let replaced = matches!(&r.rendered, Err(e) if e == lcc_engine::media::SUPERSEDED);
                    if replaced && slot.is_view() && r.loaded.is_some() && r.request_id >= self.epoch {
                        session.accept_replaced(&r);
                    }
                    continue;
                }
                session.accept(&r);
                None // superseded successful drafts have no failure diagnostic
            } else {
                let source = self.requests.remove(&slot).and_then(|request| request.source);
                session.accept(&r);
                source
            };
            if slot.is_view() {
                self.shown_ids.insert(slot, r.request_id);
            }
            // a render that stayed on the GPU: drawn from its texture
            if let (Some(frame), Ok(rendered)) = (r.frame.clone(), r.rendered.as_ref()) {
                if self.pending.get(&slot).is_some_and(|p| p.0 == r.key) {
                    self.pending.remove(&slot);
                }
                self.failed.remove(&slot);
                let histogram = rendered.histogram.clone();
                if self.show_frame(ctx, slot, &r, &frame, histogram, ms) {
                    if slot == Slot::Main {
                        self.last_main_ms = ms;
                        if let Some(asked) = self.main_asked.remove(&r.request_id) {
                            let now = crate::now_ms();
                            if self.loupe_log.len() >= LOUPE_LOG_LEN {
                                self.loupe_log.pop_front();
                            }
                            self.loupe_log.push_back(LoupeRender {
                                at_ms: now,
                                width: frame.width,
                                height: frame.height,
                                draft: main_draft,
                                render_ms: ms,
                                latency_ms: (now - asked).max(0.0),
                            });
                        }
                    }
                    changed = true;
                }
                continue;
            }
            // (results from an offload, which doesn't know the display profile: converted here)
            if let Some(d) = &self.display {
                let before = r.display;
                lcc_engine::display::present(&mut r, d);
                if r.display != before {
                    color = None; // converted after the worker made its texture pixels
                }
            }
            if self.pending.get(&slot).is_some_and(|p| p.0 == r.key) {
                self.pending.remove(&slot);
            }
            // made ahead for the photo the loupe is now waiting for: it is the loupe's stand-in
            // (the loupe's own request for it, if one was started, is dropped when it arrives)
            if matches!(slot, Slot::QuickAhead(_)) && r.rendered.is_ok() && self.pending.get(&Slot::Preview).is_some_and(|p| p.0 == r.key) {
                self.pending.remove(&Slot::Preview);
                self.requests.remove(&Slot::Preview);
                self.quick_tried.remove(&slot);
                slot = Slot::Preview;
            }
            let rendered = match r.rendered {
                Ok(x) => {
                    self.failed.remove(&slot);
                    // a decode that now succeeds lifts its photo's failure (the file was fixed
                    // under the same key, or the earlier failure was transient)
                    if let Slot::Thumb(_) | Slot::ThumbQuick(_) = slot {
                        self.thumb_failures.remove(&(r.photo, matches!(slot, Slot::ThumbQuick(_))));
                    }
                    x
                }
                Err(e) => {
                    if !matches!(slot, Slot::Prefetch(_) | Slot::QuickAhead(_)) {
                        let message = render_failure_message(slot, r.photo, source.as_ref(), &e);
                        #[cfg(test)]
                        self.failure_messages.push(message.clone());
                        log::warn!("{message}");
                        self.failed.insert(slot, (r.key, e.clone(), self.catalog_rev, r.photo));
                        // Remember the decode failure past the slot's eviction and the catalog
                        // revision moving on (issue #448): the file is not decoded again at this
                        // key. A bounded map: a library of unreadable files cannot grow it forever.
                        if let Slot::Thumb(_) | Slot::ThumbQuick(_) = slot {
                            if self.thumb_failures.len() >= THUMB_FAILURES_MAX {
                                self.thumb_failures.clear();
                            }
                            self.thumb_failures.insert((r.photo, matches!(slot, Slot::ThumbQuick(_))), (r.key, e));
                        }
                    }
                    // (an HDR view's older frame no longer shows what is asked for)
                    if let Slot::HdrView(view) = slot {
                        self.hdr_view.remove(&view);
                        self.hdr_kept.remove(&view);
                        self.hdr_view_result.remove(&view);
                    }
                    continue;
                }
            };
            if let Slot::Prefetch(n) = slot {
                // (its source was accepted above; the photos prepared further than the nearest each
                // way are in the preview cache, which the loupe fits: their pictures are not kept here)
                if self.prefetched.get(&slot) == Some(&r.key) && usize::from(n) < crate::panels::detail::FIRST_FAR_PREFETCH {
                    self.ready_views.insert(slot, (RenderResult { rendered: Ok(rendered), loaded: None, ..r }, color, ms));
                }
                continue;
            }
            if let Slot::Window(_) = slot {
                match self.window_req.get(&r.request_id) {
                    Some(g) => {
                        self.window_shown.insert(slot, *g);
                    }
                    None => continue,
                }
            }
            if let Slot::Tile { .. } = slot {
                if let Some(&(key, tile_key, window, tile)) = self.tile_meta.get(&slot)
                    && key == r.key
                    && let Some(img) = crate::region::crop_tile(&rendered.image, window, tile)
                {
                    let tex = self.tile_tex(ctx, tile_key, &img, ms);
                    self.tiles.insert(tile_key, tex, img.width.saturating_mul(img.height).saturating_mul(4), false);
                    changed = true;
                }
                continue;
            }
            if let Slot::HdrView(view) = slot {
                if rendered.deep.is_some() || texels.is_some() {
                    // (a result that gets here is for the geometry asked for now: older ones were retired)
                    let window = self.hdr_geometry.get(&view).and_then(|g| g.window);
                    let size = [rendered.image.width, rendered.image.height];
                    self.hdr_view_result.insert(view, HdrResult { photo: r.photo, key: r.key, size, deep: rendered.deep, window, texels });
                    self.hdr_view_ms.insert(view, ms);
                    changed = true;
                }
                continue;
            }
            if slot == Slot::PanelHistogram {
                self.panel_histogram = Some((r.photo, r.key, rendered.histogram));
                changed = true;
                continue;
            }
            if slot == Slot::HdrHistogram {
                if rendered.histogram.hdr.is_some() {
                    self.live_hdr_histogram = Some((r.photo, r.key, rendered.histogram));
                    changed = true;
                }
                continue;
            }
            if let Slot::ThumbQuick(id) = slot {
                if self.textures.contains_key(&Slot::Thumb(id)) {
                    continue; // the real thumbnail won the race
                }
                if r.quick == Some(QuickSource::Cached) {
                    // the photo's own cached thumbnail: final
                    slot = Slot::Thumb(id);
                }
            }
            if let Slot::Thumb(id) = slot {
                self.textures.remove(&Slot::ThumbQuick(id));
            }
            let img = &rendered.image;
            let color = color.unwrap_or_else(|| Arc::new(color_image(img)));
            // the loupe's render stays readable on the CPU for the colour readout under the pointer
            let pixels = (self.keep_pixels || slot == Slot::Main).then(|| color.clone());
            let name = format!("{slot:?}");
            // the whole frame: a full-quality render shown so far is kept for a return to it (Crop
            // on and off), and the new one gets a texture of its own
            if slot == Slot::Main {
                if !main_draft {
                    self.full_views.insert(r.key);
                }
                if self.textures.get(&slot).is_some_and(|t| t.key != r.key && self.full_views.contains(&t.key))
                    && let Some(t) = self.textures.remove(&slot)
                {
                    self.keep_view(t);
                }
            }
            // pixels again: the slot's GPU-resident texture is let go
            self.native_frames.remove(&slot);
            if let Some(id) = self.natives.remove(&slot)
                && let Some(screen) = self.screen.as_mut()
            {
                screen.free(id);
            }
            match self.textures.get_mut(&slot) {
                Some(t) => {
                    t.tex.set(color, egui::TextureOptions::LINEAR);
                    t.native = None;
                    t.pixels = pixels;
                    t.key = r.key;
                    t.photo = r.photo;
                    t.size = [img.width, img.height];
                    t.histogram = Some(keep_waveform(slot, rendered.histogram));
                    t.warnings = rendered.warnings;
                    t.ms = ms;
                    t.quick = r.quick;
                    t.placement = r.placement;
                }
                None => {
                    let tex = ctx.load_texture(name, color, egui::TextureOptions::LINEAR);
                    self.textures.insert(
                        slot,
                        Tex {
                            key: r.key,
                            photo: r.photo,
                            tex,
                            size: [img.width, img.height],
                            histogram: Some(keep_waveform(slot, rendered.histogram)),
                            ms,
                            quick: r.quick,
                            pixels,
                            warnings: rendered.warnings,
                            native: None,
                            placement: r.placement,
                        },
                    );
                }
            }
            if slot == Slot::Main {
                self.last_main_ms = ms;
                if let Some(asked) = self.main_asked.remove(&r.request_id) {
                    if !main_draft {
                        self.last_full_main_ms = ms;
                    }
                    let now = crate::now_ms();
                    if self.loupe_log.len() >= LOUPE_LOG_LEN {
                        self.loupe_log.pop_front();
                    }
                    self.loupe_log.push_back(LoupeRender {
                        at_ms: now,
                        width: img.width,
                        height: img.height,
                        draft: main_draft,
                        render_ms: ms,
                        latency_ms: (now - asked).max(0.0),
                    });
                }
            }
            changed = true;
        }
        changed
    }

    /// Free the GPU renderer's recycled buffers once nothing has rendered for [`IDLE_TRIM`] (they
    /// are reallocated by the next render; while working, renders reuse them). Batch work (an
    /// export, Build Previews) renders on other threads and fills the pools again after a trim:
    /// once it has finished, the pools are trimmed again (they kept up to the pool limit per card,
    /// 2 GB each with a 16 GB budget, for the rest of the session).
    #[cfg(not(target_arch = "wasm32"))]
    fn trim_when_idle(&mut self, ctx: &egui::Context) {
        let gpu = IdleGpu {
            batch_jobs: &|| lcc_engine::gpu::batch_usage().jobs + lcc_engine::gpu::helper_usage().0,
            pooled: &|| lcc_engine::gpu::memory().pooled,
            trim: &|| {
                lcc_engine::gpu::trim_pool(0);
                lcc_engine::memory::release();
            },
        };
        if let Some(wait) = self.idle_trim_step(std::time::Instant::now(), &gpu) {
            ctx.request_repaint_after(wait);
        }
    }

    /// One frame of [`Self::trim_when_idle`] at `now`, against `gpu` (the GPU's counters, or a test's):
    /// whether to look again after a while.
    #[cfg(not(target_arch = "wasm32"))]
    fn idle_trim_step(&mut self, now: std::time::Instant, gpu: &IdleGpu<'_>) -> Option<std::time::Duration> {
        if !self.pending.is_empty() {
            self.idle = None;
            return None;
        }
        let (since, trimmed) = *self.idle.get_or_insert((now, false));
        if trimmed {
            if (gpu.batch_jobs)() > 0 {
                // (looked at again once it has had time to finish)
                return Some(IDLE_TRIM);
            }
            if (gpu.pooled)() > self.pooled_after_trim {
                self.idle = Some((now, false));
                return Some(IDLE_TRIM);
            }
            return None;
        }
        let waited = now.saturating_duration_since(since);
        if waited >= IDLE_TRIM {
            (gpu.trim)();
            // (what a trim cannot free does not start another one)
            self.pooled_after_trim = (gpu.pooled)();
            self.idle = Some((since, true));
            None
        } else {
            Some(IDLE_TRIM - waited)
        }
    }

    /// Bytes the per-view stage caches (CPU images and GPU buffers) may hold together.
    pub fn stage_budget(&self) -> usize {
        #[cfg(test)]
        if let Some(b) = self.stage_budget_override {
            return b;
        }
        lcc_engine::memory::budget() / STAGE_BUDGET_SHARE
    }

    /// Clear stage caches, least useful first, until what they hold fits [`Self::stage_budget`].
    fn trim_stages(&mut self) {
        let sizes: Vec<(Slot, usize)> = self.stages.iter().map(|(slot, c)| (*slot, c.bytes() + lcc_engine::gpu::stage_bytes(c))).collect();
        for slot in stage_trim_order(&sizes, self.stage_budget()) {
            if let Some(c) = self.stages.get(&slot) {
                c.clear();
                self.stages_trimmed += 1;
            }
        }
    }

    /// What the renderer holds: per-view stage caches (CPU images and GPU buffers) and textures.
    pub fn memory(&self) -> serde_json::Value {
        let cpu: usize = self.stages.values().map(|s| s.bytes()).sum();
        let gpu: usize = self.stages.values().map(|s| lcc_engine::gpu::stage_bytes(s)).sum();
        let tex: usize = self.textures.values().map(|t| t.size[0] * t.size[1] * 4).sum();
        let copies: usize = self.textures.values().filter_map(|t| t.pixels.as_ref()).map(|p| p.pixels.len() * 4).sum();
        serde_json::json!({
            "stageCaches": {"count": self.stages.len(), "cpuBytes": cpu, "gpuBytes": gpu, "budgetBytes": self.stage_budget(), "trimmed": self.stages_trimmed, "sharedSourceBytes": lcc_engine::gpu::shared_source_bytes(), "autoMaskStrokeBytes": lcc_engine::gpu::kept_stroke_bytes()},
            "textures": {"count": self.textures.len(), "bytes": tex, "cpuCopyBytes": copies},
            "tiles": {"count": self.tiles.len(), "bytes": self.tiles.bytes(), "budgetBytes": self.tiles.budget()},
        })
    }

    /// CPU copies of the current textures by id (see [`Self::keep_pixels`]); a view drawn straight
    /// from the GPU is read back from it (only here: drawing it never does).
    pub fn cpu_textures(&self) -> HashMap<egui::TextureId, crate::softpaint::CpuTexture> {
        let natives = self.textures.iter().filter_map(|(slot, t)| {
            let pixels = match (&t.pixels, t.native, self.native_frames.get(slot), self.screen.as_ref()) {
                (None, Some(_), Some(frame), Some(screen)) => Arc::new(screen.read(frame)?),
                _ => return None,
            };
            Some((t.id(), crate::softpaint::CpuTexture::linear(pixels)))
        });
        self.textures
            .values()
            .chain(self.tiles.values())
            .filter_map(|t| Some((t.id(), crate::softpaint::CpuTexture::linear(t.pixels.clone()?))))
            .chain(natives)
            .collect()
    }

    /// Drop the import review thumbnails (a new review, or the dialog closed).
    pub fn forget_imports(&mut self) {
        self.pool.reprioritize(|s, p| if matches!(s, Slot::Import(_)) { None } else { Some(p) });
        self.queue.retain(|q| !matches!(q.slot, Slot::Import(_)));
        self.pending.retain(|s, _| !matches!(s, Slot::Import(_)));
        self.requests.retain(|s, _| !matches!(s, Slot::Import(_)));
        self.textures.retain(|s, _| !matches!(s, Slot::Import(_)));
        self.quick_tried.retain(|s, _| !matches!(s, Slot::Import(_)));
    }

    /// Drop the Find Duplicates window's thumbnails (the window closed).
    pub fn forget_duplicates(&mut self) {
        self.pool.reprioritize(|s, p| if matches!(s, Slot::Duplicates(_)) { None } else { Some(p) });
        self.queue.retain(|q| !matches!(q.slot, Slot::Duplicates(_)));
        self.pending.retain(|s, _| !matches!(s, Slot::Duplicates(_)));
        self.requests.retain(|s, _| !matches!(s, Slot::Duplicates(_)));
        self.textures.retain(|s, _| !matches!(s, Slot::Duplicates(_)));
        self.quick_tried.retain(|s, _| !matches!(s, Slot::Duplicates(_)));
    }

    /// Drop thumbnails that aren't in `keep` (bounded memory for huge libraries), and queued
    /// thumbnail jobs for photos that scrolled out of `keep`.
    pub fn evict_thumbs(&mut self, keep: &std::collections::HashSet<PhotoId>, max: usize) {
        self.thumb_inputs.retain(|id, _| keep.contains(id));
        let dropped = self.pool.reprioritize(|s, p| match s {
            Slot::Thumb(id) | Slot::ThumbQuick(id) if !keep.contains(id) && (p <= 11 || p == crate::panels::grid::STAND_IN_ON_SCREEN) => None,
            _ => Some(p),
        });
        for s in dropped {
            self.pending.remove(&s);
            self.requests.remove(&s);
            self.quick_tried.remove(&s);
        }
        let requests = &mut self.requests;
        let pending = &mut self.pending;
        self.queue.retain(|q| match q.slot {
            Slot::Thumb(id) if !keep.contains(&id) && q.priority <= 10 => {
                pending.remove(&q.slot);
                requests.remove(&q.slot);
                false
            }
            _ => true,
        });
        let thumbs = self.thumb_textures();
        if thumbs <= max {
            return;
        }
        self.textures.retain(|s, _| !matches!(s, Slot::Thumb(id) | Slot::ThumbQuick(id) if !keep.contains(id)));
        self.quick_tried.retain(|s, _| !matches!(s, Slot::ThumbQuick(id) if !keep.contains(id)));
    }
}

/// Include the source identity in diagnostics for every catalog-backed render slot (#1890).
/// Debug-format paths so whitespace/control characters cannot forge another log entry.
fn render_failure_message(slot: Slot, photo: PhotoId, source: Option<&lcc_catalog::Source>, error: &str) -> String {
    match source {
        Some(lcc_catalog::Source::File { path }) => format!("render {slot:?}, {photo:?}, file {path:?}: {error}"),
        Some(lcc_catalog::Source::Demo { scene }) => format!("render {slot:?}, {photo:?}, demo scene {scene}: {error}"),
        None => format!("render {slot:?}, {photo:?}, source unavailable: {error}"),
    }
}

#[cfg(test)]
mod second_card_tests {
    use super::*;

    #[test]
    fn the_tiles_of_a_one_to_one_view_render_on_the_second_card_and_the_view_on_the_window_card() {
        assert!(on_second_card(Slot::Tile { pane: PANE_AFTER, col: 2, row: 3 }));
        for slot in [Slot::Main, Slot::Window(PANE_AFTER), Slot::Preview, Slot::Before, Slot::Compare(0), Slot::Hover] {
            assert!(!on_second_card(slot), "{slot:?} draws on the window's card");
        }
    }
}

#[cfg(test)]
mod thumbnail_tests {
    use super::*;
    use lcc_catalog::{Catalog, Op, Photo, Source};

    #[test]
    fn render_failures_identify_the_file_for_thumbnail_and_view_slots() {
        let source = Source::File { path: "/photos/trip/IMG_0042.nef".into() };
        for slot in [Slot::Thumb(PhotoId(42)), Slot::Main, Slot::Before, Slot::Compare(1), Slot::Window(0)] {
            let message = render_failure_message(slot, PhotoId(42), Some(&source), "unsupported compression");
            assert!(message.contains("/photos/trip/IMG_0042.nef"), "{message}");
            assert!(message.contains("PhotoId(42)"), "{message}");
            assert!(message.ends_with(": unsupported compression"), "{message}");
        }
    }

    #[test]
    fn render_failure_paths_are_escaped_and_missing_sources_still_identified() {
        let source = Source::File { path: "line\nbreak.nef".into() };
        let message = render_failure_message(Slot::Main, PhotoId(42), Some(&source), "decode failed");
        assert!(!message.contains('\n'));
        assert!(message.contains("line\\nbreak.nef"));
        assert!(render_failure_message(Slot::Main, PhotoId(42), None, "decode failed").contains("PhotoId(42), source unavailable"));
        let demo = Source::Demo { scene: 7 };
        assert!(render_failure_message(Slot::Main, PhotoId(42), Some(&demo), "decode failed").contains("demo scene 7"));
    }

    struct Blocked;
    impl RenderOffload for Blocked {
        fn try_start(&mut self, _: Slot, job: RenderJob) -> Option<RenderJob> {
            Some(job)
        }
        fn finished(&mut self) -> Vec<(Slot, RenderResult, f64)> {
            vec![]
        }
    }
    fn photo() -> Arc<Photo> {
        Arc::new(Photo::new(PhotoId(1), Source::File { path: "audit.jpg".into() }, "audit.jpg", "JPEG", 6000, 4000, "2026-01-01"))
    }
    #[test]
    fn snapshot_tracks_cow_edits_undo_reload_and_replacement() {
        let mut catalog = Catalog::default();
        catalog.apply(Op::AddPhoto { photo: Box::new((*photo()).clone()) }).unwrap();
        let old = catalog.photo(PhotoId(1)).unwrap().clone();
        let mut r = Renderer::default();
        r.remember_thumb(&old, 384, 77, false);
        r.pending.insert(Slot::Thumb(PhotoId(1)), (77, 10));
        assert!(r.thumb_current(&old, 384, 10));
        assert!(!r.thumb_current(&old, 512, 10));
        let mut settings = lcc_develop::DevelopSettings::default();
        settings.light.exposure = 1.0;
        let undo = catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(settings), label: "edit".into(), edited: None }).unwrap();
        assert!(!r.thumb_current(catalog.photo(PhotoId(1)).unwrap(), 384, 10));
        catalog.apply(undo).unwrap();
        assert!(!r.thumb_current(catalog.photo(PhotoId(1)).unwrap(), 384, 10));
        catalog
            .apply(Op::SetContent {
                id: PhotoId(1),
                width: 6000,
                height: 4000,
                file_size: 99,
                content_hash: Some("changed".into()),
                preview_only: None,
                preview_checked: String::new(),
            })
            .unwrap();
        assert!(!r.thumb_current(catalog.photo(PhotoId(1)).unwrap(), 384, 10));
        assert!(!r.thumb_current(&photo(), 384, 10));
        r.forget_all();
        assert!(r.thumb_inputs.is_empty());
    }
    #[test]
    fn queue_promotions_running_work_and_cancellation() {
        let p = photo();
        let slot = Slot::Thumb(p.id);
        let mut session = Session::new();
        session.catalog.apply(Op::AddPhoto { photo: Box::new((*p).clone()) }).unwrap();
        let job = session.thumb_job(p.id, 384).unwrap();
        let key = job.key;
        let mut r = Renderer::default();
        r.set_offload(Box::new(Blocked));
        r.remember_thumb(&p, 384, key, false);
        r.request(slot, job, 5);
        assert!(r.thumb_current(&p, 384, 5));
        assert!(!r.thumb_current(&p, 384, 10));
        r.request(slot, session.thumb_job(p.id, 384).unwrap(), 10);
        assert_eq!(r.pending.get(&slot), Some(&(key, 10)));
        r.queue.clear(); // running work: cannot reprioritize, just as Renderer::request
        assert!(r.thumb_current(&p, 384, 5));
        r.pending.clear();
        assert!(!r.thumb_current(&p, 384, 10));
        r.evict_thumbs(&Default::default(), 600);
        assert!(r.thumb_inputs.is_empty());
    }
    fn queued_quick_thumbnail(id: PhotoId) -> (Arc<Photo>, QuickJob) {
        let mut session = Session::new();
        session.media.preview_loader = Some(Arc::new(|_, _| Some(lcc_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })));
        let mut raw = (*photo()).clone();
        raw.id = id;
        raw.kind = lcc_catalog::MediaKind::Raw;
        raw.develop = Arc::new(raw.camera_defaults());
        session.catalog.apply(Op::AddPhoto { photo: Box::new(raw) }).unwrap();
        let p = session.catalog.photo(id).unwrap().clone();
        let job = session.thumb_job(id, 384).unwrap();
        (p, session.quick_thumb_job(&job).unwrap())
    }

    #[test]
    fn queued_standin_promotes_from_prefetch_to_visible_without_restarting() {
        let (p, quick) = queued_quick_thumbnail(PhotoId(1));
        let (_, other) = queued_quick_thumbnail(PhotoId(2));
        let slot = Slot::ThumbQuick(p.id);
        // Deterministic queue: no native worker starts.
        let mut r = Renderer { pool: JobPool::new(0), ..Default::default() };
        r.remember_thumb(&p, 384, quick.key, true);
        r.request_quick(slot, quick.clone(), 6); // grid prefetch: 5 + 1
        r.request_quick(Slot::ThumbQuick(PhotoId(2)), other, 9); // filmstrip: 8 + 1
        let request_id = r.requests[&slot].id;
        assert!(r.thumb_current(&p, 384, 5));
        assert!(!r.thumb_current(&p, 384, 10));
        let visible = crate::panels::grid::stand_in_priority(10);
        r.request_quick(slot, quick.clone(), visible); // visible grid
        assert!(r.thumb_current(&p, 384, 10));
        r.request_quick(slot, quick.clone(), visible);
        assert_eq!(r.requests[&slot].id, request_id);
        assert_eq!(r.pool.queued(), 2);
        r.pool.run_inline(1);
        let done = r.pool.try_recv().unwrap();
        assert_eq!(done.slot, slot);
        assert_eq!(done.result.result.request_id, request_id);
        assert_eq!(done.result.result.quick, Some(QuickSource::Embedded));
        // A finished result awaiting collection also must not start another extraction.
        r.request_quick(slot, quick, 6);
        assert!(r.thumb_current(&p, 384, 5));
        assert_eq!(r.pool.queued(), 1);
    }

    #[test]
    fn queued_standin_demotes_when_it_leaves_the_visible_grid() {
        let (p, quick) = queued_quick_thumbnail(PhotoId(1));
        let (_, other) = queued_quick_thumbnail(PhotoId(2));
        let slot = Slot::ThumbQuick(p.id);
        let mut r = Renderer { pool: JobPool::new(0), ..Default::default() };
        r.remember_thumb(&p, 384, quick.key, true);
        r.request_quick(slot, quick.clone(), 11);
        r.request_quick(Slot::ThumbQuick(PhotoId(2)), other, 9);
        assert!(!r.thumb_current(&p, 384, 5));
        r.request_quick(slot, quick, 6);
        assert!(r.thumb_current(&p, 384, 5));
        r.pool.run_inline(1);
        assert_eq!(r.pool.try_recv().unwrap().slot, Slot::ThumbQuick(PhotoId(2)));
    }

    #[test]
    fn failures_retry_after_revision_and_quick_only_is_not_final() {
        let p = photo();
        let slot = Slot::Thumb(p.id);
        let mut r = Renderer::default();
        r.remember_thumb(&p, 384, 77, false);
        r.pending.insert(Slot::ThumbQuick(p.id), (77, 11));
        assert!(!r.thumb_current(&p, 384, 10));
        r.failed.insert(slot, (77, "no decoder".into(), 0, p.id));
        assert!(r.thumb_current(&p, 384, 10));
        assert_eq!(r.failure_of(slot, p.id), Some("no decoder"));
        assert_eq!(r.failure_of(slot, PhotoId(p.id.0 + 1)), None, "another photo's error");
        r.catalog_rev = 1;
        assert!(!r.thumb_current(&p, 384, 10));
    }

    #[test]
    fn a_fast_draft_is_drawn_in_the_frame_that_asked_for_it() {
        let mut s = Session::new();
        s.media.file_loader = Some(Arc::new(|_, _, _| Ok((lcc_raster::Rgb32f::new(8, 8), Default::default()))));
        s.catalog.apply(Op::AddPhoto { photo: Box::new((*photo()).clone()) }).unwrap();
        let mut r = Renderer::with_worker_threads(1);
        let ctx = egui::Context::default();
        let budget = std::time::Duration::from_secs(10);
        // the last loupe render took longer than the budget (a CPU render): not waited for
        r.last_main_ms = 1e6;
        r.request(Slot::Main, s.render_job(PhotoId(1), 64, 64, false, true).unwrap(), 100);
        r.wait_for_draft(&ctx, &mut s, budget);
        assert!(r.is_pending(Slot::Main) && !r.textures.contains_key(&Slot::Main));
        // a fast one (GPU drafts): the texture is the new render when the wait returns
        r.last_main_ms = 1.0;
        r.request(Slot::Main, s.render_job(PhotoId(1), 32, 32, false, true).unwrap(), 100);
        let want = r.wanted(Slot::Main);
        r.wait_for_draft(&ctx, &mut s, budget);
        assert!(!r.is_pending(Slot::Main));
        assert_eq!(r.textures.get(&Slot::Main).map(|t| t.key), want);
    }

    /// Note 8 of 2026-10-31: on a photo just landed on, a slider did nothing for 2–5 s: its drafts
    /// waited behind the loupe's first full render (raw decode + render). Now a draft starts beside
    /// it, takes over the decode already running instead of decoding the file again, and the full
    /// render it replaced stops once the source is decoded (the source is kept all the same).
    #[test]
    fn a_draft_does_not_wait_for_the_first_full_render_and_shares_its_decode() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let decodes = Arc::new(AtomicUsize::new(0));
        let (release, released) = std::sync::mpsc::channel::<()>();
        let released = Arc::new(std::sync::Mutex::new(released));
        let mut s = Session::new();
        let (d, gate) = (decodes.clone(), released.clone());
        s.media.file_loader = Some(Arc::new(move |_, _, _| {
            d.fetch_add(1, Ordering::SeqCst);
            // the raw decode takes as long as the test says
            let _ = gate.lock().unwrap().recv_timeout(std::time::Duration::from_secs(30));
            let mut image = lcc_raster::Rgb32f::new(16, 16);
            image.data.fill([0.2, 0.2, 0.2]);
            Ok((image, Default::default()))
        }));
        let p = Photo::new(PhotoId(1), Source::File { path: "note8-slider.nef".into() }, "note8-slider.nef", "NEF", 6000, 4000, "2026-01-01");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        s.execute("library.select", &serde_json::json!({"ids": [1], "active": 1})).unwrap();
        let mut r = Renderer::with_worker_threads(2);
        let ctx = egui::Context::default();
        // the photo is landed on: its first full render starts decoding
        r.request(Slot::Main, s.loupe_job(PhotoId(1), 64, 64, true).unwrap(), 100);
        let started = std::time::Instant::now();
        while decodes.load(Ordering::SeqCst) == 0 && started.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(decodes.load(Ordering::SeqCst), 1);
        // a slider moves: the draft (built before anything is decoded) starts beside the full render
        s.execute("develop.set", &serde_json::json!({"control": "light.exposure", "value": 0.5})).unwrap();
        let draft = s.loupe_job(PhotoId(1), 48, 48, true).unwrap().draft();
        let want = draft.key;
        r.request(Slot::Main, draft, 100);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(decodes.load(Ordering::SeqCst), 1, "the draft waits for the decode running, it does not decode again");
        drop(release);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while r.textures.get(&Slot::Main).is_none_or(|t| t.key != want) && std::time::Instant::now() < deadline {
            r.pool.wait(std::time::Duration::from_millis(50));
            r.poll(&ctx, &mut s);
        }
        assert_eq!(r.textures.get(&Slot::Main).map(|t| t.key), Some(want), "the draft is on screen");
        while r.completed < 2 && std::time::Instant::now() < deadline {
            r.pool.wait(std::time::Duration::from_millis(50));
            r.poll(&ctx, &mut s);
        }
        assert_eq!(r.completed, 2);
        assert_eq!(decodes.load(Ordering::SeqCst), 1, "one decode for both");
        let log: Vec<_> = r.loupe_log.iter().map(|l| l.draft).collect();
        assert_eq!(log, vec![true], "the replaced full render (old settings) never reached the screen");
        assert!(r.failure(Slot::Main).is_none(), "a replaced render is no failure");
        // the decoded source is the session's: the next render does not decode again
        let next = s.loupe_job(PhotoId(1), 64, 64, true).unwrap();
        assert!(matches!(next.source, lcc_engine::media::SourceRef::Loaded(_)), "the decode was kept");
    }

    // Given a loupe render asked for after the frame's poll (the refine of a still photo while an
    // interaction stays open), the renderer asks for the frame that will collect it; the loupe log
    // says which renders were drafts and the full one's time is kept for the refine delay
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_late_request_keeps_frames_coming_and_the_loupe_log_tells_drafts_from_full_renders() {
        let mut s = Session::new();
        s.media.file_loader = Some(Arc::new(|_, _, _| Ok((lcc_raster::Rgb32f::new(8, 8), Default::default()))));
        s.catalog.apply(Op::AddPhoto { photo: Box::new((*photo()).clone()) }).unwrap();
        let mut r = Renderer::with_worker_threads(1);
        let ctx = egui::Context::default();
        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = asked.clone();
        ctx.set_request_repaint_callback(move |i| seen.lock().unwrap().push(i.delay));
        r.keep_polling(&ctx);
        assert!(asked.lock().unwrap().is_empty(), "nothing pending: no frames asked for");
        r.request(Slot::Main, s.render_job(PhotoId(1), 32, 32, false, true).unwrap().draft(), 100);
        r.keep_polling(&ctx);
        assert!(asked.lock().unwrap().iter().any(|d| *d <= std::time::Duration::from_millis(16)), "{:?}", asked.lock().unwrap());
        r.last_main_ms = 1.0;
        r.wait_for_draft(&ctx, &mut s, std::time::Duration::from_secs(10));
        assert!(!r.is_pending(Slot::Main), "the draft");
        r.request(Slot::Main, s.render_job(PhotoId(1), 32, 32, false, true).unwrap(), 100);
        r.wait_for_draft(&ctx, &mut s, std::time::Duration::from_secs(10));
        assert!(!r.is_pending(Slot::Main), "the full render");
        let log: Vec<_> = r.loupe_log.iter().map(|l| l.draft).collect();
        assert_eq!(log, vec![true, false]);
        assert!(r.last_full_main_ms >= 0.0 && r.loupe_log.iter().all(|l| l.width > 0 && l.latency_ms >= 0.0));
    }

    // Given a 60 Hz display, a high quality adjustment update as fast as the GPU display fit makes
    // it (~5 ms) is waited for, so it is drawn in the frame that asked for it; a draft that slow is
    // not (its share of the frame is smaller), and neither is the update on a 144 Hz display
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_fast_full_quality_update_is_waited_for_at_60_hz() {
        let mut r = Renderer::with_worker_threads(1);
        r.last_main_ms = 5.0;
        let budget = crate::panels::detail::wait_budget;
        assert!(r.waits_for_draft(budget(1.0 / 60.0, false)), "a full-quality update at 60 Hz");
        assert!(!r.waits_for_draft(budget(1.0 / 60.0, true)), "a draft as slow");
        assert!(!r.waits_for_draft(budget(1.0 / 144.0, false)), "at 144 Hz");
        // (a frame time that is not a number waits for nothing)
        assert!(!r.waits_for_draft(budget(f32::NAN, false)));
    }

    #[derive(Default)]
    struct Work {
        jobs: Vec<(Slot, RenderJob)>,
        finished: Vec<(Slot, RenderResult, f64)>,
    }
    struct Manual(std::rc::Rc<std::cell::RefCell<Work>>);
    impl RenderOffload for Manual {
        fn try_start(&mut self, slot: Slot, job: RenderJob) -> Option<RenderJob> {
            self.0.borrow_mut().jobs.push((slot, job));
            None
        }
        fn finished(&mut self) -> Vec<(Slot, RenderResult, f64)> {
            std::mem::take(&mut self.0.borrow_mut().finished)
        }
    }
    /// The open photo's full-resolution source decoded ahead of a zoom is no neighbour: while it
    /// decodes, the neighbours still get both their turns and the background preparation is not
    /// held back (2026-11-09: it was pending for 20 s while a wedding folder was read, and every
    /// photo ahead waited behind it).
    #[test]
    fn the_full_resolution_warm_up_is_not_a_neighbour_render() {
        let (mut app, _work, _ctx) = fixture();
        app.renderer.pool = JobPool::new(0);
        let id = PhotoId(1);
        let job = app.session.loupe_job(id, 64, 64, true).expect("a loupe job");
        app.renderer.prefetch(Slot::Prefetch(FULL_SOURCE_PREFETCH), job.clone(), 10);
        assert!(app.renderer.is_pending(Slot::Prefetch(FULL_SOURCE_PREFETCH)), "queued");
        assert_eq!(app.renderer.prefetches_pending(), 0, "not counted as a neighbour");
        app.renderer.prefetch(Slot::Prefetch(0), job, 10);
        assert_eq!(app.renderer.prefetches_pending(), 1, "the neighbour is");
    }

    /// 1:1 previews made ahead for the neighbours (180 MB each for a 45 MP photo) that finish
    /// together are uploaded one per frame, never all in the frame of a drag or a zoom; the photo
    /// the loupe waits for is never held back (2026-11-10: two landing in one frame made a 106 ms
    /// paint).
    #[test]
    fn big_pictures_made_ahead_are_uploaded_one_per_frame() {
        let (mut app, _work, ctx) = fixture();
        app.renderer.pool = JobPool::new(0);
        app.renderer.big_upload_bytes = 4 << 20;
        let side = 1200; // 5.8 MB, over that
        let big = |app: &mut crate::LccApp, slot: Slot, key: u64| {
            let mut out = app.session.render_job(PhotoId(1), 64, 64, false, true).unwrap().run();
            let r = &mut app.renderer;
            let id = lcc_preview::next_tick();
            r.requests.insert(slot, Requested { id, source: None });
            r.pending.insert(slot, (key, 1));
            out.request_id = id;
            out.key = key;
            if let Ok(x) = out.rendered.as_mut() {
                x.image = lcc_raster::Rgba8::new(side, side);
            }
            r.adopted.push((slot, out, None, 1.0));
        };
        big(&mut app, Slot::QuickAhead(0), 11);
        big(&mut app, Slot::QuickAhead(1), 12);
        app.renderer.poll(&ctx, &mut app.session);
        let has = |r: &Renderer, s: Slot| r.textures.get(&s).is_some_and(|t| t.size[0] == side);
        assert!(has(&app.renderer, Slot::QuickAhead(0)), "the first is uploaded at once");
        assert!(!has(&app.renderer, Slot::QuickAhead(1)), "the second waits for the next frame");
        assert!(app.renderer.is_pending(Slot::QuickAhead(1)), "still on its way, so the next frame is asked for");
        app.renderer.poll(&ctx, &mut app.session);
        assert!(has(&app.renderer, Slot::QuickAhead(1)), "uploaded in the next frame");
        // the photo the loupe waits for: shown at once, even after a big upload in this frame
        big(&mut app, Slot::QuickAhead(2), 13);
        big(&mut app, Slot::QuickAhead(3), 14);
        app.renderer.pending.insert(Slot::Preview, (14, 110));
        app.renderer.poll(&ctx, &mut app.session);
        assert!(has(&app.renderer, Slot::QuickAhead(2)));
        assert!(has(&app.renderer, Slot::Preview), "the loupe's stand-in is never held back");
    }

    fn fixture() -> (crate::LccApp, std::rc::Rc<std::cell::RefCell<Work>>, egui::Context) {
        fixture_with(Session::new(), (*photo()).clone())
    }
    fn fixture_with(mut s: Session, photo: Photo) -> (crate::LccApp, std::rc::Rc<std::cell::RefCell<Work>>, egui::Context) {
        s.media.file_loader = Some(Arc::new(|_, _, _| {
            let mut image = lcc_raster::Rgb32f::new(8, 8);
            image.data.fill([0.3, 0.2, 0.1]);
            Ok((image, Default::default()))
        }));
        // (`with_fs` sets the file system's thumbnail loader, which wins over `file_loader`: thumbnails use the stub too)
        s.media.reduced_loader = None;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        let mut app = crate::LccApp::new(s, crate::Services::default());
        app.renderer.keep_pixels = true;
        let work = std::rc::Rc::new(std::cell::RefCell::new(Work::default()));
        app.renderer.set_offload(Box::new(Manual(work.clone())));
        let ctx = egui::Context::default();
        app.renderer.poll(&ctx, &mut app.session);
        (app, work, ctx)
    }
    fn take_job(work: &std::rc::Rc<std::cell::RefCell<Work>>) -> (Slot, RenderJob) {
        work.borrow_mut().jobs.pop().unwrap()
    }

    /// The loupe's stand-ins made ahead are taken over at the step (no job, the texture
    /// at once), a photo still wanted keeps its slot as the photos shift, and a step to a photo whose
    /// stand-in is still being made shows it when it arrives.
    #[test]
    fn a_step_takes_over_the_camera_preview_made_ahead() {
        let gate = Arc::new(std::sync::Mutex::new(()));
        let mut s = Session::new();
        let g = gate.clone();
        s.media.preview_loader = Some(Arc::new(move |_: &str, _| {
            drop(g.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
            Some(lcc_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })
        }));
        let raw = |id: u64| {
            let mut p = Photo::new(PhotoId(id), Source::File { path: format!("r{id}.cr3") }, &format!("r{id}.cr3"), "CR3", 6000, 4000, "2026-01-01");
            p.kind = lcc_catalog::MediaKind::Raw;
            p.develop = Arc::new(p.camera_defaults());
            p
        };
        let (mut app, _, ctx) = fixture_with(s, raw(1));
        for id in 2..=4 {
            app.session.catalog.apply(Op::AddPhoto { photo: Box::new(raw(id)) }).unwrap();
        }
        let job = |app: &mut crate::LccApp, id: u64| app.session.quick_view_job(PhotoId(id), 64, true).unwrap();
        let wait = |app: &mut crate::LccApp, done: &dyn Fn(&Renderer) -> bool| {
            let t0 = std::time::Instant::now();
            while !done(&app.renderer) {
                assert!(t0.elapsed() < std::time::Duration::from_secs(20), "the stand-ins never arrived");
                std::thread::sleep(std::time::Duration::from_millis(5));
                app.renderer.poll(&ctx, &mut app.session);
            }
        };
        let ahead_of = |r: &Renderer, id: u64| r.textures.iter().any(|(s, t)| matches!(s, Slot::QuickAhead(_)) && t.photo == PhotoId(id));
        let jobs = vec![job(&mut app, 2), job(&mut app, 3)];
        app.renderer.look_ahead(jobs, 96);
        wait(&mut app, &|r| ahead_of(r, 2) && ahead_of(r, 3));
        // the step to photo 2: its stand-in is the loupe's at once, nothing is made again
        let q = job(&mut app, 2);
        app.renderer.request_quick(Slot::Preview, q, 110);
        assert!(!app.renderer.is_pending(Slot::Preview));
        assert_eq!(app.renderer.textures.get(&Slot::Preview).map(|t| t.photo), Some(PhotoId(2)));
        assert!(!ahead_of(&app.renderer, 2));
        // the next look-ahead (3 and 4): 3 keeps its slot and is not made again
        let held = gate.lock().unwrap();
        let jobs = vec![job(&mut app, 3), job(&mut app, 4)];
        app.renderer.look_ahead(jobs, 96);
        assert!(ahead_of(&app.renderer, 3), "a photo still wanted keeps what was made for it");
        let pending_ahead: Vec<Slot> = app.renderer.pending.keys().copied().filter(|s| matches!(s, Slot::QuickAhead(_))).collect();
        assert_eq!(pending_ahead.len(), 1, "only photo 4 is being made: {pending_ahead:?}");
        // the step to photo 4 while its stand-in is still being made: shown when it arrives
        let q = job(&mut app, 4);
        app.renderer.request_quick(Slot::Preview, q, 110);
        drop(held);
        wait(&mut app, &|r| r.textures.get(&Slot::Preview).is_some_and(|t| t.photo == PhotoId(4)) && !r.is_pending(Slot::Preview));
        wait(&mut app, &|r| !r.pending.keys().any(|s| matches!(s, Slot::QuickAhead(_) | Slot::Preview)));
        assert_eq!(app.renderer.textures.get(&Slot::Preview).map(|t| t.photo), Some(PhotoId(4)));
    }

    fn finish(app: &mut crate::LccApp, work: &std::rc::Rc<std::cell::RefCell<Work>>, ctx: &egui::Context, slot: Slot, result: RenderResult) {
        work.borrow_mut().finished.push((slot, result, 0.0));
        app.renderer.poll(ctx, &mut app.session);
    }
    fn request(app: &mut crate::LccApp) {
        crate::panels::grid::request_thumb(app, PhotoId(1), 256, 10);
    }

    /// Draws GPU-resident renders under one fixed id, and reads them back from the test's device.
    #[cfg(not(target_arch = "wasm32"))]
    struct TestScreen(lcc_engine::gpu::wgpu::Device, lcc_engine::gpu::wgpu::Queue);
    #[cfg(not(target_arch = "wasm32"))]
    impl ScreenTextures for TestScreen {
        fn show(&mut self, _: &lcc_engine::media::ScreenFrame, _: Option<egui::TextureId>) -> Option<egui::TextureId> {
            Some(egui::TextureId::User(7))
        }
        fn free(&mut self, _: egui::TextureId) {}
        fn read(&self, frame: &lcc_engine::media::ScreenFrame) -> Option<egui::ColorImage> {
            let img = lcc_engine::gpu::read_texture(&self.0, &self.1, &frame.texture, frame.width, frame.height).ok()?;
            Some(color_image(&img))
        }
    }

    /// A headless screenshot (driven by the control channel: `keep_pixels`) of a view drawn straight
    /// from the GPU has its pixels. Before, a mid-drag `ui.screenshot {"headless": true}` showed the
    /// photo as a white rectangle: the GPU-resident draft left no CPU copy (review of PR #1065).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_headless_screenshot_reads_back_a_view_drawn_from_the_gpu() {
        use lcc_engine::gpu::wgpu;
        let device = (|| {
            let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
            desc.backends = lcc_engine::gpu::backend::window_backends();
            desc.backend_options = lcc_engine::gpu::backend::backend_options();
            let instance = lcc_engine::gpu::new_instance(desc);
            let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { label: Some("test window"), ..Default::default() })).ok()
        })();
        let Some((device, queue)) = device else {
            lcc_testkit::skip!(Gpu, "no GPU adapter");
            return;
        };
        let (w, h) = (5usize, 3usize);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let texels: Vec<u8> = (0..w * h).flat_map(|i| [i as u8 * 10, 100, 200, 255]).collect();
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &texels,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w as u32 * 4), rows_per_image: Some(h as u32) },
            wgpu::Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 },
        );
        let (mut app, work, ctx) = fixture();
        assert!(app.renderer.keep_pixels);
        app.renderer.set_screen_textures(Box::new(TestScreen(device, queue)));
        let job = app.session.render_job(PhotoId(1), 64, 64, false, true).unwrap().draft();
        app.renderer.request(Slot::Main, job, 100);
        let (slot, job) = take_job(&work);
        let mut result = job.run();
        result.frame = Some(lcc_engine::media::ScreenFrame { texture, width: w, height: h });
        finish(&mut app, &work, &ctx, slot, result);
        let t = app.renderer.textures.get(&Slot::Main).unwrap();
        assert_eq!(t.id(), egui::TextureId::User(7), "the draft is drawn from the GPU");
        assert!(t.pixels.is_none());
        let cpu = app.renderer.cpu_textures();
        let shot = cpu.get(&egui::TextureId::User(7)).expect("the headless screenshot has the view's pixels");
        assert_eq!(shot.image.size, [w, h]);
        assert_eq!(shot.image.pixels[4], egui::Color32::from_rgb(40, 100, 200));
        // a render that comes back as pixels lets the GPU-resident one go
        app.renderer.request(Slot::Main, app.session.render_job(PhotoId(1), 64, 64, false, true).unwrap(), 100);
        let (slot, job) = take_job(&work);
        let result = job.run();
        finish(&mut app, &work, &ctx, slot, result);
        let t = app.renderer.textures.get(&Slot::Main).unwrap();
        assert!(t.native.is_none() && t.pixels.is_some(), "pixels again");
    }

    /// A drag's GPU-drawn draft over a full render keeps that render for a return to it (Crop on and
    /// off, an undo), and leaving the view frees the draft's GPU texture.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_gpu_drawn_draft_keeps_the_full_render_and_leaving_frees_its_texture() {
        use lcc_engine::gpu::wgpu;
        struct Counting(std::rc::Rc<std::cell::RefCell<Vec<egui::TextureId>>>);
        impl ScreenTextures for Counting {
            fn show(&mut self, _: &lcc_engine::media::ScreenFrame, id: Option<egui::TextureId>) -> Option<egui::TextureId> {
                Some(id.unwrap_or(egui::TextureId::User(9)))
            }
            fn free(&mut self, id: egui::TextureId) {
                self.0.borrow_mut().push(id);
            }
        }
        let device = (|| {
            let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
            desc.backends = lcc_engine::gpu::backend::window_backends();
            let instance = lcc_engine::gpu::new_instance(desc);
            let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
        })();
        let Some((device, _queue)) = device else {
            lcc_testkit::skip!(Gpu, "no GPU adapter");
            return;
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: 4, height: 4, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let freed = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let (mut app, work, ctx) = fixture();
        app.renderer.set_screen_textures(Box::new(Counting(freed.clone())));
        let full = app.session.render_job(PhotoId(1), 64, 64, false, true).unwrap();
        let full_key = full.key;
        app.renderer.request(Slot::Main, full, 100);
        let (slot, job) = take_job(&work);
        let result = job.run();
        finish(&mut app, &work, &ctx, slot, result);
        // (a draft of another edit: its own key)
        let mut draft = app.session.render_job(PhotoId(1), 64, 64, false, true).unwrap().draft();
        draft.key ^= 0x5555;
        app.renderer.request(Slot::Main, draft, 100);
        let (slot, job) = take_job(&work);
        let mut result = job.run();
        result.frame = Some(lcc_engine::media::ScreenFrame { texture, width: 4, height: 4 });
        finish(&mut app, &work, &ctx, slot, result);
        assert_eq!(app.renderer.textures[&Slot::Main].id(), egui::TextureId::User(9), "the draft is drawn from the GPU");
        assert!(app.renderer.kept_views.iter().any(|t| t.key == full_key), "the full render is kept");
        app.renderer.release(Slot::Main);
        assert_eq!(*freed.borrow(), vec![egui::TextureId::User(9)], "leaving the view frees the draft's texture");
    }

    /// With GPU-resident textures shown, a view's drafts ask to stay on the GPU; full renders and
    /// thumbnails never do.
    #[test]
    fn only_view_drafts_ask_to_stay_on_the_gpu() {
        struct Never;
        impl ScreenTextures for Never {
            fn show(&mut self, _: &lcc_engine::media::ScreenFrame, _: Option<egui::TextureId>) -> Option<egui::TextureId> {
                None
            }
            fn free(&mut self, _: egui::TextureId) {}
        }
        let (mut app, _work, _ctx) = fixture();
        let job = app.session.render_job(PhotoId(1), 64, 64, false, true).unwrap();
        assert!(!app.renderer.job_for(Slot::Main, job.clone().draft()).on_screen, "no screen textures: pixels");
        let (draft, full, thumb) = (job.clone().draft(), job.clone(), job.draft());
        app.renderer.set_screen_textures(Box::new(Never));
        assert!(app.renderer.job_for(Slot::Main, draft).on_screen);
        assert!(!app.renderer.job_for(Slot::Main, full).on_screen);
        assert!(!app.renderer.job_for(Slot::Thumb(PhotoId(1)), thumb).on_screen);
    }

    fn relink_destination() -> lcc_testkit::TempFile {
        let path = lcc_testkit::TempFile::new(&format!("lc-render-relinked-{}", lcc_preview::next_tick()), "replacement.nef");
        std::fs::write(&path, b"replacement source").unwrap();
        path
    }

    #[test]
    fn completed_failure_keeps_requested_source_after_relink() {
        let replacement = relink_destination();
        for slot in [Slot::Thumb(PhotoId(1)), Slot::Main, Slot::Before, Slot::Compare(1), Slot::Window(0)] {
            let (mut app, work, ctx) = fixture();
            app.session.media.file_loader = Some(Arc::new(|_, _, _| Err("simulated decode failure".into())));
            let job = app.session.render_job(PhotoId(1), 64, 64, false, true).unwrap();
            app.renderer.request(slot, job, 100);
            let (submitted, job) = take_job(&work);
            assert_eq!(submitted, slot);
            let result = job.run();
            assert!(result.rendered.is_err(), "the old request has already failed");
            app.session.execute("photo.relink", &serde_json::json!({"id": 1, "path": replacement.path()})).unwrap();
            assert!(
                matches!(&app.session.catalog.photo(PhotoId(1)).unwrap().source, Source::File { path } if path.ends_with(replacement.file_name().unwrap().to_str().unwrap()))
            );
            finish(&mut app, &work, &ctx, slot, result);
            assert_eq!(app.renderer.failure(slot), Some("simulated decode failure"));
            assert!(!app.renderer.is_pending(slot));
            let message = app.renderer.failure_messages.last().expect("the collector emitted a diagnostic");
            assert!(message.contains("audit.jpg"), "{message}");
            assert!(!message.contains("lc-render-relinked-"), "the failed request did not read the replacement: {message}");
            assert!(message.ends_with(": simulated decode failure"), "{message}");
        }
        std::fs::remove_file(replacement).unwrap();
    }

    #[test]
    fn superseded_failure_does_not_take_the_new_requests_source() {
        let replacement = relink_destination();
        let (mut app, work, ctx) = fixture();
        app.session.media.file_loader = Some(Arc::new(|_, _, _| Err("simulated decode failure".into())));
        let slot = Slot::Main;
        let old_job = app.session.render_job(PhotoId(1), 64, 64, false, true).unwrap();
        app.renderer.request(slot, old_job, 100);
        let (_, old_job) = take_job(&work);
        let old_result = old_job.run();
        app.session.execute("photo.relink", &serde_json::json!({"id": 1, "path": replacement.path()})).unwrap();
        let new_job = app.session.render_job(PhotoId(1), 64, 64, false, true).unwrap();
        app.renderer.request(slot, new_job, 100);
        let (_, new_job) = take_job(&work);
        finish(&mut app, &work, &ctx, slot, old_result);
        assert!(app.renderer.failure_messages.is_empty(), "obsolete failures stay suppressed");
        assert!(app.renderer.is_pending(slot));
        finish(&mut app, &work, &ctx, slot, new_job.run());
        let message = app.renderer.failure_messages.last().expect("new failure is reported");
        assert!(message.contains("lc-render-relinked-"), "{message}");
        assert!(!message.contains("audit.jpg"), "{message}");
        assert!(app.renderer.requests.is_empty(), "completed request identity is released");
        std::fs::remove_file(replacement).unwrap();
    }

    /// An edited raw: a fast draft of its edit from the camera JPEG (never the camera JPEG itself,
    /// unedited) is the cell's first picture, ahead of every render, and the real thumbnail behind
    /// it is background work that waits while another cell on screen still lacks its first picture.
    #[test]
    fn an_edited_raw_shows_a_draft_of_its_edit_first_and_the_render_waits_behind_the_stand_ins() {
        let mut s = Session::new();
        s.media.preview_loader = Some(Arc::new(|_, _| Some(lcc_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })));
        let mut raw = (*photo()).clone();
        raw.kind = lcc_catalog::MediaKind::Raw;
        let mut d = raw.camera_defaults();
        d.light.exposure = 0.3;
        raw.develop = Arc::new(d);
        let (mut app, work, ctx) = fixture_with(s, raw);
        app.renderer.pool = JobPool::new(0);
        let id = PhotoId(1);
        assert!(!app.session.embedded_thumb_is_final(app.session.catalog.photo(id).unwrap()));
        crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        assert_eq!(app.renderer.pending[&Slot::ThumbQuick(id)].1, crate::panels::grid::STAND_IN_ON_SCREEN, "the stand-in comes before renders");
        assert!(work.borrow().jobs.is_empty(), "no raw render before the stand-in");
        // the cell on screen has no picture yet: background work waits for it
        crate::panels::grid::note_cell(&mut app, id);
        assert!(app.pictures_busy(app.renderer.frame()));
        assert!(lcc_engine::prep::pictures_busy());
        assert_eq!(app.renderer.pool.run_inline(1), 1);
        app.renderer.poll(&ctx, &mut app.session);
        assert_eq!(app.renderer.thumb(id).and_then(|t| t.quick), Some(QuickSource::Draft), "the draft of the edit is on screen");
        // another cell still waits for its picture in this frame: the real render is held back
        app.pictures_missing(app.renderer.frame());
        crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        assert!(work.borrow().jobs.is_empty(), "the real thumbnail waits while a cell lacks its first picture");
        // a couple of frames later every cell has one: the real render follows, at background priority
        app.renderer.poll(&ctx, &mut app.session);
        app.renderer.poll(&ctx, &mut app.session);
        assert!(!app.pictures_busy(app.renderer.frame()));
        crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        let (slot, _) = take_job(&work);
        assert_eq!(slot, Slot::Thumb(id), "the edit is rendered behind the stand-in");
        assert_eq!(app.renderer.thumb(id).and_then(|t| t.quick), Some(QuickSource::Draft), "the stand-in stays until then");
    }

    #[test]
    fn grid_queued_standin_changes_priority_without_restarting() {
        let mut s = Session::new();
        s.media.preview_loader = Some(Arc::new(|_, _| Some(lcc_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })));
        let mut raw = (*photo()).clone();
        raw.kind = lcc_catalog::MediaKind::Raw;
        raw.develop = Arc::new(raw.camera_defaults());
        let (mut app, work, ctx) = fixture_with(s, raw);
        app.renderer.pool = JobPool::new(0); // deterministic: no quick job starts until run_inline
        let id = PhotoId(1);
        let slot = Slot::ThumbQuick(id);

        crate::panels::grid::request_thumb(&mut app, id, 384, 5);
        let request_id = app.renderer.requests[&slot].id;
        assert_eq!(app.renderer.pending[&slot].1, 6);
        assert_eq!(app.renderer.pool.queued(), 1);
        crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        assert_eq!(app.renderer.pending[&slot].1, crate::panels::grid::STAND_IN_ON_SCREEN, "the grid must promote its queued stand-in");
        assert_eq!(app.renderer.requests[&slot].id, request_id);
        assert_eq!(app.renderer.pool.queued(), 1);

        crate::panels::grid::request_thumb(&mut app, id, 384, 5);
        assert_eq!(app.renderer.pending[&slot].1, 6, "the grid must demote its queued stand-in");
        assert_eq!(app.renderer.requests[&slot].id, request_id);
        assert_eq!(app.renderer.pool.queued(), 1);
        let built = app.renderer.thumb_jobs_built;
        app.renderer.pool.run_inline(1);
        crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        assert_eq!(app.renderer.pending[&slot].1, 6, "completed work must not be restarted");
        assert_eq!(app.renderer.requests[&slot].id, request_id);
        assert_eq!(app.renderer.pool.queued(), 0);
        app.renderer.poll(&ctx, &mut app.session);
        assert_eq!(app.renderer.textures[&slot].quick, Some(QuickSource::Embedded));

        crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        crate::panels::grid::request_thumb(&mut app, id, 384, 5);
        assert_eq!(app.renderer.thumb_jobs_built, built);
        assert_eq!(app.renderer.pool.queued(), 0);
        assert!(!app.renderer.is_pending(Slot::Thumb(id)));
        assert!(!app.renderer.textures.contains_key(&Slot::Thumb(id)));
        assert!(work.borrow().jobs.is_empty(), "the final embedded thumbnail needs no real render");
    }

    #[test]
    fn unchanged_ready_and_running_frames_build_no_jobs() {
        let (mut app, work, ctx) = fixture();
        request(&mut app);
        assert_eq!(app.renderer.thumb_jobs_built, 1);
        for _ in 0..180 {
            request(&mut app);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 1);
        let (slot, job) = take_job(&work);
        finish(&mut app, &work, &ctx, slot, job.run());
        for _ in 0..180 {
            request(&mut app);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 1);
        assert!(work.borrow().jobs.is_empty());
    }

    /// Two views showing one photo at different sizes in the same frames (the slideshow preview and
    /// the navigator) share the larger thumbnail instead of replacing each other's render forever.
    #[test]
    fn views_asking_for_two_sizes_of_one_photo_share_the_larger_thumbnail() {
        let (mut app, work, ctx) = fixture();
        for _ in 0..60 {
            crate::panels::grid::request_thumb(&mut app, PhotoId(1), 512, 9);
            crate::panels::grid::request_thumb(&mut app, PhotoId(1), 256, 8);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 1, "one render, not one per frame");
        let (slot, job) = take_job(&work);
        finish(&mut app, &work, &ctx, slot, job.run());
        assert!(!app.renderer.is_pending(Slot::Thumb(PhotoId(1))), "the render was kept");
        for _ in 0..60 {
            crate::panels::grid::request_thumb(&mut app, PhotoId(1), 256, 8);
            crate::panels::grid::request_thumb(&mut app, PhotoId(1), 512, 9);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 1);
        assert!(work.borrow().jobs.is_empty());
    }

    #[test]
    fn a_slider_drag_leaves_the_edited_photos_thumbnail_until_it_ends() {
        let (mut app, work, ctx) = fixture();
        request(&mut app);
        let (slot, job) = take_job(&work);
        finish(&mut app, &work, &ctx, slot, job.run());
        app.session.execute("library.select", &serde_json::json!({"ids": [1], "active": 1})).unwrap();
        app.session.execute("develop.beginInteraction", &serde_json::json!({"label": "Exposure"})).unwrap();
        for v in [0.1, 0.2, 0.3] {
            app.session.execute("develop.set", &serde_json::json!({"control": "light.exposure", "value": v})).unwrap();
            app.renderer.poll(&ctx, &mut app.session);
            request(&mut app);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 1, "the drag's frames render no thumbnail of the photo");
        assert!(work.borrow().jobs.is_empty());
        app.session.execute("develop.endInteraction", &serde_json::json!({})).unwrap();
        request(&mut app);
        assert_eq!(app.renderer.thumb_jobs_built, 2, "the thumbnail follows once the drag ends");
    }

    #[test]
    fn unchanged_queued_frames_build_no_jobs() {
        let (mut app, _, _) = fixture();
        app.renderer.set_offload(Box::new(Blocked));
        request(&mut app);
        for _ in 0..180 {
            request(&mut app);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 1);
        assert_eq!(app.renderer.queue.len(), 1);
    }

    #[test]
    fn resizing_the_disk_cache_keeps_textures_and_skips_rebuilds() {
        let lib = lcc_testkit::TempDir::new("lc-ui-thumb-resize");
        let _ = std::fs::remove_dir_all(&lib);
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        let (mut app, work, ctx) = fixture_with(s, (*photo()).clone());
        request(&mut app);
        let (slot, job) = take_job(&work);
        finish(&mut app, &work, &ctx, slot, job.run());
        assert!(app.renderer.thumb(PhotoId(1)).is_some());
        let cache = app.session.media.rendered.clone();
        app.session.execute("library.preferences", &serde_json::json!({"cacheMb": 300})).unwrap();
        app.renderer.poll(&ctx, &mut app.session);
        // thumbnails are keyed by content: a new budget leaves them (and the cache) valid
        assert!(app.renderer.thumb(PhotoId(1)).is_some());
        assert!(Arc::ptr_eq(&cache, &app.session.media.rendered));
        for _ in 0..30 {
            request(&mut app);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 1);
        assert!(work.borrow().jobs.is_empty());
        // an explicit clear still drops them
        app.session.execute("library.clearPreviews", &serde_json::json!({})).unwrap();
        app.renderer.poll(&ctx, &mut app.session);
        assert!(app.renderer.thumb(PhotoId(1)).is_none());
        drop(app);
        let _ = std::fs::remove_dir_all(&lib);
    }

    /// A render that panics (a decoder bug) is a failure like any other: the slot stops waiting,
    /// the error is kept (not retried every frame), and the pool's workers live on. Before, the
    /// worker died without an answer and the slot stayed pending forever.
    #[test]
    fn a_panicking_render_is_a_failure_not_a_dead_worker() {
        let (mut app, _, ctx) = fixture_with(Session::new(), (*photo()).clone());
        app.session.media.file_loader = Some(Arc::new(|_, _, _| panic!("decoder bug")));
        app.renderer.offload = None; // the real job pool
        let slot = Slot::Thumb(PhotoId(1));
        let ask = |app: &mut crate::LccApp| {
            if let Some(job) = app.session.thumb_job(PhotoId(1), 256) {
                app.renderer.request(slot, job, 30);
            }
            app.renderer.poll(&ctx, &mut app.session);
        };
        ask(&mut app);
        let t0 = std::time::Instant::now();
        while app.renderer.is_pending(slot) {
            assert!(t0.elapsed() < std::time::Duration::from_secs(20), "the render never answered");
            std::thread::sleep(std::time::Duration::from_millis(5));
            app.renderer.poll(&ctx, &mut app.session);
        }
        assert_eq!(app.renderer.failure(slot), Some("the render failed unexpectedly: decoder bug"));
        let workers = app.renderer.live_workers();
        assert!(workers > 0, "the worker that panicked is still there");
        let done = app.renderer.completed;
        for _ in 0..30 {
            ask(&mut app);
        }
        assert!(!app.renderer.is_pending(slot) && app.renderer.completed == done, "no retry loop");
        assert_eq!(app.renderer.live_workers(), workers);
    }

    #[test]
    fn raw_quick_path_frames_build_no_jobs() {
        // an unedited raw: the grid shows its embedded preview (held here until released) and stops
        // there — the camera JPEG at 4 416 × 2 944 already covers a 512 px thumbnail, so decoding the
        // whole raw behind it only costs the file read (45 s a photo on a 1.2 MB/s share)
        let gate = Arc::new(std::sync::Mutex::new(()));
        let held = gate.lock().unwrap();
        let mut s = Session::new();
        let g = gate.clone();
        s.media.preview_loader = Some(Arc::new(move |_: &str, _| {
            drop(g.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
            Some(lcc_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })
        }));
        let mut raw = (*photo()).clone();
        raw.kind = lcc_catalog::MediaKind::Raw;
        raw.develop = Arc::new(raw.camera_defaults());
        let (mut app, _, ctx) = fixture_with(s, raw);
        app.renderer.set_offload(Box::new(Blocked));
        let quick = Slot::ThumbQuick(PhotoId(1));
        // the stand-in is running
        for _ in 0..180 {
            request(&mut app);
        }
        assert!(app.renderer.is_pending(quick));
        assert_eq!(app.renderer.thumb_jobs_built, 1);
        drop(held);
        let t0 = std::time::Instant::now();
        while !app.renderer.textures.contains_key(&quick) {
            assert!(t0.elapsed() < std::time::Duration::from_secs(60), "the embedded preview never arrived");
            std::thread::sleep(std::time::Duration::from_millis(5));
            app.renderer.poll(&ctx, &mut app.session);
        }
        // the embedded preview is the thumbnail: no real render is queued behind it, and the grid
        // asking every frame neither rebuilds the job nor re-requests the preview
        for _ in 0..180 {
            request(&mut app);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 1, "an unedited raw's embedded preview is the thumbnail");
        assert!(app.renderer.queue.is_empty(), "nothing is decoded behind it");
        assert!(!app.renderer.is_pending(Slot::Thumb(PhotoId(1))));

        // an edit takes the embedded preview out of the picture: the real thumbnail is rendered then
        let mut edited = (*photo()).clone();
        edited.kind = lcc_catalog::MediaKind::Raw;
        edited.develop = Arc::new({
            let mut d = edited.camera_defaults();
            d.light.exposure = 0.5;
            d
        });
        let (mut app, _, _ctx) = fixture_with(Session::new(), edited);
        app.renderer.set_offload(Box::new(Blocked));
        for _ in 0..180 {
            request(&mut app);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 1);
        assert!(app.renderer.is_pending(Slot::Thumb(PhotoId(1))) || !app.renderer.queue.is_empty(), "an edited raw renders its thumbnail");
    }

    #[test]
    fn closing_import_review_cancels_queued_and_running_identities() {
        let (mut app, work, ctx) = fixture();
        let job = app.session.thumb_job(PhotoId(1), 256).unwrap();
        app.renderer.request(Slot::Import(0), job.clone(), 10);
        let (slot, running) = take_job(&work);
        app.renderer.set_offload(Box::new(Blocked));
        app.renderer.request(Slot::Import(1), job, 10);
        app.renderer.forget_imports();
        assert!(!app.renderer.is_pending(Slot::Import(0)));
        assert!(!app.renderer.is_pending(Slot::Import(1)));
        assert!(app.renderer.queue.is_empty());
        work.borrow_mut().finished.push((slot, running.run(), 0.0));
        app.renderer.set_offload(Box::new(Manual(work.clone())));
        app.renderer.poll(&ctx, &mut app.session);
        assert!(!app.renderer.textures.contains_key(&slot));
        assert_eq!(app.session.media.source_usage().0, 0);
    }

    #[test]
    fn loupe_shows_superseded_drafts_newer_than_its_pixels_during_a_drag() {
        for draft in [true, false] {
            let (mut app, work, ctx) = fixture();
            let mut jobs = Vec::new();
            for exposure in [0.0, 1.0, 2.0] {
                let mut edit = lcc_develop::DevelopSettings::default();
                edit.light.exposure = exposure;
                app.session.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit), label: "drag".into(), edited: None }).unwrap();
                let job = app.session.loupe_job(PhotoId(1), 8, 8, true).unwrap();
                let job = if draft { job.draft() } else { job };
                app.renderer.request(Slot::Main, job, 100);
                jobs.push(take_job(&work).1);
            }
            let keys: Vec<u64> = jobs.iter().map(|j| j.key).collect();
            let mut jobs = jobs.into_iter();
            let (first, second, last) = (jobs.next().unwrap(), jobs.next().unwrap(), jobs.next().unwrap());
            // the first draft lands while the newest is still rendering: the loupe moves on
            finish(&mut app, &work, &ctx, Slot::Main, first.run());
            assert_eq!(app.renderer.textures.get(&Slot::Main).map(|t| t.key), Some(keys[0]));
            assert!(app.renderer.is_pending(Slot::Main));
            finish(&mut app, &work, &ctx, Slot::Main, last.run());
            assert_eq!(app.renderer.textures.get(&Slot::Main).map(|t| t.key), Some(keys[2]));
            // a draft older than the pixels shown never replaces them
            finish(&mut app, &work, &ctx, Slot::Main, second.run());
            assert_eq!(app.renderer.textures.get(&Slot::Main).map(|t| t.key), Some(keys[2]));
            assert!(!app.renderer.is_pending(Slot::Main));
        }
    }

    #[test]
    fn returning_to_displayed_hdr_settings_supersedes_pending_changes() {
        struct Output;
        impl crate::HdrOutput for Output {
            fn headroom(&mut self) -> Option<f32> {
                Some(4.0)
            }
            fn upload(&mut self, _: u8, _: usize, _: usize, _: &[f32]) -> Result<egui::TextureId, String> {
                Ok(egui::TextureId::User(1))
            }
        }
        let (mut app, work, _) = fixture();
        app.hdr_headroom = Some(4.0);
        app.services.hdr_output = Some(Box::new(Output));
        let mut job = app.session.render_job(PhotoId(1), 8, 8, false, true).unwrap();
        Arc::make_mut(&mut job.settings).hdr.enabled = true;
        let view = HDR_LOUPE;
        let key = crate::panels::detail::request_hdr(&mut app, view, job.clone(), true, 100).unwrap();
        take_job(&work);
        app.renderer.hdr_view.insert(view, HdrView { photo: PhotoId(1), key, tex: egui::TextureId::User(1), size: [8, 8], window: None, slot: 0 });
        crate::panels::detail::request_hdr(&mut app, view, job.clone().draft(), true, 100);
        take_job(&work);
        crate::panels::detail::request_hdr(&mut app, view, job, true, 100);
        assert_eq!(app.renderer.wanted(Slot::HdrView(view)), Some(key));
    }

    #[test]
    fn a_failed_hdr_render_takes_the_older_frame_down() {
        let (mut app, work, ctx) = fixture();
        app.session.media.file_loader = Some(Arc::new(|_, _, _| Err("simulated decode failure".into())));
        let (view, slot) = (HDR_LOUPE, Slot::HdrView(HDR_LOUPE));
        let job = crate::panels::detail::hdr_view_job(app.session.render_job(PhotoId(1), 8, 8, false, true).unwrap(), Some(4.0));
        app.renderer.prepare_hdr_view(view, &job);
        app.renderer
            .hdr_view
            .insert(view, HdrView { photo: PhotoId(1), key: job.key ^ 1, tex: egui::TextureId::User(1), size: [8, 8], window: None, slot: 0 });
        app.renderer.request(slot, job.clone(), 100);
        assert!(app.renderer.hdr_frame(view, PhotoId(1), job.key).is_some(), "the older frame stays up while its update renders");
        let (_, pending) = take_job(&work);
        finish(&mut app, &work, &ctx, slot, pending.run());
        assert!(app.renderer.hdr_frame(view, PhotoId(1), job.key).is_none(), "a failed update leaves no stale frame up");
        assert!(app.renderer.hdr_view.is_empty());
    }

    #[test]
    fn a_zoomed_hdr_pane_shows_hdr_until_its_window_comes_and_its_tiles_if_that_fails() {
        use crate::panels::detail::{HdrPane, WindowCtx, WindowView, hdr_window};
        struct Output;
        impl crate::HdrOutput for Output {
            fn headroom(&mut self) -> Option<f32> {
                Some(4.0)
            }
            fn upload(&mut self, _: u8, _: usize, _: usize, _: &[f32]) -> Result<egui::TextureId, String> {
                Ok(egui::TextureId::User(1))
            }
        }
        let (mut app, work, ctx) = fixture();
        app.hdr_headroom = Some(4.0);
        app.services.hdr_output = Some(Box::new(Output));
        let mut edit = (*app.session.catalog.photo(PhotoId(1)).unwrap().develop).clone();
        edit.hdr.enabled = true;
        app.session.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit), label: "hdr".into(), edited: None }).unwrap();
        app.session.media.file_loader = Some(Arc::new(|_, _, _| Err("simulated decode failure".into())));
        let c = WindowCtx {
            id: PhotoId(1),
            frame_edge: Some(8),
            aspect: 1.0,
            texture_side: 4096,
            crop_tool: false,
            interacting: false,
            look: 0,
            holding: false,
            overlay: lcc_pipeline::Overlay::None,
            proof: None,
            full_preview: None,
        };
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(8.0, 8.0));
        let v = WindowView { pane: PANE_AFTER, before: false, img: r, target: r, visible: r };
        // no whole frame in HDR under it yet: its tiles
        assert!(matches!(hdr_window(&mut app, &c, &v, (8, 8)), HdrPane::Sdr));
        app.renderer
            .hdr_view
            .insert(HDR_LOUPE, HdrView { photo: PhotoId(1), key: 1, tex: egui::TextureId::User(2), size: [8, 8], window: None, slot: 0 });
        assert!(matches!(hdr_window(&mut app, &c, &v, (8, 8)), HdrPane::Pending), "the whole frame's HDR shows meanwhile");
        let (slot, job) = work.borrow_mut().jobs.drain(..).find(|(s, _)| matches!(s, Slot::HdrView(_))).expect("the HDR window job");
        finish(&mut app, &work, &ctx, slot, job.run());
        assert!(matches!(hdr_window(&mut app, &c, &v, (8, 8)), HdrPane::Sdr), "a failed HDR window: the tiles");
    }

    // Given pans across a zoomed HDR pane, the textures of windows nothing shows or keeps any
    // more are handed back to be freed: with the cache on no more than the window shown and the
    // ones kept stay, with it off only the one shown
    #[test]
    fn zoomed_hdr_windows_nothing_keeps_are_freed() {
        let view = HDR_ZOOMED + PANE_AFTER;
        let at = |x| Some(lcc_pipeline::PixelWindow { x, y: 0, w: 64, h: 64 });
        for cache in [true, false] {
            let mut r = Renderer::default();
            r.set_hdr_cache(cache);
            let mut live = std::collections::BTreeSet::new();
            for (i, x) in (0..30).map(|i| (i, i * 64)) {
                let slot = r.hdr_upload_slot(view, at(x));
                live.insert(slot);
                r.adopt_hdr(
                    view,
                    HdrView { photo: PhotoId(1), key: i as u64, tex: egui::TextureId::User(u64::from(slot)), size: [64, 64], window: at(x), slot },
                );
                for freed in r.hdr_unused_slots() {
                    assert_ne!(freed, r.hdr_view[&view].slot, "the window shown is never freed");
                    live.remove(&freed);
                }
                let kept = r.hdr_kept.get(&view).map_or(0, Vec::len);
                assert_eq!(live.len(), 1 + kept, "cache {cache}: a texture per window shown or kept, no more");
            }
            let most = if cache { 1 + HDR_WINDOWS_KEPT } else { 1 };
            assert!(live.len() <= most, "cache {cache}: {} textures", live.len());
            // a new photo: its frames go, and so do their textures
            r.forget_all();
            assert_eq!(r.hdr_unused_slots().len(), live.len());
        }
    }

    // Given a GPU's memory and a screen, the recent zoomed HDR windows are offered only where
    // they fit in their share of it; an unknown size doesn't block them, and nonsense sizes
    // don't panic
    #[test]
    fn the_hdr_zoom_cache_needs_room_on_the_gpu() {
        const GB: u64 = 1 << 30;
        let uhd = [3840.0, 2160.0];
        assert!(!hdr_cache_fit(uhd, Some(GB)).fits(), "1 GB on a 4K screen");
        assert!(hdr_cache_fit(uhd, Some(8 * GB)).fits(), "8 GB on a 4K screen");
        assert!(hdr_cache_fit(uhd, None).fits(), "unknown");
        // a bigger screen needs more, up to the cap per pane
        assert!(hdr_cache_fit([1920.0, 1080.0], None).need < hdr_cache_fit(uhd, None).need);
        assert_eq!(hdr_cache_fit([1e9, 1e9], None).need, hdr_cache_fit([20_000.0, 20_000.0], None).need);
        for bad in [[f32::NAN, 1.0], [f32::INFINITY, -5.0], [0.0, 0.0]] {
            let fit = hdr_cache_fit(bad, Some(GB));
            assert!(fit.need > 0 && fit.need <= hdr_cache_fit(uhd, None).need, "{bad:?}: {fit:?}");
        }
    }

    #[test]
    fn a_zoomed_pane_keeps_other_places_windows_but_not_older_looks_of_its_own() {
        let mut r = Renderer::default();
        let view = HDR_ZOOMED + PANE_AFTER;
        let at = |x| Some(lcc_pipeline::PixelWindow { x, y: 0, w: 64, h: 64 });
        let adopt = |r: &mut Renderer, key: u64, window| {
            let slot = r.hdr_upload_slot(view, window);
            r.adopt_hdr(view, HdrView { photo: PhotoId(1), key, tex: egui::TextureId::User(u64::from(slot)), size: [64, 64], window, slot });
            slot
        };
        // a drag at one place: each newer look takes the place of the last, nothing piles up
        let first = adopt(&mut r, 1, at(0));
        for key in 2..20 {
            assert_eq!(adopt(&mut r, key, at(0)), first, "a newer look of the same window");
        }
        assert!(r.hdr_kept.get(&view).is_none_or(|k| k.is_empty()));
        // pans: the windows of other places are kept (each in a texture of its own) to come back to
        let mut slots = vec![first];
        for (i, x) in [64, 128, 192].into_iter().enumerate() {
            slots.push(adopt(&mut r, 100 + i as u64, at(x)));
        }
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(slots.len(), 4, "a window shown again never shares a texture with one kept");
        assert!(r.reuse_hdr(view, 19), "back at the first place: its last look");
        assert_eq!(r.hdr_view[&view].window, at(0));
        assert!(!r.reuse_hdr(view, 5), "an older look of it is not kept");
        // at most HDR_WINDOWS_KEPT windows are kept
        for x in 0..20 {
            adopt(&mut r, 1000 + x, at(256 + 64 * x as usize));
        }
        assert!(r.hdr_kept[&view].len() <= HDR_WINDOWS_KEPT);
    }

    #[test]
    fn an_older_hdr_frame_is_not_shown_for_a_render_nothing_runs() {
        let (mut app, _work, _) = fixture();
        let job = crate::panels::detail::hdr_view_job(app.session.render_job(PhotoId(1), 8, 8, false, true).unwrap(), Some(4.0));
        let view = HDR_SECOND;
        app.renderer
            .hdr_view
            .insert(view, HdrView { photo: PhotoId(1), key: job.key ^ 1, tex: egui::TextureId::User(1), size: [8, 8], window: None, slot: 0 });
        assert!(app.renderer.hdr_frame(view, PhotoId(1), job.key).is_none(), "no update on its way: the frame is stale");
        app.renderer.request(Slot::HdrView(view), job.clone(), 100);
        assert!(app.renderer.hdr_frame(view, PhotoId(1), job.key).is_some());
        assert!(app.renderer.hdr_frame(view, PhotoId(2), job.key).is_none(), "another photo's frame never stands in");
        app.renderer.forget_all();
        assert!(app.renderer.hdr_frame(view, PhotoId(1), job.key).is_none());
    }

    #[test]
    fn a_crop_or_rotation_drops_the_held_hdr_frame_and_a_tone_change_keeps_it() {
        let (mut app, _work, _) = fixture();
        let view = HDR_LOUPE;
        let job = crate::panels::detail::hdr_view_job(app.session.render_job(PhotoId(1), 8, 8, false, true).unwrap(), Some(4.0));
        let held = HdrView { photo: PhotoId(1), key: job.key, tex: egui::TextureId::User(1), size: [8, 8], window: None, slot: 0 };
        let changes: [(&str, fn(&mut lcc_develop::DevelopSettings), bool); 4] = [
            ("exposure", |s| s.light.exposure += 1.0, true),
            ("crop aspect", |s| s.crop.aspect = Some((1, 1)), false),
            ("crop flip", |s| s.crop.flip_h = !s.crop.flip_h, false),
            ("rotation", |s| s.orientation = lcc_geom::Orientation::Rotate90, false),
        ];
        for (what, change, kept) in changes {
            app.renderer.prepare_hdr_view(view, &job);
            app.renderer.hdr_view.insert(view, held);
            let mut next = job.clone();
            change(Arc::make_mut(&mut next.settings));
            app.renderer.prepare_hdr_view(view, &next);
            assert_eq!(app.renderer.hdr_view.contains_key(&view), kept, "{what}");
        }
    }

    #[test]
    fn renders_with_float_samples_keep_their_texture_pixels_unless_they_are_hdr_views() {
        let (mut app, work, ctx) = fixture();
        let mut job = app.session.render_job(PhotoId(1), 8, 8, false, true).unwrap();
        Arc::make_mut(&mut job.settings).hdr.enabled = true;
        let deep = crate::panels::detail::hdr_view_job(job, Some(4.0));
        assert!(deep.clone().run().rendered.unwrap().deep.is_some(), "the job carries float samples");
        for slot in [Slot::Main, Slot::Before, Slot::Second, Slot::HdrHistogram, Slot::Compare(0)] {
            assert!(Renderer::run_for(slot, || deep.clone().run(), &None, None).color.is_some(), "{slot:?}");
        }
        assert!(Renderer::run_for(Slot::HdrView(HDR_LOUPE), || deep.clone().run(), &None, None).color.is_none(), "uploaded from its floats");
        // its texels are made beside the render, for the headroom asked for, exactly as an upload would
        let done = Renderer::run_for(Slot::HdrView(HDR_LOUPE), || deep.clone().run(), &None, Some(2.5));
        let samples = done.result.rendered.as_ref().unwrap().deep.as_ref().unwrap();
        let lcc_pipeline::DeepSamples::F32(rgb) = &samples.samples else { panic!("float HDR required") };
        let expected = lcc_engine::hdr_texture::encode_rgba16f(rgb, samples.width, samples.height, 2.5).unwrap();
        assert_eq!(done.texels, Some((expected, 2.5)));
        assert!(Renderer::run_for(Slot::Main, || deep.clone().run(), &None, Some(2.5)).texels.is_none(), "only HDR views");
        // and through the renderer: a view's texture still arrives
        app.renderer.request(Slot::Second, deep, 100);
        let (slot, job) = take_job(&work);
        finish(&mut app, &work, &ctx, slot, job.run());
        assert!(app.renderer.textures.contains_key(&Slot::Second));
    }

    #[test]
    fn hdr_retention_tracks_region_and_rejects_obsolete_geometry() {
        let (mut app, work, ctx) = fixture();
        let view = HDR_ZOOMED;
        let slot = Slot::HdrView(view);
        let job = app.session.render_job(PhotoId(1), 8, 8, false, true).unwrap();
        let mut job = crate::panels::detail::hdr_view_job(job, Some(4.0));
        job.request.window = Some(lcc_pipeline::PixelWindow { x: 0, y: 0, w: 4, h: 4 });
        app.renderer.prepare_hdr_view(view, &job);
        app.renderer.request(slot, job.clone(), 100);
        let (_, old) = take_job(&work);
        app.renderer
            .hdr_view
            .insert(view, HdrView { photo: PhotoId(1), key: job.key, tex: egui::TextureId::User(1), size: [4, 4], window: None, slot: 0 });
        app.renderer.prepare_hdr_view(view, &job.clone().draft());
        assert!(app.renderer.hdr_view.contains_key(&view), "tone changes retain the region");
        job.request.window.as_mut().unwrap().x = 4;
        job.key ^= 42;
        app.renderer.prepare_hdr_view(view, &job);
        assert!(app.renderer.hdr_view.contains_key(&view), "a pan keeps the frame there is, drawn where it belongs");
        app.renderer.request(slot, job, 100);
        let (_, new) = take_job(&work);
        finish(&mut app, &work, &ctx, slot, old.run());
        assert!(app.renderer.hdr_view_result.is_empty(), "old geometry is rejected even before new pixels arrive");
        finish(&mut app, &work, &ctx, slot, new.run());
        assert!(app.renderer.hdr_view_result.contains_key(&view));
    }

    #[test]
    fn hdr_drafts_progress_and_reject_out_of_order_results() {
        let (mut app, work, ctx) = fixture();
        let slot = Slot::HdrView(HDR_LOUPE);
        let mut jobs = Vec::new();
        for exposure in [3.0, 4.0, 5.0] {
            let mut edit = lcc_develop::DevelopSettings::default();
            edit.hdr.enabled = true;
            edit.light.exposure = exposure;
            app.session.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit), label: "drag".into(), edited: None }).unwrap();
            let job = app.session.render_job(PhotoId(1), 8, 8, false, true).unwrap().draft();
            let job = crate::panels::detail::hdr_view_job(job, Some(4.0));
            app.renderer.prepare_hdr_view(HDR_LOUPE, &job);
            app.renderer.request(slot, job, 100);
            jobs.push(take_job(&work).1);
        }
        let keys: Vec<_> = jobs.iter().map(|j| j.key).collect();
        let mut jobs = jobs.into_iter();
        let (first, second, last) = (jobs.next().unwrap(), jobs.next().unwrap(), jobs.next().unwrap());
        finish(&mut app, &work, &ctx, slot, first.run());
        assert_eq!(app.renderer.hdr_view_result[&HDR_LOUPE].key, keys[0]);
        assert!(app.renderer.is_pending(slot), "updates continue while dragging");
        finish(&mut app, &work, &ctx, slot, last.run());
        finish(&mut app, &work, &ctx, slot, second.run());
        let HdrResult { key, deep, .. } = &app.renderer.hdr_view_result[&HDR_LOUPE];
        assert_eq!(*key, keys[2], "late older frames never replace newer pixels");
        let lcc_pipeline::DeepSamples::F32(rgb) = &deep.as_ref().expect("float samples").samples else { panic!("float HDR required") };
        assert!(rgb.iter().any(|v| *v > 1.0), "highlights survive the draft pipeline");
        let final_job = app.session.render_job(PhotoId(1), 8, 8, false, true).unwrap();
        let final_render = crate::panels::detail::hdr_view_job(final_job, Some(4.0)).run().rendered.unwrap();
        let lcc_pipeline::DeepSamples::F32(final_rgb) = &final_render.deep.unwrap().samples else { panic!("float HDR required") };
        assert_eq!(rgb.len(), final_rgb.len());
        assert!(rgb.iter().zip(final_rgb).all(|(a, b)| (a - b).abs() < 1e-4), "uniform tonal draft matches the final HDR render");
        app.renderer.forget_all();
        assert!(app.renderer.hdr_view_result.is_empty());
    }

    #[test]
    fn a_drag_gets_its_live_hdr_histogram_beside_the_drafts() {
        let (mut app, work, ctx) = fixture();
        let mut jobs = Vec::new();
        for exposure in [1.0, 2.0] {
            let mut edit = lcc_develop::DevelopSettings::default();
            edit.hdr.enabled = true;
            edit.light.exposure = exposure;
            app.session.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit), label: "drag".into(), edited: None }).unwrap();
            let job = app.session.render_job(PhotoId(1), 32, 32, false, true).unwrap();
            app.renderer.request(Slot::HdrHistogram, job, 95);
            jobs.push(take_job(&work).1);
        }
        let keys: Vec<u64> = jobs.iter().map(|j| j.key).collect();
        let mut jobs = jobs.into_iter();
        let (first, last) = (jobs.next().unwrap(), jobs.next().unwrap());
        // superseded while the drag goes on, it still lands: the histogram keeps moving
        finish(&mut app, &work, &ctx, Slot::HdrHistogram, first.run());
        let live = app.renderer.live_hdr_histogram.clone().expect("a live HDR histogram");
        assert_eq!((live.0, live.1), (PhotoId(1), keys[0]));
        assert!(live.2.hdr.is_some(), "HDR bins");
        finish(&mut app, &work, &ctx, Slot::HdrHistogram, last.run());
        assert_eq!(app.renderer.live_hdr_histogram.as_ref().map(|l| l.1), Some(keys[1]));
        // only the histogram is kept: no texture
        assert!(!app.renderer.textures.contains_key(&Slot::HdrHistogram));
    }

    #[test]
    fn library_reset_rejects_old_job_even_when_photo_id_and_key_are_reused() {
        let (mut app, work, ctx) = fixture();
        request(&mut app);
        let (slot, old) = take_job(&work);
        let old_result = old.run();
        app.renderer.forget_all();
        let photo = app.session.catalog.photo(PhotoId(1)).unwrap().as_ref().clone();
        app.session = lcc_engine::Session::new();
        app.session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        app.session.media.file_loader = Some(Arc::new(|_, _, _| {
            let mut image = lcc_raster::Rgb32f::new(8, 8);
            image.data.fill([0.1, 0.2, 0.9]);
            Ok((image, Default::default()))
        }));
        app.renderer.poll(&ctx, &mut app.session);
        request(&mut app);
        let (_, new) = take_job(&work);
        assert_eq!(old_result.key, new.key);
        assert_ne!(old_result.request_id, new.request_id);
        let expected = new.clone().run().rendered.unwrap().image.as_bytes();
        finish(&mut app, &work, &ctx, slot, new.run());
        finish(&mut app, &work, &ctx, slot, old_result);
        assert_eq!(
            app.renderer.thumb(PhotoId(1)).unwrap().pixels.as_ref().unwrap().pixels.iter().flat_map(|c| c.to_array()).collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn late_results_and_errors_cannot_replace_newest_pixels_or_sources() {
        for order in [0, 1, 2] {
            let (mut app, work, ctx) = fixture();
            request(&mut app);
            let (slot, old) = take_job(&work);
            let mut edit = lcc_develop::DevelopSettings::default();
            edit.light.exposure = 1.0;
            app.session.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit), label: "edit".into(), edited: None }).unwrap();
            request(&mut app);
            let (_, new) = take_job(&work);
            let expected = new.clone().run().rendered.unwrap().image.as_bytes();
            if order != 0 {
                finish(&mut app, &work, &ctx, slot, new.run());
                // An obsolete source and error must not poison the current successful request.
                let mut stale = old.run();
                if order == 2 {
                    stale.rendered = Err("old failure".into());
                }
                finish(&mut app, &work, &ctx, slot, stale);
            } else {
                finish(&mut app, &work, &ctx, slot, old.run());
                assert!(app.renderer.thumb(PhotoId(1)).is_none());
                assert_eq!(app.session.media.source_usage().0, 0);
                finish(&mut app, &work, &ctx, slot, new.run());
            }
            assert_eq!(
                app.renderer.thumb(PhotoId(1)).unwrap().pixels.as_ref().unwrap().pixels.iter().flat_map(|c| c.to_array()).collect::<Vec<_>>(),
                expected
            );
            assert!(app.renderer.failure(slot).is_none());
        }
    }

    #[test]
    fn undo_to_existing_texture_supersedes_pending_edit() {
        let (mut app, work, ctx) = fixture();
        request(&mut app);
        let (slot, first) = take_job(&work);
        let key = first.key;
        finish(&mut app, &work, &ctx, slot, first.run());
        let original = app.renderer.thumb(PhotoId(1)).unwrap().pixels.clone().unwrap();
        let mut edit = lcc_develop::DevelopSettings::default();
        edit.light.exposure = 1.0;
        let undo =
            app.session.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit), label: "edit".into(), edited: None }).unwrap();
        request(&mut app);
        let (_, edited) = take_job(&work);
        app.session.catalog.apply(undo).unwrap();
        request(&mut app);
        let (_, restored) = take_job(&work);
        assert_eq!(restored.key, key);
        finish(&mut app, &work, &ctx, slot, restored.run());
        finish(&mut app, &work, &ctx, slot, edited.run());
        assert_eq!(app.renderer.thumb(PhotoId(1)).unwrap().pixels.as_ref().unwrap().pixels, original.pixels);
    }

    #[test]
    fn explicit_clear_repairs_same_key_bad_pixels_and_rejects_old_same_key_job() {
        let (mut app, work, ctx) = fixture();
        let job = app.session.thumb_job(PhotoId(1), 256).unwrap();
        let expected = job.clone().run().rendered.unwrap().image;
        let (cache, key) = job.cache.clone().unwrap();
        let mut bad = expected.clone();
        bad.data.fill([255, 0, 255, 255]);
        cache.put(key, Arc::new(bad.clone()));
        request(&mut app);
        let (slot, corrupt) = take_job(&work);
        finish(&mut app, &work, &ctx, slot, corrupt.run());
        assert_eq!(app.renderer.thumb(PhotoId(1)).unwrap().pixels.as_ref().unwrap().pixels[0].to_array(), [255, 0, 255, 255]);
        // An old job can have the SAME render key and stale decoded pixels.
        let mut old = job.clone();
        old.source =
            lcc_engine::media::SourceRef::Loaded(Box::new(lcc_engine::media::DecodedSource::new(Arc::new(lcc_raster::Rgb32f::new(8, 8)), None)));
        app.renderer.textures.clear();
        app.renderer.request(slot, old, 10);
        let (_, old) = take_job(&work);
        app.session.execute("library.clearPreviews", &serde_json::json!({})).unwrap();
        app.renderer.poll(&ctx, &mut app.session);
        assert!(app.renderer.thumb(PhotoId(1)).is_none());
        request(&mut app);
        let (_, repaired) = take_job(&work);
        assert_eq!(repaired.key, job.key);
        finish(&mut app, &work, &ctx, slot, repaired.run());
        finish(&mut app, &work, &ctx, slot, old.run());
        assert_eq!(cache.get(key).unwrap().as_bytes(), expected.as_bytes());
        assert_eq!(
            app.renderer.thumb(PhotoId(1)).unwrap().pixels.as_ref().unwrap().pixels.iter().flat_map(|c| c.to_array()).collect::<Vec<_>>(),
            expected.as_bytes()
        );
    }

    #[test]
    fn thumb_failure_reports_only_a_thumbnail_that_will_not_arrive() {
        let id = PhotoId(5);
        let mut r = Renderer::default();
        assert_eq!(r.thumb_failure(id), None, "nothing failed");
        // An unedited raw asks only for the embedded preview: its failure is final.
        r.failed.insert(Slot::ThumbQuick(id), (1, "no embedded preview".into(), 0, id));
        assert_eq!(r.thumb_failure(id), Some("no embedded preview"));
        // While the full render is still on its way, the cell is loading, not unreadable.
        r.pending.insert(Slot::Thumb(id), (2, 10));
        assert_eq!(r.thumb_failure(id), None, "a pending thumbnail may still arrive");
        r.failed.insert(Slot::Thumb(id), (2, "decode failed".into(), 0, id));
        assert_eq!(r.thumb_failure(id), Some("decode failed"));
    }

    #[test]
    fn thumb_failure_waits_for_a_new_quick_request_but_not_for_a_retry_of_the_failed_one() {
        let id = PhotoId(6);
        let mut r = Renderer::default();
        r.failed.insert(Slot::ThumbQuick(id), (1, "no embedded preview".into(), 0, id));
        // the size changed: a new embedded-preview request (another key) is on its way, so the cell is loading
        r.pending.insert(Slot::ThumbQuick(id), (3, 10));
        assert_eq!(r.thumb_failure(id), None, "a replacement of the failed quick render is pending");
        // the very request that failed is asked for again (after an eviction): until it answers the failure stands
        r.pending.insert(Slot::ThumbQuick(id), (1, 10));
        assert_eq!(r.thumb_failure(id), Some("no embedded preview"));
    }

    #[test]
    fn thumb_failure_waits_for_a_new_full_request_but_not_for_a_retry_of_the_failed_one() {
        let id = PhotoId(7);
        let mut r = Renderer::default();
        r.failed.insert(Slot::Thumb(id), (1, "decode failed".into(), 0, id));
        // the size changed or the file was relinked: a replacement with another key is on its way, so the cell is loading
        r.pending.insert(Slot::Thumb(id), (3, 10));
        assert_eq!(r.thumb_failure(id), None, "a replacement of the failed full render is pending");
        // the very request that failed is asked for again: until it answers the failure stands
        r.pending.insert(Slot::Thumb(id), (1, 10));
        assert_eq!(r.thumb_failure(id), Some("decode failed"));
    }

    /// An unsupported or malformed file must not be decoded again at every visit (issue #448):
    /// the failure stands across the slot's eviction and the catalog revision moving on (which
    /// background thumbnail writes do all the time), until the photo renders under another key.
    #[test]
    fn a_failed_thumbnail_is_not_decoded_again_after_the_catalog_moves_on() {
        let id = PhotoId(1);
        let (mut app, work, ctx) = fixture();
        app.session.media.file_loader = Some(Arc::new(|_, _, _| Err("unsupported raw (DNG compression 9)".into())));
        crate::panels::grid::request_thumb(&mut app, id, 256, 10);
        let (slot, job) = take_job(&work);
        assert_eq!(slot, Slot::Thumb(id));
        let result = job.run();
        assert!(result.rendered.is_err());
        finish(&mut app, &work, &ctx, slot, result);
        assert_eq!(app.renderer.thumb_failure(id), Some("unsupported raw (DNG compression 9)"));
        // the catalog moves on (an edit, a background write), the cell scrolls out and back
        app.session.catalog.revision += 1;
        app.renderer.evict_thumbs(&Default::default(), 600);
        crate::panels::grid::request_thumb(&mut app, id, 256, 10);
        assert!(work.borrow().jobs.is_empty(), "the same broken file must not be decoded again");
        assert!(!app.renderer.is_pending(Slot::Thumb(id)));
        assert_eq!(app.renderer.thumb_failure(id), Some("unsupported raw (DNG compression 9)"), "the cell still says why it has no picture");

        // a changed photo renders under another job key: that is tried again
        let mut settings = (*app.session.catalog.photo(id).unwrap().develop).clone();
        settings.light.exposure += 0.5;
        app.session.catalog.apply(Op::SetDevelop { id, settings: Arc::new(settings), label: "test".into(), edited: None }).unwrap();
        crate::panels::grid::request_thumb(&mut app, id, 256, 10);
        assert_eq!(work.borrow().jobs.len(), 1, "an edited photo is decoded again");
        let (slot, _) = take_job(&work);
        assert_eq!(slot, Slot::Thumb(id));
    }

    /// A stand-in whose decode failed is not re-requested after `quick_tried` lost the attempt
    /// (the cell scrolled away and back), which read and decoded the file yet again (issue #448).
    #[test]
    fn a_failed_thumbnail_standin_is_not_requested_again_after_an_eviction() {
        let id = PhotoId(2);
        let mut r = Renderer { pool: JobPool::new(0), ..Default::default() };
        let ctx = egui::Context::default();
        let mut s = Session::new();
        let job = || QuickJob {
            request_id: 0,
            photo: id,
            key: 42,
            cached: vec![],
            embedded: None,
            embedded_cache: None,
            embedded_on_disk: false,
            small: None,
            latest: Vec::new(),
            fit: None,
            draft: None,
            want: None,
            kept: None,
        };

        r.request_quick(Slot::ThumbQuick(id), job(), 5);
        r.pool.run_inline(1);
        r.poll(&ctx, &mut s);
        assert_eq!(r.thumb_failure(id), Some("no quick preview"));

        // the eviction dropped `quick_tried` with the cell: still not decoded again
        r.quick_tried.clear();
        r.request_quick(Slot::ThumbQuick(id), job(), 5);
        assert_eq!(r.pool.queued(), 0, "the failed stand-in is not requested again");
        assert!(!r.is_pending(Slot::ThumbQuick(id)));
        assert_eq!(r.thumb_failure(id), Some("no quick preview"));
        // another key (a new look or size) is on its way, so the cell is loading again
        r.pending.insert(Slot::ThumbQuick(id), (43, 6));
        assert_eq!(r.thumb_failure(id), None);
    }

    /// A missing photo's cell asks for its thumbnail again once the original is back (a drive mounted while the
    /// app runs), whether it showed the photo's kept thumbnail or an error: both stand for the thumbnail's key, so
    /// nothing else would until a restart, a relink or an edit. The kept picture stays on screen meanwhile; while
    /// the original is still missing, nothing is asked again.
    #[test]
    fn a_missing_photos_cell_asks_again_once_its_original_is_back() {
        let id = PhotoId(1);
        let there = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (mut app, work, ctx) = fixture();
        let probe = there.clone();
        app.session.media.availability.set_probe(Arc::new(move |_| probe.load(std::sync::atomic::Ordering::Relaxed)));
        let back = |app: &mut crate::LccApp, on: bool| {
            there.store(on, std::sync::atomic::Ordering::Relaxed);
            // (what the availability worker does when it finds a file back)
            let probe = there.clone();
            app.session.media.availability.set_probe(Arc::new(move |_| probe.load(std::sync::atomic::Ordering::Relaxed)));
        };

        // the kept thumbnail is on screen
        request(&mut app);
        let (slot, job) = take_job(&work);
        let mut kept = job.run();
        kept.quick = Some(QuickSource::Kept);
        finish(&mut app, &work, &ctx, slot, kept);
        assert_eq!(app.renderer.thumb(id).and_then(|t| t.quick), Some(QuickSource::Kept));
        request(&mut app);
        assert!(work.borrow().jobs.is_empty(), "the original is still missing: nothing is asked again");
        back(&mut app, true);
        request(&mut app);
        assert_eq!(work.borrow().jobs.len(), 1, "the original is back: its thumbnail is asked for again");
        assert!(app.renderer.thumb(id).is_some(), "the kept picture stays on screen until the render arrives");
        let (slot, job) = take_job(&work);
        let fresh = job.run();
        finish(&mut app, &work, &ctx, slot, fresh);
        assert_eq!(app.renderer.thumb(id).map(|t| t.quick), Some(None), "the fresh render replaced it");
        request(&mut app);
        assert!(work.borrow().jobs.is_empty(), "nothing more to ask");

        // an error shown while the original was missing: asked again once it is back
        let mut settings = (*app.session.catalog.photo(id).unwrap().develop).clone();
        settings.light.exposure += 0.5;
        app.session.catalog.apply(Op::SetDevelop { id, settings: Arc::new(settings), label: "test".into(), edited: None }).unwrap();
        back(&mut app, false);
        request(&mut app);
        let (slot, job) = take_job(&work);
        let failed = RenderResult { rendered: Err("No such file or directory".into()), ..job.run() };
        finish(&mut app, &work, &ctx, slot, failed);
        assert!(app.renderer.thumb_failure(id).is_some());
        request(&mut app);
        assert!(work.borrow().jobs.is_empty(), "still missing: the failure stands");
        back(&mut app, true);
        request(&mut app);
        assert_eq!(work.borrow().jobs.len(), 1, "the original is back: asked again");

        // gone again before the render read it: the kept thumbnail comes back, and is not asked for again on every
        // frame while the availability hasn't seen the file go
        let (slot, job) = take_job(&work);
        let mut kept = job.run();
        kept.quick = Some(QuickSource::Kept);
        finish(&mut app, &work, &ctx, slot, kept);
        request(&mut app);
        request(&mut app);
        assert!(work.borrow().jobs.is_empty(), "asked again once per availability generation, not on every frame");
    }

    /// A file that is there but can't be read keeps its failure: only an original found back is asked for again.
    #[test]
    fn an_unreadable_file_that_is_there_is_not_asked_for_again() {
        let id = PhotoId(1);
        let (mut app, work, ctx) = fixture();
        app.session.media.availability.set_probe(Arc::new(|_| true));
        request(&mut app);
        let (slot, job) = take_job(&work);
        let failed = RenderResult { rendered: Err("unsupported raw".into()), ..job.run() };
        finish(&mut app, &work, &ctx, slot, failed);
        for _ in 0..3 {
            request(&mut app);
        }
        assert!(work.borrow().jobs.is_empty(), "the unreadable file is not read again");
        assert_eq!(app.renderer.thumb_failure(id), Some("unsupported raw"));
    }

    #[test]
    fn undo_to_embedded_thumb_rejects_late_full_render() {
        let mut s = Session::new();
        s.media.preview_loader = Some(Arc::new(|_, _| Some(lcc_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })));
        let mut raw = (*photo()).clone();
        raw.kind = lcc_catalog::MediaKind::Raw;
        raw.develop = Arc::new(raw.camera_defaults());
        let (mut app, work, ctx) = fixture_with(s, raw);
        app.renderer.pool = JobPool::new(0);
        let id = PhotoId(1);
        let quick_slot = Slot::ThumbQuick(id);
        let full_slot = Slot::Thumb(id);
        let settings_a = app.session.catalog.photo(id).unwrap().develop.clone();

        crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        assert_eq!(app.renderer.pool.run_inline(1), 1);
        app.renderer.poll(&ctx, &mut app.session);
        let (key_a, pixels_a) = {
            let shown = app.renderer.thumb(id).unwrap();
            assert_eq!(shown.quick, Some(QuickSource::Embedded));
            (shown.key, shown.pixels.as_ref().unwrap().pixels.clone())
        };
        assert!(app.renderer.textures.contains_key(&quick_slot));
        assert!(!app.renderer.textures.contains_key(&full_slot));
        assert!(work.borrow().jobs.is_empty());

        let mut settings_b = (*settings_a).clone();
        settings_b.light.exposure += 0.5;
        app.session.commit("Exposure", Op::SetDevelop { id, settings: Arc::new(settings_b), label: "Exposure".into(), edited: None }).unwrap();
        assert!(!app.session.embedded_thumb_is_final(app.session.catalog.photo(id).unwrap()));
        crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        // Manual owns dispatched jobs: hold B until the real Undo command restores A.
        let (slot, held_b) = take_job(&work);
        assert_eq!(slot, full_slot);
        let key_b = held_b.key;
        assert_ne!(key_b, key_a);
        assert!(app.renderer.is_pending(full_slot));

        app.run("edit.undo", serde_json::json!({})).unwrap();
        assert_eq!(app.session.catalog.photo(id).unwrap().develop.as_ref(), settings_a.as_ref());
        assert!(app.session.embedded_thumb_is_final(app.session.catalog.photo(id).unwrap()));
        crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        {
            let restored = app.renderer.thumb(id).unwrap();
            assert_eq!(restored.key, key_a);
            assert_eq!(restored.pixels.as_ref().unwrap().pixels, pixels_a);
            assert_eq!(restored.quick, Some(QuickSource::Embedded));
        }
        assert!(work.borrow().jobs.is_empty(), "restoring embedded-final A needs no full render");
        assert_eq!(app.renderer.pool.queued(), 0);

        let result_b = held_b.run();
        assert_eq!(result_b.key, key_b);
        let rendered_b = result_b.rendered.as_ref().expect("held edited thumbnail must render before the stale-result oracle");
        assert_ne!(color_image(&rendered_b.image).pixels, pixels_a, "the held result must be distinguishable from A");
        finish(&mut app, &work, &ctx, slot, result_b);

        let shown = app.renderer.thumb(id).unwrap();
        assert_eq!(shown.key, key_a, "late edited Thumb(B) must not replace restored embedded ThumbQuick(A)");
        assert_eq!(shown.pixels.as_ref().unwrap().pixels, pixels_a);
        assert_eq!(shown.quick, Some(QuickSource::Embedded));
        let quick = app.renderer.textures.get(&quick_slot).expect("the correct embedded thumbnail must remain installed");
        assert_eq!(quick.key, key_a);
        assert_eq!(quick.quick, Some(QuickSource::Embedded));
        assert_eq!(quick.pixels.as_ref().unwrap().pixels, pixels_a);
        assert!(!app.renderer.is_pending(full_slot));
    }
}

#[cfg(test)]
mod stage_budget_tests {
    use super::*;

    const MB: usize = 1 << 20;

    // Given stage caches within the budget, nothing is cleared
    #[test]
    fn nothing_is_trimmed_within_the_budget() {
        let sizes = [(Slot::Main, 300 * MB), (Slot::Window(PANE_AFTER), 100 * MB)];
        assert_eq!(stage_trim_order(&sizes, 512 * MB), vec![]);
    }

    // Given HDR views on an HDR display, their caches are reused by a drag like their 8-bit
    // views', but together hold no more than the budget on top of them: past it the loupe's,
    // which every tick reuses, is the one kept
    #[test]
    fn hdr_caches_stay_within_the_budget_and_keep_the_loupes() {
        use lcc_engine::pipeline::{OutputDepth, RenderRequest, SourceInfo, render_cached};
        let src = Arc::new(lcc_raster::Rgb32f::from_fn(64, 48, |x, y| [x as f32 / 16.0, y as f32 / 12.0, 2.0]));
        let mut settings = lcc_develop::DevelopSettings::default();
        settings.hdr.enabled = true;
        settings.effects.clarity = 20.0;
        let mut renderer = Renderer::default();
        let mut fill = |slot: Slot, depth: OutputDepth| {
            let cache = Arc::new(StageCache::default());
            render_cached(&src, &SourceInfo::default(), &settings, &RenderRequest { depth, ..RenderRequest::fit(64, 48) }, &cache);
            renderer.stages.insert(slot, cache);
        };
        fill(Slot::Main, OutputDepth::U8);
        let hdr = [Slot::HdrView(HDR_LOUPE), Slot::HdrView(HDR_ZOOMED + PANE_AFTER), Slot::HdrView(HDR_BEFORE), Slot::HdrView(HDR_COMPARE)];
        for slot in hdr {
            fill(slot, OutputDepth::F32Hdr);
        }
        let bytes = |r: &Renderer, slot: Slot| r.stages.get(&slot).map_or(0, |c| c.bytes());
        let hdr_bytes = |r: &Renderer| hdr.iter().map(|s| bytes(r, *s)).sum::<usize>();
        let (main, loupe, all) = (bytes(&renderer, Slot::Main), bytes(&renderer, Slot::HdrView(HDR_LOUPE)), hdr_bytes(&renderer));
        assert!(main > 0 && loupe > 0);
        // room for all of them: a drag keeps every stage
        renderer.stage_budget_override = Some(main + all);
        renderer.trim_stages();
        assert_eq!(hdr_bytes(&renderer), all);
        // room for the 8-bit views and one HDR view: the HDR caches shrink to the budget
        renderer.stage_budget_override = Some(main.max(loupe));
        for _ in 0..3 {
            renderer.trim_stages();
            assert!(hdr_bytes(&renderer) <= main.max(loupe), "HDR caches are bounded");
            assert_eq!(bytes(&renderer, Slot::HdrView(HDR_LOUPE)), loupe, "the loupe's HDR stages stay for the next tick");
            assert_eq!(bytes(&renderer, Slot::Main), main, "the 8-bit loupe's stages stay");
        }
    }

    // Given too much held, the caches nothing is looking at go first (a hover render's, a compare
    // tile's: rebuilt cheaply), the least useful first, and only as many as needed
    #[test]
    fn the_idle_caches_go_first_and_only_as_many_as_needed() {
        let sizes =
            [(Slot::Main, 300 * MB), (Slot::Hover, 100 * MB), (Slot::Second, 80 * MB), (Slot::Compare(0), 60 * MB), (Slot::Compare(1), 60 * MB)];
        // 600 held, 500 allowed: the hover render's 100 is enough
        assert_eq!(stage_trim_order(&sizes, 500 * MB), vec![Slot::Hover]);
        // 400 allowed: the compare tiles' too (380 left), then, at 350, the second window's
        assert_eq!(stage_trim_order(&sizes, 400 * MB), vec![Slot::Hover, Slot::Compare(0), Slot::Compare(1)]);
        assert_eq!(stage_trim_order(&sizes, 350 * MB), vec![Slot::Hover, Slot::Compare(0), Slot::Compare(1), Slot::Second]);
    }

    // Given the open photo's views over the budget, they are not trimmed: they are what a slider
    // drag reuses on every tick, and clearing them every frame costs far more than the memory
    // (issue #323 review: 3.6x slower drags on a 2800 px canvas). They are counted and reported,
    // and only a runaway (more than HARD_FACTOR times the budget) cuts the window and before
    // caches, never the whole-frame view's.
    #[test]
    fn the_open_photos_views_are_trimmed_only_in_a_runaway_and_never_the_whole_frame() {
        let views = [(Slot::Main, 400 * MB), (Slot::Window(PANE_AFTER), 500 * MB), (Slot::Window(PANE_BEFORE), 500 * MB), (Slot::Before, 100 * MB)];
        assert_eq!(stage_trim_order(&views, 400 * MB), vec![], "1.5 GB against a 400 MB budget is a busy Before/After, not a runaway");
        // against a 100 MB budget (HARD_FACTOR 4 = 400 MB) it is: the before side goes first, then the window
        assert_eq!(stage_trim_order(&views, 100 * MB), vec![Slot::Window(PANE_BEFORE), Slot::Before, Slot::Window(PANE_AFTER)]);
        let mut with_idle = views.to_vec();
        with_idle.push((Slot::Hover, 50 * MB));
        assert_eq!(stage_trim_order(&with_idle, 400 * MB), vec![Slot::Hover]);
        // a whole-frame view of any size stays
        assert_eq!(stage_trim_order(&[(Slot::Main, 9000 * MB)], MB), vec![]);
    }

    // Given caches of slots that hold nothing, they are never "trimmed"
    #[test]
    fn empty_caches_are_left_alone() {
        assert_eq!(stage_trim_order(&[(Slot::Hover, 0), (Slot::Second, 900 * MB)], 100 * MB), vec![Slot::Second]);
    }

    /// Batch work that pools GPU buffers after the idle trim (an export, Build Previews: renders on
    /// other threads) gets them trimmed again once it is done: before, the trim ran once per idle
    /// spell of the view and the pools kept up to their limit for the rest of the session. (A fake
    /// GPU: the process-wide pool is shared with the tests running beside this one; the trim of
    /// both cards' real pools is `lcc-gpu`'s `tests/helper_trim.rs`, a process of its own.)
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn buffers_pooled_by_batch_work_after_the_idle_trim_are_trimmed_again() {
        use std::cell::Cell;
        let (jobs, pooled, trims) = (Cell::new(0usize), Cell::new(0u64), Cell::new(0u32));
        let gpu = IdleGpu {
            batch_jobs: &|| jobs.get(),
            pooled: &|| pooled.get(),
            trim: &|| {
                trims.set(trims.get() + 1);
                pooled.set(0);
            },
        };
        let mut r = Renderer::default();
        let t0 = std::time::Instant::now();
        let at = |s: u64| t0 + std::time::Duration::from_secs(s);
        // the view's idle trim
        pooled.set(100);
        assert_eq!(r.idle_trim_step(at(0), &gpu), Some(IDLE_TRIM));
        assert_eq!(r.idle_trim_step(at(4), &gpu), None);
        assert_eq!((trims.get(), pooled.get()), (1, 0));
        // an export renders on other threads: looked at again, not trimmed under it
        jobs.set(2);
        pooled.set(500);
        assert_eq!(r.idle_trim_step(at(5), &gpu), Some(IDLE_TRIM));
        assert_eq!(trims.get(), 1);
        // it ends: its buffers are trimmed after the idle delay
        jobs.set(0);
        assert_eq!(r.idle_trim_step(at(8), &gpu), Some(IDLE_TRIM), "a new idle spell starts");
        assert_eq!(r.idle_trim_step(at(12), &gpu), None);
        assert_eq!((trims.get(), pooled.get()), (2, 0), "trimmed again");
        // what a trim could not free starts no further trims (no repaint every few seconds)
        let stuck = IdleGpu { batch_jobs: &|| 0, pooled: &|| 7, trim: &|| {} };
        r.idle = Some((at(12), false));
        assert_eq!(r.idle_trim_step(at(16), &stuck), None);
        assert_eq!(r.idle_trim_step(at(30), &stuck), None);
        assert_eq!(r.idle.map(|(_, trimmed)| trimmed), Some(true));
    }

    // Given real stage caches over the budget, the renderer clears them (the open photo's last)
    #[test]
    fn the_renderer_trims_real_stage_caches() {
        use lcc_engine::pipeline::{RenderRequest, SourceInfo, render_cached};
        let src = std::sync::Arc::new(lcc_raster::Rgb32f::from_fn(320, 240, |x, y| [x as f32 / 320.0, y as f32 / 240.0, 0.3]));
        let mut r = Renderer::default();
        for slot in [Slot::Main, Slot::Hover] {
            let cache: Arc<StageCache> = Default::default();
            let mut s = lcc_engine::develop::DevelopSettings::default();
            s.effects.clarity = 50.0;
            render_cached(&src, &SourceInfo::default(), &s, &RenderRequest::fit(320, 240), &cache);
            assert!(cache.bytes() > 0);
            r.stages.insert(slot, cache);
        }
        let total: usize = r.stages.values().map(|c| c.bytes()).sum();
        r.stage_budget_override = Some(total * 3 / 4);
        r.trim_stages();
        assert_eq!(r.stages[&Slot::Hover].bytes(), 0, "the hover render's cache went");
        assert!(r.stages[&Slot::Main].bytes() > 0, "the open photo's stayed");
        assert_eq!(r.stages_trimmed, 1);
        r.stage_budget_override = Some(0);
        r.trim_stages();
        assert!(r.stages[&Slot::Main].bytes() > 0, "the open photo's view is never trimmed");
        r.trim_stages();
        assert_eq!(r.stages_trimmed, 1, "and nothing is trimmed again");
    }

    // The renderer counts and trims what it holds, and says so
    #[test]
    fn the_renderer_reports_its_stage_budget() {
        let r = Renderer::default();
        let m = r.memory();
        assert!(m["stageCaches"]["budgetBytes"].as_u64().unwrap() > 0, "{m}");
        assert_eq!(m["stageCaches"]["trimmed"], 0);
        assert!(m["stageCaches"]["sharedSourceBytes"].is_u64(), "the one device copy of a big original is reported: {m}");
    }
}

#[cfg(test)]
mod color_image_tests {
    use super::*;

    // The texture a render becomes is what egui would make of its bytes, whatever the alpha
    #[test]
    fn the_fast_conversion_equals_eguis() {
        let img = lcc_raster::Rgba8::from_fn(37, 29, |x, y| {
            let a = match (x + y) % 5 {
                0 => 0,
                1 => 17,
                2 => 128,
                _ => 255,
            };
            [(x * 7) as u8, (y * 9) as u8, (x * y) as u8, a]
        });
        let want = egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.as_bytes());
        let got = color_image(&img);
        assert_eq!(got.size, want.size);
        assert_eq!(got.pixels, want.pixels);
    }

    #[test]
    fn an_empty_image_is_an_empty_texture() {
        let img = lcc_raster::Rgba8::new(0, 0);
        assert!(color_image(&img).pixels.is_empty());
    }
}

#[cfg(test)]
mod extended_linear_tests {
    use super::*;

    #[test]
    fn job_for_skips_display_profile_for_extended_linear_surface() {
        let mut r = Renderer::default();
        r.set_surface_extended_linear(true);

        let job = RenderJob {
            drawn_view_cache: false,
            on_screen: false,
            cache_generation: 0,
            request_id: 1,
            source_key: None,
            photo: lcc_catalog::PhotoId(1),
            level: lcc_engine::media::SourceLevel::Full,
            source: lcc_engine::media::SourceRef::File {
                path: "/tmp/test.jpg".to_string(),
                max_edge: 100,
                loader: None,
                fallback: None,
                denoise: None,
                options: Default::default(),
                reduced: None,
            },
            origin: lcc_catalog::Source::File { path: "/tmp/test.jpg".to_string() },
            info: Default::default(),
            display_fit: None,
            hdr_texels: None,
            superseded: None,
            discard_source: false,
            latest: None,
            kept: None,
            placement: None,
            settings: Default::default(),
            request: lcc_pipeline::RenderRequest {
                max_w: 100,
                max_h: 100,
                exact_size: false,
                quality: Default::default(),
                apply_crop: false,
                overlay: Default::default(),
                space: Default::default(),
                depth: Default::default(),
                proof: None,
                window: None,
                display: None,
                downscale: Default::default(),
                alpha: false,
            },
            key: 0,
            cache: None,
            view_cache: None,
            stages: None,
            display: None,
            slide: None,
            preview_cut: None,
        };

        let result = r.job_for(Slot::Main, job.clone());
        assert!(result.display.is_none(), "display profile should be None for extended linear surface");
    }
}
