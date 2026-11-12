# Convert Negative

**Photo ▸ Convert Negative** renders a scan of a film negative (a camera or scanner capture of colour
or black-and-white film) as a positive. The conversion is part of the photo's edit, like every other
setting: it is non-destructive, it can be switched off, copied to the other frames of a roll, synced,
saved in a preset and undone, and previews and exports convert with the same numbers.

## Using it

1. Select the scan and choose **Photo ▸ Convert Negative**. The conversion goes on and the **Convert
   Negative** panel opens. Switching it on measures the scan once: the film base (the unexposed film,
   orange on colour negative film), each channel's black and white point, and a balance that makes the
   scene average grey. The measurement looks inside the crop, 8 % in from each edge, so the film holder
   and the sprocket holes stay out of it.
2. **Simple** shows the film preset (Standard, Portra 160 / 400, Ektar 100, Gold 200, ColorPlus 200,
   UltraMax 400, Superia 400, C200, Pro 400H, CineStill 800, Monochrome) and six sliders: Exposure,
   Brightness, Contrast, Temp, Tint and Saturation.
3. **Advanced** adds:
   - **Film Base**: click the selector, then click the unexposed border of the film on the photo. The
     picked colour is the film base (black) over the measured one; the measured white point still
     applies. **Clear** goes back to the measurement.
   - **Gray Point**: the same selector for something in the scene that should come out grey.
   - **Tone** (Lab, Linear and Cinematic profiles) and **Color** (Neutral, Lab Warm, Lab Cool, Print,
     Clean); "From Preset" uses the film preset's own. **White Balance** (As Shot, Warm, Cool, Tungsten
     Film) starts at As Shot: presets carry their warmth in Temp and Tint instead.
   - Highlights, Shadows, Whites, Blacks, Fade, Glow and **Dynamic Range** (stops of scene brightness
     between black and white; fewer render mid-tones brighter, as lab scans do).
   Switching back to Simple ignores the advanced settings without forgetting them.
4. **Measure Again** after cropping to the frame (or when the first measurement took in the holder).
   **Done** (or Return) goes back to the Edit panel. Everything else in the Edit panel then edits the
   positive.

The **Film negative** box switches the conversion off and on again without losing its settings, as does
the section's eye (`develop.sectionEnabled` with `negative`); resetting the section switches the whole
conversion off.

To convert a roll, convert one frame, then **Copy Edit Settings** with **Convert Negative** ticked and
paste or sync to the rest: they convert with that frame's measurement, preset, picks and sliders, as a
lab scans a roll. A copy states the whole conversion: copied from a photo that isn't a negative, it
switches the conversion off; copied without picks, it clears the photo's own.

### Sliders

| Control | Range | Default | Panel |
|---|---|---|---|
| `negative.exposure` | −5 … 5 stops | 0 | Simple |
| `negative.brightness`, `.contrast`, `.temp`, `.tint`, `.saturation` | −50 … 50 | 0 | Simple |
| `negative.highlights`, `.shadows`, `.whites`, `.blacks` | −50 … 50 | 0 | Advanced |
| `negative.fade`, `.glow` | −100 … 100 | 0 | Advanced |
| `negative.range` (Dynamic Range) | 1 … 10 stops | 3 | Advanced |

The Advanced sliders and choices apply only while the panel is in Advanced. Values from a file, a preset
or a client are kept in these ranges.

## How it works

The conversion is the film model of `lightcraft-film`: density grows with log exposure in the straight
part of the film's characteristic curve, so each channel is read relative to the film base (which also
removes the orange mask), levelled between its black and white points in density space, and then
coloured and toned. A preset carries what belongs to the film (the contrast of each layer, its typical
mask colour, a tone profile, a colour model, warmth and toning); the measurement carries what belongs
to the scan. Without a measurement the preset's typical mask colour stands in for the film base.

In the pipeline it is the first step of the scene-linear stage ([`crates/pipeline/src/negative.rs`](../crates/pipeline/src/negative.rs)),
before the camera's local tone mapping and white balance, so the measurement, the selectors and the
conversion all read the same scan values, and the automatic adjustments (Auto, auto white balance and
its selector, the B&W mix) see the positive. GPU renders run that stage on the CPU while a negative is
converted.

The measurement and the selectors read the preview-size copy of the scan. The measurement uses the
crop's box; a straighten angle is not taken into account, so measure before straightening, or crop
inside the frame.

The film presets are starting points built from the stocks' published characteristics; the film names
are their makers' trademarks and only say which stock a recipe takes after.

## Commands

| Command | What it does |
|---|---|
| `photo.convertNegative` | Photo ▸ Convert Negative: switches the conversion on (measuring it if it has no measurement) and opens the panel. No photo selected: an error. |
| `develop.negative` | Switches the conversion on or off and sets its choices; see the parameters below. Returns the conversion, with `presetName` and, when it measured, the `border` used (%). |
| `develop.negativePick` | `{x, y, target?}`: the film base (`target: "base"`, the default) or the gray point (`"gray"`) from the scan at a normalized point (outside 0 … 1: the nearest edge). Switches the conversion on and to Advanced, and measures it first if it has no measurement. |
| `develop.set` | The sliders above, by id. |

`develop.negative` parameters (each optional; an unknown name or a wrong type is an error, and then
nothing changes):

| Parameter | Type | Default | Meaning |
|---|---|---|---|
| `on` | bool | `true` | Switches the conversion on (`false`: off, still applying the other choices) |
| `advanced` | bool | unchanged | The Advanced panel |
| `preset` | string | unchanged | A film preset, any spelling (`portra400`, `Portra 400`, `PORTRA_400`) |
| `tone` | string | unchanged | `lab_standard`, `lab_soft`, `lab_hard`, `lab_highlight_soft`, `lab_highlight_hard`, `lab_shadow_soft`, `lab_shadow_hard`, `linear`, `linear_deep`, `linear_flat`, `cinematic`, `cinematic_flat`, `cinematic_rich`; `""` = the preset's |
| `look` | string | unchanged | `neutral`, `lab_warm`, `lab_cool`, `print`, `clean`; `""` = the preset's |
| `wb` | string | unchanged | `as_is`, `warm`, `cool`, `tungsten_film`; `""` = As Shot |
| `measure` | bool | `false` | Measure the scan again (also while off) |
| `border` | number | 8 | % of each edge the measurement leaves out, 0 … 45 |
| `clearBase`, `clearGray` | bool | `false` | Forget the picked film base or gray point |

Switching on measures the scan when it has no measurement yet.

```sh
lightcraft-cli run --import scan.tif develop.negative preset=portra400 app.export path=positive.jpg
```
