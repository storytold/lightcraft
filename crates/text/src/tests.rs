use crate::style::TextBlock as TextLayer;
use crate::style::{Caps, CharStyle, Color, FontFeature, Orientation, ParagraphRun, ParagraphStyle, TextAlign, TextDirection, TextRun, TextShape};
use crate::{Rendered, TextEngine, Xform, fonts};

fn point(text: &str, size_pt: f32) -> TextLayer {
    TextLayer { text: text.into(), font_family: "Inter".into(), size_pt, ..Default::default() }
}

fn styled(text: &str, style: CharStyle) -> TextLayer {
    TextLayer { text: text.into(), runs: vec![TextRun { len: text.len(), style }], ..Default::default() }
}

fn with_para(mut t: TextLayer, p: ParagraphStyle) -> TextLayer {
    t.paragraphs = vec![ParagraphRun { len: t.text.len(), style: p }];
    t
}

fn width(l: &crate::TextLayout) -> f32 {
    l.lines.iter().map(|l| l.x1 - l.x0).fold(0.0, f32::max)
}

/// Renders at 72 dpi with the text origin at `(x, y)`.
fn render(e: &mut TextEngine, t: &TextLayer, x: f64, y: f64) -> (crate::TextLayout, Rendered) {
    e.render(t, 72.0, &Xform([1.0, 0.0, 0.0, 1.0, x, y]))
}

/// Sum of alpha over `[x0, y0, x1, y1)` (target pixels).
fn alpha_in(r: &Rendered, [x0, y0, x1, y1]: [i32; 4]) -> f64 {
    let mut s = 0.0;
    for y in y0.max(r.y0)..y1.min(r.y0 + r.height as i32) {
        for x in x0.max(r.x0)..x1.min(r.x0 + r.width as i32) {
            let i = ((y - r.y0) as usize * r.width + (x - r.x0) as usize) * 4 + 3;
            s += f64::from(r.rgba[i]);
        }
    }
    s
}

fn alpha_sum(r: &Rendered) -> f64 {
    r.rgba.chunks_exact(4).map(|p| f64::from(p[3])).sum()
}

fn rect(r: &Rendered) -> [i32; 4] {
    [r.x0, r.y0, r.x0 + r.width as i32, r.y0 + r.height as i32]
}

#[test]
fn raster_coverage_scales_and_is_deterministic() {
    let mut e = TextEngine::new();
    let (_, r12) = render(&mut e, &point("Ink", 12.0), 10.0, 50.0);
    let (_, r24) = render(&mut e, &point("Ink", 24.0), 10.0, 50.0);
    let (s12, s24) = (alpha_sum(&r12), alpha_sum(&r24));
    assert!(s12 > 20.0, "{s12}");
    let ratio = s24 / s12;
    assert!((3.6..4.4).contains(&ratio), "ink is proportional to size squared: {ratio}");
    // Placement: anchored at (10, 50) baseline.
    let [x0, _, _, y1] = rect(&r12);
    assert!((8..=11).contains(&x0) && (50..=53).contains(&y1), "{:?}", rect(&r12));
    let (_, again) = render(&mut e, &point("Ink", 12.0), 10.0, 50.0);
    assert_eq!(again, r12);
    // Golden total ink for "Ink" in Inter 12 px (PhotoCraft's measurement, +-3%).
    assert!((s12 - 44.4).abs() / 44.4 < 0.03, "ink sum {s12}");
}

#[test]
fn styles_change_pixels() {
    let mut e = TextEngine::new();
    let base = CharStyle { size_pt: 30.0, ..Default::default() };
    let ink = |e: &mut TextEngine, s: CharStyle| {
        let (_, r) = render(e, &styled("Hi", s), 5.0, 40.0);
        (alpha_sum(&r), rect(&r))
    };
    let (plain, prect) = ink(&mut e, base.clone());
    let (bold, _) = ink(&mut e, CharStyle { faux_bold: true, ..base.clone() });
    assert!(bold > plain * 1.1, "{plain} -> {bold}");
    let (under, urect) = ink(&mut e, CharStyle { underline: true, ..base.clone() });
    assert!(under > plain && urect[3] > prect[3]);
    let (_, irect) = ink(&mut e, CharStyle { faux_italic: true, ..base.clone() });
    assert!(irect[2] > prect[2], "slanted top extends right");
    let (_, srect) = ink(&mut e, CharStyle { baseline_shift_pt: 10.0, ..base.clone() });
    assert_eq!(srect[1], prect[1] - 10);
    let (_, hrect) = ink(&mut e, CharStyle { horizontal_scale: 2.0, ..base.clone() });
    assert!((hrect[2] - hrect[0]) as f32 > (prect[2] - prect[0]) as f32 * 1.7);
}

