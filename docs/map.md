# Map

The Map view (View ▸ Map, ⌘⌥3 / Ctrl+Alt+3) shows the photos that have a GPS position on a slippy map.
This page describes where the map's tiles come from, what is sent over the network, and the commands
behind it. Later sections (pins and clusters, the Info panel map, geotagging, the location filter, saved
locations, track logs, keys and menus) are filled in as those parts land.

## Map view

The filmstrip runs under the map, as in Detail: View ▸ Filmstrip, `/` (F6), or the Filmstrip button at the
right of the bottom bar, which in the Map view takes the place of the grid's thumbnail size. It shows the
same photos as the grid. The selection shows both ways:

- Selected photos' pins are yellow. A cluster holding selected photos is yellow and carries a small blue
  bubble with how many of its photos are selected.
- A click on a pin selects its photo; a click on a cluster selects all its photos, the first one active.
  The filmstrip shows them selected and scrolls to the active one. ⌘-click (Ctrl-click) adds them to the
  selection instead. A double-click on a pin opens Detail; on a cluster it zooms in.

`map.state` reports the view (`lat`, `lon`, `zoom`), the map canvas, whether the filmstrip is drawn, every pin
or cluster drawn in the last frame (`ids`, `count`, `selected`, screen `x` / `y`) and a drop in progress.

## Geotagging

Drag photos from the filmstrip onto the map to set where they were taken. The drag carries the selection,
or only the photo dragged when it isn't selected. While they are over the map a pin with their count
follows the pointer; release to give every one of them that point's position, overwriting a position they
had. That is one undo step, "Set GPS Coordinates" (`photo.setGps`). A release over the map's toolbar or
location filter bar, or outside the map, changes nothing. Drops are limited to the map's ±85.05° latitude
(the edge of the Web Mercator projection); the longitude of a drop on the world's repeat at low zoom is
wrapped into −180…180.

Not yet: moving a pin by dragging it, Lock Markers stopping that, and Remove GPS Coordinates in the Photo
menu and a pin's menu (`photo.clearGps` removes a position today).

## Tile sources and cache

The tiles are raster images in the Web Mercator projection (256 px, zoom 0–19). Where they come from is a
library preference (`prefs.json` → `mapSource`), set with the Map Style list at the bottom of the map or
the `map.tileSource` command:

| Source | Id | Tiles | Max zoom | Credit shown on the map |
|--------|----|-------|----------|-------------------------|
| OpenStreetMap (default) | `osm` | `https://tile.openstreetmap.org/{z}/{x}/{y}.png` | 19 | © OpenStreetMap contributors |
| OpenTopoMap | `opentopomap` | `https://tile.opentopomap.org/{z}/{x}/{y}.png` | 17 | Map data: © OpenStreetMap contributors, SRTM \| Map style: © OpenTopoMap (CC-BY-SA) |
| Offline | `offline` | none: a grey map, nothing is fetched and no tile service runs | 19 | — |
| Custom | your id | an `https://…/{z}/{x}/{y}…` template you give, with its credit | 0–24 | the credit you give (required) |

The credit of the source in use is always painted at the bottom right of the map, never behind a toggle.
A stored source is checked again when the library opens: one that is damaged or not `https://` falls back
to OpenStreetMap and the rest of `prefs.json` is kept.

**Cache.** Tiles are cached on disk in the per-user cache folder: `~/Library/Caches/LightCraft/maptiles` on
macOS, `%LOCALAPPDATA%\LightCraft\Cache\maptiles` on Windows, `$XDG_CACHE_HOME/lightcraft/maptiles` (else
`~/.cache/lightcraft/maptiles`) elsewhere, `<exe dir>/LightCraftData/Cache/maptiles` in portable mode, or
`$LIGHTCRAFT_MAP_CACHE` when set. Each source has its own subfolder. The cache follows the server's
`Cache-Control` / `Expires` headers (7 days when there are none), refreshes an expired tile with a
conditional request (`If-None-Match` / `If-Modified-Since`; a `304` only renews it) while still showing it,
writes atomically (a temporary file renamed into place), deletes and fetches again an entry that is
damaged, and stays under 500 MB by deleting the least recently used tiles. `map.cacheInfo` reports its size;
`map.clearCache` empties it.

On screen, up to 256 tile textures are kept (least recently used dropped first); a tile that is not
loaded yet is drawn from a loaded tile up to four zoom levels above it, else the map's grey shows.

