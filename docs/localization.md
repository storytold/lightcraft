# Localization

The interface ships in the language it is written in (English) plus every language in the table in
`crates/ui-egui/src/i18n.rs`. Today that is English, Simplified Chinese (`zh-hans`), Traditional
Chinese (`zh-hant`, Taiwan), Japanese (`ja`), Brazilian Portuguese (`pt-br`), Spanish (`es`), German (`de`), Russian (`ru`), French (`fr`), Ukrainian (`uk`), Czech (`cs`), Italian (`it`), Georgian (`ka`), Swedish (`sv`), Finnish (`fi`), Indonesian (`id`), Vietnamese (`vi`) and Polish (`pl`). This file is the reference for **adding or maintaining a language**; the per-language notes
are in [`localization-zh-hans.md`](localization-zh-hans.md), [`localization-zh-hant.md`](localization-zh-hant.md),
[`localization-ja.md`](localization-ja.md), [`localization-pt-br.md`](localization-pt-br.md), [`localization-es.md`](localization-es.md),
[`localization-de.md`](localization-de.md), [`localization-ru.md`](localization-ru.md),
[`localization-cs.md`](localization-cs.md), [`localization-fi.md`](localization-fi.md), [`localization-ka.md`](localization-ka.md), [`localization-id.md`](localization-id.md), [`localization-vi.md`](localization-vi.md) and [`localization-pl.md`](localization-pl.md).

## Adding a language