#[test]
fn multicolor_runs() {
    let mut e = TextEngine::new();
    let red = CharStyle { size_pt: 40.0, color: Color::rgb(1.0, 0.0, 0.0), ..Default::default() };
    let blue = CharStyle { color: Color::rgb(0.0, 0.0, 1.0), ..red.clone() };
    let t = TextLayer { text: "HH".into(), runs: vec![TextRun { len: 1, style: red }, TextRun { len: 1, style: blue }], ..Default::default() };
    let (l, r) = render(&mut e, &t, 0.0, 40.0);
    assert_eq!(l.glyphs.len(), 2);
    let opaque: Vec<&[f32]> = r.rgba.chunks_exact(4).filter(|p| p[3] > 0.99).collect();
    assert!(opaque.iter().any(|p| p[0] > 0.99 && p[2] < 0.01));
    assert!(opaque.iter().any(|p| p[2] > 0.99 && p[0] < 0.01));
    assert_eq!(r.to_rgba8().len(), r.width * r.height * 4);
}

#[test]
fn forced_line_break_renders_two_rows() {
    let mut e = TextEngine::new();
    let text = "one\u{2028}two";
    let (l, r) = render(&mut e, &point(text, 20.0), 0.0, 40.0);
    assert_eq!(l.lines.len(), 2);
    let row = |i: usize| {
        let ln = &l.lines[i];
        [-2, (ln.baseline - ln.ascent + 40.0) as i32, 200, (ln.baseline + 40.0) as i32]
    };
    assert!(alpha_in(&r, row(0)) > 1.0 && alpha_in(&r, row(1)) > 1.0);
    let one_end = l.lines[0].x1.ceil() as i32 + 2;
    assert!(alpha_in(&r, [one_end, row(0)[1], 200, row(0)[3]]) < 1e-3, "ink after the first line's text");
}

#[test]
fn variable_font_axes_and_outlines() {
    // Outlines (vector output) follow the layout and the transform.
    let mut e = TextEngine::new();
    let l = e.layout(&point("Ab", 20.0), 72.0);
    let outs = crate::render::outlines(&l, &Xform([1.0, 0.0, 0.0, 1.0, 100.0, 100.0]));
    assert_eq!(outs.len(), 2);
    for o in &outs {
        assert!(matches!(o.first(), Some(crate::PathEl::MoveTo(p)) if p[0] >= 99.0 && p[1] <= 101.0));
    }
}

#[test]
fn hostile_sizes_do_not_panic() {
    let mut e = TextEngine::new();
    for size in [0.0, -5.0, f32::NAN, f32::INFINITY, 1e9, 1e-9] {
        let t = point("Hello 縦書き", size);
        let _ = render(&mut e, &t, 0.0, 0.0);
        let _ = render(&mut e, &TextLayer { orientation: Orientation::Vertical, ..t.clone() }, 0.0, 0.0);
        let _ = render(&mut e, &TextLayer { shape: TextShape::Box { x: 0.0, y: 0.0, width: size, height: size }, ..t }, 0.0, 0.0);
    }
    let _ = render(&mut e, &point("x", 12.0), f64::NAN, f64::INFINITY);
}

#[test]
fn bundled_fonts_cover_latin() {
    let mut e = TextEngine::new();
    let fams = e.fonts.families();
    assert!(fams.iter().any(|f| f == "Inter"), "{fams:?}");
    let l = e.layout(&point("Hello, Wörld! 0123", 12.0), 72.0);
    assert_eq!(l.glyphs.len(), "Hello, Wörld! 0123".chars().count());
    assert!(l.glyphs.iter().all(|g| g.id != 0), "no .notdef");
    assert_eq!(e.fonts.faces("Inter").len(), 3);
}

#[test]
fn registering_fonts_moves_the_generation() {
    let mut e = TextEngine::new();
    let g = fonts::generation();
    e.fonts.register_font_data(fonts::INTER_REGULAR.to_vec());
    assert!(fonts::generation() > g);
}

#[test]
fn literal_psd_tabs_shape_as_whitespace_without_shifting_text_offsets() {
    let mut e = TextEngine::new();
    let src = "A\tB";
    let l = e.layout(&point(src, 20.0), 72.0);
    let plain = e.layout(&point("AB", 20.0), 72.0);
    assert_eq!(l.lines.len(), 1);
    assert!(l.glyphs.iter().all(|g| g.id != 0), "a tab must not render a tofu glyph");
    assert!(width(&l) > width(&plain), "the tab reserves whitespace");
    assert!(l.clusters.iter().all(|c| c.range.end <= src.len()), "the source byte offsets stay valid");
    assert!(l.caret(2).0 > l.caret(1).0, "caret moves across the tab");
}

