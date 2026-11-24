### Image Quality Scanner: ratings and flags

- The Image Quality Scanner can now write the **star rating** a score asks for: **Write star ratings onto photos the scan can rate**, with five thresholds you can drag (they start at 20, 40, 55, 70 and 85). Either the ratings or the flags can be written alone.
- **Your own ratings and flags are kept.** A rating or flag you set is never changed by a scan; a rating or flag an earlier scan wrote is the scan's own, so rescanning may change it. **Overwrite existing** replaces both instead, for the run you tick it on.
- Both land in the catalog the moment a scan finishes, appear in the grid straight away, and follow your XMP sidecar setting; the whole run is still one undo step.
- From a shell: `<cli> quality-scan --auto-rate --auto-flag --overwrite-metadata --star-breaks 15,35,50,65,80`, and `photo.qualityScan` takes `autoRate`, `overwriteMetadata` and `starBreaks`.
