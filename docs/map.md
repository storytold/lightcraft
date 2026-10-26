# Map

LightCraft shows where photos were taken on a map drawn from raster map tiles. This page explains what the map
fetches, from where, how it follows the tile provider's rules, what that means for your privacy, and how to turn it
off.

Status: the Map view (below) shows the photos in view as pins and clusters over the tiles, you can drag photos onto it
to set their location (and move or remove pins), filter the filmstrip by location, save named places (saved locations,
some of them private), draw a GPS track log (GPX) and tag photos from it, and the Info panel shows a small map of the
active photo's place. Nothing is fetched until a map is on screen.

## The Map view

View ▸ Map, or the map pin in the bottom bar (next to People), shows the photos in view (the current source, filter
and sort: the same photos as the grid) on a map, with the filmstrip below.

- **Pins**: each photo with a GPS position is a pin. Pins closer than about 60 px merge into a cluster showing how
  many photos it holds; zooming in splits clusters until every photo has its own pin. Pins of selected photos are blue,
  the active photo's has a thicker ring.
- **Selecting**: a click on a pin or cluster selects its photos (the filmstrip shows them selected); ⌘-click or
  ⇧-click adds them to the selection; a double-click opens the first one in Detail. Resting the pointer on a pin shows
  its photo's thumbnail (the first photo's, with the count, for a cluster).
- **Moving**: drag to pan; the mouse wheel, a pinch or ⌘-scroll zooms around the pointer; a two-finger scroll pans;
  a double-click on the map zooms in. The buttons at the top right zoom out / in and **Fit to Photos** (shows every
  photo with a position); ⌘- / ⌘= / ⌘0 do the same in this view.
- The first time the map is shown it fits the photos with a position; after that it stays where you left it for the
  session.
- "N photos without GPS" at the bottom left counts the photos in view that have no position (drag them onto the map
  from the filmstrip, set one in the Info panel, or use Photo ▸ Auto-Tag from Tracklog…).
- The tile source's attribution is always shown at the bottom right. Tiles not loaded yet show a lower zoom's tile
  magnified, or grey. In offline mode the map is grey with the note "Map tiles are off".

Photos whose GPS position is invalid (out of range, NaN) are counted as without GPS, never placed. Clusters are
recomputed only when the view moves, from positions projected once per change of the photo list: 50 000 photos on
screen take about 4 ms (release build), a few thousand well under 1 ms.

## Geotagging: putting photos on the map

As in Lightroom Classic's Map module:

