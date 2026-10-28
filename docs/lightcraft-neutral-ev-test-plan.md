# Neutral camera EV correction: reproducible A/B

This plan isolates the neutral shadow lift in the latest tonal process, V3. Reference media and
renders stay under gitignored `local-references/`. Use CPU rendering for both sides; compare
Lightroom exports qualitatively because their Adobe Monochrome profile differs from LightCraft's
default profile plus B&W treatment.

## Question and controls

The V3 camera tone LUT adds up to `+0.35 EV` in the dark transition band around −8 to −3 EV. Does
this help the backlit bridge without flattening the soft portrait? Test that correction alone. Keep
all six global Light controls at zero, use V3 with B&W treatment, and use the same RAW, crop,
profile, CLI version, output colour space, size and CPU backend for every render. Check the bridge,
portrait and church façade. The façade is an architectural proxy; it is not the missing city-square
example.

V1 and V2 must remain unchanged: `base_tone` routes them through `camera_hdr`, while only V3 calls
`camera_hdr_v3`. The V3-only correction is the `ev + 0.35 * shadow_weight` expression in
`crates/pipeline/src/tone.rs`. Blacks, Whites, Highlights, Shadows and local Whites are also V3
behavior and must remain identical between the ON and OFF variants.

## Render procedure

Build and render the ON binary from the checkout under test:

```sh
cargo build --release -p lightcraft-cli
python3 tools/render_references.py local-references/neutral-ev-cases.json \
  --output local-references/runs/neutral-ev-v3-on \
  --cli target/release/lightcraft-cli
```

For OFF, make a disposable copy of the exact same source snapshot (including the current
uncommitted diff, if any). In `ToneMap::camera_hdr_process`, replace only
`ev + 0.35 * shadow_weight` with `ev`, then verify that this is the only semantic code difference.
Build it with a separate target directory and render into a fresh output directory:

```sh
CARGO_TARGET_DIR=/tmp/lightcraft-neutral-ev-target cargo build --release \
  --manifest-path /tmp/lightcraft-neutral-ev-off/Cargo.toml -p lightcraft-cli
python3 tools/render_references.py local-references/neutral-ev-cases.json \
  --output local-references/runs/neutral-ev-v3-off \
  --cli /tmp/lightcraft-neutral-ev-target/release/lightcraft-cli
```

The helper sets `LIGHTCRAFT_GPU=0` and records its manifest, settings, commands, logs and PNGs.
Confirm the manifest uses `{"process":3,"treatment":"bw"}` and all six global Light values are
zero. Never render the two variants over existing output directories.

## Readout

1. Record commit, full Git status, binary-diff SHA-256, CLI version, RAW/reference SHA-256, output
   dimensions and render commands beside the outputs.
2. Compare ON to OFF first. Inspect normalized-coordinate ROIs fixed before viewing: portrait
   forehead, lit cheek, shadow cheek, under-chin and dark background; bridge asphalt, cable shadows,
   adjacent asphalt, sky and lamp edge; façade stone, statue and door.
3. Report linear-luminance mean and P10/P50/P90, near-black and clipped shares, and local
   shadow-to-adjacent-surface differences. Inspect gradients for banding and halos. Use the matching
   Lightroom neutral export only as visual context, never as evidence that the EV lift caused a
   difference between renderers.
4. Repeat at 1200 px for scale consistency. If bridge separation improves while portrait shadow
   modeling worsens, record that trade-off; do not infer a universal cause from these three scenes.

The earlier ON/OFF run on the pre-consolidation V4 working state found the neutral lift's largest
change on the bridge asphalt and cable shadows, while its effect on portrait skin was small. Re-run
this process-3 plan after substantive changes; previous files under `local-references/runs/` are
historical experiment outputs.
