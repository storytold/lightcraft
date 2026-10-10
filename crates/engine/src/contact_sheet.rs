//! Paginated contact sheets, rendered with the normal export pipeline and packaged as PDF.
use crate::export::{Anchor, ExportFormat, ExportOptions, Watermark, draw_watermark, encode_image};
use crate::{Session, media::RenderJob};
use dac_pipeline::{OutputDepth, OutputSpace};
use dac_raster::Rgba8;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Options {
    pub paper: String,
    pub landscape: bool,
    pub columns: usize,
    pub rows: usize,
    pub captions: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { paper: "a4".into(), landscape: false, columns: 3, rows: 4, captions: true }
    }
}

impl Options {
    pub fn parse(p: &Value) -> Result<Self, String> {
        let mut settings = p.as_object().cloned().ok_or("params must be an object")?;
        settings.remove("path");
        settings.remove("ids");
        settings.remove("id");
        let o: Self = serde_json::from_value(Value::Object(settings)).map_err(|e| e.to_string())?;
        o.layout()?;
        Ok(o)
    }

    fn layout(&self) -> Result<Layout, String> {
        if !(1..=8).contains(&self.columns) || !(1..=10).contains(&self.rows) {
            return Err("columns must be 1–8 and rows 1–10".into());
        }
        let (mut w, mut h) = match self.paper.as_str() {
            "a4" => (595.28, 841.89),
            "letter" => (612.0, 792.0),
            _ => return Err("paper must be a4 or letter".into()),
        };
        if self.landscape {
            std::mem::swap(&mut w, &mut h);
        }
        // Fixed 150 dpi, bounded paper sizes and grid dimensions keep allocations predictable.
        let width = (w * 150.0 / 72.0_f64).round() as usize;
        let height = (h * 150.0 / 72.0_f64).round() as usize;
        Ok(Layout { points: (w, h), width, height, cell_w: (width - 120) / self.columns, cell_h: (height - 120) / self.rows })
    }
}

struct Layout {
    points: (f64, f64),
    width: usize,
    height: usize,
    cell_w: usize,
    cell_h: usize,
}

pub struct Prepared {
    options: Options,
    items: Vec<(String, RenderJob)>,
}

pub struct Document {
    pub bytes: Vec<u8>,
    pub pages: usize,
    pub photos: usize,
}

pub fn prepare(session: &mut Session, p: &Value) -> Result<Prepared, String> {
    let options = Options::parse(p)?;
    if let Some(ids) = p.get("ids") {
        let a = ids.as_array().ok_or("ids must be an array of photo ids")?;
        if a.iter().any(|v| v.as_u64().is_none()) {
            return Err("ids must contain only photo ids".into());
        }
    }
    if p.get("id").is_some_and(|v| v.as_u64().is_none()) {
        return Err("id must be a photo id".into());
    }
    let ids = session.targets(p);
    if ids.is_empty() || ids.len() > 1000 {
        return Err("select between 1 and 1000 photos".into());
    }
    let l = options.layout()?;
    if ids.len().div_ceil(options.columns * options.rows) > 100 {
        return Err("contact sheets are limited to 100 pages; increase the grid size or select fewer photos".into());
    }
    let mut items = Vec::with_capacity(ids.len());
    for id in ids {
        let name = session.catalog.photo(id).ok_or("no such photo")?.file_name.clone();
        let job = session.export_job(id, l.cell_w - 16, l.cell_h - 48, OutputSpace::Srgb, OutputDepth::U8)?;
        items.push((name, job));
    }
    Ok(Prepared { options, items })
}

impl Prepared {
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Cancellation produces no partial document; callers write only after this succeeds.
    pub fn run(self, progress: &mut dyn FnMut(usize, &str) -> bool) -> Result<Document, String> {
        let l = self.options.layout()?;
        let capacity = self.options.columns * self.options.rows;
        let photos = self.items.len();
        let pages = photos.div_ceil(capacity);
        let mut pdf = Pdf::new(pages, l.points);
        let mut page = Rgba8::filled(l.width, l.height, [255; 4]);
        for (i, (name, job)) in self.items.into_iter().enumerate() {
            if !progress(i, &name) {
                return Err("Contact sheet cancelled".into());
            }
            let image = job.run().rendered?.image;
            let slot = i % capacity;
            let x = 60 + slot % self.options.columns * l.cell_w;
            let y = 60 + slot / self.options.columns * l.cell_h;
            // The renderer fits within the requested rectangle and applies crop/orientation.
            if image.width > l.cell_w - 16 || image.height > l.cell_h - 48 {
                return Err("render exceeded contact sheet cell".into());
            }
            blit(&mut page, &image, x + (l.cell_w - image.width) / 2, y + (l.cell_h - 40 - image.height) / 2)?;
            if self.options.captions {
                let mut caption = Rgba8::filled(l.cell_w - 16, 28, [255; 4]);
                draw_watermark(
                    &mut caption,
                    &Watermark {
                        text: caption_text(&name, l.cell_w - 16),
                        size: 0.5,
                        opacity: 1.0,
                        anchor: Anchor::Center,
                        inset: 0.0,
                        color: [32; 3],
                        shadow: false,
                        ..Default::default()
                    },
                );
                blit(&mut page, &caption, x + 8, y + l.cell_h - 36)?;
            }
            if slot + 1 == capacity || i + 1 == photos {
                let jpg = encode_image(&page, &ExportOptions { format: ExportFormat::Jpeg, quality: 92, ..Default::default() })?;
                if pdf.bytes.len().saturating_add(jpg.len()) > 256 * 1024 * 1024 {
                    return Err("contact sheet exceeds 256 MiB; select fewer photos".into());
                }
                pdf.page(i / capacity, &jpg, l.width, l.height);
                page.data.fill([255; 4]);
            }
        }
        if !progress(photos, "PDF") {
            return Err("Contact sheet cancelled".into());
        }
        Ok(Document { bytes: pdf.finish(), pages, photos })
    }
}

