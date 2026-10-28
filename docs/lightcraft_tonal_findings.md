# LightCraft – Tonwertverarbeitung: Erkenntnisse und nächste Schritte

**Stand:** 28. Oktober 2026
**Projekt:** `storytold/lightcraft`, lokaler Entwicklungsbranch `feature/improve-tonal-range`
**Zweck:** Übergabe an Codex CLI. Beobachtungen, Hypothesen und Tests sind bewusst getrennt.

## 1. Ziel

Die Regler **Lichter, Tiefen, Weiß, Schwarz** und die neutrale RAW-Tonwertabbildung sollen präziser und vielseitiger werden. Lightroom dient als Qualitätsreferenz, nicht als zwingend pixelgenau zu reproduzierendes Ziel. Insbesondere dürfen Verbesserungen an extremen Gegenlichtszenen nicht zu schlechteren, sanften Tonwertübergängen bei Porträts führen.

## 2. Vorhandene Arbeit / Git-Stand prüfen

Es wurden mindestens zwei Zwischenstände committed:

1. Anpassung der Tonwertregler in `crates/pipeline/src/finish.rs` und `crates/pipeline/src/tone.rs`.
2. Anpassung der neutralen Kameratonkurve in `tone.rs` (gezielter EV-Lift im Schattenbereich).

**Nicht blind auf dieses Dokument vertrauen:** Zuerst `git status`, `git log -5 --oneline`, `git diff`, Implementierung und tatsächlichen Rendering-Pfad prüfen. Einige experimentelle Änderungen (separater lokaler Kontrast, Debug-Modi) wurden wieder zurückgenommen. Welche Variante momentan committed ist, entscheidet ausschließlich der lokale Code.

Wichtige bisher verwendete Codeformen (nur Referenz, am Checkout verifizieren):

```rust
// Schatten in finish.rs
let ws = 1.0 - smooth(-4.8, 0.3, base);
let shadow_weight = ws * (0.35 + 0.65 * (1.0 - ws).sqrt());
let shadow_gate = 1.0 - smooth(-5.5, -1.8, base);
delta += ss * 3.0 * shadow_weight * shadow_gate
    + hh * 2.5 * wh.sqrt();
```

```rust
// Neutrale Tonkurve in ToneMap::camera_hdr(), tone.rs
let shadow_weight =
    smooth(-8.0, -6.0, ev)
    * (1.0 - smooth(-4.0, -3.0, ev));
let corrected_ev = ev + 0.35 * shadow_weight;
let y = curve.apply(GREY * 2f32.powf(corrected_ev));
```

Weitere frühere Varianten: Schwarzwirkung in `tone.rs` wurde von `0.07` auf ungefähr `0.49` verstärkt; Weißregler und Lichter ebenfalls geändert. Das aktuelle Verhalten und die Wirkpfade im Code bestätigen.

## 3. Technische Messungen an der Warschauer Brücke

Ein RAW von der Fujifilm X-T1 wurde als besonders schwieriges Gegenlichtmotiv verwendet: Sonne hinter Brückenseilen, sehr heller Himmel, dunkler Asphalt mit langen Seilschatten.

Die für dieses RAW protokollierte *Kameratonkurve* (neutral, nach vereinfachter sRGB-Kodierung) lieferte ungefähr:

| EV relativ zu Grau | sRGB 8-bit |
|---:|---:|
| -7 | 0.9 |
| -6 | 5.2 |
| -5 | 29.0 |
| -4 | 67.3 |
| -3 | 112.5 |
| -2 | 162.8 |

Der Schwarzfuß komprimiert sehr dunkle Werte stark. Die visuelle EV-Karte zeigte unterschiedliche Eingangsbelichtungen für Asphalt und Seilschatten. Der `l1-base`-Debug-Modus zeigte die Schatten als deutliche lokale Abweichungen; die effektive Maske `shadow_weight*shadow_gate` war über dem Gehweg recht einheitlich (~0.34) und im hellen Himmel nahe null. Dies sind bildspezifische Messungen, **keine** allgemeine Aussage über alle RAW-Dateien.

**Wichtiger Testfallstrick:** Auf dem Mac wechselte zwischenzeitlich die Shell; `LIGHTCRAFT_GPU=0` war nicht mehr gesetzt. Dadurch liefen Änderungen an `finish.rs` teilweise nicht im ausgeführten Rendering-Pfad. Das wurde durch eine gezielte `panic!` im CPU-Pfad bestätigt. Für reproduzierbare CPU-Tests immer explizit starten:

```bash
LIGHTCRAFT_GPU=0 cargo run --release -p lightcraft
```

Weitere Testverfälschung: Einmal wurden alte und neue `delta +=`-Zeilen gleichzeitig verwendet, was die Schattenaufhellung doppelt anwendete. Vor A/B-Tests sicherstellen, dass jede Korrektur genau einmal aktiv ist.

## 4. Neue Vergleichsergebnisse auf mehreren Motiven

### A. Warschauer Brücke / starkes Gegenlicht

Die neue Fassung kann die zuvor sehr dunklen Asphaltflächen deutlich besser öffnen und dabei den hellen Himmel weitgehend schützen. Auch die Trennung von Asphalt und Seilschatten wurde verbessert. Eine manuell/automatisch optimierte LightCraft-Bearbeitung kann einer Lightroom-Referenz nahekommen, verbleibende Unterschiede zeigen sich u. a. in Wolken-/Lichtübergängen.

Die Referenzentwicklung wurde nicht mit denselben numerischen Reglerwerten erzeugt; Pixelähnlichkeit allein beweist daher keine gleiche Reglercharakteristik.

### B. Stadtplatz mit Kopfsteinpflaster / diffuse Tageslichtszene

Bei **identischen Einstellungen** ergab die modifizierte Version sichtbar kräftigere Tonwerttrennung als der unveränderte aktuelle LightCraft-Stand: Fugen, Kleidungsstücke und Fassaden erschienen differenzierter. Diese Tendenz blieb erkennbar, als **Struktur, Klarheit und Dunst entfernen auf 0** gesetzt wurden. Es war außerdem eine Vignette aktiv. Das stützt die Hypothese, dass die geänderte Tonwertverarbeitung – nicht nur die Detailwerkzeuge – zum Unterschied beiträgt.

Aber: Mehr lokaler Kontrast ist nicht grundsätzlich besser; harte Tonwertwirkung muss bei anderen Motiven geprüft werden.

### C. Sanftes Schwarzweiß-Porträt – wichtige Regression

Drei Fassungen, vom Nutzer in dieser Reihenfolge geliefert:

1. **Neutral** (Referenz)
2. **Unsere modifizierte LightCraft-Version**
3. **Unveränderte Originalversion von LightCraft**

**Fotografischer Befund:** Bei diesem sanften Porträt wirkt die Originalversion (3) überzeugender als die modifizierte Fassung (2). In der modifizierten Fassung erscheinen Stirn/Wange und teilweise Hintergrund heller und die sanften Abstufungen bzw. Gesichtsmodellierung flacher. Die Originalversion verankert die Schatten stärker, beispielsweise unter dem Kinn und im Hintergrund, ohne die weiche Lichtwirkung so stark zu beeinträchtigen.

Das ist der entscheidende neue Erkenntnisgewinn: Die bislang für Gegenlicht und Architektur nützlichen Korrekturen können bei Porträts eine Regression darstellen.

**Wichtiger Vorbehalt:** Vor einer kausalen Schlussfolgerung unbedingt sicherstellen, dass Bild 2 und 3 mit exakt denselben Einstellungen, RAW-Profilen, Schwarzweißmischungen, Exportparametern und Rendering-Backends entstanden. Die drei Bildtitel allein beweisen das nicht. *Das neutrale Bild ist keine garantierte identische Reglerreferenz zu beiden Entwicklervarianten.*

### D. Lampe vor dem Himmel / Halo-Verdacht

Bei einem anderen bearbeiteten Vergleich (Lightroom vs. LightCraft) fällt an einer dunklen Lampe vor hellem Himmel ein **heller Halo** in LightCraft auf, während Lightroom dort einen dunkleren Verlauf zeigt. Mögliche Ursachen: lokaler Kontrast, Klarheit, Dunstentfernung, räumliche Glättung, maskierte Operationen oder Reihenfolge der Verarbeitung. Eine Ursache wurde bislang **nicht** nachgewiesen.

