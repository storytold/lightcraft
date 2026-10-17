# Suomi

Suomen (`fi`, englanniksi **Finnish**, omakielinen nimi **Suomi**) saa käyttöön valitsemalla
**Muokkaa → Kieli → Suomi** tai **Asetukset → Yleiset → Kieli**. Muutos tulee voimaan heti ja
tallentuu seuraavaa käynnistystä varten. Komento on `app.language.finnish`; ohjauskanava
hyväksyy `ui.set {"language":"fi"}` sekä tunnisteet `fi-FI` ja `fi_FI.UTF-8`. Ikkunattomassa
ajossa voi käyttää `LIGHTCRAFT_LANGUAGE=fi`.

Luettelot `crates/ui-egui/locales/fi.json` ja `fi-formats.json` on laadittu tekoälyavusteisesti
(Claude) englanninkielisten lähdemerkkijonojen merkityksestä, kokonaisina fraaseina. Avainlista
on kaikkien muiden luetteloiden yhdiste. Suomenkielinen äidinkielinen puhuja (@roopeg) tarkistaa
käännöksen; ammattikääntäjä ei ole sitä käynyt läpi.

## Käytännöt ja laajuus

- Valikot: Tiedosto, Muokkaa, Näytä, Kuva, Ikkuna, Ohje.
- Library = Kirjasto, Develop = Kehitys, Collection = Kokoelma, Album = Albumi,
  Preset = Esiasetus, Catalog = Katalogi (Lightroom-katalogi; keyword and model lists stay
  ”luettelo”), Snapshot = Tilannevedos, Version = Versio, Print (module) = Tulostus, the
  command Print… = Tulosta….
- Exposure = Valotus, Contrast = Kontrasti, Clarity = Selkeys, Texture = Tekstuuri,
  Dehaze = Sumunpoisto, Vibrance = Eloisuus, Saturation = Värikylläisyys, Hue = Sävy,
  Tint = Värisävy, Temperature = Lämpötila, White Balance = Valkotasapaino.
- Highlights = Huippuvalot, Shadows = Varjot, Whites = Valkoiset, Blacks = Mustat.
- Sharpening = Terävöinti, Noise Reduction = Kohinanpoisto, Grain = Rakeisuus,
  Vignette = Vinjetti, Color Grading = Väriluokittelu, Tone Curve = Sävykäyrä,
  Defringe = Värivirheen korjaus, Feather = Häivytys.
- Crop = Rajaus, Straighten = Suorista, Mask = Maski, Brush = Sivellin,
  Radial = Säteittäinen, Linear Gradient = Lineaarinen liukuväri,
  Soft Proofing = Vedoksen esikatselu.
- Spot removal: Spot = Täplä, Heal = Paranna, Clone = Kloonaa. Point Color = Pistevärit and
  Point Curve = Pistekäyrä keep ”piste”.
- Flag = Lippu, Pick = Valitse (picked photo = Valittu kuva), Reject = Hylätty, Rating = Luokitus (Star rating =
  Tähtiluokitus), Label = Merkintä, Color Label = Värimerkintä, Keyword = Avainsana,
  Smart Album = Älyalbumi, Filmstrip = Filminauha, Virtual Copy = Virtuaalikopio,
  Stack = Pino, Template = Mallipohja (AI Model = Malli), Detection = Havaitseminen, Recently Deleted = Äskettäin poistetut, My Photos = Omat kuvat.
- Päivämäärä on muodossa `20.9.2026` (ilman etunollia), viikonpäivä alkaa pienellä kirjaimella
  (`sunnuntai 20.9.2026`), kuukausi–vuosi on `9/2026` ja kellonaika 24-tuntinen `16:04:05`.
- Yksiköt: `kt`, `Mt`, `Gt`; prosentin edessä on väli (`50 %`).
- Määrät kirjoitetaan sanan jälkeen (”Tuotuja kuvia: 22”, ”Odottavia kuvia: 2”), koska
  suomessa lukusanan jälkeinen substantiivi on yksikön partitiivissa (1 kuva, 2 kuvaa), eikä
  englannin monikkopääte `s` voi valita muotoa. Englannin pääte kulutetaan `{:.0}`-merkinnällä.
  Kun lause taipuu yksikössä kaikille luvuille (”{n} kuvalta”, ”{n} kuvassa”), luku voi olla
  lauseen sisällä.
- Mukana toimitettu Inter-fontti sisältää ä, ö ja å sekä lainausmerkit ” ”. Lisäfonttia ei
  tarvita.

Mitä uutta -ikkunan versiotiedotteet jäävät englanniksi, kuten useimmissa luetteloissa. Alemman
tason tekniset virheilmoitukset ja osa käännöskerroksen ulkopuolisista viesteistä jäävät
englanniksi. Käännös ei muuta kirjastotietoja eikä englanninkielisiä lähdemerkkijonoja.

## Testaus

`i18n::tests` tarkistaa komennon rekisteröinnin, vaihdon valikosta ja ohjauskanavasta,
kielen tallennuksen, luetteloiden kattavuuden, paikkamerkit, määrämuodot, päivämäärän, ajan ja
suomen kirjaimet. Rust tarkistaa muotoilumallit käännösvaiheessa.

Kielentarkistuksessa kannattaa katsoa erityisesti: ”Väriluokittelu” (Color Grading),
”Valoalueet / Valkoiset” (Highlights / Whites), ”Näyttövedos” (Soft Proofing),
”Älyalbumi”, ”Pino” (Stack), ”Pistevärit” (Point Color), ”Tekoälykohinanpoisto” sekä
luovien esiasetusten nimet. Kielen lisäämisen ohje on tiedostossa
[localization.md](localization.md).