fn caption_text(name: &str, width: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    let clean: String = name.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let cells: Vec<_> = clean.graphemes(true).collect();
    // A conservative one-em budget at 14 px also accommodates the optional CJK faces.
    let limit = (width / 14).max(3);
    if cells.len() <= limit {
        return clean;
    }
    let left = limit / 2;
    let right = limit - left - 1;
    format!("{}…{}", cells.iter().take(left).copied().collect::<String>(), cells.iter().skip(cells.len() - right).copied().collect::<String>())
}

fn blit(dst: &mut Rgba8, src: &Rgba8, x: usize, y: usize) -> Result<(), String> {
    if x.checked_add(src.width).is_none_or(|w| w > dst.width) || y.checked_add(src.height).is_none_or(|h| h > dst.height) {
        return Err("contact sheet image outside page".into());
    }
    for row in 0..src.height {
        let start = (y + row) * dst.width + x;
        let a = dst.data.get_mut(start..start + src.width).ok_or("invalid page buffer")?;
        let b = src.data.get(row * src.width..(row + 1) * src.width).ok_or("invalid render buffer")?;
        for (dst, src) in a.iter_mut().zip(b) {
            let alpha = u32::from(src[3]);
            for channel in 0..3 {
                dst[channel] = ((u32::from(src[channel]) * alpha + u32::from(dst[channel]) * (255 - alpha) + 127) / 255) as u8;
            }
            dst[3] = 255;
        }
    }
    Ok(())
}

// Minimal PDF 1.4: one JPEG XObject per page, explicit byte lengths and classic xref offsets.
struct Pdf {
    bytes: Vec<u8>,
    offsets: Vec<usize>,
    points: (f64, f64),
}

impl Pdf {
    fn new(pages: usize, points: (f64, f64)) -> Self {
        let mut p = Self { bytes: b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec(), offsets: vec![0], points };
        p.object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let kids = (0..pages).map(|i| format!("{} 0 R", 3 + i * 3)).collect::<Vec<_>>().join(" ");
        p.object(2, format!("<< /Type /Pages /Count {pages} /Kids [{kids}] >>").as_bytes());
        p
    }

    fn object(&mut self, id: usize, body: &[u8]) {
        self.offsets.push(self.bytes.len());
        self.bytes.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
        self.bytes.extend_from_slice(body);
        self.bytes.extend_from_slice(b"\nendobj\n");
    }

    fn stream(&mut self, id: usize, dictionary: &str, bytes: &[u8]) {
        let mut body = format!("<< {dictionary} /Length {} >>\nstream\n", bytes.len()).into_bytes();
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\nendstream");
        self.object(id, &body);
    }

