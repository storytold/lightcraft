### Large Lightroom catalogs keep their tables

- One oversized or damaged record in a Lightroom catalog is now left out on its own instead of dropping its whole table: a single huge cached-XMP record no longer costs every other photo its Lightroom creator, copyright and rights fields. The photo whose record is too large still loses those Lightroom fields and keeps only what its files say. A damaged record in a table every import needs (photos, files, folders) leaves out that photo, and the result says it may be missing. The result window counts what was left out, per table.
- Keyword assignments are read a row at a time, so a catalog with more than a million of them keeps its keywords.
- Text with invalid characters imports with replacement characters, counted in the result, instead of losing its record.
