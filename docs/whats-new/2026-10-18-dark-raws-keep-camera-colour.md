### Dark raws keep their camera's colour and tone
- A raw whose camera JPEG is too dark (or too colourless) to fit a look to no longer drops its camera profile's
  colour for the dull neutral fallback: it keeps the profile's colour, with the JPEG's tone curve when one fits, else
  the default tone curve. Photos of profiled cameras that were underexposed and pushed back up keep their colour: on
  NIKON D780 frames 3.7 EV under with a local profile, from about 40% of the properly exposed frame's colourfulness
  to about 95%.
- the command-line `calibrate` tool now also learns the camera's tone curve from all the photos it reads, and a profile
  made with it carries the curve. A photo of that camera follows its own JPEG's curve as far as that JPEG shows it
  and the camera's curve beyond, so an underexposed shot pushed back up matches a properly exposed one in
  brightness instead of coming out 4–7 L* darker. Profiles made before keep working; refit them to get the curve.
