# Hebrew (`he`) interface localization

LightCraft's Hebrew interface locale is registered as `he`, uses the endonym `עברית`,
and declares the `Hebr` script. The initial catalog is **partial**.
When a string has not yet been translated, the interface shows the English source
text rather than crashing. This does not mean the whole interface is localized.

## Catalogs

- `crates/ui-egui/locales/he.json`: 1173 ordinary source-English keys and Hebrew UI strings.
- `crates/ui-egui/locales/he-formats.json`: 14 dynamic Rust format templates. Untranslated
  format messages continue to use the English fallback.
- The language command is `app.language.hebrew`; the settings picker and
  command lookup use the same `Locale::He`.

Catalog comparison at upstream `5a29fe60` (2026-11-24): 1,169 of 3,540
German plain-catalog keys and 14 of 329 German format-catalog keys overlap.
Four active Hebrew plain keys are absent from the German catalog. These are
catalog comparisons, not measured live-interface coverage.

Keep JSON keys byte-for-byte identical to the English source messages, preserve
format placeholders, and validate all new translations against their call sites.

## Catalog migration

The upstream [craft-i18n roadmap](https://github.com/storytold/craft-i18n/blob/main/ROADMAP.md)
places LightCraft's JSON-to-TSV migration in milestone 3c. Keep both JSON catalogs
as the source for this draft until cutover. Import their source/value pairs into
`locales/he/lightcraft.tsv`, deduplicating identical shared rows against
`locales/he/common.tsv`; keep app-specific overrides and record attribution.
Convert Rust positional format arguments to the new runtime's named placeholders
together with the corresponding source call sites. Do not publish independent
duplicate translations or change Hebrew's draft/RTL status as part of the import.

## Checks

```sh
cargo test -p lightcraft-ui-egui i18n::tests
CRAFT_FONTS_DIR=../craft-fonts cargo test -p lightcraft-ui-egui i18n::tests
LIGHTCRAFT_LANGUAGE=he lightcraft-cli snapshot --demo --script tour.jsonl -o hebrew-ui.png --size 1600x1000
```

The second command requires the optional checkout of
[craft-fonts](https://github.com/storytold/craft-fonts), which provides
Hebrew-capable faces. Without it, the UI may show missing-glyph boxes.
The screenshot command requires a built CLI and a valid control script.

## Visual limitations to review

Language registration and Unicode text shaping do **not** prove that every
panel is mirrored correctly. In a native review, verify the settings picker,
menu labels, Hebrew/English text and numbers, file paths, keyboard shortcuts,
accessibility labels, clipping, and glyph fallback. Image canvases, timelines,
numeric axes and other directional content should not be blindly mirrored.

The tests and source language catalog alone do not establish that the
application has passed a Hebrew RTL usability review.
