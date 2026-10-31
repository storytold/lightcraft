//! How the interface writes what it reads off a photo or a control: one way to say an exposure,
//! a minus that is a minus, figures that stand in columns, and a file name shortened in the
//! middle so that its number stays.
//!
//! Before: one exposure read "f / 2.8", "f/11.0" and "f/11" with "35 mm" and "35mm", "1/250 sec",
//! "1/250 s" and "1/250" in four places, in three orders; a negative value had a hyphen;
//! Inter's figures are of unequal width, so a value changed its left edge while it was dragged
//! and counts did not line up; and a long file name on the filmstrip lost its end, where the
//! number of the frame is.

use std::sync::Arc;

use egui::{Color32, FontId};
use lightcraft_catalog::Meta;

/// The sign of a negative number.
pub(crate) const MINUS: char = '\u{2212}';

/// An aperture: "f/2.8", "f/11". Nothing for a number that is none.
pub(crate) fn aperture(f: f32) -> Option<String> {
    if !f.is_finite() || f <= 0.0 {
        return None;
    }
    let tenths = (f64::from(f) * 10.0).round();
    Some(if tenths % 10.0 == 0.0 { format!("f/{:.0}", tenths / 10.0) } else { format!("f/{:.1}", tenths / 10.0) })
}

/// A focal length: "35 mm", "16.5 mm".
pub(crate) fn focal(mm: f32) -> Option<String> {
    if !mm.is_finite() || mm <= 0.0 {
        return None;
    }
    let tenths = (f64::from(mm) * 10.0).round();
    Some(if tenths % 10.0 == 0.0 { format!("{:.0} mm", tenths / 10.0) } else { format!("{:.1} mm", tenths / 10.0) })
}

/// A shutter speed as the catalog holds it ("1/250", "8"), with its unit in the language of the
/// interface: "1/250 sec". One that names its unit already ("30s") stays as it is.
pub(crate) fn shutter(held: &str) -> Option<String> {
    let held = held.trim();
    if held.is_empty() {
        return None;
    }
    Some(if held.ends_with(|c: char| c.is_alphabetic()) { held.to_string() } else { crate::i18n::tr_format!("{} sec", held) })
}

/// A sensitivity: "ISO 100".
pub(crate) fn iso(iso: u32) -> String {
    format!("ISO {iso}")
}

/// What a photo was taken with, in the order a photographer says it: shutter speed, aperture,
/// sensitivity, focal length. Only what the photo has.
pub(crate) fn exposure(m: &Meta) -> Vec<String> {
    [shutter(&m.shutter), m.aperture.and_then(aperture), m.iso.map(iso), m.focal_mm.and_then(focal)].into_iter().flatten().collect()
}

/// [`exposure`] on one line ("1/250 sec · f/2.8 · ISO 100 · 35 mm"), or nothing.
pub(crate) fn exposure_line(m: &Meta) -> Option<String> {
    Some(exposure(m)).filter(|parts| !parts.is_empty()).map(|parts| parts.join(" · "))
}

/// A number as written by `format!`, with a minus for its hyphen ("−1.25").
pub(crate) fn signed(written: &str) -> String {
    match written.strip_prefix('-') {
        Some(rest) => format!("{MINUS}{rest}"),
        None => written.to_string(),
    }
}

/// The widths of the characters of `text` in `font`, and of the font's widest figure.
fn widths(painter: &egui::Painter, text: &str, font: &FontId) -> (Vec<(char, f32)>, f32) {
    painter.ctx().fonts_mut(|fonts| {
        let cell = ('0'..='9').map(|c| fonts.glyph_width(font, c)).fold(0.0, f32::max);
        (text.chars().map(|c| (c, fonts.glyph_width(font, c))).collect(), cell)
    })
}

/// The room a figure lacks to fill a cell as wide as the widest figure (none for other characters).
fn room(c: char, width: f32, cell: f32) -> f32 {
    if c.is_ascii_digit() && width.is_finite() { (cell - width).max(0.0) } else { 0.0 }
}

/// `text` on one line with every figure in a cell as wide as the widest figure of the font, so
/// that numbers stand in columns and a changing value keeps its place. (The font's own figures
/// are of unequal width, and egui cannot ask it for its tabular ones.) The text of the galley is
/// `text` unchanged.
///
/// A figure stands at the end of its cell, so the galley is as wide as its cells: for a count,
/// which is figures only. A number with a sign is [`value`]'s.
pub(crate) fn figures(painter: &egui::Painter, text: &str, font: FontId, color: Color32) -> Arc<egui::Galley> {
    let (widths, cell) = widths(painter, text, &font);
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let mut buffer = [0u8; 4];
    for (c, width) in widths {
        job.append(c.encode_utf8(&mut buffer), room(c, width, cell), egui::TextFormat { font_id: font.clone(), color, ..Default::default() });
    }
    painter.layout_job(job)
}

