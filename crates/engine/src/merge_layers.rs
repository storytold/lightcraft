//! Merge to Layers (issue #769): an HDR merge's frames as a layered PSD/PSB, ready to blend by hand.
//!
//! Each layer is one source frame **as the merge aligned it** (global fit, full-size refinement, local tile-by-tile
//! flow, Lanczos-3: [`lcc_merge::merge_hdr_frames_aligned`]), so the layers line up as well as the merge's own result.
//! Every frame is rendered at its own exposure with the reference's colour and default develop settings, in the chosen
//! colour space at 8 or 16 bits; pixels the warp pulled from outside the frame are transparent. Optionally the merged
//! result sits at the bottom ("Merged") and the deghost map on top as a hidden grayscale layer.
//!
//! Lens corrections are not applied to the layers (the merged DNG still carries them), so every layer, the merged one
//! included, stays in the same geometry.
//!
//! Sources: own design.

use lcc_merge::{AlignedFrame, DngSamples, HdrResult};
use lcc_pipeline::{OutputDepth, OutputSpace, Quality, RenderRequest};
use lcc_raster::{Plane, Rgb32f};
use serde::{Deserialize, Serialize};

use crate::cmd::edit_in::{MAX_LAYER_PIXELS, MAX_LAYERS, PSD_MAX_SIDE};

/// The layered document's format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayersFormat {
    /// PSD, or PSB when a side passes 30,000 px.
    #[default]
    Psd,
    /// Always PSB.
    Psb,
}

impl LayersFormat {
    pub fn extension(self) -> &'static str {
        match self {
            LayersFormat::Psd => "psd",
            LayersFormat::Psb => "psb",
        }
    }
}

/// What `merge.hdr {layers: true, …}` writes beside (or instead of nothing but) the DNG.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LayerOutput {
    pub format: LayersFormat,
    /// 8 or 16.
    pub bit_depth: u8,
    pub color_space: OutputSpace,
    /// A "Merged" layer (the automatic result) at the bottom.
    pub include_merged: bool,
    /// The deghost map as a hidden grayscale layer on top (when the merge deghosted).
    pub deghost_mask: bool,
}

impl Default for LayerOutput {
    fn default() -> Self {
        LayerOutput { format: LayersFormat::Psd, bit_depth: 16, color_space: OutputSpace::Srgb, include_merged: false, deghost_mask: false }
    }
}

impl LayerOutput {
    /// Layers the document holds for `frames` sources (the deghost map counted when asked for).
    pub fn layer_count(&self, frames: usize) -> usize {
        frames.saturating_add(usize::from(self.include_merged)).saturating_add(usize::from(self.deghost_mask))
    }

    /// Refuse a document too large to hold: more than [`MAX_LAYERS`] layers or canvas × layers above
    /// [`MAX_LAYER_PIXELS`].
    pub fn check_size(&self, frames: usize, canvas_pixels: usize) -> Result<(), String> {
        let layers = self.layer_count(frames);
        if layers > MAX_LAYERS {
            return Err(format!("Merge to Layers holds at most {MAX_LAYERS} layers; this merge would make {layers}"));
        }
        let total = (canvas_pixels as u64).saturating_mul(layers as u64);
        if total > MAX_LAYER_PIXELS {
            return Err(format!(
                "Merge to Layers would hold {:.0} million pixels ({layers} layers of {:.1} MP), above its limit of {} million: select fewer photos or turn off the merged/deghost layers",
                total as f64 / 1e6,
                canvas_pixels as f64 / 1e6,
                MAX_LAYER_PIXELS / 1_000_000
            ));
        }
        Ok(())
    }
}

/// One rendered layer: RGB16 samples and straight alpha (0..=65535) at the canvas size.
struct Layer {
    name: String,
    rgba: Vec<u16>,
    visible: bool,
}

