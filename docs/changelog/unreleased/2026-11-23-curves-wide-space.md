## Develop

- **Changed:** process V10 applies the Tone Curve (point and region curves, Refine Saturation) in ProPhoto primaries with gamma 1.8 before the output conversion instead of on the output's sRGB-encoded values; Lightroom channel-curve edits match better and no longer depend on the export colour space. Brightness and Levels move with it, still before the curve (#1142).
