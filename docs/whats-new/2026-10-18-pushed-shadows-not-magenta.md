### Pushed shadows of raws no longer turn magenta
- Raw decoding no longer clips each colour channel at zero. Near black, sensor noise through the camera's colour
  matrix has negative values, and clipping kept only the positive half of each channel's noise, so dark areas
  pushed up came out purple or magenta (a lens-cap frame pushed 4 EV rendered magenta; it is now black). Shadows
  pushed with Exposure or Shadows keep their real colour, most visibly with saturating camera profiles.
