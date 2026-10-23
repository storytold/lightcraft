# LightCraft in Georgian

Choose **Edit → Language → ქართული** or **Settings → General → Language**. The choice applies at once
and is saved for the next launch (`language: "ka"` in `ui.json`); the control command is
`app.language.georgian`. `LIGHTCRAFT_LANGUAGE` selects it for headless runs and accepts `ka`, `ka-GE`
and `ka_GE.UTF-8`; the saved value is always `ka`. LightCraft does not follow the system language,
for Georgian as for every other language.

The Georgian catalogs cover every key of the other catalogs plus all command and slider labels:
menus, shortcuts, library, editing, crop, masks, people, import/export, settings, built-in presets and
profiles, history, versions, date headings, status messages, the smart-album rule editor and the
unsaved-changes dialog. The embedded release notes are translated too. Technical errors from lower
layers stay in English, as in the other languages. They were written from the meaning of the English
labels; no proprietary translation resources were consulted.

File names, command ids, template placeholders and albums, presets or metadata you named yourself are
not translated. Descriptions for SAM 3 go to the model unchanged.

## Georgian script

Mkhedruli has no upper and lower case: `i18n::upper_first` leaves Georgian letters alone, and the
interface never uses Mtavruli (a test checks the whole catalog). Commands are verbal nouns. Counts
are count-neutral, noun first (`იმპორტირებულია ფოტო: 3`); English plural endings are dropped with
`{:.0}`. Dates read `კვირა, 20.09.2026` (`{weekday}, {day:02}.{month:02}.{year}`), months `09.2026`.

Inter has no Georgian letters, so the UI draws Georgian with craft-fonts' Noto Sans Georgian (`Geor`),
like the Thai and Arabic faces: desktop release builds embed it, and a web build made with
`CRAFT_FONTS_DIR` keeps it in the WASM (`crates/engine/build.rs`). The release web build doesn't use
craft-fonts yet, and a build without `CRAFT_FONTS_DIR` has no Georgian face and shows boxes.

## Terminology

| Source | Georgian |
|---|---|
| Library / Develop | ბიბლიოთეკა / დამუშავება |
| File / Edit / View | ფაილი / რედაქტირება / ხედი |
| Settings / General / Language | პარამეტრები / ზოგადი / ენა |
| Import / Export | იმპორტი / ექსპორტი |
| Photos / Albums / Folders | ფოტოები / ალბომები / საქაღალდეები |
| Smart Album | სმარტ-ალბომი |
| Crop | ჩამოჭრა |
| Masks / Masking | ნიღბები / ნიღბვა |
| Heal / Remove | აღდგენა / მოშორება |
| Presets / Profile | პრესეტები / პროფილი |
| History / Versions | ისტორია / ვერსიები |
| Light / Color / Effects / Detail | სინათლე / ფერი / ეფექტები / დეტალი |
| Exposure / Contrast | ექსპოზიცია / კონტრასტი |
| Highlights / Shadows | ღია ტონები / ჩრდილები |
| Whites / Blacks | თეთრები / შავები |
| Texture / Clarity / Dehaze | ტექსტურა / სიცხადე / ბურუსის მოცილება |
| Vibrance / Saturation | სიხალასე / გაჯერება |
| Hue / Luminance | ფერის ტონი / სიკაშკაშე |
| Tint | ტონირება |
| Tone Curve / Color Grading | ტონალური მრუდი / ფერის გრადაცია |
| Sharpening / Noise Reduction | გამკვეთრება / ხმაურის შემცირება |
| Lens Corrections / Geometry | ობიექტივის კორექციები / გეომეტრია |
| Vignette / Grain | ვინიეტი / მარცვლოვნება |
| Feather / Threshold | კიდეების შერბილება / ზღვარი |
| Rating / Flag / Keywords | შეფასება / დროშა / საკვანძო სიტყვები |
| Reset / Revert | ჩამოყრა / წინა ვერსიაზე დაბრუნება |
| Undo / Redo | დაბრუნება / გამეორება |

Please follow this table and the shared ArtCraft Georgian glossary when you add strings.

## Maintenance and checks

Fixed texts: `crates/ui-egui/locales/ka.json` (2,719 entries). Messages with values:
`crates/ui-egui/locales/ka-formats.json` (249 entries). The English keys stay unchanged; Rust checks
the placeholders at build time. Strings not yet in the plain catalog show in English.

```sh
cargo test -p lightcraft-ui-egui i18n::tests -- --nocapture
LIGHTCRAFT_LANGUAGE=ka lightcraft-cli snapshot --demo -o georgian.png --size 1600x1000
```

The tests check catalog coverage, placeholders, switching language by menu and control channel,
saving, the `ka` locale tags, dates, count wording, the absence of Mtavruli and that sentence case
keeps Georgian lowercase. More notes are in
[localization.md](localization.md).