    fn page(&mut self, index: usize, jpg: &[u8], width: usize, height: usize) {
        let id = 3 + index * 3;
        let (w, h) = self.points;
        self.object(
            id,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w:.2} {h:.2}] /Resources << /XObject << /Im {} 0 R >> >> /Contents {} 0 R >>",
                id + 2,
                id + 1
            )
            .as_bytes(),
        );
        self.stream(id + 1, "", format!("q {w:.2} 0 0 {h:.2} 0 0 cm /Im Do Q").as_bytes());
        self.stream(
            id + 2,
            &format!("/Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode"),
            jpg,
        );
    }

    fn finish(mut self) -> Vec<u8> {
        let start = self.bytes.len();
        self.bytes.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", self.offsets.len()).as_bytes());
        for offset in self.offsets.iter().skip(1) {
            self.bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        self.bytes.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n", self.offsets.len()).as_bytes());
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_settings_and_selection() {
        let mut s = Session::with_demo();
        for p in [
            json!({"columns": 0}),
            json!({"rows": 11}),
            json!({"paper": "poster"}),
            json!({"landscape": "yes"}),
            json!({"typo": 2}),
            json!({"ids": ["bad"]}),
            json!({"ids": []}),
        ] {
            assert!(prepare(&mut s, &p).is_err(), "{p}");
        }
        let a = Options::default().layout().unwrap();
        let b = Options { landscape: true, ..Default::default() }.layout().unwrap();
        assert_eq!((a.width, a.height), (b.height, b.width));
        let id = s.catalog.photos().next().unwrap().id.0;
        assert!(prepare(&mut s, &json!({"ids": [id, u64::MAX]})).is_err());
        assert!(prepare(&mut s, &json!({"ids": vec![id; 1001]})).is_err());
        assert!(prepare(&mut s, &json!({"ids": vec![id; 101], "columns": 1, "rows": 1})).is_err());
    }

    #[test]
    fn transparent_pixels_composite_over_white() {
        let mut dst = Rgba8::filled(3, 1, [255; 4]);
        let src = Rgba8 { width: 3, height: 1, data: vec![[0, 0, 0, 0], [255, 0, 0, 128], [10, 20, 30, 255]] };
        blit(&mut dst, &src, 0, 0).unwrap();
        assert_eq!(dst.data, vec![[255; 4], [255, 127, 127, 255], [10, 20, 30, 255]]);
        assert!(blit(&mut dst, &src, usize::MAX, 0).is_err());
    }

    #[test]
    fn unicode_captions_are_rasterized() {
        assert_eq!(caption_text("very-long-éééé-filename.jpg", 140), "very-….jpg");
        assert_eq!(caption_text("café\n.jpg", 300), "café .jpg");
        let mut img = Rgba8::filled(360, 28, [255; 4]);
        draw_watermark(
            &mut img,
            &Watermark {
                text: "Été – Straße café.jpg".into(),
                size: 0.5,
                opacity: 1.0,
                anchor: Anchor::Center,
                inset: 0.0,
                color: [32; 3],
                shadow: false,
                ..Default::default()
            },
        );
        assert!(img.data.iter().filter(|p| p[0] < 128).count() > 100);
        let mut ascii = Rgba8::filled(360, 28, [255; 4]);
        draw_watermark(
            &mut ascii,
            &Watermark {
                text: "Ete - Strasse cafe.jpg".into(),
                size: 0.5,
                opacity: 1.0,
                anchor: Anchor::Center,
                inset: 0.0,
                color: [32; 3],
                shadow: false,
                ..Default::default()
            },
        );
        assert_ne!(img, ascii);
    }

    #[test]
    fn pagination_xref_and_cancellation() {
        let mut s = Session::with_demo();
        let ids: Vec<_> = s.catalog.photos().take(3).map(|p| p.id.0).collect();
        let p = json!({"ids": ids, "columns": 1, "rows": 2, "captions": false});
        let mut seen = Vec::new();
        let doc = prepare(&mut s, &p)
            .unwrap()
            .run(&mut |i, _| {
                seen.push(i);
                true
            })
            .unwrap();
        assert_eq!((doc.pages, doc.photos), (2, 3));
        assert_eq!(seen, vec![0, 1, 2, 3]);
        let text = String::from_utf8_lossy(&doc.bytes);
        assert!(text.contains("/Count 2 /Kids [3 0 R 6 0 R]"));
        let xref = text.rsplit("startxref\n").next().unwrap().lines().next().unwrap().parse::<usize>().unwrap();
        assert!(doc.bytes[xref..].starts_with(b"xref\n0 9\n"));
        let xref_text = std::str::from_utf8(&doc.bytes[xref..]).unwrap();
        for (index, line) in xref_text.lines().skip(3).take(8).enumerate() {
            let offset = line.split_whitespace().next().unwrap().parse::<usize>().unwrap();
            assert!(doc.bytes[offset..].starts_with(format!("{} 0 obj\n", index + 1).as_bytes()));
        }
        assert!(prepare(&mut s, &p).unwrap().run(&mut |_, _| false).is_err());
    }

    fn first_page(bytes: &[u8]) -> Rgba8 {
        let start = bytes.windows(2).position(|v| v == [0xff, 0xd8]).unwrap();
        let end = bytes[start..].windows(2).position(|v| v == [0xff, 0xd9]).unwrap() + start + 2;
        dac_codecs::decode(&bytes[start..end], Default::default()).unwrap().to_srgb8()
    }

    #[test]
    fn sheet_uses_current_edits_and_captions() {
        let mut s = Session::with_demo();
        let p = json!({"columns": 1, "rows": 2, "captions": false});
        let plain = first_page(&prepare(&mut s, &p).unwrap().run(&mut |_, _| true).unwrap().bytes);
        s.execute("develop.set", &json!({"control": "light.exposure", "value": 2.0})).unwrap();
        let bright = first_page(&prepare(&mut s, &p).unwrap().run(&mut |_, _| true).unwrap().bytes);
        let sum = |img: &Rgba8| img.data.iter().map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2])).sum::<u64>();
        assert!(sum(&bright) > sum(&plain), "sheet ignored develop settings");
        let captions = first_page(&prepare(&mut s, &json!({"columns": 1, "rows": 2})).unwrap().run(&mut |_, _| true).unwrap().bytes);
        assert!(sum(&captions) < sum(&bright), "captions absent from PDF pixels");
        assert_eq!((plain.width, plain.height), (1240, 1754));
    }
}
