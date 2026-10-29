# LightCraft auf Deutsch

Wähle **Bearbeiten → Sprache → Deutsch** oder **Einstellungen → Allgemein → Sprache**.
Die Auswahl gilt sofort und wird für den nächsten Start gespeichert (`language: "de"` in `ui.json`).

Die deutschen Kataloge decken alle Schlüssel der bisherigen Übersetzungen sowie sämtliche
Befehls- und Reglerbezeichnungen ab: Menüs, Tastenkürzel, Fotoverwaltung, Bearbeitung,
Zuschnitt, Masken, Personen, Import/Export, Einstellungen, eingebaute Vorgaben und Profile,
Verlauf, Versionen, Datumsüberschriften und Statusmeldungen. Auch der Regel-Editor für
Smart-Alben, Platzhalterhilfen und der Dialog bei ungespeicherten Änderungen sind angebunden.
Die eingebetteten Versionshinweise sind ebenfalls übersetzt. Technische Fehler aus tieferen
Schichten bleiben wie bei den bestehenden Sprachen auf Englisch.

Dateinamen, Befehls-IDs, Vorlagenplatzhalter und selbst benannte Alben, Vorgaben oder Metadaten
werden nicht übersetzt. Die vorhandenen Inter-Schriften enthalten die deutschen Zeichen;
für Deutsch ist kein zusätzlicher Font-Download nötig. Beschreibungen für SAM 3 werden
unverändert an das Modell übergeben; die Beispiele verwenden dessen englische Begriffe.

## Begriffe

Eine Durchsicht aller Oberflächentexte und Meldungen (2026-10-24, mit einem Agenten erstellt und
von einem Muttersprachler stichprobenartig geprüft; die Versionshinweise nicht) hat diese Begriffe
festgelegt. Neue Texte verwenden sie ebenfalls:

| Englisch | Deutsch | Anmerkung |
|---|---|---|
| Panel | Bedienfeld | wie in Lightroom; **Bereich** bleibt für Abschnitte (Sections) der Seitenleiste |
| Sidecar | Begleitdatei (XMP-Begleitdatei) | |
| Slide | Dia | passend zu „Diashow“ |
| Crop | Zuschnitt, zuschneiden | |
| Flag / Pick / Reject | Markierung / ausgewählt / abgelehnt | |
| Label (Color Label) | Farbmarkierung | nie nur „Markierung“, das ist die Ausgewählt/Abgelehnt-Kennzeichnung |
| Tint | Tönung | wie in Lightroom; „Tonung“ nur für Tonungsvorgaben wie Sepiatonung |
| Clipping | Beschneidung | wie Lightroom („Lichter-/Tiefenbeschneidung“); „Beschnitt“ ist ein Druckbegriff |
| Shutter Speed | Verschlusszeit | |
| Upright | Perspektivkorrektur | |
| Smart Album | Smart-Album | |
| Activity stack | Aktivitätsstapel | |

Typografie: Gedankenstrich mit Leerzeichen als Halbgeviertstrich (` – `), Auslassungspunkte
ohne Leerzeichen direkt am Wort (`Importieren…`), deutsche Anführungszeichen („…“). „Ihre“
nur, wo es eindeutig „von ihnen“ heißt; die Anrede ist *du*, ein Satz mit „Ihre Fotos“ am
Anfang liest sich sonst wie die Sie-Form.

## Wartung und Prüfung

Feste Texte: `crates/ui-egui/locales/de.json`. Meldungen mit Werten:
`crates/ui-egui/locales/de-formats.json`. Die englischen Schlüssel bleiben unverändert;
Platzhalter werden beim Build durch Rust geprüft. Englische Pluralendungen werden dort,
wo sie nicht passen, mit `{:.0}` ausgeblendet; mengenabhängige Meldungen verwenden neutrale
Formulierungen. Das Datum erscheint beispielsweise als `Sonntag, 20.09.2026`.

```sh
cargo test -p lightcraft-ui-egui i18n::tests
LIGHTCRAFT_LANGUAGE=de lightcraft-cli snapshot --demo -o deutsch.png --size 1600x1000
```

Die Tests prüfen Platzhalter, Sprachwechsel per Menü und Steuerkanal, Speicherung,
Datumsformate, unveränderte Benutzernamen und gerenderte Texte sowie Umlaute und ß. Fehlende
Übersetzungen erscheinen auf Englisch; `i18n::tests::translation_gaps_report` listet sie auf. Ein weiterer Render-Test prüft die Schaltflächenbreiten; der Importtest prüft
auch den Sprachwechsel nach der Hintergrundprüfung der Quellen. Akzeptierte Sprachangaben umfassen `de`, `de-DE`, `de_AT.UTF-8` und `de-CH`;
gespeichert wird stets `de`. Weitere Hinweise stehen in [localization.md](localization.md).