- **Drop photos on the map**: drag photos from the filmstrip onto the map and let go where they were taken. A drag
  takes the selected photos (or, when the photo you grab isn't selected, just that one); while they are over the map
  a pin with their count follows the pointer, and the drop sets every one of them to the point under it. Photos that
  already had a position are moved (Lightroom does the same). One undo step (Edit ▸ Undo "Set GPS Coordinates")
  puts all of them back where they were, or off the map.
- **Move a pin**: drag a single photo's pin to its new place (one undo step). A cluster isn't moved: dragging it pans
  the map, as a drag on the empty map does, so zoom in until the photo has its own pin. A pin dropped outside the map
  (on the filmstrip, say) stays where it was.
- **Lock Markers** (the map's toolbar, top right) stops pins from being moved; dragging one then pans the map. It is
  saved with the window layout (`ui.json`), and `map.lockMarkers {on?}` sets or toggles it.
- **Remove GPS Coordinates**: Photo ▸ Remove GPS Coordinates clears the position of the selected photos; a right-click
  on a pin offers the same for that pin's photos. One undo step.
- The map can't show the poles (Web Mercator stops at ±85.05° latitude), so a drop reaches 85.05° north or south at
  most; drop just inside the top edge to get the edge's latitude. Longitudes wrap around the world. Type a position
  closer to a pole in the Info panel's GPS field.
- Where positions are saved: in the library, and in XMP sidecars (EXIF GPS: `exif:GPSLatitude` /
  `exif:GPSLongitude`) when they are written (Photo ▸ Save Metadata to File, or at once with File ▸ Automatically
  Write Changes into XMP on). A position you set or clear in LightCraft replaces the sidecar's, so a cleared one (or an undone geotag) is
  removed from it; LightCraft doesn't read GPS from sidecars, so a sidecar's position for a photo whose position you
  never touched in LightCraft is left alone.
- Dropping files from the system's file manager onto the map isn't supported (they import, as anywhere in the app).

| Command | Params | Result |
|---|---|---|
| `photo.setGps` | `{ids?, lat, lon}` | Set the position of the photos (default: the selection): `lat` −90…90, `lon` in degrees (wrapped into −180…180); anything else, or ids that are none of the library's photos, is refused → `{changed, lat, lon}` |
| `photo.removeGps` | `{ids?}` | Clear the position of the photos (default: the selection) → `{changed}` |
| `map.lockMarkers` | `{on?}` | Lock Markers on / off (toggles without `on`) → `{on}` |

`photo.setGps` and `photo.removeGps` are library commands (also in `lightcraft-cli` and the headless MCP server);
`map.lockMarkers` is a UI command. `map.state` also reports `lockMarkers`, `movingPin` (`{id, x, y}` while a pin is dragged) and `drop` (`{count, x, y,
lat, lon}` while photos are dragged over the map). Widgets: `map:lockMarkers`, `map:pinMenu:removeGps` (the pin's
menu). Agents can hold a drag across requests with `ui.pointer {screen: true, events: [{kind: "down", …}, {kind:
"drag", …}]}` and drop it with `{kind: "up"}` (see the control protocol).

## The location filter

The bar above the map, as Lightroom Classic's Location Filter, narrows the filmstrip by where the photos are:

| Choice | The filmstrip shows |
|---|---|
| **Visible on Map** | the photos whose position is inside the map's view; it follows the map as you pan and zoom, once the view rests for a quarter of a second |
| **Tagged** | the photos with a GPS position |
| **Untagged** | the photos without one (the map shows no pins; drag them onto it to place them) |
| **None** | every photo in view (the default) |

- Each choice shows its count, of the photos in view (the current source, filter and sort); the one chosen is
  highlighted. The counts follow the map as it moves (Visible on Map's at once; the filmstrip when the view rests).
- It narrows on top of the library's filter and search, which it never changes: leaving the Map view (to the grid,
  Detail, People…) brings back exactly the photos you had. Coming back to the map, the choice is still there (for the
  session; it isn't saved with the layout). The selection, ⌘A and the arrow keys follow the filmstrip.
- Pins come from every photo with a position in view, so in Visible on Map the pins on screen and the filmstrip are the
  same photos.
- Across the antimeridian (a view of the Pacific) the area wraps around ±180°; zoomed out past the whole world it holds
  every longitude, and a view reaching the map's top or bottom edge reaches the pole. A photo whose position is invalid
  (NaN, out of range) counts as untagged.
- Counting is one pass over the positions already projected for the pins (20 000 photos: well under 1 ms); narrowing
  the filmstrip is one pass over the photos in view, done only when the view rests or the choice changes.

The bar is built on two smart-album rule fields that any filter or smart album can use (Location group of the rule
editor; `library.filter {ruleSet}`, `album.createSmart`, `album.setRules`):

| Field | Operators | Value |
|---|---|---|
| `hasGps` (Has GPS) | is | yes / no: the photo has a valid position |
| `gpsArea` (GPS Area) | `inside`, `outside` | `{south, west, north, east}` in degrees: latitudes −90…90 (south not north of north), longitudes −180…180, all finite numbers; `east < west` wraps across ±180° (`{west: 170, east: -170}` is the 20° around it), `west: -180, east: 180` is every longitude. Edges count as inside. A photo without a position is neither inside nor outside |

Anything else (a missing edge, text, NaN, out of range, south north of north) is refused by the commands and marked in
the rule editor.

| Command | Params | Result |
|---|---|---|
| `map.locationFilter` | `{mode?: "visible" \| "tagged" \| "untagged" \| "none"}` | Choose the filter (without `mode`: report) → `{mode, visible, tagged, untagged, all, area, countMs, shown}`: the counts, `area` (what Visible on Map narrows to, `{south, west, north, east}`; null otherwise) and `shown` (photos the filmstrip has) |

`map.state` reports the same as `locationFilter`. Widgets: `map:filterBar`, `map:filter:visible`, `map:filter:tagged`,
`map:filter:untagged`, `map:filter:none`.

## Saved locations

As Lightroom Classic's Saved Locations: a saved location is a name, a centre and a radius (1 m to 1000 km), drawn on
the map as a circle with its name above it. A location can be **private**.

- **The list** sits over the map's top-left corner (a click on its header folds it): every saved location with the
  number of photos in view inside it (the photos the map shows: the current source and filter, before the location
  filter bar), private ones marked "private" and drawn with a dashed purple edge. It is on the map, not in a docked
  panel, because the docked panels are shared by every view and the Library panel is closed by default; the map's
  canvas can't be closed, so the list is there whenever the map is. On a map narrower than about 470 points or lower
  than 200 the list waits until there is room (the circles and the commands still work).
- **A click on a row** shows the location: it is selected and the map centres on it at a zoom that holds the whole
  circle. **The checkbox** selects the photos in view inside it (ticked while exactly those are selected; a second
  click clears the selection). Right-click a row for Go to Location, Select Photos in Location, Rename…, Private and
  Delete Location… (which asks first: there is no undo for it).
- **New Location** (+ in the list's header): at the map's centre (a radius of a sixth of the map's shorter side), or
  from the selection (centred on the selected photos' positions, with a radius that holds them all, 10 % to spare,
  at least 100 m). Either asks for the name.
- **On the map**, a click on a location's centre dot or its name selects it; a click on the empty map drops the
  selection. The selected location has a handle at its centre (drag to move it) and one on its east edge (drag to
  change the radius). The change is saved when the button is released over the map (released elsewhere, nothing
  changes). **Lock Markers** locks these handles as it locks photo pins (a drag on them pans the map instead): the lock
  is there so that a drag over the map can't move anything by accident, and a location's centre and edge are markers
  too. Rename and Private still work from the list.
- **Measured on the Earth**: "inside" means within the radius along the Earth's surface (great-circle distance on a
  sphere of 6 371 km), edge included, so a circle across the antimeridian holds photos on both sides of ±180° and one
  that covers a pole holds every longitude near it. On the map, such a circle is drawn whole: across ±180° as one
  shape, around a pole as a band reaching the map's top (or bottom) edge, since the map stops at ±85.05°.

**Private locations.** A photo inside any private location:

- is exported **without its location**, exactly as if Export ▸ "Remove location info" were on for that photo alone:
  no GPS position, and no sublocation / city / state / country fields. Photos outside every private location export
  as before, and the photo's position in the library is never touched. Making the location public again (or deleting
  it) brings the location back to later exports. This applies to the rendered formats (JPEG, PNG, TIFF, AVIF…),
  like "Remove location info"; **Original** and **DNG** exports copy the file (with its own embedded EXIF GPS) and its
  edits as they are, so choose a rendered format for photos you want to share without their place;
- hides the **Info panel's map** (the panel says "In the private location “Home”: its map is hidden" with **Show Map**
  next to it, which shows it for that photo until another one becomes active). The GPS field, its text and Open in
  Browser stay, and the Map view still shows the photo's pin: private is about what leaves the computer and what is
  on screen by default, not about hiding the photo from you.

**Where they are kept.** Saved locations belong to the library, in `locations.json` next to the catalog (written
atomically on every change, as `prefs.json` is). They are not part of the catalog's journal: changing them is not an
undo step (Lightroom Classic's saved-location edits aren't part of a photo's history either), so Delete Location asks
first. The file is checked as it is read: if it doesn't parse it is kept aside as `locations.json.corrupt-<time>` and
the library opens without saved locations (a warning says so); entries that aren't valid (a missing or NaN number, a
latitude outside ±90, a radius outside 1 m…1000 km, an empty or over-long name, a repeated id, past 10 000 locations)
are left out, and the file is kept aside the same way before it is written again. A file that can't be read at all is
never overwritten that session. A library holds at most 10 000 saved locations; counting them over the photos uses a
latitude-sorted index, and the map counts again only when the photos or the locations change (`map.locations` with 3 000 locations: about 16 ms in a test build).

**Smart albums and filters** gain **GPS Radius** (`gpsRadius`, Location group): `inside` / `outside` a circle
`{lat, lon, radius}` (degrees, metres; the same checks), measured the same way. It is what a saved location tests,
written into the rule, so a smart album keeps working when the location is renamed or deleted.

| Command | Params | Result |
|---|---|---|
| `map.locations` | — | `{locations: [{id, name, lat, lon, radius, private, photos, inView}], count, countMs}`: `photos` inside it in the library (not counting deleted photos), `inView` of the photos in view |
| `map.location.add` | `{name, lat, lon, radius?, private?}` or `{name, fromSelection: true, ids?, radius?, private?}` | Save a location: `name` 1–200 characters (trimmed), `lat` −90…90, `lon` −180…180, `radius` in metres 1–1 000 000 (default 1000), all finite; `fromSelection` centres it on the selected (or `ids`) photos' positions with a radius that holds them → the location with `photos` and `inView` |
| `map.location.update` | `{id, name?, lat?, lon?, radius?, private?}` | Change a location, checked as a whole (nothing changes when any part is refused) → the location with its counts |
| `map.location.remove` | `{id}` | Delete it (the photos keep their positions) → `{removed, name}` |
| `map.location.photos` | `{id, inView?}` | `{id, ids, count}`: the library's photos inside it (`inView: true`: only the photos in view, in their order) |
| `map.location.show` | `{id}` | (UI) Select it and centre the map on it → the map state |
| `map.location.select` | `{id}` or `{id: null}` | (UI) Select it on the map (handles), or none → `{selected}` |
| `map.location.selectPhotos` | `{id}` | (UI) Select the photos in view inside it (the checkbox) → `{selected}` |

The first five are library commands (`lightcraft-cli`, the headless MCP server, the app); the others need the map.
When `locations.json` couldn't be written the change stays for the session and the result carries `notSaved` with
the reason. `map.state` reports `locations` (those drawn: `{id, name, private, x, y, radiusPx, handle: {x, y},
inView, selected}`, screen points, at most 500), `selectedLocation`, `movingLocation` (`{id, lat, lon, radius,
part}` while a handle is dragged) and `privateMiniMap` (`{photo, location}` while the Info panel hides its map).
Widgets: `map:locations` (the list), `map:locationsHeader`, `icon:locationNew`, `map:locationNew:centre`,
`map:locationNew:selection`, `map:location:<id>` (a row), `map:location:<id>:check`, `map:location:<id>:count`,
`map:location:<id>:centre` and `map:location:<id>:radius` (the handles on the map), `map:locationMenu:show`,
`…:selectPhotos`, `…:rename`, `…:private`, `…:delete`, `button:showPrivateMap` (Info panel).

## Track logs

As in Lightroom Classic's Map module (the Tracklog button), a GPS track log (a GPX 1.0 / 1.1 file from a phone app,
watch or logger) can be drawn on the map and used to tag photos:

- **Tracklog** (top right of the map) ▸ **Load Tracklog…** opens a `.gpx` file. Its tracks are drawn as pink lines and
  the map fits them. Each continuous logging span (a GPX track segment) is its own line: a gap where the logger was off
  or lost reception is not joined. A line crossing the ±180° meridian is drawn the short way across it.
- A file with several tracks lists them in the menu, with **All Tracks** (the default). Picking one draws the others
  dimmed, fits the map to it and makes it the one tagging uses, as in Lightroom.
- The menu shows the active track's **start, end and duration** on the camera's clock: GPX times are UTC, so they are
  shifted by the **time zone offset** (Set Time Zone Offset…, e.g. `+02:00` or `-5`); with none set they are UTC and
  say so. The offset is the one tagging uses for capture times without a recorded zone (a photo whose Exif records its
  own offset uses that).
- **Auto-Tag N Selected Photos** tags the selected photos from the active track by capture time
  (`photo.autoTagTracklog`, as Photo ▸ Auto-Tag from Tracklog… does): interpolated between the points of a segment,
  else the nearest point within 10 minutes, never across a gap; photos that already have a position keep it. If no
  offset is set it asks for the camera's time zone first (and keeps it with the track). All the photos tagged are one
  undo step; a message says how many were tagged and how many fell outside the track's time range.
- The selected photos are **marked on the track** (white dots) where Auto-Tag would put them, so a wrong offset shows
  before anything is written; the menu's Auto-Tag item says how many fall within the track. Marks are drawn for up to
  5 000 selected photos.
- **Fit Map to Tracklog**, **Load Another Tracklog…** and **Clear Tracklog** are in the same menu.
- The track log belongs to the session: it is not saved in the catalog and is gone after a restart (a new file keeps
  the offset chosen for the last one).

Limits and errors: a file larger than 50 MB or with more than 2 million timed points is refused with a message saying
so, as are a file that isn't GPX, one without tracks, and one whose points have no times (tagging needs them); the
track already loaded stays. Points with invalid coordinates (NaN, out of range) are skipped; points beyond the map's
±85.05° latitude are drawn on its edge; timestamps out of order are sorted.

Drawing cost: each track is projected and ranked for simplification once when loaded (Douglas–Peucker ranks: keeping
the points ranked above a tolerance is exactly the Douglas–Peucker line at that tolerance, so no dropped point is more
than 0.6 px from the line drawn, and every line keeps its ends). Each zoom level then takes one pass over the points,
and each view change clips only the chunks on screen. On a 2-million-point track (release build): loading's
projection and ranking about 230 ms, a zoom level 1–10 ms, a frame's clipping well under 1 ms (0.5 ms with 38 000
points on screen).

