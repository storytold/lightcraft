# Čeština

Češtinu (`cs`, English name **Czech**, native name **Čeština**) zapnete přes **Úpravy → Jazyk →
Čeština** nebo **Nastavení → Obecné → Jazyk**. Změna platí ihned a ukládá se pro další spuštění.
Příkaz je `app.language.czech`; řídicí kanál přijímá `ui.set {"language":"cs"}` i značky
`cs-CZ` a `cs_CZ.UTF-8`. Pro běh bez okna lze nastavit `LIGHTCRAFT_LANGUAGE=cs`.

Katalogy `crates/ui-egui/locales/cs.json` a `cs-formats.json` vznikly s pomocí AI pro @Losminek,
po celých frázích podle významu anglických zdrojových řetězců. Výchozí seznam klíčů vycházel
z největšího, ukrajinského katalogu; doplnilo se sjednocení ostatních katalogů a položky
používané kódem. Překlad byl porovnán s konvencemi rozhraní aplikace a prošel nezávislou
revizí rozsahu a terminologie. Rodilý mluvčí ani profesionální překladatel jej zatím nekorigoval.

## Konvence a rozsah

- Nabídky: Soubor, Úpravy, Zobrazení, Fotografie, Okno, Nápověda.
- Library = Knihovna, Develop = Vyvolat, Collection = Kolekce, Preset = Přednastavení.
- Flag = Příznak, Pick Flag = Příznak Vybráno, Soft Proofing = Měkký nátisk.
- Clarity = Zřetelnost, Dehaze = Odstranit opar, Vibrance = Živost, Saturation = Sytost.
- Highlights = Světla, Shadows = Stíny, Whites = Bílé, Blacks = Černé.
- Filmstrip = Filmový pás, Virtual Copy = Virtuální kopie, Snapshot = Snímek stavu.
- Datum má podobu `20. 9. 2026`, čas je 24hodinový (`16:04:05`).
- Čísla používají společné formátování aplikace; tento jazyk nepřidává vlastní číselný formatter.
- Počty používají neutrální formulace jako „Importováno fotografií: 22“ nebo
  „Čekající fotografie: 2“. Anglická přípona se podle společných pravidel spotřebuje pomocí
  `{:.0}`. Stávající mechanismus rozlišuje pouze jednotné a množné číslo; samostatné české
  plurálové pravidlo se nepřidává.
- Písmo Inter dodávané s aplikací obsahuje české znaky i uvozovky „…“. Další font není potřeba.

Katalog obsahuje 2 394 běžných položek a 297 formátových šablon. Každý běžný klíč existuje
v jiném katalogu nebo je používán kódem. Oproti německému katalogu má 215 dalších klíčů,
všechny s doloženým použitím: popisky příkazů, menu, ovládacích prvků a nastavení,
texty HDR/exportu a obličejů a kontextové hodnoty Ano/Ne. Z toho 66 klíčů již existuje
v dalších katalozích a 149 doplňuje současné překladové lookupy. Doplnění z 18. 10. 2026
přidalo texty novějších funkcí (prezentace, tisk, přednastavení efektů masek, zaostřovací
body) a prošlo stejnou kontrolou.

Řádkové fragmenty poznámek k vydáním byly odstraněny, včetně 163 fragmentů převzatých
ze sdílených katalogů; text poznámek v dialogu Co je nového používá anglický fallback.
Společné nadpisy používané také v UI (například Library nebo Masking) zůstávají přeložené.

Úplné pokrytí katalogů neznamená úplnou jazykovou paritu: technické chyby nižších vrstev
a některá hlášení mimo překladové rozhraní zůstávají anglicky. Překlad nemění data knihovny
ani anglické zdrojové řetězce. Rozložení panelů a seznamu jazyků je beze změny.

## Ověření a revize

Testy `i18n::tests` ověřují registraci příkazu, přepínání přes nabídku a řídicí kanál,
uložení jazyka, pokrytí katalogů, placeholdery, neutrální formulace počtů, datum, čas
a české glyfy. Překladové šablony kontroluje také Rust při sestavení.

Při jazykové korektuře doporučujeme zkontrolovat „asistované třídění“, „sada“ pro Stack,
„bodová barva“, „měkký nátisk“ a názvy kreativních přednastavení.
Postup přidávání jazyků je v [localization.md](localization.md).

V této změně se doplnilo volání překladu u jednoduchých popisků v panelech osob a obličejů,
volbě souboru modelu a podmínkách jeho instalace, výběru nepojmenovaných obličejů,
pojmenování obličeje v detailu, instrukce tisku PDF a upozornění na PNG PQ,
včetně textů barevného prostoru HDR. Anglické zdrojové texty zůstaly stejné.
