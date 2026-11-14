# Greek (`el`) localization notes

Catalogs: `crates/ui-egui/locales/el.json` (interface) and `el-formats.json` (messages with values).
Written from the plain meaning of the English, with an AI agent, and reviewed by a native speaker.
No other product's translation was consulted. Help ▸ What's New stays in English for now.

## Terms

| English | Greek | Note |
|---|---|---|
| Develop (raw) | εμφάνιση | as in developing film |
| Pick / Reject | επιλεγμένη / απορριμμένη | |
| Flag (verb) | σήμανση, επισημαίνω | the noun is «σημαία» |
| Preset | προκαθορισμένη ρύθμιση | «προεπιλογή» is Default |
| Burst | ριπή | |
| Culling | διαλογή | |
| Stack | στοίβα | |
| Smart preview | έξυπνη προεπισκόπηση | |
| Sidecar (XMP) | συνοδευτικό αρχείο XMP | |
| Fade (slides) | σταδιακή εναλλαγή | |
| Fringe | παρυφή | |
| Aqua (colour mixer) | τυρκουάζ | «κυανό» is Cyan |
| Highlights / Shadows | φωτεινές περιοχές / σκιές | shared with PhotoCraft's Greek |

## Typography and grammar

- Formal address (εσείς) in instructions, verbal nouns for menu items and buttons («Εξαγωγή…», «Δημιουργία άλμπουμ»).
- Greek quotes « », the Greek question mark «;», and a comma rather than the ano teleia «·» between
  clauses; a «·» stays only where the English uses it as a separator.
- Counts: Greek nouns change with the number and formats get no count-based form, so a count reads
  "label: {n}" («Φωτογραφίες που εισήχθησαν: {n}») and the English plural suffix is consumed as `{:.0}`.
- Dates are numeric, day first: «Τρίτη, 28/10/2026», «10/2026».
- Menu paths use →, never ▸ (see the `UNPAINTABLE` test).
