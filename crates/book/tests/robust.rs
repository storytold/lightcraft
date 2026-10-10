//! Never-crash (P6.2): saved books (creations in the catalog) and the `book.*` ops that agents
//! and the UI send are untrusted; a damaged document or a hostile parameter is an error or a
//! clamped render, never a panic or an unbounded allocation.

use dac_book::render::{Options, render_page};
use dac_book::{Book, ops};
use dac_layout::render::PhotoSource;
use dac_raster::Rgba8;
use dac_text::TextEngine;
use serde_json::{Value, json};

struct Tiny;

impl PhotoSource for Tiny {
    fn image(&self, photo: &str, _long: usize) -> Result<Rgba8, String> {
        if photo.ends_with('7') {
            return Err("offline".into());
        }
        Ok(Rgba8::from_fn(6, 4, |x, y| [x as u8 * 40, y as u8 * 50, 90, 255]))
    }
}

fn calls() -> Vec<(&'static str, Value)> {
    vec![
        ("book.new", json!({"size": "a4", "kind": "pdf", "cover": "softcover"})),
        ("book.settings", json!({"size": "custom", "width": 9, "height": 6, "jpegQuality": 80, "resolution": 300})),
        ("book.addPage", json!({"template": "4-grid", "after": 1})),
        ("book.addPage", json!({"template": "1-caption"})),
        ("book.addPage", json!({"blank": true})),
        ("book.movePage", json!({"page": 3, "to": 1})),
        ("book.template", json!({"page": 1, "template": "1-caption"})),
        ("book.place", json!({"page": 2, "cell": 0, "photo": 9})),
        ("book.place", json!({"page": "front", "cell": 0, "photo": "1"})),
        ("book.swap", json!({"from": {"page": 1, "cell": 0}, "to": {"page": 2, "cell": 0}})),
        ("book.cell", json!({"page": 1, "cell": 0, "zoom": 2, "pan": [0.5, -0.5], "padding": 6, "fill": false})),
        ("book.text", json!({"page": 1, "cell": 1, "text": "Hello {Title}"})),
        ("book.photoText", json!({"page": 1, "cell": 0, "text": "{Filename}", "position": "above", "style": {"size": 8}})),
        ("book.pageText", json!({"page": 1, "text": "Chapter one", "position": "top"})),
        ("book.type", json!({"page": 1, "cell": 1, "style": {"columns": 2, "tracking": 50, "color": [10, 20, 30], "align": "justify"}})),
        ("book.textPreset", json!({"name": "Mine", "style": {"size": 14}})),
        ("book.background", json!({"color": "#f0e8d8", "graphic": "frame"})),
        ("book.background", json!({"page": 2, "photo": "1", "photoOpacity": 0.2})),
        ("book.guides", json!({"bleed": false, "fillerText": false})),
        ("book.pageNumbers", json!({"show": true, "position": "bottomCenter"})),
        ("book.autoLayout", json!({"photos": ["1", "2", "3", "17"], "preset": "one per page"})),
        ("book.removePage", json!({"page": 2})),
    ]
}

fn sample() -> Book {
    let mut b = Book::default();
    for (id, p) in calls() {
        let _ = ops::apply(&mut b, id, &p);
    }
    b
}

fn render_all(b: &Book, e: &mut TextEngine) {
    let opt = Options { dpi: 4.0, bleed: true, editor: true, ..Options::default() };
    for r in b.all_pages().into_iter().take(40) {
        let _ = render_page(b, r, &Tiny, e, &opt);
    }
}

#[test]
fn damaged_book_documents_never_panic() {
    let seed = sample().to_json().unwrap();
    assert!(Book::from_json(&seed).is_ok());
    let mut e = TextEngine::new();
    dac_fuzzkit::run_json("book.document", &[&seed], 400, |s| {
        let _ = Book::from_json(s);
        // unvalidated too: a book edited in memory reaches the renderer without `from_json`
        if let Ok(b) = serde_json::from_str::<Book>(s) {
            render_all(&b, &mut e);
        }
    });
}

#[test]
fn hostile_op_parameters_never_panic() {
    let base = sample();
    let calls = calls();
    let params: Vec<String> = calls.iter().map(|(_, p)| p.to_string()).collect();
    let seeds: Vec<&str> = params.iter().map(String::as_str).collect();
    let ids: Vec<&str> = calls.iter().map(|(id, _)| *id).chain(["book.get", "book.nope", "book.clearLayout"]).collect();
    let mut rng = dac_fuzzkit::Rng::new(5);
    let mut e = TextEngine::new();
    let mut n = 0usize;
    dac_fuzzkit::run_json("book.ops", &seeds, 3000, |s| {
        let Ok(p) = serde_json::from_str::<Value>(s) else { return };
        let mut b = base.clone();
        for _ in 0..3 {
            let id = ids[rng.below(ids.len())];
            let _ = ops::apply(&mut b, id, &p);
        }
        n += 1;
        if n.is_multiple_of(25) {
            render_all(&b, &mut e);
        }
    });
}