Commands: `map.trackLog` (a library command, so it works headless and over MCP too):

| Params | Does |
|---|---|
| `{path}` or `{gpx}` | Load a GPX file (or its text), replacing the one loaded; the Map view fits it (`fit: false` keeps the view) |
| `{active: n \| "all"}` | The track tagging uses and the map highlights (index from `tracks`) |
| `{offset: "+02:00" \| hours \| null}` | The camera clock's offset from UTC (null or `""`: UTC) |
| `{clear: true}` | Remove the track log |
| `{}` | Report |

→ `{loaded, path, name, points, untimed, tracks: [{index, name, points, segments, start, end, duration}], active,
activePoints, start, end, duration, offset, startLocal, endLocal, loadMs, generation}` (`start` / `end` in UTC,
`startLocal` / `endLocal` on the camera's clock). Several params combine; a refused one changes nothing.
`photo.autoTagTracklog {loaded: true, …}` tags from it (its active track, and its offset unless `offset` is given).
UI commands: `map.loadTrackLog {path?}` (asks for the file without `path`), `map.trackLog.autoTag {ids?, offset?}`
(asks for the offset when none is set), `map.trackLog.setOffset {offset?}`, `map.trackLog.fit`. `map.state` →
`trackLog` adds what is drawn: `drawn: {lines (per track), points, levelPoints, zoom, levelMs, clipMs}`,
`ticks: [{id, x, y}]` (the selected photos' marks, screen points) and `onTrack`.

Widgets: `map:tracklog` (the button), `map:tracklog:load`, `…:track:all`, `…:track:<index>`, `…:info`, `…:offset`,
`…:autoTag`, `…:fit`, `…:clear`.

## Map commands

Commands (UI commands: they need the map's size, so the app, `lightcraft-cli snapshot` and `mcp --connect` have them):

| Command | Params | Result |
|---|---|---|
| `view.map` | — | Switch to the Map view → `{withGps, withoutGps}` |
| `map.fit` | — | Fit the photos with a position (the world when there are none) → the map state |
| `map.setView` | `{lat?, lon?, zoom?}` | Centre (degrees) and zoom (0 to the source's max zoom; fractional allowed); what is left out stays → the map state |
| `map.zoomIn` / `map.zoomOut` | — | One zoom level in / out, around the centre → the map state |
| `map.state` | — | `{lat, lon, zoom, maxZoom, width, height, tileZoom, source, offline, attribution, withGps, withoutGps, clusters: [{ids, count, x, y, lat, lon, selected}], clusterCount, clusterMs, lockMarkers, locationFilter, movingPin, drop, tiles: {drawn, missing, textures, uploads}, miniMap, trackLog}` (`miniMap`: the Info panel's map, below; `trackLog`: see Track logs); `x`, `y` are screen points (for `ui.click`), at most 500 clusters listed |

Widgets (`ui.widgets`): `map:canvas`, `map:pin:<first photo id>`, `map:fit`, `icon:mapZoomIn`, `icon:mapZoomOut`,
`map:attribution`, `map:withoutGps`, `map:hoverThumb`, `map:tracklog`.

## The map in the Info panel

For a photo with a GPS position, the Info panel shows a small map under the location fields: the panel's width,
160 px tall, centred on the photo at zoom 14 (or the tile source's deepest zoom if less), with the photo's pin and the
tile source's attribution at its bottom right.

- With several photos selected it shows the active photo's place (as Lightroom does), and fetches nothing for the
  others. A photo without a position (or with an invalid one) has no map; its location fields stay as they are.
- It doesn't pan or zoom, so a drag or a scroll over it scrolls the panel. A click opens the Map view centred on the
  photo at the same zoom (`view.map`, then `map.setView`). **Open in Browser** below it shows the place on
  openstreetmap.org instead.
- It shares the Map view's tiles: one texture cache, one tile service. A tile one of them loaded is drawn by the
  other without being fetched or uploaded again, and with both on screen neither cancels the other's tiles.
- It fetches only while it is on screen: the Info panel open and the map scrolled into view. It asks for exactly the
  tiles it draws (no ring around them, since it doesn't pan). Close the panel or scroll the map away and its tiles are
  no longer asked for.
- Offline it stays in place, grey, with the pin and the note "Map tiles are off", so the panel keeps its shape and the
  click into the Map view (where the other photos' pins are) still works.

`map.state` reports it as `miniMap` (`null` when it isn't drawn): `{photo, lat, lon, zoom, x, y, width, height,
pin: {x, y}, tiles: {drawn, missing}}`, in screen points. Widgets: `info:miniMap`, `info:miniMapAttribution`.

## What is fetched, and when

- **Only map tiles**: 256 × 256 px PNG images of the map, by zoom level and position
  (`https://tile.openstreetmap.org/{z}/{x}/{y}.png` by default). No photos, GPS positions, file names or account data
  are ever sent.
- **Only while a map is on screen**, and **only the tiles you are looking at**: the tiles the view draws plus at most
  one ring of tiles around them (none around the Info panel's map), at the zoom level shown. When no map is on screen
  any more, the tiles it asked for and that haven't started are dropped. LightCraft never prefetches other zoom levels or areas, and
  has no "download this area for offline use". Nothing is fetched at startup.
- **Newest view first**: when you pan or zoom, tiles that left the view and haven't started are dropped; at most two
  requests run at once.
- **Gently**: after an error, a `429 Too Many Requests` or a server error, LightCraft waits (1 s, doubling up to
  5 minutes, and at least as long as the server's `Retry-After`) and then tries one request at a time until one
  succeeds. A tile the server refuses (e.g. 404) or that isn't a valid image isn't asked for again for 10 minutes.
- Tiles larger than 2 MB, images that aren't PNG / JPEG / WebP of at most 1024 px a side, and anything that fails to
  decode are errors (the map shows its placeholder), never a crash.

## OpenStreetMap's tile policy

The default tiles come from the OpenStreetMap Foundation's tile servers, which are run by volunteers and donations and
are free to use under the [OSMF tile usage policy](https://operations.osmfoundation.org/policies/tiles/). LightCraft
follows it:

- It uses the exact URL `https://tile.openstreetmap.org/{z}/{x}/{y}.png` (no subdomains, HTTPS only).
- It shows the attribution **"© OpenStreetMap contributors"** visibly on the map, never behind a toggle or off
  screen. (The map data is © OpenStreetMap contributors, available under the
  [Open Database License](https://www.openstreetmap.org/copyright).)
- Every request carries a distinct User-Agent naming the app, its version and where to find it:
  `LightCraft/<version> (+https://github.com/storytold/lightcraft)`.
- It honours the server's caching headers (`Cache-Control: max-age`, `Expires`, `ETag`, `Last-Modified`); when the
  server gives none, a tile is kept fresh for 7 days. Expired tiles are shown while LightCraft asks the server whether
  they changed (`If-None-Match` / `If-Modified-Since`; a `304 Not Modified` answer costs no download). It never sends
  `Cache-Control: no-cache` or `Pragma: no-cache`.
- It doesn't bulk-download, prefetch or offer offline areas: only the tiles in view (plus one tile around) are fetched.

Access is best effort: the OSMF can limit or block any app without notice. If that happens, use another tile server
or offline mode (below).

## Choosing the tile server

The tile source is a library preference (saved in the library's `prefs.json`), set with the `map.tileSource`
command (and in Settings once the map view lands):

| Params | Source |
|---|---|
| `{}` | Report the current source: `{id, urlTemplate, attribution, maxZoom, userAgent, offline}` |
| `{id: "osm"}` | OpenStreetMap's standard tiles (the default; max zoom 19) |
| `{offline: true}` | Offline: no network at all, no tiles; the map shows only what LightCraft draws on it |
| `{urlTemplate, attribution, maxZoom?, id?}` | Another raster tile server, e.g. one you host. The template must start with `https://` and contain `{z}`, `{x}` and `{y}` exactly once (no other placeholders); the attribution is required (it is shown on the map); `maxZoom` is 0–24 (default 18); `id` names its cache folder (`[a-z0-9._-]`, derived from the template when absent) |

Fields given alone change the current source (e.g. `{maxZoom: 16}`). Anything that doesn't pass these checks is
refused with an error and changes nothing; a hand-edited `prefs.json` with an unusable source falls back to
OpenStreetMap. Respect the terms of any server you configure: many commercial providers need an API key and forbid
some uses, and Google, Apple, Bing and Esri imagery may not be used this way.

## Privacy

Looking at the map tells the tile server (the OpenStreetMap Foundation, by default) which areas you look at, at
which zoom, from your IP address, like any web map does; see the OSMF's
[privacy policy](https://osmfoundation.org/wiki/Privacy_Policy). Your photos and their GPS positions stay on your
computer: pins are drawn locally. If you would rather nothing about your map views left your computer, choose
offline mode (`map.tileSource {offline: true}`) or a tile server you run yourself.

## The tile cache

Downloaded tiles are kept on disk so the same area isn't downloaded again:

| System | Folder |
|---|---|
| macOS | `~/Library/Caches/LightCraft/map-tiles` |
| Windows | `%LOCALAPPDATA%\LightCraft\Cache\map-tiles` |
| Linux and others | `$XDG_CACHE_HOME/lightcraft/map-tiles` (else `~/.cache/lightcraft/map-tiles`) |

`LIGHTCRAFT_MAP_CACHE` overrides the folder. Inside it every tile source has its own folder (`osm`, `custom-…`, the
id you gave), holding one `<z>/<x>/<y>.tile` file per tile (the image as the server sent it, with its caching
metadata). The cache is capped at 500 MB for all sources together: past that, the tiles used least recently are
deleted. Tiles are written atomically (a `.part` file renamed into place), and a damaged cache file is deleted and
fetched again. The app, `lightcraft-cli` and the MCP server share the folder.

- `map.cacheInfo` → `{bytes, tiles, capBytes, dir}`
- `map.clearCache` deletes every cached tile (all sources) → what was deleted. Only `.tile` / `.part` files in the
  cache's own layout are deleted, so pointing `LIGHTCRAFT_MAP_CACHE` somewhere odd can't delete anything else.

The web build has no tile cache and fetches no tiles yet.
