### Histogram display modes, vectorscope, scales and colour-coded clipping (issue #206)
- The histogram is no longer one fixed plot: the small selector under it — or a right-click on the plot, or
  `view.histogram.mode` — switches between **RGB Overlay**, the additive R/G/B view whose pairwise overlaps read
  yellow, cyan and magenta and whose three-way overlap reads neutral grey (with the luminance as a hairline over it);
  **Luminance**, one monochrome curve; **Separated Channels**, three stacked tracks on one shared scale, so a single
  channel's clipping and colour-balance shifts are unambiguous; **Chroma**, the photo's saturation as one curve; and
  **Vectorscope**, a 2D chrominance disc with 25 % / 50 % / 75 % / 100 % saturation rings, the 123° skin-tone line and
  the six 75 % primary / secondary target boxes (`R`, `Mg`, `B`, `Cy`, `G`, `Yl`), coloured by each cell's hue and
  saturation so colour casts and skin-tone alignment read at a glance.
- **Histogram Scale** (`view.histogram.scale`) switches the density scale between **Linear**, now drawn against the
  99th percentile of the bins (98th of the populated vectorscope cells) so one spike (a crushed black, a blown sky, a
  neutral backdrop) no longer flattens everything else, and **Logarithmic**, which lifts the quiet end of the
  distribution. Both choices are saved with the app settings, and either command can be given a keyboard shortcut, run
  from the control channel or reached from MCP.
- The clip indicators in the plot's top corners are triangles again: hollow while nothing clips, filled with the
  colour of the channels that do — white for all three, yellow / cyan / magenta for a pair, one channel's own colour
  for one. Resting the pointer on one previews the clipping mask on the photo without switching it on.
