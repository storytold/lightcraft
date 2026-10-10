# Global Tint direction

Negative (left) Tint adds green; positive (right) adds magenta, as the slider's track shows and as in Lightroom and the DNG specification (issue #188). It is a global white-balance change, not a Nikon-specific decoder fix.

## Reproducible evidence

The existing UI already declares `Track::Tint` as green → magenta (`crates/develop/src/controls.rs`) and paints a green left endpoint and purple right endpoint (`crates/ui-egui/src/widgets.rs`). Local mask tint also implements the correction direction: positive tint reduces green (`crates/pipeline/src/finish.rs`). The old global conversion contradicts both.

The synthetic `tint_negative_is_green_and_positive_is_magenta` regression renders an 18% neutral patch. Before the fix, setting global Tint to -50 produces sRGB `[129, 113, 128]`: red and blue exceed green, so the result is magenta. The test asserts the output direction for rendered sources, relative-WB RAW, and calibrated RAW with a nonzero As Shot reference. The complementary auto/picker test checks that correcting a green patch returns positive Tint and neutralises the patch.

Run the evidence without private samples:

```sh
cargo test -p dac-pipeline tint_ -- --nocapture
cargo test -p dac-gpu --test equivalence tint_directions -- --nocapture
cargo test -p dac-color cct
```

## Why the sign changes

`temp_tint_to_xy` describes the **source illuminant** removed by Bradford adaptation. To add magenta to the output, the source white must be greener (positive Duv), not more magenta. Therefore the correction convention requires `Duv = +tint / TINT_SCALE`, and the inverse conversion must return `tint = +Duv * TINT_SCALE`.

The CPU and GPU consume the same `wb_matrix_for` result. RAW white-point inference, auto WB and the picker use the inverse conversion. Both directions change together; local mask tint and calibration shadow tint are unchanged. The render-cache version increases to invalidate stale output.

## Existing edits

Before this change the sign was the other way round, so a saved non-zero custom Tint (a catalog edit, a preset, history) now renders with the opposite green/magenta shift; As Shot, Auto and the presets that compute their values are unaffected, and re-running Auto or the picker gives values in the new convention. Tint values imported from Lightroom XMP (`crs:Tint`) were rendered the wrong way round before and now render as in Lightroom. No saved values are rewritten.
