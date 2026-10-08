# Global Tint direction: proposed correction and compatibility

This change is under review. Negative/left Tint should add green; positive/right should add magenta. It is a global white-balance change, not a Nikon-specific decoder fix.

## Reproducible evidence

The existing UI already declares `Track::Tint` as green → magenta (`crates/develop/src/controls.rs`) and paints a green left endpoint and purple right endpoint (`crates/ui-egui/src/widgets.rs`). Local mask tint also implements the correction direction: positive tint reduces green (`crates/pipeline/src/finish.rs`). The old global conversion contradicts both.

The synthetic `tint_negative_is_green_and_positive_is_magenta` regression renders an 18% neutral patch. Before the fix, setting global Tint to -50 produces sRGB `[129, 113, 128]`: red and blue exceed green, so the result is magenta. The test asserts the output direction for rendered sources, relative-WB RAW, and calibrated RAW with a nonzero As Shot reference. The complementary auto/picker test checks that correcting a green patch returns positive Tint and neutralises the patch.

Run the evidence without private samples:

```sh
cargo test -p lightcraft-pipeline tint_ -- --nocapture
cargo test -p lightcraft-gpu --test equivalence tint_directions -- --nocapture
cargo test -p lightcraft-color cct
```

For an external behaviour reference, see Adobe's Lightroom Classic documentation, [Adjust image color and tone](https://helpx.adobe.com/lightroom-classic/help/tone-control-adjustment.html), in the white-balance/Tint section. The automated documentation fetch returned HTTP 403 during this split, so this PR's directly verified evidence is the existing UI convention and synthetic rendered pixels above; it does not claim a new observed Lightroom comparison or copy any Adobe asset/profile.

## Why the sign changes

`temp_tint_to_xy` describes the **source illuminant** removed by Bradford adaptation. To add magenta to the output, the source white must be greener (positive Duv), not more magenta. Therefore the correction convention requires `Duv = +tint / TINT_SCALE`, and the inverse conversion must return `tint = +Duv * TINT_SCALE`.

The CPU and GPU consume the same `wb_matrix_for` result. RAW white-point inference, auto WB and the picker use the inverse conversion. Both directions change together; local mask tint and calibration shadow tint are unchanged. The render-cache version increases to invalidate stale output.

## Existing edits: review required

This changes the rendering meaning of saved nonzero global Tint values, including catalog edits, presets, history and imported XMP. No values are silently rewritten and no automatic migration is included. An old Auto WB result stored as numeric settings is also an existing edit; rerunning Auto computes values in the new convention.

As Shot continues to apply an identity adjustment. For a source whose As Shot tint is zero, negating the previous custom Tint reproduces its previous appearance. This is not a universal migration: calibrated RAW can retain a nonzero, old-sign reference tint in catalog metadata, cached source information or smart previews. Those references and their provenance need consideration alongside the edit. Existing XMP may originate from another application using the intended correction convention, so indiscriminately negating all imported values is also wrong.

Before merging, decide whether to version/migrate native saved edits and reference metadata, preserve a legacy rendering mode, or accept an explicitly documented rendering change. Keep this PR a draft until that policy is reviewed. If another render-cache change lands first, rebase and increment the cache version again.
