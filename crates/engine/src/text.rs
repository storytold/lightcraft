//! Text drawing in pure Rust, shared by the export watermark and the slideshow overlays: Inter
//! SemiBold (bundled) first, then the craft-fonts CJK faces as fallbacks ([`faces`]); glyph outlines
//! from `ab_glyph`, vertical CJK cells shaped with `harfrust`. [`TextLayout`] lays a string out
//! once (lines split at `\n`, kerning within a face) and reports its size, so callers can anchor
//! it before drawing; drawing hands out per-pixel coverage, and the caller blends.
//!
//! RTL scripts are laid out in logical order without bidi reordering, and scripts no face covers
//! draw as Inter's missing-glyph box: wrong-looking, never a crash.

use ab_glyph::{Font, FontRef, Glyph, PxScale, ScaleFont, point};

/// Inter SemiBold (OFL, see assets/ATTRIBUTION.md).
pub static TEXT_FONT: &[u8] = include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf");

/// Most characters one layout draws (longer text is cut there), so hostile text stays cheap.
pub const MAX_CHARS: usize = 4000;

/// The text faces: Inter first, then the craft-fonts CJK faces (Mincho first, the watermark's
/// serif look; then any other CJK face — drawn text has no UI language, so every CJK script is
/// offered). Without craft-fonts that is Inter alone, and CJK characters draw as Inter's
/// missing-glyph box.
pub fn faces(craft: &'static [crate::fonts::CraftFont]) -> Vec<FontRef<'static>> {
    let mut cjk: Vec<_> = crate::fonts::cjk(craft).collect();
    cjk.sort_by_key(|f| !f.is_mincho());
    std::iter::once(TEXT_FONT).chain(cjk.into_iter().map(|f| f.bytes)).filter_map(|bytes| FontRef::try_from_slice(bytes).ok()).collect()
}

/// Shape a whole grapheme in one cell. CJK uses vertical origins and forms; Latin stays upright.
/// Unlike Unicode presentation-form characters, GSUB works with BIZ UD fonts too.
pub fn cell_glyphs(font: &FontRef<'_>, data: &harfrust::ShaperData, text: &str, vertical: bool, px: f32, left: f32, top: f32) -> Option<Vec<Glyph>> {
    let face = harfrust::FontRef::new(font.font_data()).ok()?;
    let mut buf = harfrust::UnicodeBuffer::new();
    buf.push_str(text);
    buf.set_direction(if vertical { harfrust::Direction::TopToBottom } else { harfrust::Direction::LeftToRight });
    buf.guess_segment_properties();
    let shaped = data.shaper(&face).build().shape(buf, harfrust::ShapeOptions::new());
    if shaped.glyph_infos().is_empty() {
        return None;
    }
    let sf = font.as_scaled(px);
    let (sx, sy) = (sf.h_scale_factor(), sf.v_scale_factor());
    let advance_x = shaped.glyph_positions().iter().map(|p| p.x_advance as f32 * sx).sum::<f32>();
    let advance_y = shaped.glyph_positions().iter().map(|p| -(p.y_advance as f32) * sy).sum::<f32>();
    let (mut x, mut y) = if vertical { (left + px / 2.0, top + (px - advance_y) / 2.0) } else { (left + (px - advance_x) / 2.0, top + sf.ascent()) };
    let mut glyphs = Vec::new();
    for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
        let gid = u16::try_from(info.glyph_id).ok().filter(|g| *g != 0)?;
        let pen = point(x + pos.x_offset as f32 * sx, y - pos.y_offset as f32 * sy);
        glyphs.push(ab_glyph::GlyphId(gid).with_scale_and_position(px, pen));
        x += pos.x_advance as f32 * sx;
        y -= pos.y_advance as f32 * sy;
    }
    Some(glyphs)
}

