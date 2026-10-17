### Long exposure times keep their step
- Photos between 1/4 s and 4 s no longer read and export as a whole stop: a 0.4 s file showed `1/2` and was
  written to the exported EXIF and XMP as 0.5 s; it now reads and exports as 0.4 s (0.8 s, 1.3 s, 2.5 s and the
  rest likewise). Times of 1/4 s and shorter are unchanged (issue #1619).