#[test]
fn metrics_are_stable_and_scale_with_dpi() {
    let mut e = TextEngine::new();
    let a = e.layout(&point("Hamburgefonstiv", 12.0), 72.0);
    let w = width(&a);
    // Inter 12 px: stable to a tenth of a pixel across runs.
    let again = width(&e.layout(&point("Hamburgefonstiv", 12.0), 72.0));
    assert_eq!(w, again);
    assert!((80.0..110.0).contains(&w), "{w}");
    let b = e.layout(&point("Hamburgefonstiv", 12.0), 144.0);
    assert!((width(&b) - 2.0 * w).abs() < 0.05, "{} vs {}", width(&b), 2.0 * w);
    // Point text: first baseline at the anchor.
    assert_eq!(a.lines[0].baseline, 0.0);
    assert!(a.lines[0].ascent > 8.0 && a.lines[0].ascent < 13.0);
}

#[test]
fn small_caps_are_synthesized_when_the_font_has_no_small_caps_feature() {
    let style = CharStyle { font_family: "Inter".into(), size_pt: 40.0, caps: Caps::SmallCaps, ..Default::default() };
    let mut engine = TextEngine::new();
    let small = engine.layout(&styled("aA", style), 72.0);
    let upper = engine.layout(&styled("AA", CharStyle { font_family: "Inter".into(), size_pt: 40.0, ..Default::default() }), 72.0);
    assert_eq!(small.glyphs.len(), 2);
    assert_eq!(small.glyphs[0].id, upper.glyphs[0].id, "lowercase uses the uppercase glyph");
    assert_eq!(small.glyphs[1].id, upper.glyphs[1].id, "uppercase remains uppercase");
    let size = |layout: &crate::TextLayout, glyph: usize| layout.faces[layout.glyphs[glyph].face as usize].size_px;
    assert!((size(&small, 0) - 28.0).abs() < 0.01, "{}", size(&small, 0));
    assert!((size(&small, 1) - 40.0).abs() < 0.01, "{}", size(&small, 1));
}

#[test]
fn auto_and_explicit_leading() {
    let mut e = TextEngine::new();
    let l = e.layout(&point("one\ntwo\rthree", 10.0), 72.0);
    assert_eq!(l.lines.len(), 3);
    assert!((l.lines[1].baseline - 12.0).abs() < 1e-3, "auto leading = 1.2 × size");
    assert!((l.lines[2].baseline - 24.0).abs() < 1e-3);
    assert_eq!(&"one\ntwo\rthree"[l.lines[2].range.clone()], "three");
    let t = styled("a\nb", CharStyle { size_pt: 10.0, leading_pt: Some(30.0), ..Default::default() });
    let l = e.layout(&t, 144.0);
    assert!((l.lines[1].baseline - 60.0).abs() < 1e-3, "{}", l.lines[1].baseline);
    // Space before/after in points.
    let t = with_para(point("a\nb", 10.0), ParagraphStyle { space_after_pt: 5.0, ..Default::default() });
    let l = e.layout(&t, 72.0);
    assert!((l.lines[1].baseline - 17.0).abs() < 1e-3, "{}", l.lines[1].baseline);
}

#[test]
fn box_text_wraps_inside_width() {
    let mut e = TextEngine::new();
    let mut t = point("The quick brown fox jumps over the lazy dog again and again", 12.0);
    t.shape = TextShape::Box { x: 10.0, y: 20.0, width: 100.0, height: 1000.0 };
    let l = e.layout(&t, 72.0);
    assert!(l.lines.len() >= 3, "{}", l.lines.len());
    for line in &l.lines {
        assert!(line.x0 >= 10.0 - 1e-3 && line.x1 <= 110.0 + 1e-3, "{line:?}");
    }
    // First baseline = top + ascender height ('d'), a bit less than the hhea ascent.
    let drop = l.lines[0].baseline - 20.0;
    assert!(drop > 0.65 * 12.0 && drop < l.lines[0].ascent, "{drop}");
    // Lines cover the text in order.
    assert_eq!(l.lines[0].range.start, 0);
    for w in l.lines.windows(2) {
        assert!(w[1].range.start >= w[0].range.end);
    }
    // Overflowing lines are hidden, like Photoshop.
    t.shape = TextShape::Box { x: 0.0, y: 0.0, width: 100.0, height: 30.0 };
    let l2 = e.layout(&t, 72.0);
    assert_eq!(l2.lines.len(), 2);
}

