# Metadata search

The existing **Search Photos** box searches saved catalog metadata and a separate, rebuildable
index of readable embedded metadata. This applies to camera photos, scans, images annotated
by other tools and generated images. There are no model downloads or in-app AI tagging settings.

New imports are indexed automatically. Free text finds EXIF software/serial numbers and readable
UserComment values, custom XMP properties, PNG text and scalar JSON values. Standard IPTC/XMP
keywords, captions, credits and locations use the existing catalog fields. ComfyUI graphs also
provide the generation-specific filters below. Automatic1111, Forge, SDNext and InvokeAI PNG
metadata are searchable through ordinary text and `meta:`; their format-specific field aliases
are not inferred.

| Search | Matches |
|---|---|
| `cat chair` | Both terms anywhere in saved metadata or the derived index |
| `"moonlit forest"` | A phrase, including in non-ComfyUI generator text |
| `meta:cat` | A metadata value or property name |
| `meta:software=forge` | A property containing `software` whose value contains `forge` |
| `meta:"tags=living room"` | Custom XMP / JSON tag values containing this phrase |
| `meta:city=berlin` | The catalog’s saved city |
| `comfy:true` | Images with a recognized ComfyUI graph |
| `comfy:false` | Images without indexed ComfyUI metadata |
| `prompt:cyberpunk` | Positive, negative or unclassified generation prompt text |
| `prompt:"neon city"` | A prompt containing this phrase |
| `positive:landscape negative:blurry` | Text on the corresponding conditioning branches |
| `model:flux` | Checkpoint, UNet or CLIP model filename |
| `lora:detail` | LoRA filename |
| `sampler:euler scheduler:karras` | Sampler and scheduler |
| `seed:123456` | Exact unsigned 64-bit seed, without floating-point rounding |
| `steps:>20 cfg:>5 denoise:<1` | Numeric generation parameters |
| `vae:ae node:custom` | VAE filename and node type |
| `width:1024 height:768` | Output or workflow dimensions |

Text fields accept case-insensitive substrings and double-quoted phrases. Numeric fields
accept `>`, `<`, `=` or an exact value. Every term must match, so these work together and
with existing filters, for example:

```text
comfy:true prompt:"neon city" model:flux lora:detail steps:>20 rating:>3
```

Quotes work with ordinary searches too: `"summer trip" camera:"Maker X2"`.
Within quotes, `\"` and `\\` represent a literal quote and backslash.
An unknown field retains its previous meaning as ordinary text, including the colon.
Existing smart albums using text filters use the same search path.

## Index an existing library or refresh changed files

1. Select images in the grid (select all to index the active library/folder view).
2. Choose **Photo → Reload from Disk**.
3. Search in **Search Photos**.

Reload rebuilds the index even when the source bytes have not changed, including for photos
imported by an older build. It replaces removed embedded values and refreshes virtual copies.
Saved titles, keywords, ratings and develop edits are retained. Unreadable originals retain
their previous index so they remain searchable offline. Per-file failures are returned by
`photo.reload` in CLI/control/MCP. Reload currently runs synchronously with the existing import
worker limits; cancellable background reload is a separate proposal.

**Photo → Read Metadata from File** refreshes custom sidecar properties as well as applying
standard XMP fields through the existing workflow. It reads sidecars, or embedded XMP for
RAW/DNG. It does not reread embedded JPEG/PNG keywords; use a sidecar for keyword changes on
those existing records. Reload refreshes embedded search data while preserving catalog keywords.

Indexing and searching never write an original or sidecar, including when automatic XMP writing
is enabled. The existing explicit Save Metadata / auto-write commands retain their usual behavior.

## Cache and catalog compatibility

The catalog format remains **9**. No new metadata fields enter `catalog.snap`, `catalog.log`,
undo records or exported sidecars. Older format-9 builds can open and edit the same library;
they ignore `metadata-search-cache.jsonl` beside the catalog files. This file contains only
extracted search values and file identities, not full workflow JSON or original image data.

