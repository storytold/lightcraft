use super::options::{ExportFormat, MetadataPolicy, SharpenAmount, SharpenFor};
use super::watermark::{Watermark, draw_watermark, draw_watermark_deep_over};
use super::{
    ChromaSubsampling, DateTime, DeepImage, DeepSamples, EncodeImage, EncodeMeta, ExportOptions, Gps, Metadata, NamedSpace, OutputDepth, OutputSpace,
    Plane, Rgba8, Samples, encode, icc,
};
use std::borrow::Cow;

/// Unsharp mask on an 8-bit image (separable box-blur approximation of a small Gaussian).
pub fn output_sharpen(img: &mut Rgba8, target: SharpenFor, amount: SharpenAmount) {
    if !sharpens(img.width, img.height, target) {
        return;
    }
    let src: Vec<[f32; 3]> = img.data.iter().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
    if let Some(out) = unsharp(img.width, img.height, &src, target, amount) {
        for (p, v) in img.data.iter_mut().zip(out) {
            for c in 0..3 {
                p[c] = v[c].round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// [`output_sharpen`] on a high-bit-depth image (float images are sharpened in linear light).
pub fn output_sharpen_deep(img: &mut DeepImage, target: SharpenFor, amount: SharpenAmount) {
    let (w, h) = (img.width, img.height);
    if !sharpens(w, h, target) {
        return;
    }
    match &mut img.samples {
        DeepSamples::U16(v) => {
            let src: Vec<[f32; 3]> = v.as_chunks::<3>().0.iter().map(|c| [c[0] as f32, c[1] as f32, c[2] as f32]).collect();
            if let Some(out) = unsharp(w, h, &src, target, amount) {
                for (d, s) in v.iter_mut().zip(out.as_flattened()) {
                    *d = s.round().clamp(0.0, 65535.0) as u16;
                }
            }
        }
        DeepSamples::F32(v) => {
            let src: Vec<[f32; 3]> = v.as_chunks::<3>().0.iter().map(|c| [c[0], c[1], c[2]]).collect();
            if let Some(out) = unsharp(w, h, &src, target, amount) {
                for (d, s) in v.iter_mut().zip(out.as_flattened()) {
                    *d = s.max(0.0);
                }
            }
        }
    }
}

/// Whether output sharpening for `target` changes a `w × h` image ([`unsharp`] returns `None` otherwise).
fn sharpens(w: usize, h: usize, target: SharpenFor) -> bool {
    target != SharpenFor::None && w >= 3 && h >= 3
}

/// The sharpened values of `src` (`w × h`), or `None` when nothing is to be done.
fn unsharp(w: usize, h: usize, src: &[[f32; 3]], target: SharpenFor, amount: SharpenAmount) -> Option<Vec<[f32; 3]>> {
    let (radius, base) = match target {
        SharpenFor::None => return None,
        SharpenFor::Screen => (1usize, 0.35f32),
        SharpenFor::Matte => (2, 0.6),
        SharpenFor::Glossy => (1, 0.5),
    };
    let k = base
        * match amount {
            SharpenAmount::Low => 0.6,
            SharpenAmount::Standard => 1.0,
            SharpenAmount::High => 1.5,
        };
    if w < 3 || h < 3 {
        return None;
    }
    let r = radius as isize;
    let box_pass = |inp: &[[f32; 3]], horizontal: bool| -> Vec<[f32; 3]> {
        let mut out = vec![[0.0; 3]; inp.len()];
        // rows in parallel: each output pixel is computed on its own, so the bits don't depend on the split
        lightcraft_raster::par_rows(&mut out, w, |y, row| {
            for (x, o) in row.iter_mut().enumerate() {
                let mut acc = [0.0f32; 3];
                for d in -r..=r {
                    let (sx, sy) = if horizontal {
                        ((x as isize + d).clamp(0, w as isize - 1) as usize, y)
                    } else {
                        (x, (y as isize + d).clamp(0, h as isize - 1) as usize)
                    };
                    let p = inp[sy * w + sx];
                    acc = [acc[0] + p[0], acc[1] + p[1], acc[2] + p[2]];
                }
                let n = (2 * r + 1) as f32;
                *o = acc.map(|v| v / n);
            }
        });
        out
    };
    let blur = box_pass(&box_pass(src, true), false);
    Some(src.iter().zip(&blur).map(|(s, b)| std::array::from_fn(|c| s[c] + (s[c] - b[c]) * k)).collect())
}

/// Encode a rendered display-referred image according to `o` (its pixels must already be in
/// `o.effective_space()`). Resizing to `long_edge` is the
/// caller's job (render at that size); sharpening is applied here.
pub fn encode_image(img: &Rgba8, o: &ExportOptions) -> Result<Vec<u8>, String> {
    encode_with_metadata(img, o, None)
}

/// Like [`encode_image`], embedding `meta` (already filtered by the policy) as EXIF + XMP.
pub fn encode_with_metadata(img: &Rgba8, o: &ExportOptions, meta: Option<&Metadata>) -> Result<Vec<u8>, String> {
    let img = finish_8bit(img, o);
    let space = o.effective_space();
    let profile = icc::write_named(named_space(space));
    let exif = meta.map(lightcraft_meta::write_exif);
    let xmp = meta.map(|m| lightcraft_meta::write_xmp(m, None));
    let meta = EncodeMeta { icc: Some(&profile), exif: exif.as_deref(), xmp: xmp.as_deref(), ppi: Some(o.ppi) };
    // Opaque photo exports need no alpha channel (#1670). Keep real transparency for
    // callers of this encoder, and leave unrelated formats on their existing path.
    let rgb: Option<Vec<u8>> = (matches!(o.format, ExportFormat::Png | ExportFormat::Tiff | ExportFormat::Webp)
        && img.data.iter().all(|p| p.last() == Some(&255)))
    .then(|| img.data.iter().flat_map(|p| p.iter().take(3).copied()).collect());
    let e = match &rgb {
        Some(data) => EncodeImage::new(img.width as u32, img.height as u32, 3, Samples::U8(data)),
        None => EncodeImage::rgba8(&img),
    };
    let r = match o.format {
        ExportFormat::Jpeg => {
            // 4:4:4 normally; 4:2:0 when targeting a file size (much smaller at equal visual quality).
            let sub = if o.limit_kb.is_some() { ChromaSubsampling::S420 } else { ChromaSubsampling::S444 };
            return encode_jpeg_with_limit(o, |q| encode::encode_jpeg(&e, q, sub, &meta));
        }
        ExportFormat::Png => encode::encode_png(&e, &meta),
        ExportFormat::Tiff => encode::encode_tiff(&e, o.tiff_compression, &meta),
        ExportFormat::Psd => encode::encode_psd(&e, &meta),
        ExportFormat::Webp => encode::encode_webp_lossless(&e, &meta),
        ExportFormat::Avif => encode::encode_avif(&e, o.quality, 8, &meta),
        ExportFormat::Jxl => lightcraft_codecs::encode_jxl(&e, &o.jxl(space, false), &meta),
        f @ (ExportFormat::Original | ExportFormat::Dng) => return Err(format!("{f:?} export does not encode pixels")),
        ExportFormat::RenderedDng => return Err("Render to DNG needs a float render".into()),
    };
    r.map_err(|e| e.to_string())
}

/// Apply the same file-size policy to ordinary JPEGs and complete HDR gain map JPEGs.
fn encode_jpeg_with_limit(o: &ExportOptions, jpeg: impl Fn(u8) -> lightcraft_codecs::Result<Vec<u8>>) -> Result<Vec<u8>, String> {
    let Some(kb) = o.limit_kb else { return jpeg(o.quality).map_err(|e| e.to_string()) };
    let limit = u64::from(kb) * 1024;
    let (mut lo, mut hi) = (1u8, o.quality.min(100));
    let mut best = None;
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let b = jpeg(mid).map_err(|e| e.to_string())?;
        if b.len() as u64 <= limit {
            best = Some(b);
            lo = mid + 1;
        } else if mid == 1 {
            break;
        } else {
            hi = mid - 1;
        }
    }
    best.ok_or_else(|| format!("cannot fit within {kb} KB"))
}

/// Output sharpening and the watermark on an 8-bit render in `o.effective_space()`: the render itself when
/// there is neither (a copy is ~100 MB at 24 MP).
fn finish_8bit<'a>(img: &'a Rgba8, o: &ExportOptions) -> Cow<'a, Rgba8> {
    if o.watermark.is_none() && !sharpens(img.width, img.height, o.sharpen) {
        return Cow::Borrowed(img);
    }
    let mut img = img.clone();
    output_sharpen(&mut img, o.sharpen, o.sharpen_amount);
    let space = o.effective_space();
    if let Some(wm) = &o.watermark {
        let wm = Watermark { color: srgb8_in(space, wm.color), target: Some(space), ..wm.clone() };
        draw_watermark(&mut img, &wm);
    }
    Cow::Owned(img)
}

/// Output sharpening and the watermark on a high-bit-depth render (the render itself when there is neither).
fn finish_deep<'a>(img: &'a DeepImage, o: &ExportOptions) -> Cow<'a, DeepImage> {
    finish_deep_over(img, None, o)
}