/// Render `img` (scene-linear, sensor orientation, `r`'s colour) as the pipeline renders a merged DNG, at full size.
fn render(img: &Rgb32f, r: &HdrResult, space: OutputSpace) -> Result<(usize, usize, Vec<u16>), String> {
    let dng = lcc_merge::write_linear_dng(img, &r.color, r.orientation, &r.metadata, r.baseline_exposure, DngSamples::Half, &[])
        .map_err(|e| e.to_string())?;
    let edge = img.width.max(img.height);
    let (src, info) = crate::files::load_bytes(&dng, edge)?;
    drop(dng);
    let s = lcc_develop::DevelopSettings::for_raw(info.as_shot_temp, info.as_shot_tint);
    let req = RenderRequest { quality: Quality::Full, apply_crop: false, space, depth: OutputDepth::U16, ..RenderRequest::fit(edge, edge) };
    let out = lcc_pipeline::render(&src, &info, &s, &req);
    let deep = out.deep.ok_or("the layer render gave no 16-bit image")?;
    let lcc_pipeline::DeepSamples::U16(v) = deep.samples else {
        return Err("the layer render gave float samples".into());
    };
    if v.len() < deep.width.saturating_mul(deep.height).saturating_mul(3) {
        return Err("the layer render is short".into());
    }
    Ok((deep.width, deep.height, v))
}

