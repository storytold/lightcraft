## Performance

- **Lightroom catalog import commits first (#1303):** the catalog's photos are committed from the catalog alone, each
  without its original read (no content hash); the originals are then read in the background in batches of 500 (one
  stoppable **Reading originals** activity, resumed when the library next opens) and fill in the content hash, file
  facts, lens data and as-shot white balance as Reload would. New engine command `library.readOriginals` and
  `library.importLightroom` parameter `readOriginalsLater`. Records made from the catalog alone now get the import
  defaults and an upright size.