/// [`finish_deep`] of an image with straight alpha `alpha`, which the watermark raises (and leaves
/// alone when there is no watermark).
fn finish_deep_over<'a>(img: &'a DeepImage, alpha: Option<&mut Plane>, o: &ExportOptions) -> Cow<'a, DeepImage> {
    if o.watermark.is_none() && !sharpens(img.width, img.height, o.sharpen) {
        return Cow::Borrowed(img);
    }
    let mut img = img.clone();
    output_sharpen_deep(&mut img, o.sharpen, o.sharpen_amount);
    if let Some(wm) = &o.watermark {
        let wm = Watermark { color: srgb8_in(img.space, wm.color), target: Some(img.space), ..wm.clone() };
        draw_watermark_deep_over(&mut img, alpha, &wm);
    }
    Cow::Owned(img)
}

/// Encode an ISO 21496-1 gain map JPEG from the SDR rendition `sdr` (8-bit, in the output space)
/// and the HDR rendition `hdr` (float linear, same space and size). The gain map is measured
/// against the base as written, so its 8-bit rounding is handed back by the map.
pub fn encode_gain_map_jpeg(sdr: &Rgba8, hdr: &DeepImage, o: &ExportOptions, meta: Option<&Metadata>) -> Result<Vec<u8>, String> {
    use lightcraft_codecs::gainmap;
    let base = finish_8bit(sdr, o);
    let hdr = finish_deep(hdr, o);
    let DeepSamples::F32(hv) = &hdr.samples else {
        return Err("HDR export needs a float HDR render".into());
    };
    let (w, h) = (base.width, base.height);
    if (hdr.width, hdr.height) != (w, h) || hv.len() < w * h * 3 {
        return Err("HDR and SDR renders differ in size".into());
    }
    let space = o.effective_space();
    let trc = space.trc();
    let sdr_lin: Vec<[f32; 3]> = base.data.iter().map(|p| [0, 1, 2].map(|c| trc.decode(p[c] as f32 / 255.0))).collect();
    let hdr_lin: &[[f32; 3]] = hv.as_chunks::<3>().0;
    let (map, gm) = gainmap::compute(&sdr_lin, hdr_lin, w, h, space.luma(), &gainmap::GainMapOptions::default()).map_err(|e| e.to_string())?;
    let profile = icc::write_named(named_space(space));
    let exif = meta.map(lightcraft_meta::write_exif);
    let xmp = meta.map(|m| lightcraft_meta::write_xmp(m, None));
    let em = EncodeMeta { icc: Some(&profile), exif: exif.as_deref(), xmp: xmp.as_deref(), ppi: Some(o.ppi) };
    let sub = if o.limit_kb.is_some() { ChromaSubsampling::S420 } else { ChromaSubsampling::S444 };
    encode_jpeg_with_limit(o, |q| gainmap::encode_jpeg(&EncodeImage::rgba8(&base), &map, &gm, q, sub, &em))
}

