---
title: Export a look as a LUT
description: Save a photo's colour and tone edits as a .cube LUT to use the same look in video editors, image editors and other apps.
order: 46
---

<!-- Draft until Export Look as LUT ships: then drop the leading _ from the file name, list it in
guides/index.md, link it from its What's New entry, and set `verified` to that release. -->

Edit a photo until you like its look, then choose **File** > **Export Look as LUT…** and save the `.cube` file. Load that file as a LUT in your video or photo editor to give other footage the same colour and tone.

## Steps

1. Select the photo whose look you want, in the grid or the **Detail** view.
2. Choose **File** > **Export Look as LUT…**.
3. Pick a folder and a name. LightCraft suggests the photo's name followed by `Look.cube`.
4. Click **Save**. A message confirms the export and lists any settings it left out.
5. In the other app, load the `.cube` file as a LUT (also called a *look*, a *color lookup* or a *3D LUT*).

## What a LUT keeps

A LUT changes each colour the same way wherever it is in the picture. It keeps the photo's:

- **Profile** and its amount
- White balance (**Temp** and **Tint**)
- **Exposure**, **Contrast**, **Whites** and **Blacks**
- Tone curves
- **Vibrance** and **Saturation**
- **Color Mixer**, **Point Color** and the black and white mix
- **Color Grading**
- **Calibration**

## What a LUT can't keep

These settings depend on where a pixel is or what surrounds it, so a LUT can't hold them. LightCraft leaves them out and names them in the message and at the top of the file:

- **Highlights** and **Shadows**
- **Texture**, **Clarity** and **Dehaze**
- Sharpening and noise reduction
- **Vignette** and **Grain**
- Masks, spot removal and red eye
- Crop, rotation, transform and lens corrections
- **Denoise**
- HDR, and a raw photo's own local tone mapping

To get a look that travels well, make it mostly with the settings in the first list.

## Use the look in LightCraft too

Choose **File** > **Import Profiles & Presets…** and pick the `.cube` file. It becomes a creative profile you can apply to other photos with an **Amount** slider.

## Things to know

- The LUT takes and gives sRGB-encoded colours. Use it on sRGB-encoded footage or images, after any camera log conversion. Video in Rec.709 uses a different curve: convert it to sRGB encoding before the LUT, or the result will be a little off.
- The file has 33 steps per colour channel, the usual size for looks. It matches LightCraft's own render closely; very saturated colours at the edge of what a screen shows can differ slightly.
- Your photo is not changed by exporting its look.
