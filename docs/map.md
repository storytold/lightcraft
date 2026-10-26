# Map

LightCraft shows where photos were taken on a map drawn from raster map tiles: the **Map view** (Lightroom Classic's
Map module, as a view of the library) and a small map in the **Info panel**. This page explains what the map does,
its keys and menus, what it fetches and from where, how it follows the tile provider's rules, what that means for
your privacy, and how to turn the network off.

## Overview

- **The Map view** (View ▸ Map, or the map pin in the bottom bar) puts the photos in view on a map as pins and
  clusters, with the filmstrip below.
- **Geotagging**: drag photos onto the map to set their location; move or remove pins.
- **Location filter**: the bar above the map narrows the filmstrip to the photos visible on the map, tagged or
  untagged.
- **Saved locations**: named circles, some of them private (their photos are exported without location).
- **Track logs**: a GPX file drawn on the map, and photos tagged from it by capture time.
- **The Info panel** shows a small map of the active photo's place.
- **Settings ▸ Map** chooses the tile server (OpenStreetMap, offline, or your own), shows the tile cache and clears it.

Nothing is fetched until a map is on screen, and nothing at all when tiles are off. There is no place search or
reverse geocoding (place names from positions): see LRC-MAP-REVGEO in the parity tracker.

## The Map view

View ▸ Map, or the map pin in the bottom bar (next to People), shows the photos in view (the current source, filter
and sort: the same photos as the grid) on a map, with the filmstrip below. Above the map is the location filter bar;
at its top left the Saved Locations list; at its top right the toolbar: **Tracklog**, **Hide Pins**, **Lock Markers**,
**Fit to Photos** and zoom out / in.

- **Moving**: drag to pan; the mouse wheel, a pinch or ⌘-scroll zooms around the pointer; a two-finger scroll pans; a
  double-click on the map zooms in. **=** / **−** (or ⌘= / ⌘−) zoom one level; **Fit to Photos** (⌘0) shows every
  photo with a position.
- The first time the map is shown it fits the photos with a position; after that it stays where you left it for the
  session.
- "N photos without GPS" at the bottom left counts the photos in view that have no position (drag them onto the map
  from the filmstrip, set one in the Info panel, or use Photo ▸ Auto-Tag from Tracklog…).
- The tile source's attribution is always shown at the bottom right. Tiles not loaded yet show a lower zoom's tile
  magnified, or grey. With tiles off the map is grey with the note "Map tiles are off (Settings ▸ Map)".

## Pins and clusters

- Each photo with a GPS position is a pin. Pins closer than about 60 px merge into a cluster showing how many photos
  it holds; zooming in splits clusters until every photo has its own pin. Pins of selected photos are blue, the active
  photo's has a thicker ring.