#[test]
fn alignment_point_and_box() {
    let mut e = TextEngine::new();
    let c = e.layout(&with_para(point("Centered", 20.0), ParagraphStyle { align: TextAlign::Center, ..Default::default() }), 72.0);
    let ln = &c.lines[0];
    assert!((ln.x0 + ln.x1).abs() < 0.01, "centered on anchor: {ln:?}");
    let r = e.layout(&with_para(point("Right", 20.0), ParagraphStyle { align: TextAlign::Right, ..Default::default() }), 72.0);
    assert!(r.lines[0].x1.abs() < 0.01 && r.lines[0].x0 < -10.0);

    let mut t = point("aaa bbb ccc ddd eee fff ggg hhh iii jjj kkk", 12.0);
    t.shape = TextShape::Box { x: 0.0, y: 0.0, width: 120.0, height: 500.0 };
    let t = with_para(t, ParagraphStyle { align: TextAlign::JustifyLeft, ..Default::default() });
    let l = e.layout(&t, 72.0);
    assert!(l.lines.len() > 1);
    let first = &l.lines[0];
    // Justified line: last glyph ends at the right edge (within a pixel).
    let last_glyph_x = l.glyphs.iter().filter(|g| (g.y - first.baseline).abs() < 1e-3).map(|g| g.x).fold(f32::MIN, f32::max);
    assert!(last_glyph_x > 110.0, "{last_glyph_x}");
    let right = with_para(
        TextLayer { shape: TextShape::Box { x: 0.0, y: 0.0, width: 200.0, height: 100.0 }, ..point("end", 12.0) },
        ParagraphStyle { align: TextAlign::Right, ..Default::default() },
    );
    let l = e.layout(&right, 72.0);
    assert!((l.lines[0].x1 - 200.0).abs() < 0.5, "{:?}", l.lines[0]);
}

#[test]
fn rtl_text_is_reordered() {
    let mut e = TextEngine::new();
    // Hebrew isn't in the bundled fonts, but bidi still resolves (glyphs may be .notdef).
    let text = "abc שלום def";
    let l = e.layout(&point(text, 12.0), 72.0);
    let heb: Vec<_> = l.clusters.iter().filter(|c| text[c.range.clone()].chars().all(|ch| ('\u{0590}'..='\u{05FF}').contains(&ch))).collect();
    assert_eq!(heb.len(), 4);
    assert!(heb.iter().all(|c| c.rtl));
    // Visual order: the first logical Hebrew letter is rightmost among them.
    let first = heb.iter().min_by_key(|c| c.range.start).unwrap();
    assert!(heb.iter().all(|c| c.x <= first.x + 1e-3));
    // Forced RTL paragraph: the Latin run ends up to the right of the Hebrew.
    let t = with_para(point("שלום abc", 12.0), ParagraphStyle { direction: TextDirection::Rtl, ..Default::default() });
    let l = e.layout(&t, 72.0);
    let a = l.clusters.iter().find(|c| &"שלום abc"[c.range.clone()] == "a").unwrap();
    let shin = l.clusters.iter().find(|c| c.range.start == 0).unwrap();
    assert!(a.x < shin.x, "abc left of the Hebrew in an RTL paragraph");
    assert!(l.clusters.iter().all(|c| c.range.end <= "שלום abc".len()));
}

fn make_ttc(fonts: &[&[u8]]) -> Vec<u8> {
    let header_len = 12 + 4 * fonts.len();
    let mut out = Vec::new();
    out.extend_from_slice(b"ttcf");
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(fonts.len() as u32).to_be_bytes());
    let mut offsets = Vec::new();
    let mut body: Vec<u8> = Vec::new();
    for f in fonts {
        while !(header_len + body.len()).is_multiple_of(4) {
            body.push(0);
        }
        let base = (header_len + body.len()) as u32;
        offsets.push(base);
        let mut copy = f.to_vec();
        let n = u16::from_be_bytes([copy[4], copy[5]]) as usize;
        for i in 0..n {
            let at = 12 + i * 16 + 8;
            let off = u32::from_be_bytes(copy[at..at + 4].try_into().unwrap());
            copy[at..at + 4].copy_from_slice(&(off + base).to_be_bytes());
        }
        body.extend(copy);
    }
    for o in offsets {
        out.extend_from_slice(&o.to_be_bytes());
    }
    out.extend(body);
    out
}

#[test]
fn truetype_collections_load() {
    let ttc = make_ttc(&[fonts::BUNDLED[0].1, fonts::BUNDLED[2].1]);
    assert_eq!(fonts::face_count(&ttc), 2);
    let mut db = fonts::FontDb::new();
    let names = db.register_font_data(ttc);
    assert_eq!(names, vec!["Inter".to_string()]);
    // Layout with the TTC-backed faces works.
    let mut e = TextEngine { fonts: db, layouter: crate::layout::Layouter::new() };
    let t = styled("semi", CharStyle { font_family: "Inter".into(), weight: 600, ..Default::default() });
    let l = e.layout(&t, 72.0);
    assert!(l.glyphs.iter().all(|g| g.id != 0));
}

#[test]
fn opentype_features_and_tracking() {
    let mut e = TextEngine::new();
    let ids = |e: &mut TextEngine, feats: Vec<FontFeature>, lig: bool| {
        let t = styled("a0", CharStyle { features: feats, ligatures: lig, ..Default::default() });
        e.layout(&t, 72.0).glyphs.iter().map(|g| g.id).collect::<Vec<_>>()
    };
    let plain = ids(&mut e, vec![], true);
    let alt = ids(&mut e, vec![FontFeature { tag: "zero".into(), value: 1 }], true);
    assert_ne!(plain, alt, "Inter's `zero` feature selects the slashed zero");
    let a = width(&e.layout(&styled("tracking", CharStyle::default()), 72.0));
    let b = width(&e.layout(&styled("tracking", CharStyle { tracking: 100.0, ..Default::default() }), 72.0));
    // 100/1000 em at 12 px per letter (8 letters; trailing spacing counts too).
    assert!((b - a - 8.0 * 1.2).abs() < 1.3, "{a} → {b}");
}

