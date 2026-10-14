# Private EOS D60 corpus and controlled captures

The maintainer's own Canon EOS D60 files in `../RawImages/` (local only, never copied into
the repository). Names, sizes and SHA-256 of every file are in
`logs/private-corpus.tsv`; no image content is recorded anywhere. All 1,561 files decode
sample-exact against LibRaw 0.22.2 `unprocessed_raw.exe -T`.

| Set | Files | What it is | What it established |
|---|---:|---|---|
| `RawImages/` | 1,516 | Everyday photos, 2005–2014, all Auto white balance; ISO 100 (925), 200 (197), 400 (287), 800 (34), 1000 (73) | Freeze revision 2: two incomplete code tables and the plane-without-0xff case (`06-crw-format.md`); the low-ISO clip level (`08-color.md`); the D60 colour profile (`assets/camera-profiles/Canon_EOS_D60.json`, 1,424 usable photos) |
| `Cap&WhiteOut/` | 4 | Lens cap and blown-out frames, ISO 100 and 1000 (2026-10-13) | Black level at ISO 100 matches the image area; at ISO 1000 the border reads about 4 DN high. Clip level about 4000 at ISO 100, 4095 at ISO 1000 (`08-color.md`) |
| `WBModes/` | 7 | One overcast scene at Auto, Daylight, Cloudy, Tungsten, Fluorescent, Flash, Custom; ISO 100, within a minute (2026-10-14) | White-balance setting n uses ColorBalance slot n (`08-color.md`) |
| `TripodGrayCard/` | 14 | The same card filling most of the frame, tripod, f/8, ISO 100; two brackets (Auto 1/16–1/6 s, Cloudy 1/12–1/5 s), each starting and ending on the same speed; 14 s each (2026-10-14) | Black level confirmed; sensor linear; the spill strip (below) |
| `GrayCard/` | 20 | An 18 % grey card (film-era), handheld, overcast; 5 frames at f/4 with exposure compensation, 15 at f/1.8 stepping 1/640–1/1600 s under Cloudy, Daylight and Auto (2026-10-14) | See below |

## Grey-card results (2026-10-14)

Measured in the middle 28 % of each frame, which lies on the card in every frame (the card
is larger and was handheld; spread across the box about 3 %, sensor noise included).

- **Tone matches the camera.** Our render's card lightness is within 0.3 L\* of the camera
  JPEG's in all six frames checked (L\* 37.6–59.0 across exposures), so the profile's
  per-photo tone fit reproduces Canon's curve at mid-grey.
- **Neutrals carry a slight magenta.** Our card is a\* +0.6 to +1.5 above the camera's,
  b\* 0.4–1.0 below (about 1–1.5 ΔE, under the usual threshold of visibility). It is the
  same small bias seen on the white-balance series' foliage, measured here on a true
  neutral. The profile matrix's rows sum to 1.005, 0.998 and 1.008, which accounts for part
  of it. Left for the chart refit, which can hold neutrals exactly.
- **Overcast neutral.** The card needs R × 2.68 and B × 1.13 (G = 1) to render neutral, the
  same in all four groups to ±0.03. That lies between the camera's Daylight (2.54 / 1.22)
  and Cloudy (2.80 / 1.13) presets; its Auto chose R 2.54–2.60, B 1.14–1.16, slightly cool,
  which our As Shot reproduces, as it should.
- **Black level:** these handheld brackets could not pin it through exposure (shutter
  accuracy at 1/640–1/1600 s, and the light dimmed about 10 % over the 90 s); the tripod
  set below settles it. *Correction:* the "border measurement of 130.4–132.0" first written
  here averaged every masked column, including the 8 next to the image, which catch spilled
  light. The decoder skips that strip and reads 127.6–128.6 on these 20 frames; the
  colour-ratio method's 124.5–127.5 is within 4 DN of it.
- **What it cannot do:** a single neutral says nothing about how colours map, so the card
  does not change the colour profile. The chart will.

## Tripod grey-card results (2026-10-14)

- **The masked strip next to the image catches spilled light.** Columns 58–65 rise with the
  scene: 157 at 1/16 s, 206 at 1/6 s, while the columns further out stay at 127–130. The
  decoder's darkest-strip rule already excludes it: its black level stays at 127.2–128.7
  through both brackets, whatever the exposure.
- **Black level confirmed.** Each bracket was fitted with level = black + k · time · (1 + drift
  · seconds), the drift taken from the repeated speed (the light fell 5–6 % within each 14 s
  bracket; without correcting for it the fit puts black near 120). Red, the channel least
  sensitive to any remaining exposure error, gives 128.0 and 128.2 against the decoder's
  127.9 and 128.3; green and blue give 129.9–133.1; the colour-ratio method gives
  125.5–126.2. All within 5 DN of the decoder, so no change.
- **The sensor is linear.** With the drift corrected, every frame lies within 1.3 DN of the
  fitted line on the Auto bracket and within 4 DN on the Cloudy one.
- **Lens falloff at f/8:** the card's level varies about ±8 % across the frame (5 × 5 cells,
  lightest at the bottom, darkest top right), part vignetting, part uneven light on the
  card. A colour chart in the middle third will see well under that.