/// A value ("−1.25", "+100") painted to end at `right_center`, each figure in the middle of a
/// cell as wide as the widest figure: the value begins and ends where any other of as many
/// figures does, and a narrow 1 does not stand off from its sign. Returns where it is.
pub(crate) fn value(painter: &egui::Painter, right_center: egui::Pos2, text: &str, font: FontId, color: Color32) -> egui::Rect {
    let (widths, cell) = widths(painter, text, &font);
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let mut buffer = [0u8; 4];
    // (half a figure's room before it and half after; what is after is given as room before
    // the next character, and after the last by where the galley is put)
    let mut after = 0.0;
    for (c, width) in widths {
        let half = room(c, width, cell) / 2.0;
        job.append(c.encode_utf8(&mut buffer), after + half, egui::TextFormat { font_id: font.clone(), color, ..Default::default() });
        after = half;
    }
    let galley = painter.layout_job(job);
    let size = galley.size() + egui::vec2(after, 0.0);
    let rect = egui::Rect::from_min_size(egui::pos2(right_center.x - size.x, right_center.y - size.y / 2.0), size);
    painter.galley(rect.min, galley, color);
    rect
}

/// `text` shortened in the middle to fit `max` (as `width` measures it): the start and the end
/// stay, the end a character longer when they cannot be equal ("IMG_20…_153012"), for the end
/// of a file name is the number of the frame. Cuts fall between letters as a reader sees them (a
/// letter keeps its combining marks: an accent written apart, an Arabic vowel sign); what cannot
/// fit at all is "…".
pub(crate) fn elide_middle(text: &str, max: f32, width: impl Fn(&str) -> f32) -> String {
    if width(text) <= max {
        return text.to_string();
    }
    let chars: Vec<&str> = unicode_segmentation::UnicodeSegmentation::graphemes(text, true).collect();
    let kept = |keep: usize| {
        let tail = keep.div_ceil(2);
        let head = keep - tail;
        let mut out: String = chars.iter().take(head).copied().collect();
        out.push('…');
        out.extend(chars.iter().skip(chars.len().saturating_sub(tail)).copied());
        out
    };
    // the most characters that fit (fewer never measure wider than more, near enough: the
    // result is measured again below)
    let (mut fits, mut too_many) = (0usize, chars.len());
    while fits + 1 < too_many {
        let middle = fits + (too_many - fits) / 2;
        if width(&kept(middle)) <= max {
            fits = middle;
        } else {
            too_many = middle;
        }
    }
    (0..=fits).rev().map(kept).find(|out| width(out) <= max).unwrap_or_else(|| "…".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exposure_is_said_one_way() {
        crate::i18n::set_language(crate::i18n::Locale::En);
        // Given what four photos were taken with
        assert_eq!(aperture(2.8).as_deref(), Some("f/2.8"));
        assert_eq!(aperture(11.0).as_deref(), Some("f/11"));
        assert_eq!(aperture(10.04).as_deref(), Some("f/10"));
        assert_eq!(aperture(1.4).as_deref(), Some("f/1.4"));
        assert_eq!(focal(35.0).as_deref(), Some("35 mm"));
        assert_eq!(focal(16.5).as_deref(), Some("16.5 mm"));
        assert_eq!(focal(100.0).as_deref(), Some("100 mm"));
        assert_eq!(shutter("1/250").as_deref(), Some("1/250 sec"));
        assert_eq!(shutter("8").as_deref(), Some("8 sec"));
        assert_eq!(shutter("30s").as_deref(), Some("30s"));
        assert_eq!(iso(100), "ISO 100");
        // Then a photo's line gives them in a photographer's order, and leaves out what it lacks
        let m = Meta { shutter: "1/250".into(), aperture: Some(2.8), iso: Some(100), focal_mm: Some(35.0), ..Default::default() };
        assert_eq!(exposure_line(&m).as_deref(), Some("1/250 sec · f/2.8 · ISO 100 · 35 mm"));
        let m = Meta { aperture: Some(11.0), ..Default::default() };
        assert_eq!(exposure_line(&m).as_deref(), Some("f/11"));
        assert_eq!(exposure_line(&Meta::default()), None);
    }

    #[test]
    fn a_number_that_is_none_says_nothing() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -2.8] {
            assert_eq!(aperture(bad), None, "{bad}");
            assert_eq!(focal(bad), None, "{bad}");
        }
        assert_eq!(shutter(""), None);
        assert_eq!(shutter("   "), None);
        // (a number too large to be a lens is still written, not a crash)
        assert_eq!(focal(f32::MAX).map(|s| s.ends_with(" mm")), Some(true));
    }

    #[test]
    fn a_negative_number_has_a_minus() {
        assert_eq!(signed("-1.25"), "−1.25");
        assert_eq!(signed("+35"), "+35");
        assert_eq!(signed("0"), "0");
        assert_eq!(signed("-"), "−");
        assert_eq!(signed(""), "");
        // (only the sign: a range keeps its hyphen)
        assert_eq!(signed("16-35"), "16-35");
    }

    #[test]
    fn figures_stand_in_cells_of_one_width() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let t = crate::theme::Tokens::for_kind(crate::theme::ThemeKind::Charcoal);
            let wide = |s: &str| figures(ui.painter(), s, t.body(), Color32::WHITE).size().x;
            let plain = |s: &str| ui.painter().layout_no_wrap(s.to_string(), t.body(), Color32::WHITE).size().x;
            // Given numbers of as many figures, the narrowest and the widest
            // Then they are as wide as one another, which as the font draws them they are not
            assert!((plain("111") - plain("444")).abs() > 2.0, "the font's own figures differ: {} and {}", plain("111"), plain("444"));
            // (to a tenth of a point: a glyph is placed on a whole pixel)
            assert!((wide("111") - wide("444")).abs() < 0.1, "{} and {}", wide("111"), wide("444"));
            assert!((wide("+100") - wide("+111")).abs() < 0.1);
            // (beside a point the font moves a figure a little, as it does a letter)
            assert!((wide("−1.25") - wide("−4.04")).abs() < 1.0, "{} and {}", wide("−1.25"), wide("−4.04"));
            assert!((plain("−1.11") - plain("−4.44")).abs() > 4.0);
            // And what is not a figure is as the font has it, and the text is unchanged
            assert!((wide("ISO") - plain("ISO")).abs() < 0.1);
            assert_eq!(figures(ui.painter(), "−1.25", t.body(), Color32::WHITE).job.text, "−1.25");
            assert_eq!(figures(ui.painter(), "", t.body(), Color32::WHITE).size().x, 0.0);
            // And a value with a sign ends where it is told to and begins where any other of as
            // many figures does, its 1 no farther from the sign than half its room
            let at = egui::pos2(300.0, 40.0);
            let (narrow, wide) =
                (value(ui.painter(), at, "−1.11", t.body(), Color32::WHITE), value(ui.painter(), at, "−4.44", t.body(), Color32::WHITE));
            assert_eq!((narrow.right(), wide.right()), (300.0, 300.0));
            assert!((narrow.left() - wide.left()).abs() < 1.0, "{narrow:?} {wide:?}");
            assert_eq!(value(ui.painter(), at, "", t.body(), Color32::WHITE).width(), 0.0);
        });
        // (nothing is painted: the font atlas made for the measuring is not uploaded anywhere)
        output.textures_delta.clear();
    }

    #[test]
    fn a_long_name_loses_its_middle_and_keeps_its_number() {
        // (a character is a unit wide here)
        let width = |s: &str| s.chars().count() as f32;
        assert_eq!(elide_middle("DSC_0001", 14.0, width), "DSC_0001");
        assert_eq!(elide_middle("IMG_20240712_153012", 14.0, width), "IMG_20…_153012");
        assert_eq!(elide_middle("IMG_20240712_153012", 5.0, width), "IM…12");
        assert_eq!(elide_middle("IMG_20240712_153012", 2.0, width), "…2");
        assert_eq!(elide_middle("IMG_20240712_153012", 1.0, width), "…");
        assert_eq!(elide_middle("IMG_20240712_153012", 0.0, width), "…");
        assert_eq!(elide_middle("IMG_20240712_153012", f32::NAN, width), "…");
        assert_eq!(elide_middle("", 0.0, width), "");
        // the name from #266: cuts fall on characters, not bytes
        assert_eq!(elide_middle("202407層三限定訂閱圖(4)", 9.0, width), "2024…圖(4)");
        // a very long name is not measured once a character
        let long = "x".repeat(100_000);
        let measured = std::cell::Cell::new(0usize);
        let counting = |s: &str| {
            measured.set(measured.get() + 1);
            s.chars().count() as f32
        };
        assert_eq!(elide_middle(&long, 9.0, counting).chars().count(), 9);
        assert!(measured.get() < 40, "{} measurements", measured.get());
    }

    /// Scenario: a name written with combining marks (an accented name as macOS stores it, a vowelled
    /// Arabic one) is cut between letters, never between a letter and its mark: no mark is left on the "…".
    #[test]
    fn a_cut_never_parts_a_letter_from_its_mark() {
        // (a letter with its mark is a unit wide here, as it is drawn)
        let width = |s: &str| s.chars().filter(|c| !('\u{300}'..='\u{36f}').contains(c) && !('\u{64b}'..='\u{652}').contains(c)).count() as f32;
        // Given "été" eight times over, each é an e and a combining acute
        let accented = "e\u{301}te\u{301}".repeat(8);
        // and a vowelled Arabic word, each letter with its haraka
        let vowelled = "\u{643}\u{64e}\u{62a}\u{64e}\u{628}\u{64e}".repeat(6);
        for name in [accented.as_str(), vowelled.as_str()] {
            for max in 2..12 {
                // When it is shortened to any width
                let short = elide_middle(name, max as f32, width);
                // Then what follows the "…" starts with a letter, and both parts are parts of the name
                let (head, tail) = short.split_once('…').expect("shortened");
                let first = tail.chars().next();
                assert!(first.is_none_or(|c| width(&c.to_string()) > 0.0), "{max}: a mark on the ellipsis in {short:?}");
                assert!(name.starts_with(head) && name.ends_with(tail), "{max}: {short:?}");
                assert!(width(&short) <= max as f32, "{max}: {short:?}");
            }
        }
    }
}