#[test]
fn hit_test_and_caret() {
    let mut e = TextEngine::new();
    let l = e.layout(&point("abc\ndef", 20.0), 72.0);
    assert_eq!(l.hit_test(-5.0, -5.0), 0);
    assert_eq!(l.hit_test(1000.0, -5.0), 3);
    assert_eq!(l.hit_test(-5.0, 24.0), 4);
    let (x, top, bottom) = l.caret(1);
    assert!(x > 0.0 && top < 0.0 && bottom > 0.0);
    let (x2, ..) = l.caret(2);
    assert!(x2 > x);
    assert_eq!(l.caret(3).0, l.lines[0].x1);
}

#[test]
fn empty_text_and_empty_lines() {
    let mut e = TextEngine::new();
    let l = e.layout(&point("", 12.0), 72.0);
    assert_eq!(l.lines.len(), 1);
    assert!(l.glyphs.is_empty());
    let l = e.layout(&point("a\n\nb", 12.0), 72.0);
    assert_eq!(l.lines.len(), 3);
    assert!((l.lines[2].baseline - 2.0 * 14.4).abs() < 1e-3);
    let (_, r) = render(&mut e, &point("", 12.0), 0.0, 0.0);
    assert_eq!(r.width, 0);
}

/// line with the paragraph's leading and no paragraph spacing, never a missing-glyph box.

#[test]
fn postscript_names() {
    let g = fonts::guess_from_postscript("MyriadPro-BoldIt");
    assert_eq!((g.family.as_str(), g.weight, g.italic), ("Myriad Pro", 700, true));
    let g = fonts::guess_from_postscript("TimesNewRomanPSMT");
    assert_eq!((g.family.as_str(), g.weight, g.italic), ("Times New Roman", 400, false));
    let g = fonts::guess_from_postscript("Arial-BoldMT");
    assert_eq!((g.family.as_str(), g.weight), ("Arial", 700));
    let mut db = fonts::FontDb::new();
    let r = db.resolve_postscript("Inter-SemiBold");
    assert_eq!((r.family.as_str(), r.weight, r.exact), ("Inter", 600, true));
    // Unknown fonts fall back gracefully in layout.
    let mut e = TextEngine::new();
    let t = styled("x", CharStyle { font_family: "Nonexistent Sans".into(), postscript_name: Some("Nope-Bold".into()), ..Default::default() });
    assert!(e.layout(&t, 72.0).glyphs[0].id != 0);
}

fn runs_of(text: &str, styles: &[(usize, CharStyle)]) -> TextLayer {
    TextLayer {
        text: text.into(),
        runs: styles.iter().map(|(len, style)| TextRun { len: *len, style: style.clone() }).collect(),
        ..Default::default()
    }
}

#[test]
fn manual_kerning_moves_the_next_glyph() {
    use crate::style::Kerning;
    let mut e = TextEngine::new();
    let s = CharStyle { size_pt: 100.0, ..Default::default() };
    let plain = e.layout(&styled("HOH", s.clone()), 72.0);
    let kerned = e.layout(&runs_of("HOH", &[(1, CharStyle { kern: 100.0, ..s.clone() }), (2, s.clone())]), 72.0);
    let x = |l: &crate::TextLayout, i: usize| l.glyphs[i].x;
    assert_eq!(x(&kerned, 0), x(&plain, 0));
    // 100/1000 em at 100 px = 10 px, for the next glyph and everything after it.
    assert!((x(&kerned, 1) - x(&plain, 1) - 10.0).abs() < 1e-3, "{} vs {}", x(&kerned, 1), x(&plain, 1));
    assert!((x(&kerned, 2) - x(&plain, 2) - 10.0).abs() < 1e-3);
    assert!((width(&kerned) - width(&plain) - 10.0).abs() < 1e-3);
    // Carets follow: the cluster after the kerned pair starts 10 px later.
    assert!((kerned.caret(1).0 - plain.caret(1).0 - 10.0).abs() < 1e-3);
    // Negative kerning tightens; kerning on the last character doesn't move anything.
    let tight = e.layout(&runs_of("HOH", &[(1, CharStyle { kern: -50.0, ..s.clone() }), (2, s.clone())]), 72.0);
    assert!((x(&tight, 1) - x(&plain, 1) + 5.0).abs() < 1e-3);
    let last = e.layout(&runs_of("HOH", &[(2, s.clone()), (1, CharStyle { kern: 500.0, ..s.clone() })]), 72.0);
    assert!((width(&last) - width(&plain)).abs() < 1e-3);
    // Off replaces the font's pair kerning: "AV" with Off is wider than with Metrics.
    let mut av = |st: CharStyle| e.layout(&styled("AV", st), 72.0).glyphs[1].x;
    let metric = av(s.clone());
    let off = av(CharStyle { kerning: Kerning::Off, ..s.clone() });
    assert!(off > metric + 1.0, "Inter kerns AV: {off} vs {metric}");
    // Centred point text stays centred around the anchor with kerning.
    let centred = with_para(
        runs_of("HOH", &[(1, CharStyle { kern: 300.0, ..s.clone() }), (2, s.clone())]),
        ParagraphStyle { align: TextAlign::Center, ..Default::default() },
    );
    let l = e.layout(&centred, 72.0);
    assert!((l.lines[0].x0 + l.lines[0].x1).abs() < 0.5, "{:?}", l.lines[0]);
}