/// A string laid out at one size: its glyphs relative to the box's top-left corner and the box's
/// size in pixels (lines are `px` apart; vertical text: columns `px` wide, right to left).
pub struct TextLayout {
    fonts: Vec<FontRef<'static>>,
    /// (face, glyph, rotation centre for glyphs turned 90° in vertical text).
    glyphs: Vec<(usize, Glyph, Option<(f32, f32)>)>,
    pub width: f32,
    pub height: f32,
}

impl TextLayout {
    /// `text` (cut at [`MAX_CHARS`]) at `px` pixels a line; `None` when there is nothing to draw
    /// (empty text, a non-finite or non-positive size).
    pub fn new(text: &str, px: f32, vertical: bool, craft: &'static [crate::fonts::CraftFont]) -> Option<TextLayout> {
        use unicode_segmentation::UnicodeSegmentation;
        if text.is_empty() || !px.is_finite() || px <= 0.0 {
            return None;
        }
        let text = match text.char_indices().nth(MAX_CHARS) {
            Some((i, _)) => text.get(..i).unwrap_or(text),
            None => text,
        };
        let fonts = faces(craft);
        let latin = fonts.first()?.clone();
        let vertical_data: Vec<_> = if vertical {
            fonts.iter().map(|font| harfrust::FontRef::new(font.font_data()).ok().map(|face| harfrust::ShaperData::new(&face))).collect()
        } else {
            Vec::new()
        };
        let mut glyphs = Vec::new();
        let mut x = 0.0f32;
        let mut y = 0.0f32;
        let mut tw = 0.0f32;
        let mut prev = None;
        let columns = text.split('\n').count();
        let mut column = 0usize;
        // Horizontal text keeps scalar layout and kerning; only vertical cells use graphemes.
        let cells: Box<dyn Iterator<Item = &str>> =
            if vertical { Box::new(text.graphemes(true)) } else { Box::new(text.char_indices().filter_map(|(i, c)| text.get(i..i + c.len_utf8()))) };
        for cell in cells {
            let Some(original) = cell.chars().next() else { continue };
            if cell == "\n" || (vertical && cell == "\r\n") {
                if vertical {
                    column = column.saturating_add(1);
                    tw = tw.max(y);
                    y = 0.0;
                } else {
                    tw = tw.max(x);
                    x = 0.0;
                    y += px;
                }
                prev = None;
                continue;
            }
            if original.is_control() {
                continue;
            }
            let cjk = matches!(original as u32, 0x3000..=0x30FF | 0x3400..=0x9FFF | 0xFF01..=0xFF60);
            if vertical && (cjk || cell.chars().count() > 1) {
                let left = columns.saturating_sub(column.saturating_add(1)) as f32 * px;
                let shaped = fonts.iter().zip(&vertical_data).enumerate().find_map(|(face, (font, data))| {
                    let data = data.as_ref()?;
                    let cluster = cell_glyphs(font, data, cell, cjk, px, left, y)?;
                    let substituted = cluster.len() != 1 || cluster.first().is_some_and(|g| g.id != font.glyph_id(original));
                    if cjk && !substituted && matches!(original, '、' | '。' | 'ー' | '（' | '）' | '「' | '」' | '『' | '』' | '【' | '】')
                    {
                        return None;
                    }
                    Some((face, cluster))
                });
                if let Some((face, cluster)) = shaped {
                    glyphs.extend(cluster.into_iter().map(|g| (face, g, None)));
                    y += px;
                    continue;
                }
            }
            let vertical_form = match original {
                '、' => '︑',
                '。' => '︒',
                '（' => '︵',
                '）' => '︶',
                '「' => '﹁',
                '」' => '﹂',
                '『' => '﹃',
                '』' => '﹄',
                '【' => '︻',
                '】' => '︼',
                '…' => '︙',
                other => other,
            };
            let has = |font: &FontRef<'_>, c: char| font.glyph_id(c).0 != 0;
            let ch = if vertical && fonts.iter().skip(1).any(|f| has(f, vertical_form)) { vertical_form } else { original };
            // The first face with the glyph; Inter (its missing-glyph box) when none has it.
            let face = fonts.iter().position(|f| has(f, ch)).unwrap_or(0);
            let sf = fonts.get(face).unwrap_or(&latin).as_scaled(PxScale::from(px));
            let id = sf.glyph_id(ch);
            if vertical {
                let left = columns.saturating_sub(column.saturating_add(1)) as f32 * px;
                let pen = point(left + (px - sf.h_advance(id)) / 2.0, y + sf.ascent());
                let rotation = matches!(original, 'ー' | '—' | '–').then_some((left + px / 2.0, y + px / 2.0));
                glyphs.push((face, id.with_scale_and_position(px, pen), rotation));
                y += px;
            } else {
                if let Some((previous_face, previous)) = prev
                    && previous_face == face
                {
                    x += sf.kern(previous, id);
                }
                glyphs.push((face, id.with_scale_and_position(px, point(x, y + sf.ascent())), None));
                x += sf.h_advance(id);
                prev = Some((face, id));
            }
        }
        let (width, height) = if vertical { (columns as f32 * px, tw.max(y)) } else { (tw.max(x), y + px) };
        Some(TextLayout { fonts, glyphs, width, height })
    }

