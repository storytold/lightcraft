//! Never-crash (P6.2): layout documents and templates come from disk (saved prints, books,
//! slideshows, user templates) and from agents; a damaged or hostile one is an error or a
//! clamped render, never a panic, an unbounded allocation or a hang.

use dac_layout::render::{PhotoSource, PlaceholderText, RenderOptions, render_document};
use dac_layout::tokens::{PhotoInfo, expand};
use dac_layout::{Document, Template, builtin_templates};
use dac_raster::Rgba8;

struct Tiny;

impl PhotoSource for Tiny {
    fn image(&self, photo: &str, _long: usize) -> Result<Rgba8, String> {
        if photo.len() % 5 == 4 {
            return Err("missing".into());
        }
        Ok(Rgba8::from_fn(6, 4, |x, y| [x as u8 * 40, y as u8 * 60, 90, 255]))
    }
    fn info(&self, photo: &str) -> PhotoInfo {
        PhotoInfo { filename: photo.to_string(), shutter: Some(1e-300), aperture: Some(f64::NAN), ..PhotoInfo::default() }
    }
}

fn opts() -> RenderOptions {
    RenderOptions { dpi: 3.0, include_bleed: true, show_guides: true, ..RenderOptions::default() }
}

fn exercise(doc: &Document) {
    let _ = doc.validate();
    let _ = doc.photos();
    let _ = render_document(doc, &Tiny, &PlaceholderText, &opts());
}

#[test]
fn documents_never_panic() {
    let photos: Vec<String> = (0..6).map(|i| i.to_string()).collect();
    let docs: Vec<String> = builtin_templates().iter().filter_map(|t| t.instantiate(&photos, false).ok()).filter_map(|d| d.to_json().ok()).collect();
    assert!(!docs.is_empty());
    let seeds: Vec<&str> = docs.iter().map(String::as_str).collect();
    dac_fuzzkit::run_json("layout.document", &seeds, 1500, |s| {
        let _ = Document::from_json(s);
        // render without validation too: documents edited in memory (by the UI or an agent) reach
        // the renderer without a round trip through `from_json`
        if let Ok(doc) = serde_json::from_str::<Document>(s) {
            exercise(&doc);
        }
    });
}

#[test]
fn templates_never_panic() {
    let json: Vec<String> = builtin_templates().iter().filter_map(|t| t.to_json().ok()).collect();
    let seeds: Vec<&str> = json.iter().map(String::as_str).collect();
    let photos: Vec<String> = (0..4).map(|i| format!("p{i}")).collect();
    dac_fuzzkit::run_json("layout.template", &seeds, 1500, |s| {
        let parsed = Template::from_json(s).ok().or_else(|| serde_json::from_str::<Template>(s).ok());
        if let Some(t) = parsed {
            for repeat in [false, true] {
                if let Ok(doc) = t.instantiate(&photos, repeat) {
                    exercise(&doc);
                }
            }
            let _ = t.instantiate(&[], false);
        }
    });
}

#[test]
fn caption_tokens_never_panic() {
    let info =
        PhotoInfo { filename: "IMG_0001.JPG".into(), title: "Tïtle".into(), shutter: Some(0.004), aperture: Some(8.0), ..PhotoInfo::default() };
    dac_fuzzkit::run_str("layout.tokens", &["{Filename} — {Title} {Shutter} f/{Aperture} {Date} {Page}/{Pages} {{}}"], 3000, |s| {
        let _ = expand(s, &info);
        let _ = expand(s, &PhotoInfo::default());
    });
}