1. **Translate the two catalogs.** Copy `locales/ja.json` and `locales/ja-formats.json` to
   `locales/<code>.json` and `locales/<code>-formats.json`, then translate the values. Keys are the
   English source strings and must stay byte-for-byte identical. Both catalogs may grow at their
   own pace: a message a catalog lacks, or translates as an empty string, shows in English, and
   nothing fails. The translation gaps report lists what each language lacks (see
   [Testing and looking at it](#testing-and-looking-at-it)). The formats catalog also holds the date
   patterns (`{year}`, `{month} {year}`, `{weekday}, {day} {month} {year}`…) that date headings and
   capture times use in every language but English; weekday names go in the plain catalog.
2. **Add the table entry** in `crates/ui-egui/src/i18n.rs`:

   ```rust
   language_table! {
       En, "en";
       ZhHans, "zh-hans", "简体中文", "Hans", include_str!("../locales/zh-hans.json");
       ZhHant, "zh-hant", "繁體中文（台灣）", "Hant", include_str!("../locales/zh-hant.json");
       Ja, "ja", "日本語", "Jpan", include_str!("../locales/ja.json");
       PtBr, "pt-br", "Português (Brasil)", "Latn", include_str!("../locales/pt-br.json");
       De, "de", "Deutsch", "Latn", include_str!("../locales/de.json");
       Ru, "ru", "Русский", "Cyrl", include_str!("../locales/ru.json");
       Uk, "uk", "Українська", "Cyrl", include_str!("../locales/uk.json");
       Cs, "cs", "Čeština", "Latn", include_str!("../locales/cs.json");
       It, "it", "Italiano", "Latn", include_str!("../locales/it.json");
       Ka, "ka", "ქართული", "Geor", include_str!("../locales/ka.json");
       Fi, "fi", "Suomi", "Latn", include_str!("../locales/fi.json");
       Vi, "vi", "Tiếng Việt", "Latn", include_str!("../locales/vi.json");
   }
   ```

   The fields are the BCP-47 code (also the settings-file value), the **endonym** shown in the
   Language menu, the ISO 15924 script (`Latn`, `Jpan`, `Hans`, `Hant`, `Kore`…), and the embedded
   catalog. `build.rs` picks up `<code>-formats.json` by name, so the macro and the format lookup
   follow automatically. Add the corresponding `app.language.<name>` entry to
   `LANGUAGE_COMMANDS` and `language_from_command` in `src/menus.rs`, and to the
   `language_commands_cover_every_language` test. Settings read the locale table; menus and
   the control channel share the language-command mapping.
3. **Make sure the glyphs exist.** The script drives which craft-fonts faces the UI installs: a
   language whose script has no face in the build shows boxes (the test says so instead of failing).
   Fonts live in [storytold/craft-fonts](https://github.com/storytold/craft-fonts), never here; a
   missing face is added there, not worked around in this repository. Sentence-cased messages go
   through `i18n::upper_first`, which leaves Georgian lowercase (upper-casing Mkhedruli would make
   Mtavruli).
4. Run `cargo xtask ci` (it includes `fmt`, `clippy`, the tests, `parity` and the wasm check).

## How a message is found

`tr("Exposure")` looks the English label up in the active language's catalog and returns the source
text unchanged when there is no entry; `tr_format!` handles text with runtime values. Both are
presentation-only: command ids, file names and user-entered metadata are never translated.

A few messages take a **context**, for a word whose shared translation reads wrongly in one place.
The catalog may then carry `"<context>|<English>"`, which wins there; without it the plain message
is used, so only the languages that need it add one. Today the one context is `smart album value`:
the Yes / No of a yes/no rule (Has Edits, Cropped…). Japanese uses it because はい / いいえ answer a
question rather than describe a photo (`"smart album value|Yes": "あり"`, `"smart album value|No":
"なし"`); every other language uses its plain Yes / No. A contextual key that names no real value
fails the i18n tests.

`tr_format!` is generated by `crates/ui-egui/build.rs`: one arm per English message, which formats
the active language's translation with `format!` (English is the fallback arm). Every catalog's
format string is therefore checked by Rust against the call site's own arguments:

```rust
tr_format!("Added {n} photo{} to “{}”", if n == 1 { "" } else { "s" }, album, n = n)
```

- A translation is an ordinary Rust format string over the same arguments. Refer to named values by
  name (`{n}`) and to positional ones by index when the word order changes (`{0}`, `{1}`):
  `"已将 {n} 张照片添加到“{1}”{0:.0}"`. Keep the source's spec (`{:.1}`, `{d:+}`, `{:.0}%`).
- `format!` rejects an unused argument, so a value a language does not need — the English plural
  suffix (`"s"` / `""`) — is consumed as `{:.0}` (a string at precision 0 prints nothing).
- A language whose formats catalog lacks a message shows the English source there, and a
  `tr_format!` call whose message is in no catalog formats the English source (the macro's last
  arm). A new message builds with no catalog entry.

## Testing and looking at it

```sh
cargo test -p lightcraft-ui-egui i18n::tests                       # catalogs load, placeholders, switching
CRAFT_FONTS_DIR=../craft-fonts cargo test -p lightcraft-ui-egui i18n::tests   # + glyph coverage
cargo test -p lightcraft-ui-egui --lib -- --exact i18n::tests::translation_gaps_report --nocapture
```

The tests fail on a broken catalog (malformed JSON, a repeated key, placeholders that differ from
the source), never on a missing translation. The last command is the translation gaps report: per
language, the count and list of plain and format messages it lacks and the number of untranslated
What's New lines. It always passes, and `cargo xtask ci` prints it after its test step. The glyph
tests are skipped without `CRAFT_FONTS_DIR` (there are no CJK faces to check then).
To look at a language, render it headless:

```sh
LIGHTCRAFT_LANGUAGE=zh-hans lightcraft-cli snapshot --demo --script tour.jsonl -o out.png --size 1600x1000
```

or, with the app running (`--control 7980`), run the language command
(`engine.execute {"command": "app.language.simplifiedChinese"}`) and take a screenshot.

## Language codes and settings

The settings file stores the BCP-47 code, never the Rust variant name, so a language can be renamed
in code without invalidating anyone's saved preference. `LIGHTCRAFT_LANGUAGE` accepts what a system
locale looks like (`zh_Hans`, `zh-CN`, `en_US`, `ja_JP.UTF-8`) and falls back to the base language
when a region has no dedicated entry.

## Release notes

Help ▸ What's New shows `docs/whats-new.md`, and each of its lines (a heading, or one wrapped line of a
bullet, without the `- ` or `### `) is looked up like any message, so a catalog can translate the notes
line by line (German does). A line that is re-wrapped or edited no longer matches its translation and
shows in English until the catalog is updated; the translation gaps report counts the untranslated
lines.