    /// Call `f(x, y, coverage)` for every pixel the text covers with its top-left corner at
    /// (`ox`, `oy`); coordinates may be negative or past the image (the caller clips).
    pub fn draw(&self, ox: f32, oy: f32, f: impl FnMut(i32, i32, f32)) {
        self.draw_in(ox, oy, f32::INFINITY, f32::INFINITY, f);
    }

    /// [`Self::draw`], skipping the glyphs that fall wholly outside `0..clip_w × 0..clip_h`
    /// (so huge text costs what shows, not what is laid out).
    /// Call `f(x, y, coverage)` for every pixel the text covers, in unrounded coordinates relative
    /// to the box's top-left corner (for callers that transform them, e.g. a turned watermark).
    pub fn draw_points(&self, mut f: impl FnMut(f32, f32, f32)) {
        for (face, g, rotation) in &self.glyphs {
            if let Some(o) = self.fonts.get(*face).and_then(|font| font.outline_glyph(g.clone())) {
                let b = o.px_bounds();
                o.draw(|gx, gy, cov| {
                    let (sx, sy) = (b.min.x + gx as f32, b.min.y + gy as f32);
                    let (sx, sy) = match rotation {
                        Some((cx, cy)) => (cx - (sy - cy), cy + (sx - cx)),
                        None => (sx, sy),
                    };
                    f(sx, sy, cov);
                });
            }
        }
    }

    pub fn draw_in(&self, ox: f32, oy: f32, clip_w: f32, clip_h: f32, mut f: impl FnMut(i32, i32, f32)) {
        for (face, g, rotation) in &self.glyphs {
            if rotation.is_none() {
                // a glyph lies within one line height below its pen's top and an em or so after it
                let (px, py) = (ox + g.position.x, oy + g.position.y);
                let em = g.scale.y.max(g.scale.x);
                if px + 2.0 * em < 0.0 || px - em > clip_w || py + em < 0.0 || py - 2.0 * em > clip_h {
                    continue;
                }
            }
            if let Some(o) = self.fonts.get(*face).and_then(|font| font.outline_glyph(g.clone())) {
                let b = o.px_bounds();
                if rotation.is_none() && (ox + b.max.x < 0.0 || ox + b.min.x > clip_w || oy + b.max.y < 0.0 || oy + b.min.y > clip_h) {
                    continue;
                }
                o.draw(|gx, gy, cov| {
                    let (sx, sy) = (b.min.x + gx as f32, b.min.y + gy as f32);
                    let (sx, sy) = match rotation {
                        Some((cx, cy)) => (cx - (sy - cy), cy + (sx - cx)),
                        None => (sx, sy),
                    };
                    f((ox + sx) as i32, (oy + sy) as i32, cov);
                });
            }
        }
    }
}
