# Negative Craft

`crates/pipeline/src/film.rs` converts a color negative or a black-and-white negative.
It runs on scene-linear transmission, before white balance, and hands a scene-linear positive to
the rest of the pipeline. Auto tone, Auto black-and-white mix, white balance, and the selector sample
that positive, not the negative. On a raw file with its own colour matrix the density is measured in
white-balanced camera RGB; the matrix only carries the positive back to Rec.2020. A JPEG is already
in the working space, so the same curve is only as true as that file. The GPU kernel `film_k`
(`crates/gpu/src/wgsl/map.wgsl`) is the same formula, including the 43 words from `FilmKernel::words`.
Settings and commands are `film.*`. A slide (`colorPos`, `bwPos`) is still
accepted by `film.setStock` so an old frame opens; it is not on the panel, because a slide is
already a positive.

## Using it

- **App:** **Window ▸ Edit Sections ▸ Negative Craft** shows the section above **Light**. It stays
  hidden until then. Choosing the menu item again hides it, as does a right-click on the header
  and **Hide Negative Craft**. A photo that is already converted still shows the section.
  Pick **Color Neg** or **B&W**, then **Convert**.
  The mask swatch is the blank film. **Auto** measures again. **Border** picks the blank film
  when the border is wrong, including a pale or near-white rebate after camera white balance.
  A blown-out lamp is refused. **Gray** (color only)
  picks one tone that should print gray and keeps that axis. While one of those clicks is
  waiting, a line under the buttons says where to click. **Crop** sits under that row. **Auto**
  cuts the scan to the picture, past sprocket holes and the printing on the rebate. **Adjust**
  opens the crop tool. **Scan** (Camera, Minilab, Flat) and **Print** (Neutral, Portrait, Vivid,
  Lab) are the dye correction and the paper, on a color negative. **Look** (Neutral, Warm, Cool,
  Paper, Cinematic) changes color and the paper shoulder. It does not replace the border, the
  gray axis, exposure, contrast, or whether conversion is on. **Exposure**, **Contrast**, and
  **Blacks** place the print. **Margin** is a percent trimmed from each edge of the frame before
  that measurement. The blank film outside the frame stays the mask. **Auto** under Crop cuts to the
  picture and does not reprint. A crop drawn before **Convert** is the whole measurement: only that
  rectangle is read, and the surround is ignored. Blank film still inside it is the base. With none
  there, the base starts neutral, so a mask taken from the discarded surround cannot tint the print.
  On a color negative, **Temp**, **Tint**, and the shadow, midtone,
  and highlight balances trim color. A black-and-white negative stops at the print sliders: the
  color looks stay off, so they cannot tint the gray. **More** on a color negative keeps the cast
  Auto removed, saturation, shoulder, the mask numbers, Film / Scene intent, scan gamma (Linear,
  1.8, 2.2), the shadow and highlight picks, and **Match roll**. On black-and-white, **More**
  keeps the shoulder, the scan gamma, and **Match roll**.
- **Commands** (CLI, MCP, control channel). A stock, scan, intent, or scan-gamma change remeasures
  when conversion is already on. Print does not.
  - `film.setEnabled {enabled: bool}` — on measures and prints; off leaves the pixels alone
  - `film.analyze {}` — the same measurement as **Auto** and **Convert**
  - `film.setStock {stock: colorNeg|bwNeg|colorPos|bwPos}`
  - `film.setScan {scan: camera|minilab|flat}`
  - `film.setPrint {print: neutral|portrait|vivid|lab}`
  - `film.setIntent {intent: stock|scene}`
  - `film.setCapture {capture: linear|gamma18|gamma22}`
  - `film.pickBase {x, y}` — **Border**
  - `film.pickGray {x, y}` — sets the axis from that tone and does not refit it
  - `film.pickShadow {x, y}` and `film.pickHighlight {x, y}` — per-channel contrast, then a new placement
  - `film.setFrame {corners: [[x, y], [x, y], [x, y], [x, y]]}`
  - `film.cropToImage {}` — **Auto** under Crop; cuts to the picture and does not reprint
  - `develop.set {control: film.inset, value: 0..40}` — **Margin**. Trims that percent from each edge of the frame and places the print again. The mask base and contrast stay. Zero measures the whole frame.
  - `film.analyzeRoll {}` — share mask color and the gray axis; each frame is exposed again so the shared gray does not move its midtone
  - `film.inspect {}` — density histogram and rebate, without writing settings
  - `preset.apply {id: lc.nc-neutral|lc.nc-warm|lc.nc-cool|lc.nc-paper|lc.nc-cine}` — the five looks,
    merged onto the keys they name

## What it does

