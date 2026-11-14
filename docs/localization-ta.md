# LightCraft in Tamil

Choose **Edit → Language → தமிழ்** or **Settings → General → Language**. The choice applies at once and
is saved for the next launch (`language: "ta"` in `ui.json`); the control command is
`app.language.tamil`. `LIGHTCRAFT_LANGUAGE` selects it for headless runs and accepts `ta`, `ta-IN`,
`ta-LK` and `ta_IN.UTF-8`; the saved value is always `ta`. LightCraft does not follow the system
language, for Tamil as for every other language.

The Tamil catalogs cover every plain and formats message the gap report counts (2,662 + 337): menus,
shortcuts, library, editing, crop, masks, people, import/export, print, slideshow, settings, built-in
presets and profiles, history, date headings, status messages and the smart-album rule editor. The
embedded release notes (What's New) stay in English. Technical errors from lower layers stay in English,
as in the other languages. 576 labels reuse the Tamil wording of the sibling apps' catalogs in
[storytold/craft-i18n](https://github.com/storytold/craft-i18n), so the suite reads the same: 454 as
merged there (the PhotoCraft, PdfCraft and shared catalogs, reviewed by native speakers) and 122 from
catalogs still in storytold/craft-i18n#226. The rest were written with an agent from the meaning of
the English labels and have not yet been reviewed by a native speaker. A native speaker's pass is
welcome.

File names, command ids, template placeholders and albums, presets or metadata you named yourself are
not translated.

## Style

- Buttons and menu commands use the plain imperative (சேமி, திற, நீக்கு, ஏற்றுமதி செய்); instructions
  and messages are polite full sentences (…செய்யவும்).
- A Tamil noun takes its plural after any count but one (1 புகைப்படம், 5 புகைப்படங்கள்), and a
  translation can't pick the form, so counts are count-neutral, noun first
  (`இறக்குமதி செய்யப்பட்ட புகைப்படங்கள்: 3`), and English plural endings are dropped with `{:.0}`.
- Dates read `ஞாயிறு, 20/09/2026` (`{weekday}, {day:02}/{month:02}/{year}`), months `09/2026`;
  the date picker's weekday initials are ஞா தி செ பு வி வெ ச.
- A case ending after a name or placeholder is joined with a hyphen (`“{name}”-இல்`, `{path}-க்கு`).
- `▸` and `▾` are written `→` and `⌄`, as in the other catalogs. Product names, file formats, key names
  and units (LightCraft, Lightroom, RAW, JPEG, ISO, EV, px) keep their spelling; digits stay 0–9.

## Tamil script

Tamil has no letter case: `i18n::upper_first` leaves Tamil letters as they are. Inter has no Tamil
letters, so the UI draws Tamil with craft-fonts' Noto Sans Tamil (`Taml`), like the Georgian, Thai and
Arabic faces: desktop builds made with `CRAFT_FONTS_DIR` embed it, and a web build made with it keeps it
in the WASM (`crates/engine/build.rs`). egui shapes the text with HarfRust, so the vowel signs sit
around their consonant. craft-fonts has the face from revision `c1db6d1`
([storytold/craft-fonts#21](https://github.com/storytold/craft-fonts/pull/21)); a build made with an
older revision, or without `CRAFT_FONTS_DIR`, has no Tamil face and shows boxes.

## Terminology

| Source | Tamil |
|---|---|
| File / Edit / View / Help | கோப்பு / திருத்து / காட்சி / உதவி |
| Settings / Language | அமைப்புகள் / மொழி |
| Import / Export | இறக்குமதி செய் / ஏற்றுமதி செய் |
| Library / Develop | நூலகம் / பதப்படுத்தல் |
| Photos / Albums / Folders | புகைப்படங்கள் / ஆல்பங்கள் / கோப்புறைகள் |
| My Photos / All Photos | என் புகைப்படங்கள் / அனைத்துப் படங்கள் |
| Smart Album / Collection | ஸ்மார்ட் ஆல்பம் / தொகுப்பு |
| Crop / Straighten | வெட்டிச் சுருக்கு / நேராக்கு |
| Exposure / Contrast | வெளிப்பாடு / மாறுபாடு |
| Highlights / Shadows / Whites / Blacks | ஒளிர்வுகள் / நிழல்கள் / வெள்ளைகள் / கருப்புகள் |
| White Balance / Temperature / Tint | வெண் சமநிலை / வெப்பநிலை / சாயம் |
| Vibrance / Saturation | துடிப்பு / செறிவு |
| Clarity / Texture / Dehaze | தெளிவு / பரப்பமைப்பு / மூடுபனி நீக்கு |
| Sharpening / Noise Reduction | கூர்மையாக்கல் / இரைச்சல் குறைப்பு |
| Vignette / Grain | வினெட் / தானியம் |
| Tone Curve / Histogram | தொனி வளைவு / ஹிஸ்டோகிராம் |
| Lens Corrections / Upright | லென்ஸ் திருத்தங்கள் / நிமிர்த்தல் |
| Masking / Mask / Brush | மறைப்புருவாக்கம் / மறைப்பு / தூரிகை |
| Presets / Profile | முன்னமைவுகள் / சுயவிவரம் |
| Keywords / Rating | முக்கிய சொற்கள் / மதிப்பீடு |
| Flag / Pick / Reject | கொடி / தேர்வு / நிராகரி |
| Stack / Virtual Copy | குவியல் / மெய்நிகர் நகல் |
| History / Snapshots | வரலாறு / உடனடி காட்சிப்பதிவுகள் |
| Recently Deleted / Restore / Reset | சமீபத்தில் நீக்கியவை / மீட்டெடு / மீட்டமை |
| Compare: Select / Candidate | தேர்வு / பரிசீலனைப் படம் |
| Culling / Burst | தரம்பிரித்தல் / தொடர் படப்பிடிப்பு |
| People / Faces / Places | நபர்கள் / முகங்கள் / இடங்கள் |
| Slideshow / Print / Map | படக்காட்சி / அச்சிடு / நிலவரைபடம் |
| Catalog / Cache / Clipping | கேட்டலாக் / தற்காலிகச் சேமிப்பு / வெட்டுப்படுதல் |

## Maintenance and checks

Fixed texts: `crates/ui-egui/locales/ta.json` (2,662 entries). Messages with values:
`crates/ui-egui/locales/ta-formats.json` (337 entries). The English keys stay unchanged; Rust checks
the placeholders at build time. Strings not yet in the plain catalog show in English.

```sh
CRAFT_FONTS_DIR=../craft-fonts cargo test -p lightcraft-ui-egui i18n::tests -- --nocapture
LIGHTCRAFT_LANGUAGE=ta lightcraft-cli snapshot --demo -o tamil.png --size 1600x1000
```

The tests check placeholders, switching language by menu and control channel, saving, the `ta`
locale tags, dates, count wording, and, with the craft-fonts Tamil face, that every letter of the
plain catalog paints. Messages Tamil lacks show in English; `i18n::tests::translation_gaps_report` lists
them. More notes are in [localization.md](localization.md).