/// than the unkerned advance, about neutral for straight stems, and never absurd.

#[test]
fn optical_kerning_tightens_open_pairs() {
    use crate::style::Kerning;
    let mut e = TextEngine::new();
    let s = CharStyle { size_pt: 100.0, ..Default::default() };
    let gap = |e: &mut TextEngine, text: &str, k: Kerning| {
        let l = e.layout(&styled(text, CharStyle { kerning: k, ..s.clone() }), 72.0);
        l.glyphs[1].x - l.glyphs[0].x
    };
    for pair in ["AV", "To", "LT", "Ty"] {
        let off = gap(&mut e, pair, Kerning::Off);
        let optical = gap(&mut e, pair, Kerning::Optical);
        assert!(optical < off - 3.0, "{pair}: optical {optical} vs unkerned {off}");
    }
    for pair in ["HH", "nn", "oo", "HO"] {
        let off = gap(&mut e, pair, Kerning::Off);
        let optical = gap(&mut e, pair, Kerning::Optical);
        assert!((optical - off).abs() < 6.0, "{pair}: optical {optical} vs unkerned {off}");
    }
    // A space breaks the pair; a manual kern replaces the automatic one (as in Photoshop).
    assert_eq!(gap(&mut e, "A V", Kerning::Optical), gap(&mut e, "A V", Kerning::Off));
    for mode in [Kerning::Optical, Kerning::Metrics] {
        let l = e.layout(&styled("AV", CharStyle { kerning: mode, kern: 100.0, ..s.clone() }), 72.0);
        let off = gap(&mut e, "AV", Kerning::Off);
        assert!((l.glyphs[1].x - l.glyphs[0].x - off - 10.0).abs() < 1e-3, "{mode:?}");
    }
}

/// lose their automatic kerning (Photoshop renders it the same way).

#[test]
fn kerning_modes_split_pairs() {
    use crate::style::Kerning;
    let mut e = TextEngine::new();
    let s = CharStyle { size_pt: 100.0, ..Default::default() };
    let off = CharStyle { kerning: Kerning::Off, ..s.clone() };
    let xs = |e: &mut TextEngine, t: &TextLayer| e.layout(t, 72.0).glyphs.iter().map(|g| g.x).collect::<Vec<_>>();
    let metric = xs(&mut e, &styled("AVAV", s.clone()));
    let plain = xs(&mut e, &styled("AVAV", off.clone()));
    let mixed = xs(&mut e, &runs_of("AVAV", &[(2, s.clone()), (1, off.clone()), (1, s.clone())]));
    let adv = |v: &[f32], i: usize| v[i + 1] - v[i];
    assert!((adv(&mixed, 0) - adv(&metric, 0)).abs() < 1e-3, "AV before stays kerned");
    assert!((adv(&mixed, 1) - adv(&plain, 1)).abs() < 1e-3, "VA into the manual character");
    assert!((adv(&mixed, 2) - adv(&plain, 2)).abs() < 1e-3, "AV out of it");
}

#[test]
fn japanese_dictionary_word_boundaries() {
    // Use only the existing bundled Latin fonts: segmentation must not require a CJK font.
    let text = "私は学生です";
    let mut fonts = fonts::FontDb::new();
    let mut context = parley::LayoutContext::<[u8; 4]>::new();
    let mut layout = parley::Layout::new();
    let mut builder = context.ranged_builder(&mut fonts.fcx, text, 1.0, false);
    builder.push_default(parley::StyleProperty::FontFamily(parley::FontFamily::named("Inter")));
    // Suppress CJK line-break opportunities so these flags expose word boundaries alone.
    builder.push_default(parley::StyleProperty::WordBreak(parley::WordBreak::KeepAll));
    builder.build_into(&mut layout, text);
    layout.break_all_lines(None);
    let mut boundaries = Vec::new();
    for line in layout.lines() {
        for run in line.runs() {
            for cluster in run.clusters() {
                if cluster.is_word_boundary() {
                    boundaries.push(cluster.text_range().start);
                }
            }
        }
    }
    assert_eq!(boundaries, vec![0, 3, 6, 12]);
}