Clear film is the per-channel base, from the rebate (or **Border**). A strong orange mask is
preferred. A pale or near-white rebate, such as old film or a raw whose camera white balance has
already lifted the mask, is the base when no orange is there. A clipped white lamp is never the
base. A click darker than the scan's own bright film is refused as too dark, not as blown out.
Among bright orange film, Auto uses the strongest orange, low in both blue and green. Density
is `D = -log10(transmission / base)`, which removes the constant orange mask and nothing else.
Scan gamma is removed first. Linear leaves the file alone. Gamma 1.8 and 2.2 raise each channel
to that power, and the print does the same, so a measured rebate is black. Frame detection and
the mask both classify that stored scan, so a pale rebate that only looks pale before gamma is
still the base, stored as the transmission.

A colour negative's straight line is `D ≈ 0.60 · log10(exposure)`. The positive is that exposure,
`(10^(D / γ) − 1)` divided by the same expression at a reference density. Density above that
reference stays above 1. It is not clipped first.
Contrast changes γ (`0.60 / grade`). Exposure is the scale that puts the middle of the picture on
middle grey, 0.18, the same grey the tone map is built around. Convert also sets contrast so the
bright end of that picture is 1: exposure and the white point are one scale, and contrast is the
slope, so both have to be solved or the print can only be moved up and down. A negative that is
still too steep at the softest grade keeps that grade, and a shoulder rolls the bright end back
to 1 without moving the middle. Margin, a look, and Match roll keep the contrast and solve
exposure again. A bright end that is close to neutral is gained
per channel, fading in toward that end, so a small bend does not tint the whites. A saturated
bright end, such as a blue sky, is left alone. Lowering Exposure does not bring a corrected
tint back. Blacks are an offset. A shoulder, when a look sets one, rolls off
highlights after that.

A color head still has to print a gray subject gray. Auto fits `D_c = fog_c + axis_c · D_green`
on bin medians of the density cloud, and keeps the fit only when the cast has the same sign at the
20th and the 80th percentile of green density. A cloud that is already neutral is left alone, so a
noisy fit cannot invent a slope. A green tree or a blue sky, present in only one half, stays.
Fog is never negative, so the mask stays black. A slope outside 0.25–3.2, or a fog above 0.8,
keeps only the constant both ends share. Clamping that line is not done: it would paint the other
end the opposite color. **Gray** sets the same axis from one tone. **Match roll** copies the axis
and places each frame's exposure again. **Auto** and **Border** fit it again.

The mask was built for color paper, not for a camera. Magenta dye absorbs blue, and cyan dye
absorbs green and a little blue (Hanson, J. Opt. Soc. Am. 40, 1950). The residual a scanner still
sees is removed with Hanson's inverse: `C = Dr`, `M = Dg − cg·C`, `Y = Db − cb·C − mb·M`. Camera
uses a wider residual than Minilab. Flat keeps integral density. An equal-density neutral moves
under that inverse; the gray axis puts it back. The paper aim is applied after the gray is
balanced. Neutral is an equal-channel gray. Portrait, Vivid, and Lab are our own paper aims.
Black-and-white ignores that aim and stays one channel. The panel does not offer a slide. A frame
already set to `colorPos` or `bwPos` stays a positive and only applies contrast.

Temperature, tint, and the three band balances are trims on the print, after exposure, so contrast
does not multiply them. At the end of a slider they move about 0.40 density.

The frame is a quad on the rebate that touches the border: orange for a color negative, bright
for black-and-white, dark for a slide. A thin negative is orange inside the picture too, so that
flood can swallow the scan. Convert then uses the sprocket and clear-film gate instead, and stores
whichever quad it found. **Auto** under **Crop** uses it, and when that quad is the whole scan it finds
the picture inside the sprockets instead. When no rebate is found the previous
quad is kept and **Convert** and **Auto** say so. A roll shares the mask color and the gray axis. Density is
already above each frame's own rebate, so the axis is copied rather than scaled. Each frame's
exposure is solved again with that axis, so the shared gray does not shift the midtone. A frame
whose rebate is more than half a stop from the roll is reported and still converted.

## Scanning

The rebate has to be in the file. A few millimetres of blank film at the edge is the measurement.
Lock the camera's white balance for the roll (Daylight, or one custom balance) so the rebate stays
the same orange on every frame. Expose so that border is bright and not clipped: a pure-white
rebate has no mask color left, and **Border** refuses it. A raw holds that measurement more faithfully
than a JPEG of the same negative.

## Limits

- A slope outside 0.25–3.2 is not clamped. Clamping it would paint the other end of the scale the
  opposite colour. A contact sheet of many pictures can land there; the axis keeps only the constant
  both ends share.
- A JPEG or a flatbed file has already been through a tone curve and a working-space conversion.
  The density formula is exact for linear camera RGB. On a rendered file it is the same curve
  applied to whatever the file stored.
- A frame with no rebate keeps the previous border. **Match roll** shares the mask color ratio and
  each frame's own rebate level, so it does not supply a mask to a frame that has none.
- A clipped rebate is refused as a base. The mask color is gone from that file.
- No manufacturer's stock profile and no scanner table. The dye numbers are a typical residual of
  a wide filter against a narrow one. The formula is the transmission model, Hanson's inverse, the
  film characteristic curve, and the gray axis. No plugin binary is used here.
