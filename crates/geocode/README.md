# lcc-geocode (L1)

Offline reverse geocoding: a GPS position becomes Sublocation / City / State / Country.

- **Data:** the GeoNames dump (<https://download.geonames.org/export/dump/>), licensed CC-BY 4.0
  (attribution: "GeoNames, geonames.org"). Nothing is bundled: the engine
  (`metadata.downloadPlaceNames`) downloads `cities15000.zip`, `admin1CodesASCII.txt` and
  `countryInfo.txt` on the user's request into the settings folder. This crate only parses and
  searches; it never touches the network.
- **Lookup:** the nearest populated place within `MAX_KM` (great-circle distance), searched in the
  1° cells around the position, so one lookup looks at a few dozen places, not all of them.
- **unzip:** just enough ZIP to read one file out of `cities15000.zip` (stored or deflated).
- **Never panics on hostile input:** bad lines are skipped, NaN / out-of-range positions find
  nothing, sizes and counts are capped.

Sources: GeoNames dump readme (file layout, prose); haversine distance (textbook); PKWARE APPNOTE
(ZIP, prose spec); the cell index is own design.
