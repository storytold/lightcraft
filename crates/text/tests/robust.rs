//! Never-crash (P6.2): text comes from users and agents (captions, titles, watermarks) and fonts
//! from the user's system; damaged fonts, hostile strings and absurd sizes or transforms are
//! laid out and rendered (or refused) without a panic or an unbounded raster.

use dac_text::fonts::guess_from_postscript;
use dac_text::{Orientation, TextBlock, TextEngine, Xform};

const INTER: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");

#[test]
fn damaged_fonts_render_or_are_refused() {
    dac_fuzzkit::run("text.font", &[INTER], 100, |b| {
        let mut e = TextEngine::new();
        let names = e.fonts.register_font_data(b.to_vec());
        let family = names.first().cloned().unwrap_or_else(|| "Inter".into());
        for orientation in [Orientation::Horizontal, Orientation::Vertical] {
            let mut block = TextBlock::plain("Wavy ﬁ fl Ŧ 123 縦書き", &family, 18.0);
            block.orientation = orientation;
            let _ = e.render(&block, 72.0, &Xform::IDENTITY);
        }
    });
}

#[test]
fn hostile_strings_and_sizes_never_panic() {
    let mut e = TextEngine::new();
    let sizes = [0.0, -12.0, f32::NAN, f32::INFINITY, 1e-30, 1e30, 300.0];
    let xforms = [Xform::IDENTITY, Xform([0.0; 6]), Xform([f64::NAN, 0.0, 0.0, 1.0, 0.0, 0.0]), Xform([1e12, 0.0, 0.0, 1e12, -1e300, 0.0])];
    let mut rng = dac_fuzzkit::Rng::new(3);
    dac_fuzzkit::run_str(
        "text.string",
        &["Hello\nworld\t\u{200d}\u{301}", "\u{202e}abc\u{202c} 👩\u{200d}👩\u{200d}👧 \u{feff}", "日本語の縦書き。"],
        300,
        |s| {
            let mut block = TextBlock::plain(s, ["Inter", "", "No Such Font", "serif"][rng.below(4)], sizes[rng.below(sizes.len())]);
            if rng.chance(2) {
                block.orientation = Orientation::Vertical;
            }
            let dpi = [72.0, 0.0, f32::NAN, 600.0][rng.below(4)];
            let (_, r) = e.render(&block, dpi, &xforms[rng.below(xforms.len())]);
            // a raster is bounded whatever the input (huge sizes are legitimate, if slow: kept out of
            // this loop)
            assert!(r.width.saturating_mul(r.height) <= 1 << 28, "{}x{}", r.width, r.height);
        },
    );
}

#[test]
fn postscript_names_never_panic() {
    dac_fuzzkit::run_str("text.psname", &["Inter-SemiBoldItalic", "TimesNewRomanPS-BoldMT", "-", "ÄÖ-Ü"], 5000, |s| {
        let _ = guess_from_postscript(s);
    });
}