/// `mask` (0..1, sensor orientation) sampled at each output pixel (nearest), as 16-bit alpha.
fn alpha_of(mask: Option<&Plane>, orientation: lcc_geom::Orientation, w: usize, h: usize) -> Vec<u16> {
    let Some(m) = mask.map(|m| m.oriented(orientation)).filter(|m| m.width > 0 && m.height > 0) else {
        return vec![u16::MAX; w.saturating_mul(h)];
    };
    let mut a = Vec::with_capacity(w.saturating_mul(h));
    for y in 0..h {
        let my = ((y as f64 + 0.5) / h as f64 * m.height as f64) as usize;
        for x in 0..w {
            let mx = ((x as f64 + 0.5) / w as f64 * m.width as f64) as usize;
            let v = m.data.get(my.min(m.height - 1) * m.width + mx.min(m.width - 1)).copied().unwrap_or(1.0);
            a.push((v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16);
        }
    }
    a
}

fn interleave(rgb: &[u16], alpha: &[u16]) -> Vec<u16> {
    let mut v = Vec::with_capacity(alpha.len().saturating_mul(4));
    for (c, a) in rgb.as_chunks::<3>().0.iter().zip(alpha) {
        // transparent pixels carry no colour: masks see an honest edge
        if *a == 0 {
            v.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            v.extend_from_slice(&[c[0], c[1], c[2], *a]);
        }
    }
    v
}

/// "−2", "+1.3", "0" — an exposure step as layer names show it.
fn ev_label(ev: f64) -> String {
    let r = (ev * 10.0).round() / 10.0;
    if r.abs() < 0.05 {
        "0".into()
    } else if r < 0.0 {
        format!("\u{2212}{}", trim(-r))
    } else {
        format!("+{}", trim(r))
    }
}

fn trim(v: f64) -> String {
    let s = format!("{v:.1}");
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

/// The layered document: `frames` (aligned, in the caller's order, named by `names`) bottom to top in shooting order
/// (the first source at the top), the merged result under them and the deghost map over them when asked.
/// `progress(fraction)` returns false to cancel.
pub fn write(
    r: &HdrResult,
    frames: Vec<AlignedFrame>,
    names: &[String],
    opts: &LayerOutput,
    progress: &dyn Fn(f32) -> bool,
) -> Result<Vec<u8>, String> {
    let (w, h) = (r.radiance.width, r.radiance.height);
    if w == 0 || h == 0 {
        return Err("nothing to write".into());
    }
    opts.check_size(frames.len(), w.saturating_mul(h))?;
    let n = frames.len();
    let total = opts.layer_count(n).max(1) as f32;
    let mut done = 0f32;
    let step = |done: &mut f32| {
        *done += 1.0;
        if progress(*done / total) { Ok(()) } else { Err("cancelled".to_string()) }
    };
    let mut canvas: Option<(usize, usize)> = None;
    let mut fit = |cw: usize, ch: usize| -> Result<(), String> {
        match canvas {
            None => {
                canvas = Some((cw, ch));
                Ok(())
            }
            Some(c) if c == (cw, ch) => Ok(()),
            Some(_) => Err("the layers rendered at different sizes".into()),
        }
    };
    let mut layers: Vec<Layer> = Vec::with_capacity(opts.layer_count(n));
    if opts.include_merged {
        let (lw, lh, rgb) = render(&r.radiance, r, opts.color_space)?;
        fit(lw, lh)?;
        let alpha = vec![u16::MAX; lw.saturating_mul(lh)];
        layers.push(Layer { name: "Merged".into(), rgba: interleave(&rgb, &alpha), visible: true });
        step(&mut done)?;
    }
    // the first source on top: PSD layers go bottom to top
    let mut frame_layers = Vec::with_capacity(n);
    for (i, f) in frames.into_iter().enumerate() {
        let img = f.pixels.to_rgb32f();
        drop(f.pixels);
        let (lw, lh, rgb) = render(&img, r, opts.color_space)?;
        drop(img);
        fit(lw, lh)?;
        let mask = f.valid.map(|v| {
            let mut p = Plane::new(w, h);
            for (o, b) in p.data.iter_mut().zip(v) {
                *o = if b { 1.0 } else { 0.0 };
            }
            p
        });
        let alpha = alpha_of(mask.as_ref(), r.orientation, lw, lh);
        let ev = r.ev.get(i).copied().unwrap_or(0.0);
        let name = format!("{} (EV {})", names.get(i).map(String::as_str).unwrap_or("Frame"), ev_label(ev));
        frame_layers.push(Layer { name, rgba: interleave(&rgb, &alpha), visible: true });
        step(&mut done)?;
    }
    frame_layers.reverse();
    layers.extend(frame_layers);
    let (cw, ch) = canvas.ok_or("nothing to write")?;
    if opts.deghost_mask && !r.ghost.data.is_empty() {
        let g = alpha_of(Some(&r.ghost), r.orientation, cw, ch);
        let rgba: Vec<u16> = g.iter().flat_map(|&v| [v, v, v, u16::MAX]).collect();
        layers.push(Layer { name: "Deghost map".into(), rgba, visible: false });
        step(&mut done)?;
    }
    psd(layers, cw, ch, opts)
}

fn psd(layers: Vec<Layer>, w: usize, h: usize, opts: &LayerOutput) -> Result<Vec<u8>, String> {
    let (w32, h32) = (u32::try_from(w).map_err(|_| "too large")?, u32::try_from(h).map_err(|_| "too large")?);
    let eight = opts.bit_depth == 8;
    let pixels = |v: Vec<u16>| {
        if eight {
            lcc_psd::PixelData::Rgba8(v.into_iter().map(|x| ((u32::from(x) * 255 + 32_767) / 65_535) as u8).collect())
        } else {
            lcc_psd::PixelData::Rgba16(v)
        }
    };
    let mut b = lcc_psd::PsdBuilder::new(w32, h32).depth(if eight { 8 } else { 16 });
    if opts.format == LayersFormat::Psb || w32 > PSD_MAX_SIDE || h32 > PSD_MAX_SIDE {
        b = b.version(lcc_psd::Version::Psb);
    }
    b = b.icc_profile(lcc_codecs::icc::write_named(crate::export::named_space(opts.color_space)));
    // the composite: visible layers over each other (straight alpha "over")
    let mut comp = vec![0f32; w.saturating_mul(h).saturating_mul(4)];
    for l in layers.iter().filter(|l| l.visible) {
        for (o, p) in comp.as_chunks_mut::<4>().0.iter_mut().zip(l.rgba.as_chunks::<4>().0) {
            let a = f32::from(p[3]) / 65535.0;
            if a <= 0.0 {
                continue;
            }
            let ob = o[3] * (1.0 - a);
            let oa = a + ob;
            for c in 0..3 {
                o[c] = (f32::from(p[c]) * a + o[c] * ob) / oa;
            }
            o[3] = oa;
        }
    }
    let comp: Vec<u16> =
        comp.as_chunks::<4>().0.iter().flat_map(|p| [p[0] as u16, p[1] as u16, p[2] as u16, (p[3] * 65535.0 + 0.5) as u16]).collect();
    for l in layers {
        let mut spec = lcc_psd::LayerSpec::new(l.name, 0, 0, w32, h32, pixels(l.rgba));
        spec.visible = l.visible;
        b.push_layer(spec);
    }
    b.composite(pixels(comp));
    b.to_bytes().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_documents_are_refused_with_the_limit() {
        let o = LayerOutput::default();
        // 64 frames of 200 MP
        let e = o.check_size(64, 200_000_000).unwrap_err();
        assert!(e.contains("limit"), "{e}");
        assert!(o.check_size(MAX_LAYERS + 1, 1).is_err());
        assert!(o.check_size(5, 24_000_000).is_ok());
    }
}