Bei einer von Codex erzeugten LightCraft-Nachstellung der Lightroom-Entwicklung waren Klarheit und weitere lokale Einstellungen stark ausgeprägt. Daher nicht automatisch die neue neutrale EV-Korrektur für den Halo verantwortlich machen. Nutzer berichtet außerdem, dass nicht alle Regler innerhalb lokaler Masken so gut funktionieren wie global; diese Pfade sollten separat untersucht werden.

## 5. Prioritäre Hypothese

Ein möglicher Hauptverursacher der Porträt-Regression ist der zusätzliche EV-Lift in `camera_hdr()`:

```rust
let corrected_ev = ev + 0.35 * shadow_weight;
```

Er ist **global tonwertabhängig**, aber nicht motiv- oder materialabhängig: Er kann bei Asphalt sinnvoll und bei Gesicht, Haaren oder Hintergrund nachteilig wirken. Noch nicht bewiesen! Alternativ oder zusätzlich könnte die geänderte Grund-/Tiefenkurve verantwortlich sein.

## 6. Nächste Tests – bitte isoliert und reproduzierbar

1. **Ausgangslage sichern:** Aktuellen Checkout und vorhandene Bilder/Settings identifizieren; CPU-Backend festhalten, gleiche RAW-Datei, gleiche Settings und identische Exporteinstellungen für alle Varianten.
2. **Porträt-A/B als höchste Priorität:** Neue Version einmal mit und einmal ohne *nur* die neutrale `+0.35 EV`-Korrektur rendern. Alle vier Regleranpassungen bleiben unangetastet. Referenz ist zugleich die unveränderte LightCraft-Version; Vergleich bevorzugt aus gleicher Pipeline.
3. **Brücken-A/B mit exakt derselben einzigen Schalteränderung** rendern und Asphalt, Seilschatten, Himmel vergleichen. So wird sichtbar, ob die Änderung tatsächlich eine Verbesserung an einem Motiv gegen einen Nachteil am anderen tauscht.
4. **Kennlinien vergleichen:** Werteverteilung und lokale Kontraste in festen ROIs (bei Porträt: Stirn, Wange, Kinn/Hals, Hintergrund; bei Brücke: Asphalt, Schattenstreifen, Himmel), aber nicht allein über Mittelwerte urteilen. Besonders auf sanfte Übergänge, Clipping, Kanten-Halos und Rauschen achten.
5. **Halo separat isolieren:** In einem weiteren Test den Ausschnitt um den Lampenkopf mit globaler Klarheit, Masken-Klarheit, Dunst entfernen und ggf. lokalen Masken einzeln deaktivieren. Verschiedene Algorithmen/Verarbeitungspfade von globalen und maskierten Einstellungen im Quellcode überprüfen.
6. **Erst nach Ursachenanalyse** Codeänderungen vorschlagen. Keine pauschale, porträtspezifische automatische Maskierung aufgrund dieses einzelnen Bildes.

## 7. Arbeitsweise für Codex CLI

- Zunächst **nur inspizieren**, keine Codeänderungen ohne Auftrag.
- Kleine, einzeln schaltbare Änderungen; bei Bedarf Testflag/Feature-Branch und strukturierte Vergleichsoutputs.
- Nicht die Lightroom-Ästhetik fest kodieren, sondern allgemein robuste fotografische Regler entwickeln.
- Ergebnisse und Unsicherheiten deutlich trennen: *Messung*, *visuelle Beobachtung*, *Hypothese*, *bestätigte Ursache*.
- Mehrere RAW-Motive als Regressionstests verwenden, nicht nur die Brücke.
- Eigenständige lokale Masken und deren Verhalten gegenüber globalen Tonwert-/Klarheitsreglern gesondert prüfen.

## 8. Sofort nutzbarer Codex-Auftrag

> Lies `docs/TONAL_FINDINGS.md` und prüfe den aktuellen Git-Stand sowie die Implementierungen in `crates/pipeline/src/finish.rs` und `crates/pipeline/src/tone.rs`. Finde den neutralen zusätzlichen EV-Lift und untersuche, wie er mit der Basisentwicklung zusammenspielt. Plane einen reproduzierbaren A/B-Test für das sanfte Porträt und die Warschauer Brücke, bei dem ausschließlich dieser Lift umgeschaltet wird. Prüfe außerdem, ob globale und maskierte Klarheit dieselbe Pipeline nutzen. Ändere zunächst keinen produktiven Code; liefere erst einen konkreten Testplan und nenne fehlende Dateien/Settings.
