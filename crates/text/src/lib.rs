//! The app's type engine, ported from PhotoCraft's `text` crate (MIT; see
//! `assets/ATTRIBUTION.md`).
//!
//! * [`fonts::FontDb`]: bundled Inter (always available, including on the web), optional system
//!   fonts from a directory scan (no fontconfig), user font data, TrueType collections,
//!   PostScript-name lookup, and the craft-fonts CJK faces ([`craft_fonts`]) as fallbacks.
//! * [`layout`]: shaping and line layout with [parley] (HarfRust shaping, bidi, line breaking),
//!   point and paragraph (box) text, per-run styles, vertical type (tategaki) with upright CJK
//!   and vertical alternates, CJK punctuation squeeze, optical kerning.
//! * [`render`]: anti-aliased rasterization (exact-area accumulation) into an RGBA buffer, and
//!   glyph outlines for vector output.
//!
//! Each [`PlacedGlyph`] keeps its face (font bytes + index) and glyph id, so a PDF writer can
//! embed and subset the exact fonts used.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod cjk;
pub mod craft_fonts;
pub mod fonts;
pub mod layout;
pub mod optical;
pub mod raster;
pub mod render;
pub mod style;

pub use craft_fonts::{CraftFont, FontBytes};
pub use fonts::{FaceInfo, FontDb, ResolvedFont};
pub use layout::{
    ClusterInfo, GlyphFace, GlyphOrient, LineInfo, PlacedGlyph, TextLayout, byte_index, char_index, hit_char, line_edge, line_index, line_step,
    text_point_inside, word_boundary,
};
/// A font face's bytes (shared blob) and collection index, as held by [`GlyphFace`].
pub use parley::FontData;
pub use raster::Xform;
pub use render::{PathEl, Rendered};
pub use style::{
    AntiAlias, Caps, CharStyle, Color, FontFeature, FontVariation, Kerning, Orientation, ParagraphRun, ParagraphStyle, TextAlign, TextBlock,
    TextDirection, TextRun, TextShape,
};

/// Font database + layout context. Create once and reuse (font loading and shaping caches).
pub struct TextEngine {
    pub fonts: FontDb,
    layouter: layout::Layouter,
}

impl Default for TextEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TextEngine {
    /// Bundled (and installed craft) fonts only: deterministic output (tests, web, export).
    pub fn new() -> Self {
        Self { fonts: FontDb::new(), layouter: layout::Layouter::new() }
    }

    /// Bundled plus installed system fonts (native desktop).
    pub fn with_system_fonts() -> Self {
        Self { fonts: FontDb::with_system_fonts(), layouter: layout::Layouter::new() }
    }

    /// Lays out a text block (text space: pixels at `dpi`; 72 = points).
    pub fn layout(&mut self, block: &TextBlock, dpi: f32) -> TextLayout {
        self.layouter.layout(&mut self.fonts, block, dpi)
    }

    /// Lays out and rasterizes a text block through `transform` (text space -> target pixels).
    pub fn render(&mut self, block: &TextBlock, dpi: f32, transform: &Xform) -> (TextLayout, Rendered) {
        let l = self.layout(block, dpi);
        let r = render::rasterize(&l, transform, block.antialias);
        (l, r)
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod vertical_tests;
