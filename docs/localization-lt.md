# Lietuvių

Lietuvių kalba (`lt`, angliškai **Lithuanian**, savivardis **Lietuvių**) įjungiama per **Redaguoti → Kalba → Lietuvių** arba **Nustatymai → Bendrieji → Kalba**. Pakeitimas įsigalioja iškart ir išsaugomas kitam paleidimui. Komanda yra `app.language.lithuanian`; valdymo kanalas priima `ui.set {"language":"lt"}` bei žymes `lt-LT` ir `lt_LT.UTF-8`. Be lango galima nustatyti `LIGHTCRAFT_LANGUAGE=lt`.

## Konvencijos

- Meniu: Failas, Redaguoti, Rodymas, Nuotrauka, Langas, Pagalba.
- Library = Biblioteka, Develop = Ryškinimas, Preset = Parinktis, Stack = Rietuvė, Masking = Kaukės, Virtual Copy = Virtuali kopija, Filmstrip = Nuotraukų juosta.
- Clarity = Aiškumas, Dehaze = Rūko šalinimas, Vibrance = Gyvumas, Saturation = Sodrumas.
- Highlights = Šviesūs tonai, Shadows = Šešėliai, Whites = Balta, Blacks = Juoda.
- Lietuviškos kabutės: „…“. Meniu keliai tekstuose: `Redaguoti → Atšaukti`.
- Data pateikiama formatu `2026-09-20` (metai-mėnuo-diena), dienos antraštė `Sekmadienis, 2026-09-20`, mėnesio `2026-09`; laikas 24 valandų (`16:04:05`).
- **Skaičiai ir daiktavardžiai.** Lietuvių kalba turi tris skaičiaus formas (1 nuotrauka, 2 nuotraukos, 10 nuotraukų), o katalogų mechanizmas skiria tik vienaskaitą ir daugiskaitą. Todėl skaičius rašomas po dvitaškio arba skliaustuose („Pridėta nuotraukų: 22“, „Ištrinti nuotraukas (3)“), o angliška daugiskaitos galūnė sugeriama per `{:.0}` (kaip aprašyta [localization.md](localization.md)).
- Programoje esantis šriftas „Inter“ turi visas lietuviškas raides (`ąčęėįšųūžĄČĘĖĮŠŲŪŽ`) ir lietuviškas kabutes; papildomo šrifto nereikia.

## Patikra

Testai `i18n::tests::languages_switch_persist_and_keep_user_values` ir `i18n::tests::lithuanian_switches_formats_dates_and_reads_every_count` tikrina perjungimą per meniu ir valdymo kanalą, kalbos išsaugojimą, žymes `lt-*`, datas, laiką, skaičius ir lietuviškus simbolius sąsajos šriftuose.
