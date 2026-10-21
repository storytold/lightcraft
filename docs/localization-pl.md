# Polski

Polski (`pl`, nazwa angielska **Polish**, endonim **Polski**) włączysz w **Edytuj → Język → Polski**
albo w **Ustawienia → Ogólne → Język**. Zmiana działa od razu i zapisuje się na kolejne uruchomienia.
Polecenie to `app.language.polish`; kanał sterujący przyjmuje `ui.set {"language":"pl"}` oraz
znaczniki `pl-PL` i `pl_PL.UTF-8`. Przy pracy bez okna ustaw `LIGHTCRAFT_LANGUAGE=pl`.

Katalogi `crates/ui-egui/locales/pl.json` i `pl-formats.json` powstały z pomocą AI, całymi frazami
według znaczenia angielskich tekstów źródłowych. Native speaker ani zawodowy tłumacz jeszcze ich
nie korygował.

## Konwencje

- Menu: Plik, Edytuj, Widok, Zdjęcie, Okno, Pomoc; ścieżki menu zapisujemy ze strzałką `→`.
- Library = Biblioteka, Develop = Wywoływanie, Preset = Ustawienie wstępne, Smart Album = Inteligentny album.
- Clarity = Przejrzystość, Dehaze = Usuń zamglenie, Highlights = Światła, Shadows = Cienie,
  Whites = Biele, Blacks = Czernie, Soft Proofing = Próba koloru.
- Stack = Stos, Filmstrip = Pasek zdjęć, Sidecar = plik pomocniczy, Keyword = słowo kluczowe.
- Slideshow = Pokaz slajdów, Contact Sheet = Arkusz kontaktowy, Watermark = Znak wodny.
- Nazwy własne i skróty (HDR, SDR, PQ, HLG, ICC, XMP, PhotoCraft, Display P3) zostają bez zmian.
- Liczebniki: mechanizm rozróżnia tylko liczbę pojedynczą i mnogą, więc zamiast odmiany używamy
  form neutralnych („Zdjęcia: 22”, „Dodano do Moich zdjęć: 3”). Angielska końcówka `"s"` jest
  zużywana przez `{:.0}`.
- Cudzysłowy to `„…”`, które zawiera dołączona czcionka Inter.

## Ostatnia zmiana

Uzupełniono brakujące wpisy: pokaz slajdów (układ, nakładki, ekrany tytułowe, szablony, eksport),
punkty ostrości, edycja zewnętrzna i PhotoCraft, profil ekranu, HDR/SDR, słowa kluczowe, przenoszenie
zdjęć między folderami oraz komunikaty z formatami (`pl-formats.json`). Technicznie niezmienione
teksty (`OK`, `Histogram`, `Display P3`…) i nazwy w języku japońskim/chińskim zostają jak w źródle.

Weryfikacja: `cargo test -p lightcraft-ui-egui i18n::tests`. Zasady dodawania języków:
[localization.md](localization.md).