/// The codecs' name of an output space (for its ICC profile).
pub fn named_space(s: OutputSpace) -> NamedSpace {
    match s {
        OutputSpace::Srgb => NamedSpace::Srgb,
        OutputSpace::DisplayP3 => NamedSpace::DisplayP3,
        OutputSpace::AdobeRgb => NamedSpace::AdobeRgb,
        OutputSpace::ProPhoto => NamedSpace::ProPhoto,
        OutputSpace::Rec2020 => NamedSpace::Rec2020,
    }
}

/// An 8-bit sRGB colour expressed in `space` (encoded with its curve).
pub fn srgb8_in(space: OutputSpace, c: [u8; 3]) -> [u8; 3] {
    if space == OutputSpace::Srgb {
        return c;
    }
    let lin = c.map(lightcraft_color::transfer::decode_srgb8);
    let m = lightcraft_color::SRGB.to_space(&space.rgb_space()).to_f32();
    let t = space.trc();
    std::array::from_fn(|i| {
        let v = m[i][0] * lin[0] + m[i][1] * lin[1] + m[i][2] * lin[2];
        (t.encode(v) * 255.0 + 0.5) as u8
    })
}

/// Encode a render according to `o`: its high-bit-depth samples when it has them (16-bit PNG/TIFF,
/// 10-bit AVIF, 32-bit float linear TIFF), else its 8-bit image.
///
/// A render with alpha ([`lightcraft_pipeline::Rendered::alpha`]) keeps it in the formats that
/// can ([`ExportFormat::keeps_alpha`]) when it is not opaque everywhere (issue #2976).
pub fn encode_rendered(r: &lightcraft_pipeline::Rendered, o: &ExportOptions, meta: Option<&Metadata>) -> Result<Vec<u8>, String> {
    let alpha = r.alpha.as_ref().filter(|_| o.format.keeps_alpha());
    match &r.deep {
        Some(d) if o.effective_depth() != OutputDepth::U8 => encode_deep_with_alpha(d, alpha, o, meta),
        _ => match alpha {
            Some(a) => encode_with_metadata(&with_alpha(&r.image, a)?, o, meta),
            None => encode_with_metadata(&r.image, o, meta),
        },
    }
}