#[test]
fn japanese_box_text_preserves_wrap_boundaries() {
    let mut engine = TextEngine::new();
    let mut text = point("日本語の文章を折り返します。", 12.0);
    text.shape = TextShape::Box { x: 0.0, y: 0.0, width: 40.0, height: 1000.0 };
    let layout = engine.layout(&text, 72.0);
    assert!(layout.lines.len() > 1);
    let mut end = 0;
    for line in &layout.lines {
        assert_eq!(line.range.start, end);
        let content = text.text.get(line.range.clone()).expect("UTF-8 line boundaries");
        assert!(!content.starts_with('。'), "closing punctuation must stay with its preceding text");
        end = line.range.end;
    }
    assert_eq!(end, text.text.len());
}

fn craft_family_of(l: &crate::TextLayout) -> Vec<String> {
    let mut v = Vec::new();
    let craft = crate::craft_fonts::installed();
    for g in &l.glyphs {
        let Some(face) = l.faces.get(g.face as usize) else { continue };
        let data: &[u8] = face.font.data.as_ref();
        if let Some(f) = craft.iter().find(|f| std::ptr::eq(f.bytes.as_ref().as_ptr(), data.as_ptr()) && f.bytes.as_ref().len() == data.len())
            && !v.contains(&f.family)
        {
            v.push(f.family.clone());
        }
    }
    v
}

#[test]
fn craft_fonts_cover_japanese_without_system_fonts() {
    crate::craft_fonts::install_from_env();
    if !crate::craft_fonts::installed().iter().any(|f| f.is_japanese()) {
        eprintln!("skipping: built without craft-fonts (set CRAFT_FONTS_DIR to a craft-fonts checkout)");
        return;
    }
    let mut e = TextEngine::new();
    let text = "日本語の文字、カタカナ。";
    let l = e.layout(&point(text, 24.0), 72.0);
    assert_eq!(l.glyphs.len(), text.chars().count());
    assert!(l.glyphs.iter().all(|g| g.id != 0), "no .notdef with craft-fonts");
    // Sans (Inter) runs fall back to one craft CJK family: the Gothic UI family, or (outside a
    // Japanese locale, where shared Han takes the Chinese forms) the Chinese one.
    let fams = craft_family_of(&l);
    let japanese_locale = crate::cjk::ui_script_order()[0] == crate::cjk::CjkScript::Japanese;
    let has_chinese = !crate::craft_fonts::chinese_families().is_empty();
    if japanese_locale || !has_chinese {
        assert_eq!(fams, vec![crate::craft_fonts::UI_JAPANESE_FAMILY]);
    } else {
        assert_eq!(fams.len(), 1, "{fams:?}");
    }
    // Serif runs fall back to a Mincho face when the build has one (not the web build).
    if crate::craft_fonts::installed().iter().any(|f| f.is_mincho()) && (japanese_locale || !has_chinese) {
        let mut t = point(text, 24.0);
        t.font_family = "Times New Roman".into();
        let l = e.layout(&t, 72.0);
        assert!(l.glyphs.iter().all(|g| g.id != 0));
        let fams = craft_family_of(&l);
        assert!(fams.len() == 1 && fams[0].contains("Mincho"), "{fams:?}");
    }
    // Latin keeps Inter.
    let l = e.layout(&point("Layer 1", 24.0), 72.0);
    assert!(craft_family_of(&l).is_empty());
}

#[test]
fn works_without_craft_fonts() {
    // Whatever the build: the bundled fonts load, Latin lays out, and Japanese never panics
    // (without craft-fonts or system fonts it may be .notdef).
    let mut e = TextEngine::new();
    assert!(e.fonts.has_family("Inter"));
    let l = e.layout(&point("日本語 Latin", 12.0), 72.0);
    assert_eq!(l.glyphs.len(), "日本語 Latin".chars().count());
    for f in crate::craft_fonts::installed() {
        assert!(e.fonts.has_family(&f.family), "{} registered under its manifest name", f.family);
    }
    if crate::craft_fonts::installed().is_empty() {
        assert!(crate::craft_fonts::japanese_families().is_empty());
        let fb = fonts::fallback_candidates(&crate::cjk::script_order(Some("ja")));
        assert!(!fb.iter().any(|f| f.contains("BIZ UD")));
    }
}