- A click on a pin or cluster selects its photos (the filmstrip shows them selected); ⌘-click or ⇧-click adds them to
  the selection; a double-click opens the first one in Detail. Resting the pointer on a pin shows its photo's
  thumbnail (the first photo's, with the count, for a cluster).
- **Hide Pins** (H, or the toolbar button, or View ▸ Map ▸ Pins) hides the photo pins and clusters, to see the
  map, the saved locations and a track log without them. Hidden pins can't be clicked, hovered or dragged; photos can
  still be dropped onto the map. The choice is saved with the window layout (`ui.json`).
- Photos whose GPS position is invalid (out of range, NaN) are counted as without GPS, never placed. Clusters are
  recomputed only when the view moves, from positions projected once per change of the photo list: 50 000 photos on
  screen take about 4 ms (release build), a few thousand well under 1 ms.

## The map in the Info panel

For a photo with a GPS position, the Info panel shows a small map under the location fields: the panel's width,
160 px tall, centred on the photo at zoom 14 (or the tile source's deepest zoom if less), with the photo's pin and the
tile source's attribution at its bottom right.

- With several photos selected it shows the active photo's place (as Lightroom does), and fetches nothing for the
  others. A photo without a position (or with an invalid one) has no map.
- It doesn't pan or zoom, so a drag or a scroll over it scrolls the panel. A click opens the Map view centred on the
  photo at the same zoom. **Open in Browser** below it shows the place on openstreetmap.org instead.
- It shares the Map view's tiles: one texture cache, one tile service. A tile one of them loaded is drawn by the other
  without being fetched or uploaded again, and with both on screen neither cancels the other's tiles.
- It fetches only while it is on screen (the Info panel open and the map scrolled into view), exactly the tiles it
  draws.
- With tiles off it stays in place, grey, with the pin and the note "Map tiles are off".
- A photo inside a private saved location has its map hidden (see Saved locations).

## Geotagging: putting photos on the map

As in Lightroom Classic's Map module:

- **Drop photos on the map**: drag photos from the filmstrip onto the map and let go where they were taken. A drag
  takes the selected photos (or, when the photo you grab isn't selected, just that one); while they are over the map
  a pin with their count follows the pointer, and the drop sets every one of them to the point under it. Photos that
  already had a position are moved (Lightroom does the same). One undo step (Edit ▸ Undo "Set GPS Coordinates")
  puts all of them back where they were, or off the map.
- **Move a pin**: drag a single photo's pin to its new place (one undo step). A cluster isn't moved: dragging it pans
  the map, so zoom in until the photo has its own pin. A pin dropped outside the map stays where it was.
- **Lock Markers** (⌘K, the toolbar button, View ▸ Map) stops pins and saved-location handles from being moved;
  dragging one then pans the map. It is saved with the window layout (`ui.json`).
- **Remove GPS Coordinates**: Photo ▸ Remove GPS Coordinates clears the position of the selected photos; a right-click
  on a pin offers the same for that pin's photos. One undo step.
- The map can't show the poles (Web Mercator stops at ±85.05° latitude), so a drop reaches 85.05° north or south at
  most; drop just inside the top edge to get the edge's latitude. Longitudes wrap around the world. Type a position
  closer to a pole in the Info panel's GPS field.
- Where positions are saved: in the library, and in XMP sidecars (EXIF GPS: `exif:GPSLatitude` /
  `exif:GPSLongitude`) when they are written (Photo ▸ Save Metadata to File, or at once with File ▸ Automatically
  Write Changes into XMP on). A position you set or clear in LightCraft replaces the sidecar's, so a cleared one (or an
  undone geotag) is removed from it; a sidecar's position for a photo whose position you never touched in LightCraft
  is left alone.
- Dropping files from the system's file manager onto the map isn't supported (they import, as anywhere in the app).

## The location filter

The bar above the map, as Lightroom Classic's Location Filter, narrows the filmstrip by where the photos are (`\`
hides or shows the bar; a hidden bar keeps its choice):

| Choice | The filmstrip shows |
|---|---|
| **Visible on Map** | the photos whose position is inside the map's view; it follows the map as you pan and zoom, once the view rests for a quarter of a second |
| **Tagged** | the photos with a GPS position |
| **Untagged** | the photos without one (the map shows no pins; drag them onto it to place them) |
| **None** | every photo in view (the default) |

- Each choice shows its count of the photos in view; the one chosen is highlighted. The counts follow the map as it
  moves (Visible on Map's at once; the filmstrip when the view rests).
- It narrows on top of the library's filter and search, which it never changes: leaving the Map view brings back
  exactly the photos you had. Coming back to the map, the choice is still there (for the session). The selection, ⌘A
  and the arrow keys follow the filmstrip.
- Pins come from every photo with a position in view, so in Visible on Map the pins on screen and the filmstrip are the
  same photos.
- Across the antimeridian (a view of the Pacific) the area wraps around ±180°; zoomed out past the whole world it holds
  every longitude, and a view reaching the map's top or bottom edge reaches the pole. A photo whose position is invalid
  counts as untagged.
- Counting is one pass over the positions already projected for the pins (20 000 photos: well under 1 ms).

The bar is built on two smart-album rule fields that any filter or smart album can use (Location group of the rule
editor; `library.filter {ruleSet}`, `album.createSmart`, `album.setRules`):

| Field | Operators | Value |
|---|---|---|
| `hasGps` (Has GPS) | is | yes / no: the photo has a valid position |
| `gpsArea` (GPS Area) | `inside`, `outside` | `{south, west, north, east}` in degrees: latitudes −90…90 (south not north of north), longitudes −180…180, all finite; `east < west` wraps across ±180° (`{west: 170, east: -170}` is the 20° around it), `west: -180, east: 180` is every longitude. Edges count as inside. A photo without a position is neither inside nor outside |

Anything else (a missing edge, text, NaN, out of range, south north of north) is refused by the commands and marked in
the rule editor.

## Saved locations

As Lightroom Classic's Saved Locations: a saved location is a name, a centre and a radius (1 m to 1000 km), drawn on
the map as a circle with its name above it. A location can be **private**.

- **The list** sits over the map's top-left corner: every saved location with the number of photos in view inside it,
  private ones marked "private" and drawn with a dashed purple edge. A click on its header, **O**, or View ▸ Map ▸
  Saved Locations ▸ Location List folds it to its header. It is on the map rather than in a docked panel because the
  docked panels are shared by every view; the map's canvas can't be closed, so the list is there whenever the map is.
  On a map narrower than about 470 points or lower than 200 it waits until there is room (the circles and the
  commands still work).
- **A click on a row** selects the location and centres the map on it at a zoom that holds the whole circle. **The
  checkbox** selects the photos in view inside it. Right-click a row for Go to Location, Select Photos in Location,
  Rename…, Private and Delete Location… (which asks first: there is no undo for it).
- **New Location** (+ in the list's header, or View ▸ Map ▸ Saved Locations ▸ New Location…): at the map's centre (a
  radius of a sixth of the map's shorter side), or from the selection (centred on the selected photos' positions, with
  a radius that holds them all, 10 % to spare, at least 100 m). Either asks for the name.
- **On the map**, a click on a location's centre dot or its name selects it; a click on the empty map drops the
  selection. The selected location has a handle at its centre (drag to move it) and one on its east edge (drag to
  change the radius), saved when the button is released over the map. Lock Markers locks these handles too.
- **Measured on the Earth**: "inside" means within the radius along the Earth's surface (great-circle distance on a
  sphere of 6 371 km), edge included, so a circle across the antimeridian holds photos on both sides of ±180° and one
  that covers a pole holds every longitude near it, and both are drawn whole.

**Private locations.** A photo inside any private location:

- is exported **without its location**, exactly as if Export ▸ "Remove location info" were on for that photo alone:
  no GPS position, and no sublocation / city / state / country fields. The photo's position in the library is never
  touched; making the location public again (or deleting it) brings the location back to later exports. This applies
  to the rendered formats (JPEG, PNG, TIFF, AVIF…); **Original** and **DNG** exports copy the file (with its own
  embedded EXIF GPS) as it is, so choose a rendered format for photos you want to share without their place;
- hides the **Info panel's map** ("In the private location “Home”: its map is hidden", with **Show Map**, which shows
  it for that photo until another one becomes active). The GPS field and Open in Browser stay, and the Map view still
  shows the photo's pin: private is about what leaves the computer and what is on screen by default.

**Where they are kept.** Saved locations belong to the library, in `locations.json` next to the catalog (written
atomically on every change). They are not part of the catalog's journal: changing them is not an undo step, so Delete
Location asks first. The file is checked as it is read: if it doesn't parse it is kept aside as
`locations.json.corrupt-<time>` and the library opens without saved locations (a warning says so); invalid entries are
left out, and the file is kept aside the same way before it is written again. A library holds at most 10 000 saved
locations; counting them uses a latitude-sorted index, recounted only when the photos or the locations change
(3 000 locations: about 16 ms in a test build). Folders of saved locations (Lightroom's location folders) aren't there
yet.

**Smart albums and filters** gain **GPS Radius** (`gpsRadius`, Location group): `inside` / `outside` a circle
`{lat, lon, radius}` (degrees, metres; the same checks), measured the same way.

## Track logs

As in Lightroom Classic's Map module, a GPS track log (a GPX 1.0 / 1.1 file from a phone app, watch or logger) can be
drawn on the map and used to tag photos. The **Tracklog** button (top right of the map) and View ▸ Map ▸ Tracklog have
its commands.

- **Load Tracklog…** opens a `.gpx` file. Its tracks are drawn as pink lines and the map fits them. Each continuous
  logging span (a GPX track segment) is its own line: a gap where the logger was off is not joined. A line crossing the
  ±180° meridian is drawn the short way across it.
- A file with several tracks lists them in the Tracklog menu, with **All Tracks** (the default). Picking one draws the
  others dimmed, fits the map to it and makes it the one tagging uses.
- The menu shows the active track's **start, end and duration** on the camera's clock: GPX times are UTC, so they are
  shifted by the **time zone offset** (Set Time Zone Offset…, e.g. `+02:00` or `-5`); with none set they are UTC and
  say so. A photo whose Exif records its own offset uses that.
- **Auto-Tag N Selected Photos** tags the selected photos from the active track by capture time (as Photo ▸ Auto-Tag
  from Tracklog… does): interpolated between the points of a segment, else the nearest point within 10 minutes, never
  across a gap; photos that already have a position keep it. Without an offset it asks for the camera's time zone
  first. One undo step; a message says how many were tagged and how many fell outside the track's time range.
- The selected photos are **marked on the track** (white dots) where Auto-Tag would put them, so a wrong offset shows
  before anything is written (up to 5 000 marks).
- **Fit Map to Tracklog**, **Load Another Tracklog…** and **Clear Tracklog** are in the same menu.
- The track log belongs to the session: it is not saved and is gone after a restart.

Limits and errors: a file larger than 50 MB or with more than 2 million timed points is refused with a message saying
so, as are a file that isn't GPX, one without tracks, and one whose points have no times; the track already loaded
stays. Points with invalid coordinates are skipped; points beyond ±85.05° latitude are drawn on the map's edge;
timestamps out of order are sorted. Each track is projected and ranked for simplification (Douglas–Peucker) once when
loaded, so no dropped point is more than 0.6 px from the line drawn; on a 2-million-point track (release build) loading
takes about 230 ms, a zoom level 1–10 ms, a frame's clipping well under 1 ms.

## Keys and menus

The Map view's keys, as in Lightroom Classic's Map module where they exist there. They work in the Map view only: H,
`\`, O and ⌘K mean something else in the other views (Remove, Show Original or the grids' Filter Bar, the mask
overlay, Add Keywords) and keep that meaning there;
in the Map view the map's command wins. Every key can be changed in Help ▸ Keyboard Shortcuts.

| Key | Command | Does |
|---|---|---|
| H | `map.showPins` | Hide / show the photo pins (LightCraft's; Classic's H hides Develop's edit pins) |
| ⌘K | `map.lockMarkers` | Lock Markers on / off (Classic: the same) |
| `\` | `map.filterBar` | Hide / show the location filter bar (Classic: the same) |
| O | `map.showLocations` | Fold / unfold the Saved Locations list (Classic's O shows / hides the saved-location overlay) |
| = / − (also ⌘= / ⌘−) | `map.zoomIn` / `map.zoomOut` | Zoom one level |
| ⌘0 | `map.fit` | Fit to Photos (Zoom to Fit's key, which the Map view turns into Fit to Photos) |

Not taken from Classic: I (map info overlay; LightCraft's I opens Info), ⌘F (map search: no geocoding), ⌘1–6 (map
styles: one style), ⌫ (Classic deletes the GPS of the selected photos; LightCraft keeps ⌫ for moving photos to Recently
Deleted, and Remove GPS Coordinates is in the Photo menu and on a pin's right-click), ⌥-drag zoom.

**View ▸ Map** has every map command (LightCraft has one menu bar for all its views, so the Map module's menus gather
here; the items are greyed outside the Map view, except Load Tracklog… and Map Tiles…):

- Fit to Photos, Zoom In, Zoom Out
- Pins, Lock Markers, Location Filter Bar, Location Filter ▸ Visible on Map / Tagged / Untagged / None
- Saved Locations ▸ New Location…, Location List
- Tracklog ▸ Load Tracklog…, Auto-Tag Selected Photos, Set Time Zone Offset…, Fit Map to Tracklog, Clear Tracklog
- Map Tiles… (opens Settings ▸ Map)

Photo ▸ Auto-Tag from Tracklog… and Photo ▸ Remove GPS Coordinates work in every view.

## Tile source and cache

### Settings ▸ Map

Edit ▸ Settings… ▸ Map (or View ▸ Map ▸ Map Tiles…):

- **Map tiles**: **OpenStreetMap** (the default), **Offline (no tiles)** (no network at all; the map is grey and shows
  only what LightCraft draws on it), or **Custom server**: a URL template, its attribution and its max zoom. A custom
  server is used only once you press **Apply** (or Return in one of its fields): while you type, the map keeps the
  source it has, so a half-typed address is never asked for tiles. A template that doesn't pass the checks below is
  refused with the reason and changes nothing.
- **Tile cache**: its size, and **Clear Cache**.
- A short **privacy** note and a link to this page.

The tile source is a library preference (saved in the library's `prefs.json`), set by `map.tileSource` (Settings ▸ Map
uses it too):

| Params | Source |
|---|---|
| `{}` | Report the current source: `{id, urlTemplate, attribution, maxZoom, userAgent, offline}` |
| `{id: "osm"}` | OpenStreetMap's standard tiles (the default; max zoom 19) |
| `{offline: true}` | Offline: no network, no tiles |
| `{urlTemplate, attribution, maxZoom?, id?}` | Another raster tile server, e.g. one you host. The template must start with `https://` and contain `{z}`, `{x}` and `{y}` exactly once (no other placeholders); the attribution is required (it is shown on the map); `maxZoom` is 0–24 (default 18); `id` names its cache folder (`[a-z0-9._-]`, derived from the template when absent) |

Fields given alone change the current source (e.g. `{maxZoom: 16}`). Anything that doesn't pass these checks is
refused with an error and changes nothing; a hand-edited `prefs.json` with an unusable source falls back to
OpenStreetMap. Respect the terms of any server you configure: many commercial providers need an API key and forbid
some uses, and Google, Apple, Bing and Esri imagery may not be used this way.

### What is fetched, and when

- **Only map tiles**: 256 × 256 px images of the map, by zoom level and position
  (`https://tile.openstreetmap.org/{z}/{x}/{y}.png` by default). No photos, GPS positions, file names or account data
  are ever sent.
- **Only while a map is on screen**, and **only the tiles you are looking at**: the tiles the view draws plus at most
  one ring of tiles around them (none around the Info panel's map), at the zoom level shown. When no map is on screen
  any more, the tiles it asked for that haven't started are dropped. LightCraft never prefetches other zoom levels or
  areas, and has no "download this area for offline use". Nothing is fetched at startup.
- **Newest view first**: when you pan or zoom, tiles that left the view and haven't started are dropped; at most two
  requests run at once.
- **Gently**: after an error, a `429 Too Many Requests` or a server error, LightCraft waits (1 s, doubling up to
  5 minutes, and at least as long as the server's `Retry-After`) and then tries one request at a time until one
  succeeds. A tile the server refuses (e.g. 404) or that isn't a valid image isn't asked for again for 10 minutes.
- Tiles larger than 2 MB, images that aren't PNG / JPEG / WebP of at most 1024 px a side, and anything that fails to
  decode are errors (the map shows its placeholder), never a crash.

### OpenStreetMap's tile policy

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

Access is best effort: the OSMF can limit or block any app without notice. If that happens, choose another tile
server or Offline in Settings ▸ Map.

### The tile cache

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
fetched again. The app, `lightcraft-cli` and the MCP server share the folder. **Clear Cache** (Settings ▸ Map,
`map.clearCache`) deletes every cached tile of every source; only `.tile` / `.part` files in the cache's own layout
are deleted, so pointing `LIGHTCRAFT_MAP_CACHE` somewhere odd can't delete anything else.

The web build has no tile cache and fetches no tiles yet.

## Privacy

- Looking at a map tells the tile server (the OpenStreetMap Foundation, by default) which areas you look at, at which
  zoom, from your IP address, with LightCraft's name and version (the User-Agent), like any web map does; see the
  OSMF's [privacy policy](https://osmfoundation.org/wiki/Privacy_Policy).
- Your photos and their GPS positions stay on your computer: pins, saved locations and track logs are drawn locally,
  and nothing is sent while no map is on screen.
- If you would rather nothing about your map views left your computer, choose **Offline** in Settings ▸ Map
  (`map.tileSource {offline: true}`) or a tile server you run yourself.
- What leaves with your photos is a separate matter: exports keep GPS unless Export ▸ "Remove location info" is on, or
  the photo is inside a private saved location.

## Commands

Library commands work in the app, `lightcraft-cli` and the headless MCP server; UI commands need the map's window, so
the app, `lightcraft-cli snapshot` and `mcp --connect` have them. `{on?}` sets a toggle, or flips it without `on`.

| Command | Kind | Params | Result |
|---|---|---|---|
| `view.map` | UI | — | Switch to the Map view → `{withGps, withoutGps}` |
| `map.fit` | UI | — | Fit the photos with a position (the world when there are none) → the map state |
| `map.setView` | UI | `{lat?, lon?, zoom?}` | Centre (degrees) and zoom (0 to the source's max zoom; fractional allowed); what is left out stays → the map state |
| `map.zoomIn` / `map.zoomOut` | UI | — | One zoom level in / out, around the centre → the map state |
| `map.showPins` | UI | `{on?}` | Show / hide the photo pins → `{on}` |
| `map.lockMarkers` | UI | `{on?}` | Lock Markers → `{on}` |
| `map.filterBar` | UI | `{on?}` | Show / hide the location filter bar → `{on}` |
| `map.showLocations` | UI | `{on?}` | Unfold / fold the Saved Locations list → `{on}` |
| `map.locationFilter` | UI | `{mode?: "visible" \| "tagged" \| "untagged" \| "none"}` | Choose the filter (without `mode`: report) → `{mode, visible, tagged, untagged, all, area, countMs, shown}` (`area`: what Visible on Map narrows to, null otherwise; `shown`: photos the filmstrip has) |
| `map.state` | UI | — | What the map shows (below) |
| `map.settings` | UI | — | Open Settings ▸ Map |
| `photo.setGps` | library | `{ids?, lat, lon}` | Set the position of the photos (default: the selection): `lat` −90…90, `lon` in degrees (wrapped into −180…180); anything else, or ids that are none of the library's photos, is refused; one undo step → `{changed, lat, lon}` |
| `photo.removeGps` | library | `{ids?}` | Clear the position of the photos (default: the selection); one undo step → `{changed}` |
| `map.locations` | library | — | `{locations: [{id, name, lat, lon, radius, private, photos, inView}], count, countMs}`: `photos` inside it in the library (deleted photos not counted), `inView` of the photos in view |
| `map.location.add` | library | `{name, lat, lon, radius?, private?}` or `{name, fromSelection: true, ids?, radius?, private?}` | Save a location: `name` 1–200 characters (trimmed), `lat` −90…90, `lon` −180…180, `radius` in metres 1–1 000 000 (default 1000), all finite; `fromSelection` centres it on the selected (or `ids`) photos' positions with a radius that holds them → the location with `photos` and `inView` |
| `map.location.update` | library | `{id, name?, lat?, lon?, radius?, private?}` | Change a location, checked as a whole (nothing changes when any part is refused) → the location with its counts |
| `map.location.remove` | library | `{id}` | Delete it (the photos keep their positions) → `{removed, name}` |
| `map.location.photos` | library | `{id, inView?}` | `{id, ids, count}`: the library's photos inside it (`inView: true`: only the photos in view, in their order) |
| `map.location.new` | UI | `{fromSelection?}` | Ask for a name, then save a location at the map's centre (or around the selection) |
| `map.location.show` | UI | `{id}` | Select it and centre the map on it → the map state |
| `map.location.select` | UI | `{id}` or `{id: null}` | Select it on the map (handles), or none → `{selected}` |
| `map.location.selectPhotos` | UI | `{id}` | Select the photos in view inside it (the checkbox) → `{selected}` |
| `map.trackLog` | library | `{path}` / `{gpx}`, `{active: n \| "all"}`, `{offset: "+02:00" \| hours \| null}`, `{clear: true}`, `{}` | Load a GPX file or text (the Map view fits it; `fit: false` keeps the view), pick the active track, set the camera clock's offset from UTC, remove the track log, or report. Several params combine; a refused one changes nothing → `{loaded, path, name, points, untimed, tracks: [{index, name, points, segments, start, end, duration}], active, activePoints, start, end, duration, offset, startLocal, endLocal, loadMs, generation}` |
| `map.loadTrackLog` | UI | `{path?}` | Load a track log (asks for the file without `path`) |
| `map.trackLog.autoTag` | UI | `{ids?, offset?}` | Auto-Tag the selected photos from the active track (asks for the offset when none is set) |
| `map.trackLog.setOffset` | UI | `{offset?}` | Set the time zone offset (asks without `offset`) |
| `map.trackLog.fit` | UI | — | Fit the map to the track log |
| `photo.autoTagTracklog` | library | `{loaded: true, …}` (or a file) | Tag photos from a track log by capture time (its active track, and its offset unless `offset` is given) |
| `map.tileSource` | library | see Tile source and cache | The tile source |
| `map.cacheInfo` | library | — | `{bytes, tiles, capBytes, dir}` (`dir` null: no disk cache) |
| `map.clearCache` | library | — | Delete every cached tile (all sources) → `{bytes, tiles, dir}` (what was deleted) |

`map.state` → `{lat, lon, zoom, maxZoom, width, height, tileZoom, source, offline, attribution, withGps, withoutGps,
clusters: [{ids, count, x, y, lat, lon, selected}], clusterCount, clusterMs, lockMarkers, showPins, filterBar,
locationList, locationFilter, movingPin, drop, tiles: {drawn, missing, textures, uploads}, miniMap, locations,
selectedLocation, movingLocation, privateMiniMap, trackLog}`: `x`, `y` are screen points (for `ui.click`), at most 500
clusters listed (none while pins are hidden); `movingPin` (`{id, x, y}`) while a pin is dragged; `drop` (`{count, x,
y, lat, lon}`) while photos are dragged over the map; `miniMap` the Info panel's map (`{photo, lat, lon, zoom, x, y,
width, height, pin: {x, y}, tiles: {drawn, missing}}`, null when it isn't drawn); `locations` those drawn (`{id, name,
private, x, y, radiusPx, handle: {x, y}, inView, selected}`); `movingLocation` (`{id, lat, lon, radius, part}`) while a
handle is dragged; `privateMiniMap` (`{photo, location}`) while the Info panel hides its map; `trackLog` the
`map.trackLog` report plus what is drawn (`drawn: {lines, points, levelPoints, zoom, levelMs, clipMs}`, `ticks: [{id,
x, y}]`, `onTrack`). When `locations.json` couldn't be written a location command's result carries `notSaved` with the
reason.

Widgets (`ui.widgets`): `map:canvas`, `map:pin:<first photo id>`, `map:fit`, `icon:mapZoomIn`, `icon:mapZoomOut`,
`map:hidePins`, `map:lockMarkers`, `map:attribution`, `map:withoutGps`, `map:hoverThumb`, `map:pinMenu:removeGps`;
the filter bar `map:filterBar`, `map:filter:visible`, `…:tagged`, `…:untagged`, `…:none`; saved locations
`map:locations`, `map:locationsHeader`, `icon:locationNew`, `map:locationNew:centre`, `map:locationNew:selection`,
`map:location:<id>`, `map:location:<id>:check`, `map:location:<id>:count`, `map:location:<id>:centre`,
`map:location:<id>:radius`, `map:locationMenu:show`, `…:selectPhotos`, `…:rename`, `…:private`, `…:delete`,
`button:showPrivateMap`; track logs `map:tracklog`, `map:tracklog:load`, `…:track:all`, `…:track:<index>`, `…:info`,
`…:offset`, `…:autoTag`, `…:fit`, `…:clear`; the Info panel `info:miniMap`, `info:miniMapAttribution`; Settings ▸ Map
`button:settingsMapSource-0` (OpenStreetMap), `-1` (Offline), `-2` (Custom), `field:settingsMapUrl`,
`field:settingsMapAttribution`, `field:settingsMapMaxZoom`, `button:settingsMapApply`, `button:settingsMapClearCache`,
`link:settingsMapDocs`; the menu `menusub:Map`. Agents can hold a drag across requests with `ui.pointer {screen: true,
events: [{kind: "down", …}, {kind: "drag", …}]}` and drop it with `{kind: "up"}` (see the control protocol).
