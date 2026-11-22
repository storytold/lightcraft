//! Mask thumbnails for the Masking panel (as Lightroom Classic and Capture One show them): each
//! mask and each of its components drawn white on black over a small rendition of the photo.
//!
//! The photo is rendered once per look (without masks, ~96 px) on a background thread; each
//! thumbnail is then evaluated on the UI thread — a few milliseconds at that size — a handful per
//! frame at most, and cached as a texture by what it shows. Kept in egui's memory, so the panel
//! needs no state of its own. The small rendition is taken before lens corrections, Upright and
//! Transform, where a mask's coordinates live (`Mask::on_photo`).
//!
//! Ported from OneSlips' fork (own design there).

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use egui::{Color32, ColorImage, TextureHandle, TextureOptions};
use lcc_catalog::PhotoId;
use lcc_develop::{DevelopSettings, Mask};

use crate::LccApp;

/// Long edge of the photo the thumbnails are evaluated on.
const EDGE: usize = 96;
/// Thumbnails evaluated per frame (the rest follow on the next frames).
const PER_FRAME: usize = 4;
/// Textures kept.
const KEEP: usize = 256;

struct Base {
    key: u64,
    w: usize,
    h: usize,
    img: lcc_raster::Rgb32f,
    log_l: lcc_raster::Plane,
}

#[derive(Default)]
struct State {
    base: Option<Base>,
    pending: Option<(u64, mpsc::Receiver<Option<(usize, usize, Vec<u8>)>>)>,
    tex: HashMap<u64, TextureHandle>,
    /// Evaluated this frame (`frame` number, count).
    budget: (u64, usize),
}

type Shared = Arc<Mutex<State>>;

fn shared(ctx: &egui::Context) -> Shared {
    let id = egui::Id::new("mask-thumbs");
    ctx.data_mut(|d| d.get_temp_mut_or_insert_with::<Shared>(id, Shared::default).clone())
}

fn mix(h: u64, v: u64) -> u64 {
    (h ^ v).wrapping_mul(0x0100_0000_01b3).rotate_left(29)
}

fn hash_str(h: u64, s: &str) -> u64 {
    s.bytes().fold(h, |h, b| mix(h, u64::from(b)))
}

/// The key of photo `id`'s look without masks (the thumbnails' background).
fn base_key(id: PhotoId, d: &DevelopSettings) -> u64 {
    let mut d = d.clone();
    d.masks.clear();
    d.crop = Default::default();
    mix(mix(0xcbf2_9ce4_8422_2325, id.0), d.hash64())
}

/// The thumbnail of `mask` (a whole mask, or one component made into a mask) on photo `id`, if
/// it is ready; asks for what is missing (repaints follow).
pub fn thumb(app: &mut LccApp, ctx: &egui::Context, id: PhotoId, d: &DevelopSettings, mask: &Mask) -> Option<TextureHandle> {
    let st = shared(ctx);
    let mut st = st.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let bk = base_key(id, d);
    // the photo's small rendition
    if st.base.as_ref().is_none_or(|b| b.key != bk) {
        let got = match &st.pending {
            Some((k, rx)) if *k == bk => match rx.try_recv() {
                Ok(r) => Some(r),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(None),
            },
            _ => {
                let mut nd = d.clone();
                nd.masks.clear();
                // on the photo before the warp, as masks are (and uncropped)
                nd.geometry = Default::default();
                nd.optics.lens_profile = false;
                nd.optics.distortion = 0.0;
                nd.crop.flip_h = false;
                nd.crop.flip_v = false;
                if let Some(job) = app.session.preview_job(id, EDGE, EDGE, false, &nd) {
                    let (tx, rx) = mpsc::channel();
                    let spawned = std::thread::Builder::new().name("mask-thumbs".into()).spawn(move || {
                        let r = job.run().rendered.ok().map(|r| {
                            let img = r.image;
                            (img.width, img.height, img.data.iter().flat_map(|p| [p[0], p[1], p[2]]).collect::<Vec<u8>>())
                        });
                        let _ = tx.send(r);
                    });
                    if spawned.is_ok() {
                        st.pending = Some((bk, rx));
                    }
                }
                None
            }
        };
        match got {
            Some(Some((w, h, rgb))) if w > 0 && h > 0 && rgb.len() >= w * h * 3 => {
                let lin = |c: u8| {
                    let c = f32::from(c) / 255.0;
                    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
                };
                let px = |x: usize, y: usize| {
                    let i = (y * w + x) * 3;
                    match rgb.get(i..i + 3) {
                        Some(&[r, g, b]) => [lin(r), lin(g), lin(b)],
                        Some(_) => [0.0; 3],
                        None => [0.0; 3],
                    }
                };
                let img = lcc_raster::Rgb32f::from_fn(w, h, px);
                let log_l = lcc_raster::Plane::from_fn(w, h, |x, y| {
                    let p = img.data.get(y * w + x).copied().unwrap_or_default();
                    (0.2627 * p[0] + 0.678 * p[1] + 0.0593 * p[2]).max(1e-6).log2()
                });
                st.base = Some(Base { key: bk, w, h, img, log_l });
                st.pending = None;
            }
            Some(_) => {
                st.pending = None;
                return None;
            }
            None => {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return None;
            }
        }
    }
    let key = hash_str(bk, &serde_json::to_string(mask).unwrap_or_default());
    if let Some(t) = st.tex.get(&key) {
        return Some(t.clone());
    }
    let frame = ctx.cumulative_frame_nr();
    if st.budget.0 != frame {
        st.budget = (frame, 0);
    }
    if st.budget.1 >= PER_FRAME {
        ctx.request_repaint();
        return None;
    }
    st.budget.1 += 1;
    let base = st.base.as_ref()?;
    let (w, h) = (base.w, base.h);
    let frame_geom = lcc_pipeline::geometry::Frame::new(w, h, &DevelopSettings::default(), false);
    let mut m = mask.clone();
    m.visible = true;
    m.adjust.amount = 100.0;
    let alpha = lcc_pipeline::masks::evaluate_one(&m, &frame_geom, w, h, &base.img, &base.log_l, 0.0, None);
    let pixels: Vec<Color32> = alpha.data.iter().map(|a| Color32::from_gray((a.clamp(0.0, 1.0) * 255.0) as u8)).collect();
    if pixels.len() != w * h {
        return None;
    }
    let tex = ctx.load_texture(
        format!("mask-thumb-{key:x}"),
        ColorImage { size: [w, h], source_size: egui::vec2(w as f32, h as f32), pixels },
        TextureOptions::LINEAR,
    );
    if st.tex.len() >= KEEP {
        st.tex.clear();
    }
    st.tex.insert(key, tex.clone());
    Some(tex)
}

/// Draw `tex` fitted into `r` (black where it doesn't reach), or an empty frame while it isn't ready.
pub fn paint(p: &egui::Painter, r: egui::Rect, tex: Option<&TextureHandle>, border: Color32) {
    p.rect_filled(r, 2.0, Color32::BLACK);
    if let Some(t) = tex {
        let [w, h] = t.size();
        let s = (r.width() / w.max(1) as f32).min(r.height() / h.max(1) as f32);
        let size = egui::vec2(w as f32 * s, h as f32 * s);
        let rr = egui::Rect::from_center_size(r.center(), size);
        p.image(t.id(), rr, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
    }
    p.rect_stroke(r, 2.0, egui::Stroke::new(1.0, border), egui::StrokeKind::Inside);
}