Searches read in-memory values only: no file I/O, image probing, decompression or JSON parsing.
On library open the cache is checked against catalog photo IDs, source paths and content hashes.
Opening and searching a library do not stat or parse every original or sidecar, so offline drives
and network libraries remain usable. Relinking or refreshing a source invalidates stale derived data.
Externally edited images or sidecars remain cached until **Reload from Disk** or **Read Metadata
from File** refreshes them, just as saved catalog metadata does; there is no new filesystem watcher.
The same explicit refresh is needed if an older LightCraft build changed custom sidecar properties.

A missing, corrupted or incompatible cache never prevents opening the library. Delete it while
LightCraft is closed and use Reload from Disk to rebuild. Standard saved keywords and edits remain
available without it; additional embedded values need the originals to rebuild. Cache-only updates
are derived bookkeeping, not undo steps. Undoing a source replacement can require another Reload.
Experimental format-10 libraries from earlier versions of this proposal are still refused as newer
catalogs; this build does not downgrade them. Use a format-9 library or a backup.

During import the cache appends changed records, with periodic atomic compaction, rather than
rewriting the entire library after every import batch. Cache write failures are logged and retried
on the next command; they do not prevent saving catalog edits.

## Tags from external tools

CLIP, YOLO or another external tool can write standard XMP `dc:subject` keywords, embedded in a
supported format or in an XMP sidecar. These remain ordinary keywords: search `cat chair` or
`keyword:cat keyword:chair` to require both terms. Tag before importing, or use **Read Metadata
from File** on existing sidecars.

To keep AI annotations out of the Keyword List, the external tool must store them in a separate
XMP property (for example a custom namespace with an RDF bag of strings), or a readable JSON/text
PNG field. The index searches those values without importing them as keywords. Search `cat chair`
or `meta:tags=cat meta:tags=chair`. LightCraft does not generate tags or define a new interchange
schema; arbitrary external databases and standalone JSON files are not indexed.

## Coverage and resource limits

Only metadata supported by the existing readers is indexed, not every binary maker note or proprietary
container. EXIF/TIFF/GPS readable rows and custom XMP properties are supported; editable standard XMP
properties are searched from the catalog so removing a keyword does not leave a stale embedded copy.
IPTC uses the existing supported descriptive fields. Opaque develop settings, thumbnails, ICC profiles
and fields identified as base64 are excluded. EXIF ASCII / Unicode UserComment text is decoded with
an 8 KiB read limit; binary comments with other character sets are not interpreted.

PNG text checks CRCs, bounds decompression to 4 MiB per chunk and 8 MiB total, and accepts at most
32 chunks. JSON keeps serde_json's recursion limit, plus a 24-level / 10,000-visit extraction budget.
The general index is capped at 16 KiB of keys/values per photo, 4 KiB per value and 512 properties.
Long text and unusually large metadata may therefore be partially indexed. Values are lower-cased
and deduplicated. The cache reader accepts at most 256 MiB, 100,000 live records and 128 KiB per record;
excess entries can be rebuilt but may not survive reopening. Browser storage hosts use the same
cache format through `Store`.

ComfyUI API-format prompt graphs and UI-format workflows work with either `prompt` or `workflow`
alone. Standard widgets and named custom inputs provide semantic fields; unfamiliar positional
widget values remain free-text searchable. Graph JSON is never treated as prompt text. Positive/
negative classification follows conditioning connections. See [the metadata crate](../crates/meta/README.md)
for parser details. Malformed metadata is skipped without blocking import.

## View embedded metadata

Select an image and choose **Photo → All Metadata…**, or press **I**, scroll down the Info panel
and click **All Metadata…**. The existing viewer now includes **ComfyUI generation** (readable
fields with original case) and **PNG text / generator metadata** (raw embedded text). Search
fields and **Copy displayed metadata** work on these groups too.

All PNG text keywords are eligible, including `parameters` blocks used by Automatic1111 and
compatible generators, and InvokeAI metadata/graph keys. These are displayed as stored; their
readable text and JSON values are indexed as described above.

Text decoding checks chunk CRCs and bounds decompression: up to 32 text chunks, 4 MiB per chunk
and 8 MiB total. The raw viewer limits each row to 16 KiB and all raw rows to 64 KiB, with an
explicit truncation marker. It never edits the original PNG. This viewer is not a metadata editor.