#[test]
fn word_and_line_navigation() {
    use crate::layout::{byte_index, char_index, hit_char, line_edge, line_index, line_step, word_boundary};
    assert_eq!(word_boundary("hello big world", 0, true), 5);
    assert_eq!(word_boundary("hello big world", 7, false), 6);
    assert_eq!(word_boundary("hello big world", 15, false), 10);
    assert_eq!(word_boundary("hello", 0, false), 0);
    assert_eq!(word_boundary("hello", 5, true), 5);
    assert_eq!(word_boundary("", 4, true), 0);
    assert_eq!(word_boundary("ab", 100, true), 2);
    assert_eq!(word_boundary("ab, cd", 0, true), 2);
    assert_eq!(word_boundary("ab, cd", 2, true), 6);
    assert_eq!(word_boundary("Größe", 0, true), 5);

    let mut e = TextEngine::new();
    let text = "AäB\ncd";
    for vertical in [false, true] {
        let mut t = point(text, 20.0);
        if vertical {
            t.orientation = Orientation::Vertical;
        }
        let l = e.layout(&t, 72.0);
        let n = text.chars().count();
        for i in 0..=n {
            let b = byte_index(text, i);
            let [(x0, y0), (x1, y1)] = l.caret_segment(b);
            let (idx, line) = hit_char(&l, text, (x0 + x1) / 2.0, (y0 + y1) / 2.0);
            assert_eq!(idx, i, "vertical {vertical} index {i}");
            assert_eq!(line, line_index(&l, b), "vertical {vertical} index {i}");
        }
        assert!(l.lines.len() >= 2, "vertical {vertical}");
        let end0 = char_index(text, l.lines[0].range.end);
        let start1 = char_index(text, l.lines[1].range.start);
        assert_eq!(line_edge(&l, text, 1, true), end0, "vertical {vertical}");
        assert_eq!(line_edge(&l, text, 1, false), 0, "vertical {vertical}");
        assert_eq!(line_edge(&l, text, start1, false), start1, "vertical {vertical}");
        assert_eq!(line_edge(&l, text, start1, true), n, "vertical {vertical}");
        let x = l.caret(byte_index(text, 0)).0;
        let next = line_step(&l, text, 0, x, 1);
        assert!((start1..=n).contains(&next), "vertical {vertical} line_step -> {next}");
        assert_eq!(line_step(&l, text, 0, x, -1), 0, "vertical {vertical}");
        assert_eq!(line_step(&l, text, n, x, 1), n, "vertical {vertical}");
    }

    // A remembered column stays on the short line's start; the caret's own x falls off its end.
    let text = "WWWWWW\nI";
    let l = e.layout(&point(text, 30.0), 72.0);
    let end0 = line_edge(&l, text, 0, true);
    // end0 is the first line's end, which is not the second line. Step from there.
    let kept = line_step(&l, text, end0, l.caret(0).0, 1);
    let jumped = line_step(&l, text, end0, l.caret(byte_index(text, end0)).0, 1);
    assert_eq!(kept, char_index(text, l.lines[1].range.start), "kept column");
    assert_eq!(jumped, char_index(text, l.lines[1].range.end), "own column");
    assert!(jumped > kept);
}

const THAI_SAMPLE: &str = "ภาษาไทย สวัสดีครับ ผู้ที่น้ำ";

#[test]
fn thai_in_latin_font_falls_back_to_installed_thai_font() {
    // #1909: Thai typed in a Latin-only font (a newly chosen font has no PostScript name to find
    // the original Thai face) must fall back to an installed Thai-capable font, not .notdef.
    let mut e = TextEngine::with_system_fonts();
    let Some(thai) = fonts::THAI_FAMILIES.iter().find(|f| e.fonts.has_family(f)) else {
        eprintln!("skipped: no Thai-capable font installed");
        return;
    };
    let l = e.layout(&point(THAI_SAMPLE, 24.0), 72.0);
    assert!(!l.glyphs.is_empty());
    assert!(l.glyphs.iter().all(|g| g.id != 0), "Thai drawn with .notdef although {thai} is installed");
    // Latin next to Thai keeps the chosen font; only the Thai clusters fall back.
    let l = e.layout(&point("Thai ไทย", 24.0), 72.0);
    assert!(l.glyphs.iter().all(|g| g.id != 0));
    let (first, last) = (l.glyphs.first().map(|g| g.face), l.glyphs.last().map(|g| g.face));
    assert_ne!(first, last, "Latin and Thai drawn with the same face");
}

#[test]
fn thai_fallback_candidates_cover_every_platform() {
    // Logic-level half of #1909 (runs without Thai fonts): Windows, macOS and Linux each have a
    // Thai-capable family in the fallback candidates, ahead of the broad last-resort fonts.
    for order in [crate::cjk::script_order(None), crate::cjk::script_order(Some("ja"))] {
        let fb = fonts::fallback_candidates(&order);
        let last = fb.iter().position(|f| *f == "Arial Unicode MS").unwrap();
        for fam in ["Leelawadee UI", "Tahoma", "Thonburi", "Noto Sans Thai"] {
            let i = fb.iter().position(|f| *f == fam).unwrap_or_else(|| panic!("{fam} missing from {fb:?}"));
            assert!(i < last, "{fam} after the last-resort fonts");
        }
    }
}
