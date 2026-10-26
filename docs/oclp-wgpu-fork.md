# Proposal: share the OCLP wgpu compatibility fork

LightCraft uses the same wgpu 30.0.1 backport proposed for
[ArtCraft Launcher](https://github.com/storytold/craft-launcher/pull/917) and
[PhotoCraft](https://github.com/storytold/photocraft/pull/9711).
It restores configured internal indirect-call validation for window, rendering
and model devices while preserving the existing startup recovery chain.

## Dependency and ownership

- Prototype: <https://github.com/ITJamie/wgpu-fork>
- Branch: `codex/metal-msl-intel-v30.0.1`
- Immutable commit: `21c08e23135ba27ed1c6b68177ecc20c0d076dd9`
- Base: upstream `v30.0.1`, preserving the released API used by egui.
- Backports: <https://github.com/gfx-rs/wgpu/pull/10725> and
  <https://github.com/gfx-rs/wgpu/pull/10731>.

The fork probes supported Metal Shading Language versions, uses pipeline
workgroup limits when the Intel compiler rejects the shader attribute, and
emits runtime-sized storage arrays as pointers on Intel. Shader bounds checks
remain enabled. `WGPU_VALIDATION_INDIRECT_CALL` still explicitly overrides the
configured validation flag; without it, LightCraft preserves wgpu's defaults.

Related reports are [#250](https://github.com/storytold/lightcraft/issues/250)
and [#1939](https://github.com/storytold/lightcraft/issues/1939). The earlier
[#1356](https://github.com/storytold/lightcraft/pull/1356) proposed vendoring
wgpu-hal for MSL probing and was closed without merging. This proposal instead
shares a release-based fork, including the Intel compiler workarounds. The MSL
fallback follows alecanales' original
[patch](https://github.com/storytold/photocraft/pull/5910).

A maintainer should decide whether to create `storytold/wgpu` and preserve the
backport commit and branch there. Then switch the patch URL in `Cargo.toml`,
keep the reviewed commit pin, and regenerate `Cargo.lock`. The apps should share
this dependency and return to an upstream release after both fixes land and
hardware validation passes.

## Validation

```sh
cargo build --locked -p lightcraft --features heif
cargo xtask ci
```

Use a fresh `LIGHTCRAFT_CONFIG_DIR` and a separate `--library` directory as
specified in [the control protocol](control-protocol.md). Import only synthetic
images, wait for the Detail render to finish, verify `ui.inspect`'s `perf.gpu`,
and inspect an internal screenshot. Reject a screenshot that falls back to
headless CPU rendering as evidence of native-window success.

On the OCLP Mac, set `LIGHTCRAFT_GPU_BACKEND=metal`,
`WGPU_VALIDATION_INDIRECT_CALL=1`, `MTL_DEBUG_LAYER=0` and
`MTL_SHADER_VALIDATION=0`. External Metal validation layers are incompatible
with its patched framework; internal wgpu validation stays enabled.
`WGPU_POWER_PREF=high` or `low` selects the window's adapter preference; confirm
the actual adapter in logs. The photo-rendering device independently requests
high performance, so an Intel window does not establish Intel compute coverage
on a dual-GPU Mac.

On Apple M4 Max, an isolated native run used Metal for both the window and
photo rendering, applied +0.35 exposure to a synthetic 512 × 384 PNG and exported
it without a reported GPU fallback. The completed Detail view and native
screenshot were visually inspected. The debug window responded after
approximately two seconds.

Both the [Clippy prerequisite](https://github.com/storytold/lightcraft/pull/3085)
and this compatibility branch independently passed all eight `cargo xtask ci`
stages: formatting, strict workspace Clippy, HEIF, tests/doctests, parity,
layers, assets and WebAssembly. Validation used Rust 1.96.1 and the documented
`CRAFT_FONTS_DIR` input from `storytold/craft-fonts`, commit
`aced4137c022f1f587624d48e81ec77e1914a9cc`. No fonts are added to this PR.
The full-CI log reports 5,183 passes and 42 ignored tests; the CI report records
147 availability-skip entries for corpus data, models and missing
script-specific fonts.

On MacBookPro11,5 running macOS 15.8.1 through OCLP:

- Native build, strict workspace Clippy, all 83 app tests and all 12 focused
  backend/validation regression tests passed.
- Separate runs confirmed AMD Radeon R9 M370X and Intel Iris Pro Graphics for
  the window. Both used AMD for photo compute, completed the synthetic photo's
  +0.35 exposure edit and PNG export without a reported GPU fallback, and
  produced native screenshots that were visually inspected.
- All 30 focused AMD GPU-versus-CPU edit comparisons passed without skips:
  worst mean difference 0.0001 and maximum difference one 8-bit level.
- The debug window responded after approximately 64 seconds with the AMD
  window and 63 seconds with the Intel window. Automatic MSL probing selected
  3.1 instead of the OS maximum 3.2.

Internal indirect-call validation remained enabled; external Metal validation
layers were disabled on OCLP. These runs do not establish Intel photo-compute,
model-inference or broader editing-workflow coverage.

## Known limits of the shared backport

The backport's full wgpu suite on Apple M4 Max ran 1,010 tests: 1,002 passed,
eight failed and six were skipped:

- `subgroup_operations`: an expected-failure mismatch.
- `all_passthrough_shaders_binary`, `all_passthrough_shaders_source`, and
  `metallib_passthrough_shader`: missing DXC or offline Metal tools.
- `binding_array_storage_buffers`, `partial_binding_array_storage_buffers`,
  `validate_capabilities`, and the `Computepass Encoding` benchmark: untriaged
  binding-array/capability failures.

This is a prototype, not a clean full-suite pass. Other OCLP machines,
Monterey, other desktop platforms, raw-camera workflows and model inference
still need validation before release adoption. Launcher and PhotoCraft results
do not establish LightCraft coverage.
