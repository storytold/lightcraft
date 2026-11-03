# Fujifilm in-camera recipes

LightCraft reads Fujifilm MakerNote settings from camera JPEGs, the EXIF in a RAF's
embedded JPEG, and Fujifilm TIFF/DNG files that retain the MakerNote. Info shows a
Film recipe card with Copy recipe. `photo.fujiRecipe {id?}` exposes the same lines
to the CLI, control channel and MCP. All Metadata includes a Fujifilm group.

The card includes the available film simulation, dynamic range, white balance and
shift, grain, highlight/shadow, Color Chrome effects, sharpness, colour, noise
reduction and clarity. Missing tags are omitted; unknown numeric values are labelled
as unknown. Tag names and values follow the public
[ExifTool Fujifilm tag table](https://exiftool.org/TagNames/FujiFilm.html).

FilmMode `0x200` is displayed as Velvia/Vivid. Monochrome, Acros and their filters,
and Sepia, are read from Saturation `0x1003` even when FilmMode `0x1401` is absent.
Synthetic regressions cover those cases and fragmented/interrupted file reads.
Manual dynamic range uses DevelopmentDynamicRange `0x1403` for DR100/DR200/DR400.
D-Range Priority `0x1443` takes precedence over the legacy DR tag: the card shows
Auto with its recorded strength, or the fixed Weak/Strong setting, instead of a
misleading DR100 row. Auto strength prefers `0x1444`, Fixed prefers `0x1445`;
either falls back to the other tag when its own is absent, as in the X-H1 files.
Missing and unknown values are
left explicit. These are metadata labels, not exposure or rendering corrections.
Contributor cbram reports 30 X-S10 RAF comparisons and 378 in-camera conversions in
[the PR discussion](https://github.com/storytold/lightcraft/pull/1209#issuecomment-6542675856);
those files have not been independently tested on this branch.

Local verification on 2026-11-01 checked Lorenzo Fontanella's 44 X-H1 ColorChecker
RAF/JPEG originals plus a landscape Auto pair against ExifTool. All 23 pairs
return identical recipes, including film simulation/filter, 5600 K white balance,
manual DR100/200/400 and D-Range Priority Weak/Strong/Auto. The originals were used
locally under their supplied development/test permission; none are redistributed.

Import and Reload from Disk store the recipe, including known absence. Reload also
refreshes corrected recipe labels when the file bytes are unchanged. Older
eligible photos are read on a worker when Info needs them; opening Info does not
change the library. Cache identity includes the library and file identity. Relink
and Convert to DNG refresh or invalidate the recipe, and undo restores it. A
synchronous Find Missing search retains a recipe only after verifying the current
file hash. A submitted/background search plan invalidates it so whole-file hash
reads do not block the UI; Info can then read it again on a worker.

Recipes were introduced in catalog format 10; this build writes format 11 to also
retain RAW+JPEG pair links. Older builds refuse newer formats. This feature does
not change image pixels, apply film-simulation rendering,
reproduce the camera JPEG or recover a custom recipe name. Search and smart-album
integration are proposed separately in [#3330](https://github.com/storytold/lightcraft/pull/3330).
