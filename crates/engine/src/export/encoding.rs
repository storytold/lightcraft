use super::options::{ExportFormat, MetadataPolicy, SharpenAmount, SharpenFor};
use super::watermark::{Watermark, draw_watermark, draw_watermark_deep};
use super::{
    ChromaSubsampling, DateTime, DeepImage, DeepSamples, EncodeImage, EncodeMeta, ExportOptions, Gps, Metadata, NamedSpace, OutputDepth, OutputSpace,
    Rgba8, Samples, encode, icc,
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
            let mut jpeg = |q: u8| encode::encode_jpeg(&e, q, sub, &meta).map_err(|e| e.to_string());
            return match o.limit_kb {
                Some(kb) => {
                    let fixed = profile.len() + exif.as_ref().map_or(0, Vec::len) + xmp.as_ref().map_or(0, |x| x.len());
                    fit_jpeg(&img, o.quality, kb, sub, fixed, &mut jpeg)
                }
                None => jpeg(o.quality),
            };
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

#[cfg(test)]
thread_local! {
    /// Full-size JPEG encodes [`fit_jpeg`] made on this thread so far (tests: at most two per photo).
    pub(crate) static FIT_ENCODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The quality of each of those encodes, in order.
    pub(crate) static FIT_QUALITIES: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The JPEG of `img` at about the highest quality up to `quality` whose file (`encode(q)`: the whole
/// file at quality q, `fixed` of its bytes, metadata and any gain map, don't change with q) fits within `kb` KB, in two full-size
/// encodes (note 28: a binary search made seven or eight of a 6000 px photo, ~30 s per photo).
///
/// The first is at `quality`; when it is too big, how the size falls with the quality is measured
/// on a small mosaic of the photo's own blocks (a sixteenth of it), scaled to the first encode's
/// size, and the second encode is at the quality that curve says fits, with a margin. Should the
/// photo still not fit (rare: the mosaic misjudged it), the curve is corrected by that encode and
/// a lower quality tried, until quality 1.
///
/// The quality is a prediction, not a search: the margin can leave it a quality or two below the
/// highest that would fit (a few percent of the file size), and a higher quality is not tried
/// once one fits. That is the trade for two encodes instead of seven or eight. The gain map of an
/// HDR JPEG is part of `fixed`, encoded once at the export's quality: one channel at half the
/// photo's width and height by default, so lowering its quality would gain little.
fn fit_jpeg(
    img: &Rgba8,
    quality: u8,
    kb: u32,
    sub: ChromaSubsampling,
    fixed: usize,
    encode: &mut dyn FnMut(u8) -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    let limit = (kb as usize).saturating_mul(1024);
    let mut full = |q: u8| {
        #[cfg(test)]
        {
            FIT_ENCODES.with(|c| c.set(c.get() + 1));
            FIT_QUALITIES.with(|v| v.borrow_mut().push(q));
        }
        encode(q)
    };
    let quality = quality.clamp(1, 100);
    let first = full(quality)?;
    if first.len() <= limit {
        return Ok(first);
    }
    let mosaic = sample_mosaic(img);
    let me = EncodeImage::rgba8(&mosaic);
    let none = EncodeMeta::default();
    let small = |q: u8| encode::encode_jpeg(&me, q, sub, &none).map(|b| b.len()).map_err(|e| e.to_string());
    let (mut q_at, mut file_at) = (quality, first.len());
    let mut m_at = small(q_at)?.max(1);
    // (an 8×8 encode's size: the headers every encode has)
    let headers = encode::encode_jpeg(&EncodeImage::rgba8(&Rgba8 { width: 8, height: 8, data: vec![[128, 128, 128, 255]; 64] }), quality, sub, &none)
        .map(|b| b.len())
        .unwrap_or(600);
    let mut margin = 0.96;
    loop {
        // file(q) ≈ fixed + k × (mosaic(q) − headers): `fixed` (metadata, profile, a gain map)
        // doesn't shrink with the quality
        let pixel_bytes = |m: usize| m.saturating_sub(headers).max(1) as f64;
        let k = file_at.saturating_sub(fixed) as f64 / pixel_bytes(m_at);
        let target = limit as f64 * margin;
        // highest quality below q_at predicted to fit (binary search on the mosaic)
        let (mut lo, mut hi) = (1u8, q_at.saturating_sub(1).max(1));
        let mut pick = 1u8;
        while lo <= hi {
            let mid = lo + (hi - lo) / 2;
            let predicted = fixed as f64 + k * pixel_bytes(small(mid)?);
            if predicted <= target {
                pick = mid;
                lo = mid + 1;
            } else if mid == 1 {
                break;
            } else {
                hi = mid - 1;
            }
        }
        let b = full(pick)?;
        if b.len() <= limit {
            return Ok(b);
        }
        if pick <= 1 {
            return Err(format!("cannot fit within {kb} KB"));
        }
        // the mosaic misjudged this photo: correct the curve with what this encode gave
        (q_at, file_at) = (pick, b.len());
        m_at = small(q_at)?.max(1);
        margin *= 0.97;
    }
}

/// A sixteenth of `img`: its 32 px blocks at every fourth row and column of blocks, side by side
/// (whole JPEG MCUs, so the mosaic's blocks code as the photo's do). Small photos are used whole.
fn sample_mosaic(img: &Rgba8) -> Rgba8 {
    const T: usize = 32;
    const STEP: usize = 4;
    let (w, h) = (img.width, img.height);
    let (cols, rows) = (w / T / STEP, h / T / STEP);
    if cols == 0 || rows == 0 {
        return img.clone();
    }
    let (mw, mh) = (cols * T, rows * T);
    let mut data = vec![[0u8; 4]; mw * mh];
    for r in 0..rows {
        for c in 0..cols {
            let (sx, sy) = (c * T * STEP + T * (STEP / 2), r * T * STEP + T * (STEP / 2));
            for y in 0..T {
                let (src_y, dst_y) = (sy + y, r * T + y);
                let src = img.data.get(src_y * w + sx..src_y * w + sx + T);
                let dst = data.get_mut(dst_y * mw + c * T..dst_y * mw + c * T + T);
                if let (Some(src), Some(dst)) = (src, dst) {
                    dst.copy_from_slice(src);
                }
            }
        }
    }
    Rgba8 { width: mw, height: mh, data }
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
    if o.watermark.is_none() && !sharpens(img.width, img.height, o.sharpen) {
        return Cow::Borrowed(img);
    }
    let mut img = img.clone();
    output_sharpen_deep(&mut img, o.sharpen, o.sharpen_amount);
    if let Some(wm) = &o.watermark {
        let wm = Watermark { color: srgb8_in(img.space, wm.color), target: Some(img.space), ..wm.clone() };
        draw_watermark_deep(&mut img, &wm);
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
    // the gain map is encoded once; a file size limit only encodes the primary image again
    let gain_jpeg = gainmap::encode_gain(&map, &gm, o.quality).map_err(|e| e.to_string())?;
    let e = EncodeImage::rgba8(&base);
    let mut primary = |q: u8| gainmap::encode_with_gain(&e, &gain_jpeg, q, sub, &em).map_err(|e| e.to_string());
    match o.limit_kb {
        Some(kb) => {
            let fixed = gain_jpeg.len() + profile.len() + exif.as_ref().map_or(0, Vec::len) + xmp.as_ref().map_or(0, |x| x.len());
            fit_jpeg(&base, o.quality, kb, sub, fixed, &mut primary)
        }
        None => primary(o.quality),
    }
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
pub fn encode_rendered(r: &lightcraft_pipeline::Rendered, o: &ExportOptions, meta: Option<&Metadata>) -> Result<Vec<u8>, String> {
    match &r.deep {
        Some(d) if o.effective_depth() != OutputDepth::U8 => encode_deep(d, o, meta),
        _ => encode_with_metadata(&r.image, o, meta),
    }
}

/// Encode a high-bit-depth image (see [`encode_rendered`]).
pub fn encode_deep(img: &DeepImage, o: &ExportOptions, meta: Option<&Metadata>) -> Result<Vec<u8>, String> {
    let meta_src = meta;
    let img = finish_deep(img, o);
    let profile = match img.samples {
        DeepSamples::F32(_) => icc::write_matrix_trc(&img.space.rgb_space(), &lightcraft_codecs::Trc::Linear),
        DeepSamples::U16(_) => icc::write_named(named_space(img.space)),
    };
    let exif = meta.map(lightcraft_meta::write_exif);
    let xmp = meta.map(|m| lightcraft_meta::write_xmp(m, None));
    let meta = EncodeMeta { icc: Some(&profile), exif: exif.as_deref(), xmp: xmp.as_deref(), ppi: Some(o.ppi) };
    let (w, h) = (img.width as u32, img.height as u32);
    let e = match &img.samples {
        DeepSamples::U16(v) => EncodeImage::new(w, h, 3, Samples::U16(v)),
        DeepSamples::F32(v) => EncodeImage::new(w, h, 3, Samples::F32(v)),
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