/// `img` with straight alpha `a` (its size) in its fourth channel.
fn with_alpha(img: &Rgba8, a: &Plane) -> Result<Rgba8, String> {
    if (a.width, a.height) != (img.width, img.height) || a.data.len() != img.data.len() {
        return Err("export: the transparency does not match the rendered image".into());
    }
    let mut out = img.clone();
    for (p, &av) in out.data.iter_mut().zip(&a.data) {
        p[3] = if av.is_finite() { (av.clamp(0.0, 1.0) * 255.0).round() as u8 } else { 255 };
    }
    Ok(out)
}

/// Encode a high-bit-depth image (see [`encode_rendered`]).
pub fn encode_deep(img: &DeepImage, o: &ExportOptions, meta: Option<&Metadata>) -> Result<Vec<u8>, String> {
    encode_deep_with_alpha(img, None, o, meta)
}

/// [`encode_deep`] keeping straight alpha `alpha` (the image's size) in PNG and TIFF when it is
/// not opaque everywhere.
fn encode_deep_with_alpha(img: &DeepImage, alpha: Option<&Plane>, o: &ExportOptions, meta: Option<&Metadata>) -> Result<Vec<u8>, String> {
    let meta_src = meta;
    let mut alpha = match alpha {
        Some(a) if (a.width, a.height) != (img.width, img.height) => return Err("export: the transparency does not match the rendered image".into()),
        a => a.cloned(),
    };
    let img = finish_deep_over(img, alpha.as_mut(), o);
    let alpha = alpha.filter(|a| match img.samples {
        DeepSamples::U16(_) => a.data.iter().any(|&v| (v.clamp(0.0, 1.0) * 65535.0).round() < 65535.0),
        DeepSamples::F32(_) => a.data.iter().any(|&v| v < 1.0),
    });
    let profile = match img.samples {
        DeepSamples::F32(_) => icc::write_matrix_trc(&img.space.rgb_space(), &lightcraft_codecs::Trc::Linear),
        DeepSamples::U16(_) => icc::write_named(named_space(img.space)),
    };
    let exif = meta.map(lightcraft_meta::write_exif);
    let xmp = meta.map(|m| lightcraft_meta::write_xmp(m, None));
    let meta = EncodeMeta { icc: Some(&profile), exif: exif.as_deref(), xmp: xmp.as_deref(), ppi: Some(o.ppi) };
    let (w, h) = (img.width as u32, img.height as u32);
    // RGB plus the alpha as a fourth sample
    fn rgba<T: Copy>(v: &[T], a: &Plane, alpha: impl Fn(f32) -> T) -> Vec<T> {
        v.as_chunks::<3>()
            .0
            .iter()
            .zip(&a.data)
            .flat_map(|(p, &av)| [p[0], p[1], p[2], alpha(if av.is_finite() { av.clamp(0.0, 1.0) } else { 1.0 })])
            .collect()
    }
    let (rgba16, rgba32);
    let e = match (&img.samples, alpha.as_ref().filter(|_| matches!(o.format, ExportFormat::Png | ExportFormat::Tiff))) {
        (DeepSamples::U16(v), Some(a)) => {
            rgba16 = rgba(v, a, |av| (av * 65535.0).round() as u16);
            EncodeImage::new(w, h, 4, Samples::U16(&rgba16))
        }
        (DeepSamples::F32(v), Some(a)) => {
            rgba32 = rgba(v, a, |av| av);
            EncodeImage::new(w, h, 4, Samples::F32(&rgba32))
        }
        (DeepSamples::U16(v), None) => EncodeImage::new(w, h, 3, Samples::U16(v)),
        (DeepSamples::F32(v), None) => EncodeImage::new(w, h, 3, Samples::F32(v)),
    };
    let r = match (o.format, &img.samples) {
        (ExportFormat::Png, DeepSamples::F32(v)) if img.space == OutputSpace::Rec2020 => lightcraft_codecs::encode_png_pq(w, h, v, &meta),
        (ExportFormat::Png, _) => encode::encode_png(&e, &meta),
        (ExportFormat::Tiff, _) => encode::encode_tiff(&e, o.tiff_compression, &meta),
        (ExportFormat::Psd, _) => encode::encode_psd(&e, &meta),
        (ExportFormat::RenderedDng, DeepSamples::F32(v)) => {
            let m = meta_src.cloned().unwrap_or_default();
            return lightcraft_raw::write_rendered_dng(img.width, img.height, v, &img.space.rgb_space(), &m, xmp.clone()).map_err(|e| e.to_string());
        }
        (ExportFormat::Avif, DeepSamples::F32(v)) if img.space == OutputSpace::Rec2020 => {
            lightcraft_codecs::encode_avif_hdr(w, h, v, o.quality, 8, &meta, o.hdr_transfer.codec())
        }
        (ExportFormat::Avif, _) => encode::encode_avif(&e, o.quality, 8, &meta),
        (ExportFormat::Jxl, DeepSamples::F32(_)) if img.space == OutputSpace::Rec2020 => {
            lightcraft_codecs::encode_jxl(&e, &o.jxl(img.space, true), &meta)
        }
        (ExportFormat::Jxl, DeepSamples::U16(_)) => lightcraft_codecs::encode_jxl(&e, &o.jxl(img.space, false), &meta),
        (f, _) => return Err(format!("{f:?} export is 8-bit only")),
    };
    r.map_err(|e| e.to_string())
}

