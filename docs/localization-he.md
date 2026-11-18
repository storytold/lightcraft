# Hebrew (`he`) interface localization

LightCraft's Hebrew interface locale is registered as `he`, uses the endonym `עברית`,
and declares the `Hebr` script. The initial catalog is **partial**.
When a string has not yet been translated, the interface shows the English source
text rather than crashing. This does not mean the whole interface is localized.

## Catalogs

- `crates/ui-egui/locales/he.json`: source-English keys and Hebrew UI strings.
- `crates/ui-egui/locales/he-formats.json`: initially empty. Messages needing
  Rust `format!` arguments continue to use the English fallback until their
  templates have been translated and validated.
- The language command is `app.language.hebrew`; the settings picker and
  command lookup use the same `Locale::He`.

Keep JSON keys byte-for-byte identical to the English source messages, preserve
format placeholders, and validate all new translations against their call sites.

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
