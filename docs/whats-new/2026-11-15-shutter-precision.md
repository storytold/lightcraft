### Shutter speeds keep their precision
- A shutter speed shows as `1/n` only when it really is one (`1/3`, `1/250`, and a half second as `1/2`);
  any other time keeps up to four significant digits (`0.3571` for 1/2.8 s, `0.625`) instead of rounding.
  The Metadata columns, All Metadata, filters and print captions show it the same way.
- Import keeps the file's exact exposure time, and export writes that time to EXIF and XMP rather than the
  shortened text.
- **Reload from Disk** repairs a shutter speed an older import rounded, on the photo and its virtual copies; a
  shutter speed you typed yourself is kept.
