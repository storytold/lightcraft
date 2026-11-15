# The modules

> Names in angle brackets are the values set in [`brand.toml`](../../brand.toml); see the [manual index](README.md).

The window follows Lightroom Classic: a module picker on top (`F5`), panels left (`F7`) and right (`F8`), a filmstrip
at the bottom (`F6`). Window → Modules switches modules; `Cmd+Alt+Up` returns to the previous one. Each module's
actions are commands, so the CLI, control channel and MCP can do everything the panels do.

## Library

Photo Grid and Square Grid (View menu), loupe (`E`), compare (`C`) and survey (`N`) views of the folder, collection or filter in view.
Press `F` for the full-screen preview of the active photo: arrows step, `I` cycles the info overlay (at Off the photo is clear of all
text, the shot-info strip with the pixel size included), `G` returns to the grid, `Esc` goes back.

- **Organise:** folders, collections and smart collections (Library → New Collection… `Cmd+N`, New Smart Collection…), collection sets,
  stacks (`Cmd+G`), virtual copies (`Cmd+'`), people (face regions from XMP or the opt-in face detector).
- **Rate and cull:** stars `0`–`5`, flags `P`/`X`/`U`, colour labels `6`–`9`; `Shift` + key moves on
  ([library-shortcuts.md](../library-shortcuts.md)).
- **Metadata:** Info and Keywords panels, metadata presets, hierarchical keywords with import/export (File →
  Import/Export Keywords…), the Painter (`Cmd+Alt+K`), Save Metadata to File (`Cmd+S`).
- **Find:** the filter bar (`Cmd+F`): text, attributes, metadata columns (including Aperture and Shutter Speed); save it as a smart collection.
  Library → Advanced Search… (`Cmd+Shift+F`) filters by structured fields; codec and frame rate are not supported yet.
- **Clean up:** Library → Find Duplicates… groups exact and look-alike copies (raw + JPEG pairs are offered as stacks, not
  removals; [duplicates.md](../duplicates.md)); Validate DNG Files checks the DNGs' checksums; Photo → Reset Export Status
  clears the exported badge; Metadata → Update DNG Preview & Metadata refreshes a DNG's preview and XMP.
- **Culling suggestions:** Assisted Culling suggests 1–5 stars and Pick / Reject for photos you have not rated or flagged;
  the smart-collection field Suggested Stars filters on them.
- **Folders:** Show Parent Folder (right-click) adds a parent to the list; a folder that cannot be read shows the reason.

## Develop

`D` opens the active photo. Its left column is Develop's own, as in Lightroom Classic: Navigator, Presets, Versions
(snapshots), History and Collections — the Library's Catalog, Folders and Keywords stay in Library, and the My Photos
sidebar is there as a panel to show from any panel's menu. Panels: Light, Color, Effects, Detail, Optics, Geometry,
Calibration (`Cmd+1`…`Cmd+5` jump to sections), plus the tools above them: Crop (`R`), Remove (`Q`), Red Eye,
Masking (`Shift+W`: brush `K`, linear `M`, radial `Shift+M`, colour and luminance range, AI
subject/sky/background/object masks). Presets (`Shift+P`), profiles, snapshots, history and versions (`Shift+V`);
before/after (`Y`, `\`); clipping (`J`); soft proofing (`S`).
Show Before (`\`) and Original Photo are momentary and never saved. Delete, `X` and the tool keys act only on a tool that
is on screen. Calibration's primary Saturation is fixed in process V9 (older photos keep their look). Create Camera
Profile accepts a ColorChecker 24 or SpyderCHECKR 24 photo.
Copy and paste settings with `Cmd+C`/`Cmd+V` (`Cmd+Shift+C` to choose which). Photo → Photo Merge makes HDR and
panorama DNGs. Every slider is a develop control: `<cli> controls` lists ids and ranges.

The row of tools under the histogram is Edit, Crop, Remove, Red Eye, Masking and Browse Profiles (the profile
browser in the panels' place; a second click puts it away). Basic starts with Auto · B&W | HDR, then the profile's
Amount and white balance.

### Tool profiles

A tool profile picks which groups and sliders the Develop panel shows. The low row at the top of the panel names the
profile in use: its menu switches profile, and the gear beside it opens **Settings ▸ Develop**, where profiles are
made.

- **Default** shows every group and slider, as the panel always did. **Witchcraft** shows the colourist's Primaries
  and HSL Curves (in either grading mode) and hides Eboda's Color Grading. Both are built in and can't be changed.
- **New** makes a profile showing everything; **Duplicate** copies the profile in use (a built-in one too) to start
  from it. Your own profiles can be renamed and deleted (deleting the one in use goes back to Default).
- **Custom** — one group per profile of the sliders you pick, in the order you set (↑ ↓), shown first in the panel. A
  slider in it moves there: it isn't drawn again in its own group.
- **Groups** — switch any group off, or open it to hide single sliders and options (the Auto · B&W | HDR row, the
  white balance selector, Contrast Mode, AI Denoise, Remove Hot Pixels, the vignette style…).

Profiles are saved with the app's settings. A profile that names a slider a later version no longer has keeps the
rest. For scripts and agents: `view.toolProfile {id}` picks one (without an id it lists them), and
`view.toolProfileEdit {op: new | duplicate | rename | delete | group | item | custom | witchcraft, …}` changes your
own — for example `{"op": "custom", "add": "light.exposure"}` or `{"op": "group", "group": "curve", "on": false}`.
The Modern layout's cards show their sections whole.

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
system keychain and the server's key fingerprint pinned once you confirm it (`web.hostKey`, `web.trustServer`, `web.upload`, `web.saveServer`). Galleries can be saved
and reopened.

## Saved creations

Books, prints, slideshows and web galleries can be saved as creations (`creation.save`): a collection holding the
photos and the layout, listed with your collections. `layout.templates` lists the built-in templates for each kind.
