# Polski

Język polski (`pl`, English name **Polish**, nazwa natywna **Polski**) włącza się przez **Edytuj → Język →
Polski** albo **Ustawienia → Ogólne → Język**. Zmiana działa od razu i jest zapamiętywana przy kolejnym
uruchomieniu. Polecenie to `app.language.polish`; kanał sterowania przyjmuje `ui.set {"language":"pl"}`
oraz znaczniki `pl-PL` i `pl_PL.UTF-8`. Bez okna można ustawić `LIGHTCRAFT_LANGUAGE=pl`.

Katalogi `crates/ui-egui/locales/pl.json` i `pl-formats.json` dodał #1100 (tłumaczenie przygotowane
z pomocą AI). Uzupełnienie opisane niżej również przygotowano z pomocą AI; przygotował je rodzimy
użytkownik języka polskiego (@lewandowskit). Profesjonalny tłumacz nie robił jeszcze korekty.

## Konwencje

- Menu: Plik, Edytuj, Widok, Zdjęcie, Okno, Pomoc.
- Library = Biblioteka, Develop = Wywoływanie, Preset = Ustawienie wstępne, Stack = Stos,
  Masking = Maskowanie, Virtual Copy = Kopia wirtualna, Filmstrip = Pasek zdjęć.
- Clarity = Przejrzystość, Dehaze = Usuń zamglenie, Vibrance = Dynamika, Saturation = Nasycenie.
- Highlights = Światła, Shadows = Cienie, Whites = Biele, Blacks = Czernie.
- Cudzysłowy polskie: „…”. Ścieżki menu w tekstach: `Edytuj → Cofnij`.
- Data ma postać `20.09.2026` (dzień.miesiąc.rok), nagłówek dnia `Niedziela, 20.09.2026`,
  miesiąca `09.2026`; czas 24-godzinny (`16:04:05`).
- **Liczby i rzeczowniki.** Polski ma trzy formy liczby mnogiej (1 zdjęcie, 2 zdjęcia, 5 zdjęć),
  a mechanizm katalogów rozróżnia tylko „jeden” i „wiele”. Dlatego liczba stoi po rzeczowniku:
  „Zaimportowane zdjęcia: 22”, „Usuń zdjęcia (3)”, „Oczekujące zdjęcia: 2”. Taka forma jest
  poprawna dla każdej liczby. Angielska końcówka liczby mnogiej jest pochłaniana przez `{:.0}`
  (jak w [localization.md](localization.md)). Po przyimku z dopełniaczem („z 10 zdjęć”, „dla 3
  zaznaczonych zdjęć”) liczba może stać przed rzeczownikiem, bo forma jest wtedy zawsze ta sama.
- Komunikaty `{n} {noun}` (np. w panelu Odszumianie AI) mają postać `{noun}: {n}`, a obie formy
  rzeczownika (`photo waiting` / `photos waiting`) tłumaczą się tak samo.
- Font Inter dołączony do aplikacji zawiera wszystkie polskie litery i cudzysłowy; inny font
  nie jest potrzebny.

## Uzupełnienie (2026-10-15)

- Przetłumaczono 40 komunikatów `pl-formats.json`, które były jeszcze po angielsku (ustawienia
  efektów masek, szablony pokazu slajdów, import katalogu Lightroom, słowa kluczowe, osoby,
  scalanie panoram, komunikat o nieoczekiwanym zakończeniu programu) i 2, w których zmieniono
  tylko strzałkę.
- Poprawiono 25 komunikatów, w których liczba stała przed rzeczownikiem w dopełniaczu i dla
  2–4, 22–24… dawała błędną formę („Usuń 2 zdjęć”), oraz `{} failed`, gdzie `{}` jest nazwą
  zadania, a nie liczbą.
- Daty: `{month} {year}` i `{weekday}, {day} {month} {year}` mają postać dzień.miesiąc.rok
  zamiast „20 9 2026”.

Katalog ma 2 159 zwykłych komunikatów i 291 szablonów formatów. Komunikaty, których nie ma w
katalogu, oraz techniczne błędy niższych warstw wyświetlają się po angielsku.

## Weryfikacja

Test `i18n::tests::polish_switches_formats_dates_and_reads_every_count` sprawdza przełączanie
z menu i kanału sterowania, zapamiętanie języka, znaczniki `pl-*`, daty, czas, liczby 0–111
(w tym 2–4 i 22–25) w liczonych komunikatach, nazwy użytkownika pozostawione bez zmian oraz
polskie znaki w fontach interfejsu. Zgodność placeholderów sprawdza
`catalogs_agree_on_placeholders_and_report_gaps`.

Postęp dodawania języków opisuje [localization.md](localization.md).
