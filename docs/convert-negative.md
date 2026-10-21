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
   - **Tone** (Lab, Linear and Cinematic profiles), **Color** (Neutral, Lab Warm, Lab Cool, Print,
     Clean) and **White Balance** (As Shot, Warm, Cool, Tungsten Film); "From Preset" uses the film
     preset's own.
   - Highlights, Shadows, Whites, Blacks, Fade, Glow and **Dynamic Range** (stops of scene brightness
     between black and white; fewer render mid-tones brighter, as lab scans do).
   Switching back to Simple ignores the advanced settings without forgetting them.
4. **Measure Again** after cropping to the frame (or when the first measurement took in the holder).
   **Done** closes the panel. Everything else in the Edit panel then edits the positive.

To convert a roll, convert one frame, then **Copy Edit Settings** with **Convert Negative** ticked and
paste or sync to the rest: they convert with that frame's measurement, as a lab scans a roll.

## How it works

The conversion is the film model of `lightcraft-film`: density grows with log exposure in the straight
part of the film's characteristic curve, so each channel is read relative to the film base (which also
removes the orange mask), levelled between its black and white points in density space, and then
coloured and toned. A preset carries what belongs to the film (the contrast of each layer, its typical
mask colour, a tone profile, a colour model, warmth and toning); the measurement carries what belongs
to the scan. Without a measurement the preset's typical mask colour stands in for the film base.

In the pipeline it is the first step of the scene-linear stage ([`crates/pipeline/src/negative.rs`](../crates/pipeline/src/negative.rs)),
before white balance, so the automatic adjustments (Auto, auto white balance and its selector, the
B&W mix) see the positive.

The film presets are starting points built from the stocks' published characteristics; the film names
are their makers' trademarks and only say which stock a recipe takes after.

## Commands

| Command | What it does |
|---|---|
| `photo.convertNegative` | Photo ▸ Convert Negative: switches the conversion on and opens the panel |
| `develop.negative` | `{on?, advanced?, preset?, tone?, look?, wb?, measure?, border?, clearBase?, clearGray?}` |
| `develop.negativePick` | `{x, y, target?: base\|gray}`: the film base or gray point from the scan at a normalized point |
| `develop.set` | the sliders: `negative.exposure`, `.brightness`, `.contrast`, `.temp`, `.tint`, `.saturation`, `.highlights`, `.shadows`, `.whites`, `.blacks`, `.fade`, `.glow`, `.range` |

```sh
lightcraft-cli run --import scan.tif develop.negative preset=portra400 app.export path=positive.jpg
```
