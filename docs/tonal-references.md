# Local tonal reference workflow

Keep your own source photographs and Lightroom exports in `local-references/` at the repository
root. The entire directory is gitignored, including manifests, renders and reports. Never commit
reference media, Adobe screenshots, presets or profiles. The batch helper uses Python's standard
library to call the Rust CLI; it is development tooling, not a product dependency.

## Prepare the references

For each photograph, provide the **original input** (RAW preferred) and Lightroom exports made
from that same input. Use sRGB SDR PNG or TIFF, identical crop, orientation and dimensions,
and no output sharpening, watermark or automatic adjustments. JPEG is usable but adds compression
error. Include a neutral export first, then change one slider at a time: Highlights, Shadows,
Whites and Blacks, ideally at -100, -50, +50 and +100. Add combinations afterwards.

Record Lightroom version, process version, selected profile, white balance, all other active
settings and export dimensions in `notes.txt`. A Lightroom profile name is observation metadata;
do not copy its profile file. Keep the other settings fixed across a series. A neutral RAW export
can already differ because of camera colour and base tone; compare the neutral images before
attributing those differences to a slider. For isolating slider behaviour, a shared sRGB TIFF input
is also useful. UI screenshots can supplement exports but are unsuitable for pixel comparisons.
Keep input copies free of XMP sidecars and embedded editing instructions: CLI import can pick up
existing edits, and an empty `set` object does not reset them.

Suggested layout:

```text
local-references/
  originals/scene.dng
  lightroom/scene-neutral.tif
  lightroom/scene-highlights-minus100.tif
  notes.txt
  cases.json
  runs/
```

`cases.json` is an array; paths are relative to its directory. Values use LightCraft's CLI control
IDs (the four tonal sliders all range from -100 to +100). `size` is optional; omit for full size.
Use the same export long edge in both applications.

```json
[
  {
    "name": "scene-neutral",
    "source": "originals/scene.dng",
    "reference": "lightroom/scene-neutral.tif",
    "set": {}
  },
  {
    "name": "scene-highlights-minus100",
    "source": "originals/scene.dng",
    "reference": "lightroom/scene-highlights-minus100.tif",
    "set": {"light.highlights": -100}
  }
]
```

The other IDs are `light.shadows`, `light.whites`, and `light.blacks`. Additional fixed LightCraft
light settings can be included in each case. The helper starts each case in a new CLI process.
For fixed settings beyond sliders, add a partial develop-settings object to each case, for example
`"settings": {"treatment": "bw"}`. This is exactly the S/W button with the default profile retained;
it is different from selecting the `lc.mono` profile or setting saturation to -100. For comparison
with Lightroom's Adobe Monochrome, record that profile name in the local notes; the two profiles
need not share a neutral tone curve. Each partial settings object is saved in the run directory and
passed to `render --settings` before applying numeric `light.*` controls from `set`.

## Run

From the repository root:

```sh
cargo build --release -p lightcraft-cli
python3 tools/render_references.py local-references/cases.json --output local-references/runs/before --dry-run
python3 tools/render_references.py local-references/cases.json --output local-references/runs/before
# After a code change, rebuild, then choose a new output directory:
cargo build --release -p lightcraft-cli
python3 tools/render_references.py local-references/cases.json --output local-references/runs/after
```

Each run writes sRGB PNGs, per-case commands/logs, a manifest snapshot and a local HTML comparison.
CPU rendering is forced to make the reference implementation explicit. Existing output directories
are refused to preserve earlier results. The helper stops on an error. It does not compute a fidelity
score: inspect the images, shadow/highlight detail and clipping before tuning. For browser comparison,
PNG references work most reliably; TIFF can be inspected directly by image tools.

The agent can read these local images directly; uploading them into chat is unnecessary. Start with
several different scenes and retain some as validation cases so tuning does not fit only one image.
Record the source revision and whether the working tree was dirty alongside each experiment.
Rendering changes must respect [process versions](process-versions.md), and CPU/GPU parity must
be checked before shipping. This workflow alone does not establish Lightroom rendering parity.
