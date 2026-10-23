# Localization parity

> **Last reviewed:** 2026-10-13 · **Last updated:** 2026-10-20 · **Change:** minor (German form of address: *du*; Indonesian and Finnish interface catalogs added; Italian: every interface string translated, native-speaker corrections; Vietnamese shipped; previously minor: de, es, fr, ja, uk catalogs filled; re-measured; Italian and Georgian interface catalogs added) · **Target:** Adobe Lightroom Classic 15.5 / Lightroom 9.6

How far each interface language is from `full`. How to add or maintain a language is in
[localization.md](localization.md); per-language notes are in `localization-<code>.md`
([zh-hans](localization-zh-hans.md), [zh-hant](localization-zh-hant.md), [ja](localization-ja.md),
[pt-br](localization-pt-br.md), [es](localization-es.md), [de](localization-de.md),
[ru](localization-ru.md), [fi](localization-fi.md), [ka](localization-ka.md), [id](localization-id.md), [vi](localization-vi.md)).

## The target

Lightroom 9.6 (installed on the owner's Mac) ships 16 interface languages, listed from its bundle's
`.lproj` folder names: English, German, Spanish, French, Italian, Japanese, Korean, Norwegian Bokmål,
Dutch, Polish, Portuguese (Brazil), Russian, Swedish, Thai, Simplified Chinese and Traditional
Chinese. It does **not** ship Hindi, Arabic, Indonesian or Vietnamese, so for those LightCraft would
go beyond the target.

## How the numbers are measured

**Measured** (a Python count over `crates/ui-egui/locales/*.json`, 2026-10-13):

- The denominator is every message any catalog knows: the union of the plain catalogs' keys (2,222
  English source strings: menus, commands, controls, panels, dialogs, tooltips, presets, What's New
  lines) plus the 249 messages of the formats catalogs (`*-formats.json`: text with runtime values and
  date patterns), 2,471 messages in all.
- A message counts as translated when the catalog has a non-empty value that differs from the English
  key. Some identical values are correct (`ISO`, `OK` in German), so the figure slightly understates
  coverage; on the other hand, strings that never reach a catalog (technical errors raised in lower
  crates, some `format!` text outside `tr_format!`) are not in the denominator at all. That is why no
  language is `full` even at 99%.
- The UI has 563 distinct `tr("…")` literals and 198 `tr_format!` messages in `crates/ui-egui` and
  `apps/`; command, control, menu and preset labels reach the catalogs through `display_label`.

Script support is checked against the code: `language_table!` in `crates/ui-egui/src/i18n.rs` lists
scripts `Latn`, `Cyrl`, `Hans`, `Hant`, `Jpan` only. The UI is egui, which has no bidirectional layout
and no complex-script shaping, so Arabic (RTL, joining) and Devanagari (conjuncts, reordering) cannot
render correctly today.

Native-speaker review: no catalog has had a formal review. Several catalogs have edits from community
contributors whose commits only touch that language (ja, zh-hans, zh-hant, fr, de, ru, uk, pt-br);
that is promising but is not recorded as review, so the column says `no (community edits)`.

Form of address: German speaks to the user with *du* ("Wähle ein Foto", "Dein Name"), never *Sie*.
LightCraft is built by its community, and *du* is how people helping each other talk. A new German
string follows that.

## The twelve key languages

