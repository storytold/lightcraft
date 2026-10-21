# lightcraft-meta (L1)

Photo metadata: EXIF, XMP (read + write), IPTC-IIM, and the containers that carry them.

## API

| Function | Purpose |
|---|---|
| `read_exif(&[u8]) -> Metadata` | TIFF-structured Exif block (whole TIFF/DNG/raw file, or JPEG APP1 payload with/without `Exif\0\0`). `try_read_exif` reports header errors. |
| `from_tiff(&Tiff) -> Metadata` | Same, from an already parsed stream (used by `lightcraft-raw`). |
| `embedded(&[u8]) -> Embedded` | Sniffs JPEG / PNG / WebP / TIFF and returns the Exif, XMP (+ extended XMP), ICC and IPTC blocks. `jpeg_segments`, `png_chunks`, `webp_chunks` are the per-container walkers. |
| `parse_xmp(&str) -> Result<XmpData>` | Metadata + the opaque `lc:settings` JSON + every property (`prefix:name`, structs flattened as `a:b/c:d`). |
| `write_xmp(&Metadata, Option<&str>) -> String` | Complete `<?xpacket?>` packet; the second argument is LightCraft's full develop settings JSON. |
| `parse_iptc(&[u8]) -> Metadata` | IIM record 2: title (2:5), keywords (2:25), by-line (2:80), copyright (2:116), caption (2:120), date/time created (2:55/2:60). |
| `extract(&[u8]) -> Metadata` | Everything for a whole file. Precedence: EXIF → IPTC fills gaps → XMP overrides user fields (rating, label, title, caption, artist, copyright, keywords, GPS) and fills the rest. |

`Metadata` carries camera (make, model, serial, software), lens (make, model, serial, specification), capture
(date-time with UTC offset and milliseconds, exposure time, f-number, ISO incl. extended ISO, focal length and
35 mm equivalent, flash, exposure bias, program, metering, white balance), orientation, GPS, dimensions and the
user fields (artist, copyright, title, caption, keywords, hierarchical keywords, rating −1…5, colour label).

## General search index

`extract_with_search(bytes)` shares container / graph parsing with import metadata extraction.
`search_metadata(bytes)` returns a derived `SearchMetadata` index for readable EXIF/TIFF/GPS rows,
PNG text, scalar JSON and custom XMP properties. `replace_sidecar(packet)` updates the sidecar
portion separately. Standard editable XMP fields stay in the catalog, so clearing a keyword
cannot leave a hidden stale copy. Opaque develop settings and binary data are excluded.

The combined index has a 16 KiB key/value budget, 4 KiB per value, 512 keys, 24 JSON traversal
levels and 10,000 visits. `valid()` checks cache entries against these bounds. Values are
lower-cased during extraction; ordinary search performs substring matching only. Storage is
an engine-owned disposable cache, never a new catalog format field.

## ComfyUI PNG metadata

`extract` and `png_chunks` also return a read-only `ComfyMetadata` index from CRC-valid
`prompt` / `workflow` text chunks (`tEXt`, `zTXt`, compressed or uncompressed `iTXt`). Latin-1
and UTF-8 text exporters are supported; international text (`iTXt`) requires UTF-8. API
graphs use named inputs; UI workflows use conditioning links and standard widget layouts.
Positive/negative prompt text, models, LoRAs, sampling parameters, seeds, VAE, dimensions,
node types and unfamiliar parameter values are indexed. Unknown positional custom widgets
remain available to free-text search; their field meanings cannot be inferred reliably.

Values are lower-cased and deduplicated within each field; raw graphs are discarded. Limits:
4 MiB per text chunk and decompressed JSON, at most 32 text chunks and about 8 MiB of
decoded text per PNG, 30,000 nodes / parameter visits, 32 levels of parameter traversal,
8 KiB per indexed string and a 64 KiB aggregate index budget (before serialization overhead).
JSON parsing retains serde_json's recursion limit. Invalid or excessive metadata is ignored
without blocking the photo's import. Generation data is never written to XMP.

Catalog integration and user searches: [Metadata search](../../docs/metadata-search.md).

Standard widget names and ordering were checked against the
[ComfyUI node definitions](https://github.com/Comfy-Org/ComfyUI/blob/master/nodes.py) and
[SDXL text encoder definition](https://github.com/Comfy-Org/ComfyUI/blob/master/comfy_extras/nodes_clip_sdxl.py).

## XMP namespaces

Written: `dc`, `xmp`, `photoshop`, `exif`, `exifEX`, `tiff`, `lr` (`hierarchicalSubject`) and ours,
`lc` = `http://ns.lightcraft.app/lc/1.0/` (`lc:settings`, an opaque JSON string).
Reading canonicalises
prefixes by namespace URI, handles attribute and element forms, `rdf:Seq`/`Bag`/`Alt` (x-default first),
`rdf:parseType="Resource"` and nested-description structs, predefined and numeric entities. Nesting depth
and node count are limited.

## Robustness

No panics on malformed input: proptests feed random bytes to every container walker and random text to the
XMP parser; truncation tests cut a sample JPEG at every byte.

## Sources

CIPA DC-008 (Exif 2.32), TIFF 6.0, Adobe DNG 1.7 (tag numbers), ISO 16684-1 / Adobe XMP Specification
Parts 1–3 (packet, RDF serialisation, standard schemas, JPEG extended XMP), IPTC-IIM 4.2 and the IPTC Photo
Metadata Standard, ITU-T T.81 (JPEG markers), ICC.1 (APP2 embedding), PNG Specification 3rd ed. (`eXIf`,
`iTXt`), the WebP RIFF container specification, Adobe Photoshop File Format Specification (image resource
blocks). XML parsing: `quick-xml` (MIT). No third-party metadata code was consulted.


`png_text_fields(bytes)` reads original PNG tEXt/zTXt/iTXt values, regardless of generator,
with CRC, chunk-count and decompression bounds shared with the ComfyUI indexer.
`comfy_display_fields(fields, dimensions)` returns readable original-case fields for the metadata
viewer; it does not store workflow JSON in the catalog. See `docs/metadata-search.md` for viewer caps.
