# The modules

> Names in angle brackets are the values set in [`brand.toml`](../../brand.toml); see the [manual index](README.md).

The window follows Lightroom Classic: a module picker on top (`F5`), panels left (`F7`) and right (`F8`), a filmstrip
at the bottom (`F6`). Window → Modules switches modules; `Cmd+Alt+Up` returns to the previous one. Each module's
actions are commands, so the CLI, control channel and MCP can do everything the panels do.

## Library

Photo Grid and Square Grid (View menu), loupe (`E`), compare (`C`) and survey (`N`) views of the folder, album or filter in view.

- **Organise:** folders, albums and smart albums (Library → New Album… `Cmd+N`, New Smart Album…), collection sets,
  stacks (`Cmd+G`), virtual copies (`Cmd+'`), people (face regions from XMP or the opt-in face detector).
- **Rate and cull:** stars `0`–`5`, flags `P`/`X`/`U`, colour labels `6`–`9`; `Shift` + key moves on
  ([library-shortcuts.md](../library-shortcuts.md)).
- **Metadata:** Info and Keywords panels, metadata presets, hierarchical keywords with import/export (File →
  Import/Export Keywords…), the Painter (`Cmd+Alt+K`), Save Metadata to File (`Cmd+S`).
- **Find:** the filter bar (`Cmd+F`): text, attributes, metadata columns; save it as a smart album.

## Develop

`D` opens the active photo. Panels: Light, Color, Effects, Detail, Optics, Geometry, Calibration (`Cmd+1`…`Cmd+5`
jump to sections), plus the tools above them: Crop (`R`), Remove (`Q`), Red Eye, Masking (`Shift+W`: brush `K`,
linear `M`, radial `Shift+M`, colour and luminance range, AI subject/sky/background/object masks). Presets (`Shift+P`),
profiles, snapshots, history and versions (`Shift+V`); before/after (`Y`, `\`); clipping (`J`); soft proofing (`S`).
Copy and paste settings with `Cmd+C`/`Cmd+V` (`Cmd+Shift+C` to choose which). Photo → Photo Merge makes HDR and
panorama DNGs. Every slider is a develop control: `<cli> controls` lists ids and ranges.

## Map

A slippy map (OpenStreetMap tiles by default; terrain, satellite or your own XYZ server) with clustered pins for
the photos in view.

- Drag photos from the filmstrip onto the map to geotag them; drag pins to move them (`map.geotag`).
- Load a GPX, KML or GeoJSON track log and Auto-Tag photos by capture time, with a camera-clock offset
  (`map.track`, `map.trackOffset`, `photo.autoTagTracklog`).
- Saved locations with a radius; a **private** location keeps its photos' GPS out of every export.
- Reverse geocoding is opt-in: offline from a downloaded GeoNames cities list (`map.geonamesDownload`, CC-BY 4.0) or
  online from a Nominatim-compatible server after consent. Nothing is sent without asking.
- Tiles are cached on disk with a size limit; the app never bulk-prefetches.

## Book

A photo book laid out from the filmstrip. Left: page templates and saved books. Centre: multi-page, spread, single
page or zoomed view. Right: Book Settings (size, cover, paper, PDF or JPEG), Auto Layout, Page, Guides, Cell, Text,
Type, Background, Export. Drag photos onto cells; drag a cell onto another to swap them. Text uses the same shaper on
screen and in the export, so the PDF matches the canvas. Export writes a PDF (photos as JPEG with an sRGB profile,
text as embedded subset fonts) or one JPEG per page. There is no print-on-demand upload (Blurb) yet.

## Slideshow

The Template Browser and saved slideshows on the left, the slide preview in the centre, Options / Layout /
Overlays / Backdrop / Titles / Playback / Music on the right. Text overlays take tokens (`{Title}`, `{Caption}`,
`{Exposure}` …). Play full screen, preview in the module, or export a JPEG sequence. View → Slideshow
(`Cmd+Alt+Enter`) remains the quick impromptu show of the current selection. No video export yet.

## Print

Single image / contact sheet, picture package and custom layouts from built-in and saved templates; page setup,
margins, cell sizes, guides, image settings (rotate to fit, stroke), page options (crop marks, page info),
identity plate and text. Output: a PDF (vector text, crop marks, an optional ICC OutputIntent), JPEG files, or the
PDF sent to an IPP/CUPS printer (Find Printers asks the CUPS server). `print.render` renders the same pages from the
CLI. Printer colour management beyond the output profile is not done yet.

## Web

A live preview of a static HTML gallery (grid, square, track or single-image layouts) with Site Info, Colour
Palette, Appearance, Image Info, Output Settings and Upload Settings. Export writes `index.html`, its assets and
the rendered images to a folder (`web.export`); Upload sends them to an SFTP server, with the password kept in the
system keychain and the server's key fingerprint pinned (`web.upload`, `web.saveServer`). Galleries can be saved
and reopened.

## Saved creations

Books, prints, slideshows and web galleries can be saved as creations (`creation.save`): a collection holding the
photos and the layout, listed with your albums. `layout.templates` lists the built-in templates for each kind.
