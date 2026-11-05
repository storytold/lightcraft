# LightCraft in Indonesian

Choose **Edit → Language → Bahasa Indonesia** or **Settings → General → Language**. The choice applies
at once and is saved for the next launch (`language: "id"` in `ui.json`); the control command is
`app.language.indonesian`. `LIGHTCRAFT_LANGUAGE` selects it for headless runs and accepts `id`, `id-ID`
and `id_ID.UTF-8`; the saved value is always `id`. LightCraft does not follow the system language, for
Indonesian as for every other language.

The Indonesian catalogs cover every plain and formats message the other catalogs have (2,258 + 297),
plus 69 command labels no catalog translated yet: menus, shortcuts, library, editing, crop, masks,
people, import/export, print, slideshow, settings, built-in presets and profiles, history, date
headings, status messages and the smart-album rule editor. The embedded release notes (What's New)
stay in English. Technical errors from lower layers stay in English, as in the other languages.
The catalogs were written with an agent from the meaning of the English labels and reviewed for
natural, standard app Indonesian; 356 labels reuse the Indonesian wording of the sibling apps
(PhotoCraft, VectorCraft, FilmCraft, FigCraft) so the suite reads the same. A native speaker's pass
is welcome.

File names, command ids, template placeholders and albums, presets or metadata you named yourself are
not translated.

## Style

- Sentence case everywhere, as in the English; names, brands and abbreviations keep their capitals.
- Buttons and menu commands use the bare verb (Simpan, Hapus, Ekspor, Pangkas); progress lines use
  me- with an ellipsis (Mengimpor…); finished-state toasts put the subject first (`{n} foto diekspor`).
- The user is addressed as *Anda*, only where a sentence needs a subject; never *kamu*.
- Indonesian keeps a noun unchanged after a count (`3 foto`), so English plural endings are dropped
  with `{:.0}`.
- Dates read `Minggu, 20/09/2026` (`{weekday}, {day:02}/{month:02}/{year}`), months `09/2026`.
- Delete is **Hapus**; remove from a container (album, stack, favorites) is **Keluarkan dari …**.
  Select is **Pilih**; the Pick flag is **Pilihan**.
- `▸` and `▾` are written `→` and `⌄`, as in the other catalogs.

## Terminology

| Source | Indonesian |
|---|---|
| Develop | Olah |
| File / Edit / View | File / Edit / Tampilan |
| Settings / General / Language | Pengaturan / Umum / Bahasa |
| Import / Export | Impor / Ekspor |
| Photos / Albums / Folders | Foto / Album / Folder |
| Smart Album | Album pintar |
| Crop | Pangkas |
| Masks / Masking | Masker |
| Exposure / Contrast | Eksposur / Kontras |
| Highlights / Shadows / Whites / Blacks | Sorotan / Bayangan / Putih / Hitam |
| Temperature / Tint | Suhu / Rona |
| Vibrance / Saturation | Vibrance / Saturasi |
| Clarity / Texture / Dehaze | Kejernihan / Tekstur / Hilangkan kabut |
| Sharpening / Noise Reduction | Penajaman / Pengurangan noise |
| Vignette / Grain | Vinyet / Butiran |
| White Balance / Tone Curve | Keseimbangan putih / Kurva tone |
| Lens Corrections | Koreksi lensa |
| Presets / Profile | Preset / Profil |
| Keywords / Rating | Kata kunci / Rating |
| Flag / Pick / Reject | Tanda / Pilihan / Tolak |
| Stack / Virtual Copy | Tumpukan / Salinan virtual |
| History / Recently Deleted | Riwayat / Baru dihapus |
| Compare / Before / After | Bandingkan / Sebelum / Sesudah |
| Slideshow / Print / Template | Tayangan slide / Cetak / Templat |
| People | Orang |
