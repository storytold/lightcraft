# Vertical text watermarks

Set `watermark.vertical` to `true` in export options, or choose Vertical in the export dialog. Text runs down each column; a newline starts the next column to its left. The size, nine anchors, inset, opacity, colour and shadow use the same controls as horizontal watermarks. Existing presets without `vertical` remain horizontal.

## Character cells

A vertical cell contains one extended grapheme cluster, not one Unicode scalar. For example, `が` and `か` followed by U+3099 COMBINING KATAKANA-HIRAGANA VOICED SOUND MARK occupy the same space and produce the same image with the Japanese fonts from craft-fonts. The source string is not normalized or rewritten. CRLF and LF each start one new column.

The renderer shapes the complete cluster, retaining all resulting glyphs and their offsets, even when a base and its combining mark remain separate glyphs. It tries the existing watermark faces in order and rejects a shaped cluster containing a missing glyph. Without a suitable font, the existing fallback draws the first character's missing-glyph box in one cell rather than allocating cells to its marks or variation selectors.

Japanese cells use the font's vertical substitutions and origins. Latin text remains upright, as in the existing watermark renderer; clusters with Latin combining marks are shaped horizontally within their cell. This is not a general mixed-script paragraph composer. It does not add rotated Latin runs, tate-chu-yoko, ruby, kinsoku, automatic column wrapping or mojikumi spacing.

## References and scope

- [Unicode UAX #29, §3 Grapheme Cluster Boundaries](https://www.unicode.org/reports/tr29/#Grapheme_Cluster_Boundaries) specifically identifies vertical-text segmentation as a use for grapheme clusters and states that boundaries must not split combining sequences. This is the basis for the cell fix.
- [OpenType `vert`](https://learn.microsoft.com/en-us/typography/opentype/spec/features_uz#tag-vert) describes upright vertical forms and their script sensitivity. The renderer uses harfrust's top-to-bottom shaping for Japanese cells, retaining the previously implemented font-origin placement.
- [W3C Requirements for Japanese Text Layout](https://www.w3.org/TR/jlreq/) describes Japanese paragraph composition, including punctuation spacing and prohibited line boundaries. Those rules are relevant to a future paragraph composer; this fix does not claim full JLReq conformance.
- [Lightroom Classic's public watermark workflow](https://helpx.adobe.com/lightroom-classic/help/using-watermark-editor.html) documents text/graphic watermarks, opacity, size, inset and nine anchors. It does not document Japanese vertical composition. LightCraft's vertical cells are an extension, not a claim to match an undocumented Lightroom vertical-text mode.

Regression coverage in `crates/engine/src/export.rs` compares actual coverage pixels for composed and decomposed Japanese and Latin text, checks multi-glyph mark placement, and retains the existing punctuation and right-to-left-column tests. Japanese font-specific comparisons run with `CRAFT_FONTS_DIR`; Latin and missing-font comparisons run without it.