/// Whether the export of `photo` under `o` leaves out its location (GPS and place names).
/// (`photo` and `catalog` let library-level privacy rules add to the export option.)
pub fn removes_location(_photo: &lightcraft_catalog::Photo, _catalog: &lightcraft_catalog::Catalog, o: &ExportOptions) -> bool {
    o.remove_location
}

/// The metadata to embed for `photo` under `o.metadata` / `o.remove_location`. `None` = embed nothing.
/// Keywords follow the catalog's keyword list (`Catalog::export_keywords`).
pub fn export_metadata(photo: &lightcraft_catalog::Photo, catalog: &lightcraft_catalog::Catalog, o: &ExportOptions) -> Option<Metadata> {
    let m = &photo.meta;
    let text = |s: &str| (!s.trim().is_empty()).then(|| s.to_string());
    // copyright info (also under "copyright only"): notice, creator, status, usage terms, info URL
    let mut out = Metadata {
        copyright: text(&m.copyright),
        copyright_marked: m.copyright_status.marked(),
        usage_terms: text(&m.usage_terms),
        copyright_url: text(&m.copyright_url),
        artist: text(&m.creator),
        software: Some("LightCraft".into()),
        ..Default::default()
    };
    match o.metadata {
        MetadataPolicy::None => return None,
        MetadataPolicy::Copyright => return Some(out),
        MetadataPolicy::All | MetadataPolicy::AllExceptCamera => {}
    }
    out.title = text(&m.title);
    out.caption = text(&m.caption);
    out.alt_text = text(&m.alt_text);
    out.extended_description = text(&m.extended_description);
    if !o.remove_location {
        out.sublocation = text(&m.location);
        out.city = text(&m.city);
        out.state = text(&m.state);
        out.country = text(&m.country);
    }
    let keywords = catalog.export_keywords(&m.keywords);
    out.keywords = keywords.flat;
    out.hierarchical_keywords = keywords.hierarchical;
    out.capture_time = photo.captured.as_deref().and_then(DateTime::parse_iso);
    out.rating = (photo.rating > 0).then_some(photo.rating as i8);
    // Pixels are exported upright: orientation is baked in.
    out.orientation = Some(lightcraft_meta::Orientation::Normal);
    if !o.remove_location {
        out.gps = m.gps.map(|(latitude, longitude)| Gps { latitude, longitude, altitude: None });
    }
    if o.metadata == MetadataPolicy::All {
        out.model = text(&m.camera);
        out.lens_model = text(&m.lens);
        out.focal_length = m.focal_mm.map(f64::from);
        out.f_number = m.aperture.map(f64::from);
        out.exposure_time = lightcraft_catalog::parse_shutter_seconds(&m.shutter);
        out.iso = m.iso;
    }
    Some(out)
}