| # | Language | Code | UI strings translated | Dialogs / tooltips / help | Script support | Native review | Status | To `full` |
|---|---|---|---:|---|---|---|---|---:|
| 1 | English | `en` | 2,304 / 2,304 (100%, source) | all; Help ▸ What's New and the shortcut sheet; no full user manual | Latin (Inter) | n/a | **full** | 0 h |
| 2 | Simplified Chinese | `zh-hans` | 1,994 / 2,304 (87%) | menus, panels, dialogs, presets, profiles, dates; lower-layer errors English | Hans via craft-fonts (Noto Sans CJK SC); IME through winit; no vertical text in UI (not in target either) | no (community edits) | partial | 6–10 h |
| 3 | Spanish | `es` | 2,362 / 2,398 (98%; 2026-10-12) | as zh-hans | Latin | no (community edits) | partial | 2–4 h (native review, lower-layer errors) |
| 4 | Hindi | `hi` | 0 (0%) | none | **Devanagari shaping missing** (egui has no complex shaping) | no | none (target: not shipped) | 50–90 h (shaping 25–50 h, shared with Arabic) |
| 5 | Arabic | `ar` | 0 (0%) | none | **RTL layout and Arabic shaping missing** | no | none (target: not shipped) | 60–110 h (RTL mirroring 30–50 h + shaping, shared with Hindi) |
| 6 | French | `fr` | 2,315 / 2,398 (97%; 2026-10-12, the rest are words spelled the same) | as zh-hans | Latin | no (community edits) | partial | 2–4 h (native review, lower-layer errors) |
| 7 | Portuguese (Brazil) | `pt-br` | 1,966 / 2,304 (85%) | as zh-hans | Latin | no (community edits) | partial | 6–10 h |
| 8 | Indonesian | `id` | 2,555 / 2,555 (100%; 2026-10-19: every plain and formats message any catalog has, plus 69 command labels; What's New left in English) | as zh-hans | Latin (Inter covers it) | no (written with an agent; native-speaker pass pending) | partial (target: not shipped) | 2–4 h (native review, lower-layer errors) |
| 9 | Japanese | `ja` | 2,383 / 2,398 (99%; 2026-10-12) | as zh-hans; vertical export watermarks | Jpan via craft-fonts (BIZ UDPGothic / UDMincho); IME through winit | no (community edits) | partial | 2–4 h (native review, lower-layer errors) |
| 10 | German | `de` | 3,644 / 3,754 (97%; 2026-10-16: every interface message and every What's New line except 59: the distinct What's New lines whose English text has a ▸ menu path have no German entry yet and wait for #2016; the rest are words spelled the same) | as zh-hans; What's New translated line by line | Latin | no (community edits) | partial | 2–4 h (native review, lower-layer errors) |
| 11 | Korean | `ko` | 0 (0%) | none | **no Korean (Hangul) face**: needs a non-Adobe OFL face in craft-fonts (Noto CJK is allowed only for Chinese) | no | none | 14–22 h |
| 12 | Vietnamese | `vi` | 2,471 / 2,471 (100% key coverage of current union; intentional English like `ISO`/`OK` kept) | menus, panels, dialogs, presets, profiles, dates, formats; lower-layer errors English | Latin with stacked diacritics (Inter) | no (community contribution) | partial (catalog complete; lower-layer errors English) | 1–3 h |

## Other languages LightCraft ships (4)

| Language | Code | UI strings translated | Script | Native review | Status | To `full` |
|---|---|---:|---|---|---|---:|
| Traditional Chinese (Taiwan) | `zh-hant` | 1,993 / 2,304 (87%) | Hant; uses the Simplified Chinese face until craft-fonts has a TC face | no (community edits) | partial | 8–14 h (incl. the font) |
| Russian | `ru` | 1,704 / 2,304 (74%) | Cyrillic (Inter) | no (community edits) | partial | 8–12 h |
| Ukrainian | `uk` | 2,383 / 2,398 (99%; 2026-10-12) | Cyrillic (Inter) | no (community edits) | partial (lower-layer errors English) | 2–4 h |
| Italian | `it` | 2,628 / 3,278 (80%, measured 2026-10-15; every interface string is translated, the other 650 are What's New lines, left in English) | Latin (Inter) | partly (written with an agent; corrections and the 2026-10-15 additions by a native speaker) | partial (lower-layer errors English) | 3–5 h |
| Georgian | `ka` | 2,398 / 2,398 (100%: every plain and formats message any catalog has on 2026-10-12, plus the 27 display labels and 397 What's New lines no other catalog translates) | Geor: no craft-fonts face yet (boxes until one is available) | yes (native speaker) | partial (lower-layer errors English) | 2–4 h (+ a Georgian face) |
| Finnish | `fi` | 2,610 / 3,308 (79%, measured 2026-10-17: 2,313 plain + 297 formats; the rest is What's New) | Latin (Inter) | yes (written with an agent; native-speaker review by @roopeg) | partial (lower-layer errors English) | 3–5 h |

Target languages LightCraft lacks: Korean, Dutch, Polish, Swedish, Norwegian Bokmål, Thai
(Thai needs shaping and line breaking without spaces). Each Latin-script one is ~10–16 h; Thai
~20–35 h on top of the shaping work.

## Shared work before any language is `full` (counted once, ~20–35 h)

- Route lower-layer error messages (catalog, raw, codecs, engine) through translatable message ids
  instead of English `format!` strings (~12–20 h). Translating them must not change what the app does:
  since #1046 the interface decides on an error's kind (`lightcraft_engine::ErrorKind`, a download's or
  Apple Photos import's `cancelled` flag), not its words; new decisions on an error go through a kind too.
- A check in `cargo xtask ci` that every user-visible literal reaches a catalog (~4–8 h).
- A user manual / help pages beyond What's New and the shortcut sheet (Lightroom links to online
  help; ~4–8 h for a translatable in-app help index).
- Native-speaker review passes (human; cannot be done by agents).

## Totals

- Bring the 10 shipped non-English catalogs to `full`: ~55–90 h (incl. the shared work).
- Add the 5 missing key languages: ~145–255 h, most of it RTL and complex shaping in egui.
- Reach the target's 16 languages: + ~50–85 h (Dutch, Polish, Swedish, Norwegian, Thai).

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-19 | minor | Indonesian interface catalog (`id`): every plain and formats message any catalog has, plus 69 command labels; What's New left in English |
| 2026-10-17 | minor | Finnish interface catalog (`fi`): 2,610 of 3,308 messages (2,313 plain + 297 formats), What's New left in English |
| 2026-10-15 | trivial | Italian (`it`): 339 interface messages and 60 format strings that were missing or still in English translated (2,628 of 3,278; only What's New left in English); native-speaker corrections: Exposure → *Esposizione*, Sharpening → *Nitidezza*, Preset → *Preset*, blurry → *sfocate* |
| 2026-10-13 | minor | Remeasured denominator (2,222 plain + 249 formats = 2,471); Vietnamese catalog complete at 2,471/2,471 (status partial: lower-layer errors still English) |
| 2026-10-12 | minor | Italian interface catalog (`it`): 2,201 of 2,398 messages, What's New left in English |
| 2026-10-11 | minor | Shared work: the interface decides on error kinds, not English error text (#1046) |
| 2026-10-12 | minor | Georgian (`ka`) row: every catalog message |
| 2026-10-10 | major | Created: per-language counts measured from the catalogs, the twelve key languages, script support, estimates |
| 2026-10-12 | minor | de, es, fr, ja and uk catalogs filled to every message another catalog knows (agent translations, native review welcome); those rows re-measured against the current 2,398 messages. Other rows keep their 2026-10-10 counts (open PRs for pt-BR, ru, zh) |
| 2026-10-16 | minor | German (`de`): 217 interface strings and 862 What's New lines added (agent translations, native review welcome); re-measured 2,572 → 3,644 of 3,754 messages (80% → 97%). The denominator grew because German now translates What's New lines no other catalog has; other rows keep their earlier counts |
| 2026-10-20 | minor | German (`de`) speaks to the user with *du*; the 11 messages that used *Sie* changed, no count changes |