## OpenStreetMap tile usage policy

LightCraft uses OpenStreetMap's tile server under its tile usage policy
(<https://operations.osmfoundation.org/policies/tiles/>, read 2026-11-14):

- **Only what is in view.** The tiles requested are those drawn plus at most one ring around them, at the
  zoom drawn. There is no prefetching, no "download area" and no offline download. When the view moves,
  queued requests that left it are dropped before they start, and the newest view's tiles nearest the
  centre go first. When no map is on screen for two frames, the queue is emptied.
- **Gently.** At most two requests at a time. After an error, a `429` or a `5xx` the host is left alone with
  exponential backoff (1 s doubling up to 5 min, at least its `Retry-After`), then asked one request at a
  time until it answers. A tile the server refuses (404, not an image) is not asked for again for 10 min.
- **Identified.** Every request carries `User-Agent: LightCraft/<version> (+https://github.com/storytold/lightcraft)`,
  and nothing about you. Requests never send `Cache-Control: no-cache` or `Pragma`.
- **Cached** as described above, honouring the server's headers.
- **Checked.** A response over 2 MiB, one that is not a PNG, JPEG or WebP image of at most 1024 px a side,
  or one that does not decode is an error for that tile, never a crash, and is not cached.
- **Visible.** While tiles are queued or loading, one "Loading map tiles" row is on the activity stack.

Sessions that a host has not put online (tests, the web build) never fetch tiles.

## Esri layers (your own ArcGIS key)

Two Esri map styles are offered once you give LightCraft your own ArcGIS Location Platform API key:
**Esri Streets** (`esri-streets`, Esri's `arcgis/streets` style) and **Esri Outdoor (topographic)**
(`esri-topo`, the `arcgis/outdoor` style). This is the "bring your own key" pattern of QGIS, darktable and
digiKam: LightCraft has no Esri account of its own, and your tile use counts against your key.

**Setting it up.** Create a free ArcGIS Location Platform account at <https://location.arcgis.com>, create an
API key with the static basemap tiles privilege (`premium:user:staticbasemaptiles`) and nothing else, and paste
it in Settings ▸ Map ▸ ArcGIS API key, then press Return or Apply (`map.setEsriKey {key}`; `""` removes it).
The key is checked for its shape only; nothing is sent until a map shows an Esri style. The Esri styles then
appear in the Map Style list at the bottom of the map.

**Without a key** the Esri styles are not in the Map Style list, `map.tileSource` refuses them with a message
saying a key is needed and where to get one, and nothing is fetched from Esri. A library saved with an Esri
style opens on OpenStreetMap with a one-line notice; `prefs.json` keeps the Esri choice, which comes back once
a key is set.

**The key is a secret.** It is kept in LightCraft's settings folder in `map-keys.json` (readable only by you
on macOS and Linux), never in the library, `prefs.json` or `ui.json`. It is not journaled, never logged, never
shown again after you apply it (Settings shows only whether one is set), and `map.tileSource`,
`map.providers` and `map.setEsriKey` report only `esriKeySet: true | false`. It is sent only to Esri's tile
server, as the `token` query parameter of tile and metadata requests. The tile cache is keyed by style and
tile (`maptiles/esri-streets/<z>/<x>/<y>.tile`), never by URL, so the key is in no file name or cache entry;
an error message that would quote a request URL has the key replaced by `[key]`.

**The service.** Tiles come from Esri's static basemap tiles service,
`https://static-map-tiles-api.arcgis.com/arcgis/rest/services/static-basemap-tiles-service/v1/arcgis/<style>/static/tile/{z}/{y}/{x}?token=<key>`
(read 2026-11-15: <https://developers.arcgis.com/rest/static-basemap-tiles/>,
<https://developers.arcgis.com/rest/static-basemap-tiles/arcgis-streets-tile-get/>). Its tiles are 512 px PNGs on
the same grid as a 256 px pyramid one level down ("zoom levels are 1 less"), so LightCraft fetches the 512 px
tile once and cuts the four 256 px tiles it holds out of it; at zoom 0 it scales the world tile down. The same
fetch queue as for OpenStreetMap applies (two requests at a time, backoff, cache with the server's
`Cache-Control`). The keyless `services.arcgisonline.com` / `server.arcgisonline.com` endpoints are not used:
Esri's terms summary (<https://www.esri.com/content/dam/arcgisonline/docs/tou_summary.pdf>, April 21, 2025)
allows them only with Esri software or an ArcGIS subscription.

**No imagery.** The static tiles service has no satellite imagery style (only `arcgis/imagery/labels`, a
labels overlay), and Esri's `arcgis/topographic` and `arcgis/imagery` styles exist only as vector styles, which
this renderer can't draw. Earlier builds of this branch offered World Imagery and World Topographic Map from the
keyless endpoints; a library saved with World Imagery opens on OpenStreetMap (its saved value can't be read
any more). Outdoor is Esri's raster topographic style.

**Credit.** While an Esri style is shown the map's credit is "Powered by Esri" followed by the style's data
credits, taken from the style's metadata (`…/arcgis/<style>/static?f=json`, its `copyrightText`), fetched once
per style with the key. The answer is checked: at most 64 KiB, JSON, one line of plain text of at most 400
characters, no markup. Until it arrives, or when it fails, the credit is the providers the service's
documentation lists: "Powered by Esri | Esri, TomTom, Garmin, FAO, NOAA, USGS, © OpenStreetMap contributors,
and the GIS User Community". The service's page says: "Applications that use this service must display both
basemap and data attribution."

**Terms: what was checked and what wasn't.** Read on 2026-11-15: the static tiles pages above (tile URL, 512
px PNG, zoom 0–22, `token` parameter, the privilege, "2 million free then $0.15 per 1,000 tiles", metadata
calls free) and <https://developers.arcgis.com/documentation/security-and-authentication/api-key-authentication/>
(API keys "are typically embedded directly into client applications", usage "is billed to the ArcGIS
subscription … associated with the developer's account who created the key"). Not verified, because the pages
were not readable: Esri's attribution guide (<https://developers.arcgis.com/documentation/esri-and-data-attribution/>
returned an empty page) and the ArcGIS Location Platform Agreement
(<https://www.esri.com/content/dam/esrisites/en-us/media/legal/platform/platform-legal.pdf>), so whether its
terms limit keeping tiles in a local cache is not confirmed. Your key, your account: read those terms before
use.

**Custom templates.** A custom tile source (`map.tileSource {urlTemplate, attribution}`) may carry your own
key in its URL, for any provider, on your own responsibility. It is stored as you typed it in `prefs.json`
(it is the source's URL), and it stays checked: `https://` only, `{z}`, `{x}` and `{y}` each once, at most
512 characters.

**Other keyless providers (checked, none added).** CARTO's free basemaps are for non-commercial use only;
OpenFreeMap serves vector tiles only, which this renderer can't draw. OpenStreetMap stays the default and
OpenTopoMap the other keyless choice.

## Privacy

The tile server sees which tiles you look at (so roughly which places), your IP address and the
User-Agent above. Nothing else leaves the machine: no photo, place name or coordinate is sent except as the
tile `z/x/y` in the URL. Reverse geocoding (place names from GPS) is offline (`metadata.reverseGeocode`,
GeoNames). Choose Offline to fetch nothing.

## Commands

| Command | What it does |
|---------|--------------|
| `map.tileSource` | `{}` reports the source; `{id: "osm" \| "opentopomap" \| "esri-streets" \| "esri-topo" \| "offline"}`, `{offline: true \| false}`, or `{urlTemplate, attribution, maxZoom?, id?}` sets it (saved in `prefs.json`); the Esri styles need a key → `{…, esriKeySet}` |
| `map.setEsriKey` | `{key}`: sets your ArcGIS API key (`""` or null removes it); shape-checked only, never journaled or reported → `{esriKeySet, notice}` |
| `map.providers` | The sources offered now, with credit and max zoom |
| `map.cacheInfo` | `{bytes, tiles, capBytes, dir}` of the tile cache |
| `map.clearCache` | Deletes every cached tile |
| `map.state` | The view, the pins drawn (with how many of their photos are selected) and a drop in progress (app only) |
| `photo.setGps` | `{ids?, lat, lon}`: sets the position of the photos (default: the selection), one undo step; lat −90…90, lon wrapped into −180…180; unknown ids are refused |
| `photo.clearGps` | `{ids?}`: removes the position of the photos, one undo step |

The tile core (Web Mercator maths, source checks, cache, fetch queue) is ported from PR #797 by Jason
Baldwin.
