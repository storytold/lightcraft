# Canon CRW: where the record is

The Canon CRW (CIFF) decoder and its clean-room record (the frozen format, the verification
summary, the colour, geometry and metadata notes, the private-corpus summary and the source log)
live in craft-libs:
[`crates/crw/clean-room/`](https://github.com/storytold/craft-libs/tree/main/crates/crw/clean-room).
The derivation trail and the frozen table-generation materials stay in LightCraft PR #3021.
LightCraft uses the crate `craft-crw` through the adapter `crates/raw/src/vendor/crw.rs`.

What stays here is LightCraft's own colour work on these cameras:

- [`10-chart-calibration.md`](10-chart-calibration.md): the EOS D60 colour matrices fitted to
  ColorChecker captures, implemented in `crates/raw/src/chart_table.rs`.
- [`logs/chart-captures.tsv`](logs/chart-captures.tsv): names, sizes and SHA-256 of the chart
  frames (no image content).

The capture statistics those documents cite (`04-color.md`, `05-private-corpus.md`, `sources.md`)
are in the craft-libs record.
