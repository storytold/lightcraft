//! Font embedding: every face used on any page is subset to the glyphs drawn (`subsetter`) and
//! embedded as a CID-keyed Type 0 font (Identity-H, CID = subset glyph id), TrueType outlines as
//! `CIDFontType2` + `FontFile2`, CFF outlines as `CIDFontType0` + `FontFile3 /OpenType`. A
//! `ToUnicode` CMap maps glyphs back to the characters they were shaped from (where the font's
//! character map says so), so text can be searched and copied.
//!
//! Faces that cannot be embedded (variable-font instances, fonts the subsetter rejects, fonts
//! whose licence forbids embedding) are drawn as filled outlines instead: the page looks the same,
//! only the text is not selectable.
//!
//! Sources: ISO 32000-1 §9.7 (composite fonts), §9.10.3 (ToUnicode); own design.

use std::collections::{BTreeMap, BTreeSet};

use skrifa::raw::TableProvider;
use skrifa::{FontRef, GlyphId, MetadataProvider};

/// Identity of a face: the font blob's id and collection index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct FaceKey {
    pub blob: u64,
    pub index: u32,
}

/// A face used in the document and the glyphs drawn with it.
pub(crate) struct FaceUse {
    pub font: dac_text::FontData,
    pub glyphs: BTreeSet<u16>,
    /// Characters drawn with this face (for ToUnicode).
    pub chars: BTreeSet<char>,
    /// Drawn as outlines (never embedded): variable instances.
    pub outlines_only: bool,
}

/// A subset font ready to write.
pub(crate) struct Embedded {
    pub base_font: String,
    pub cff: bool,
    pub data: Vec<u8>,
    /// Old glyph id -> new glyph id (CID).
    pub remap: BTreeMap<u16, u16>,
    /// CID -> advance width in 1/1000 em.
    pub widths: Vec<(u16, f32)>,
    /// CID -> character.
    pub to_unicode: Vec<(u16, char)>,
    pub ascent: f32,
    pub descent: f32,
    pub cap_height: f32,
    pub bbox: [f32; 4],
    pub italic_angle: f32,
    pub serif: bool,
    pub fixed: bool,
}

/// The OS/2 `fsType` "restricted licence embedding" bit: the font must not be embedded.
fn embedding_forbidden(font: &FontRef<'_>) -> bool {
    font.os2().is_ok_and(|os2| os2.fs_type() & 0x000F == 0x0002)
}

/// A PostScript-name-safe string (PDF names allow more, but viewers display this one).
fn sanitize(name: &str) -> String {
    let s: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(60).collect();
    if s.is_empty() { "Font".into() } else { s }
}

/// Six capital letters derived from the subset (ISO 32000-1 §9.6.4).
fn subset_tag(seed: u64) -> String {
    let mut x = seed ^ 0x9E37_79B9_7F4A_7C15;
    (0..6)
        .map(|_| {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            char::from(b'A' + ((x >> 33) % 26) as u8)
        })
        .collect()
}

impl FaceUse {
    /// Subsets and describes the face, or `None` when it must be drawn as outlines.
    pub fn embed(&self) -> Option<Embedded> {
        if self.outlines_only || self.glyphs.is_empty() {
            return None;
        }
        let data: &[u8] = self.font.data.as_ref();
        let font = FontRef::from_index(data, self.font.index).ok()?;
        if embedding_forbidden(&font) {
            return None;
        }
        // Glyph 0 (.notdef) is always kept as CID 0.
        let mut mapper = subsetter::GlyphRemapper::new();
        mapper.remap(0);
        for &g in &self.glyphs {
            mapper.remap(g);
        }
        let sub = subsetter::subset(data, self.font.index, &mapper).ok()?;
        let subset_font = FontRef::new(&sub).ok()?;
        let cff = subset_font.cff().is_ok();
        let remap: BTreeMap<u16, u16> = self.glyphs.iter().filter_map(|&g| Some((g, mapper.get(g)?))).collect();

        let upem = f32::from(font.head().ok()?.units_per_em()).max(1.0);
        let k = 1000.0 / upem;
        let metrics = font.glyph_metrics(skrifa::instance::Size::unscaled(), skrifa::instance::LocationRef::default());
        let mut widths: Vec<(u16, f32)> =
            remap.iter().map(|(&old, &new)| (new, metrics.advance_width(GlyphId::new(u32::from(old))).unwrap_or(0.0) * k)).collect();
        widths.sort_by_key(|w| w.0);

        let charmap = font.charmap();
        let mut to_unicode: BTreeMap<u16, char> = BTreeMap::new();
        for &c in &self.chars {
            if let Some(g) = charmap.map(c)
                && let Ok(g) = u16::try_from(g.to_u32())
                && let Some(&new) = remap.get(&g)
            {
                to_unicode.entry(new).or_insert(c);
            }
        }

        let ps = font
            .localized_strings(skrifa::string::StringId::POSTSCRIPT_NAME)
            .english_or_first()
            .map(|s| s.chars().collect::<String>())
            .unwrap_or_default();
        let seed = self.glyphs.iter().fold(sub.len() as u64, |a, &g| a.wrapping_mul(31).wrapping_add(u64::from(g)));
        let head = font.head().ok()?;
        let bbox = [f32::from(head.x_min()) * k, f32::from(head.y_min()) * k, f32::from(head.x_max()) * k, f32::from(head.y_max()) * k];
        let m = font.metrics(skrifa::instance::Size::unscaled(), skrifa::instance::LocationRef::default());
        let post = font.post().ok();
        let os2 = font.os2().ok();
        Some(Embedded {
            base_font: format!("{}+{}", subset_tag(seed), sanitize(&ps)),
            cff,
            data: sub,
            remap,
            widths,
            to_unicode: to_unicode.into_iter().collect(),
            ascent: m.ascent * k,
            descent: m.descent * k,
            cap_height: m.cap_height.unwrap_or(m.ascent * 0.7) * k,
            bbox,
            italic_angle: post.as_ref().map(|p| p.italic_angle().to_f32()).unwrap_or(0.0),
            serif: os2.as_ref().is_some_and(|o| matches!(o.s_family_class() >> 8, 1..=5 | 7)),
            fixed: post.is_some_and(|p| p.is_fixed_pitch() != 0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_tags() {
        assert_eq!(sanitize("Inter-Regular"), "Inter-Regular");
        assert_eq!(sanitize("A B/(C)"), "ABC");
        assert_eq!(sanitize(""), "Font");
        let t = subset_tag(42);
        assert_eq!(t.len(), 6);
        assert!(t.chars().all(|c| c.is_ascii_uppercase()));
        assert_eq!(t, subset_tag(42));
    }
}
