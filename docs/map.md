# Map

The Map view (View ▸ Map, ⌘⌥3 / Ctrl+Alt+3) shows the photos that have a GPS position on a slippy map.
This page describes where the map's tiles come from, what is sent over the network, and the commands
behind it. Later sections (pins and clusters, the Info panel map, geotagging, the location filter, saved
locations, track logs, keys and menus) are filled in as those parts land.

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

## Esri layers (off)

Earlier builds of this branch offered Esri's World Imagery, World Street Map, World Topographic Map and a
Hybrid layer, fetched from `server.arcgisonline.com` without an account. They are **off and cannot be
turned on** in this build: the Map Style list does not offer them, `map.tileSource` refuses them with an
error naming this page, and nothing is ever fetched from Esri. A library saved with an Esri layer opens on
OpenStreetMap with a one-line notice; `prefs.json` keeps the Esri choice.

Why: Esri's terms for these layers do not allow this use. Each layer's ArcGIS Online item
(e.g. <https://www.arcgis.com/home/item.html?id=10df2279f9684e4a9f6a7f08febac2a9> for World Imagery) says it is
"licensed under the Esri Master License Agreement" and links its terms. The terms summary
(<https://www.esri.com/content/dam/arcgisonline/docs/tou_summary.pdf>, "Last Updated: April 21, 2025", read
2026-11-14) lists as conditions "Use with Esri software and comply with its terms of use. If you do not have
Esri software, you must purchase an ArcGIS Online subscription", and among what you may not do,
"Systematically harvest basemap tiles through any method other than using Esri Content Packages" and "Use
content from ArcGIS Living Atlas for commercial use in your application or product". LightCraft is not Esri
software and has no ArcGIS subscription. The full terms are the Esri Master Agreement
(<https://www.esri.com/en-us/legal/terms/master-agreement>). The layers' required credits, kept in the code
for when this changes, come from each item's `accessInformation`
(<https://www.arcgis.com/sharing/rest/content/items/10df2279f9684e4a9f6a7f08febac2a9?f=json>).

Turning them on would need a maintainer decision, for example a user-supplied ArcGIS key through Esri's
token-based basemap service, with the privacy note that Esri's server then sees which places you look at.

## Privacy

The tile server sees which tiles you look at (so roughly which places), your IP address and the
User-Agent above. Nothing else leaves the machine: no photo, place name or coordinate is sent except as the
tile `z/x/y` in the URL. Reverse geocoding (place names from GPS) is offline (`metadata.reverseGeocode`,
GeoNames). Choose Offline to fetch nothing.

## Commands

| Command | What it does |
|---------|--------------|
| `map.tileSource` | `{}` reports the source; `{id: "osm" \| "opentopomap" \| "offline"}`, `{offline: true \| false}`, or `{urlTemplate, attribution, maxZoom?, id?}` sets it (saved in `prefs.json`) |
| `map.providers` | The sources offered now, with credit and max zoom |
| `map.cacheInfo` | `{bytes, tiles, capBytes, dir}` of the tile cache |
| `map.clearCache` | Deletes every cached tile |

The tile core (Web Mercator maths, source checks, cache, fetch queue) is ported from PR #797 by Jason
Baldwin.
