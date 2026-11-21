//! Adobe's `crs:` namespace and our import decision for every field of it.
//!
//! The data lives in `registry.json` (one row per field and its decision - two decisions for
//! the rare field that is both bookkeeping and read, like `HasCrop`), and `build.rs` generates
//! the lists below into `OUT_DIR` from it, so the two cannot drift apart: a bad row fails the
//! build naming the field. The registry source is the public exiftool XMP-crs tag table
//! (https://exiftool.org/TagNames/XMP.html#crs, revised 2026-10-28; Adobe publishes only the
//! namespace URI) reconciled against exiftool's own `XMP.pm` table, plus fields Lightroom
//! packets carry that table still lacks (`AsShotTemperature`, `AsShotTint`, `AutoGrayscaleMix`,
//! `CurveRefineSaturation`, `RequiresRGBTables`, `SupportsAmount2`) and the fields
//! `docs/xmp-interop.md` names. Struct sub-fields and their flat compound names
//! (`...CorrectionMasks`, `...Dabs`) are not separate properties and are not listed. The tests
//! in `tests.rs` check the generated lists against the mapping, the docs and the report (#4435).

include!(concat!(env!("OUT_DIR"), "/crs_registry.rs"));
