// Written by `tools/lr_tone_charts.py base`: LightCraft's default base curve, fitted to the tool's neutral
// grey-step chart (provenance in docs/default-tone.md). Knots of the curve only.

/// The knots' range in stops of the chart's linear value: from the darkest grey step to white.
pub const STOPS: (f64, f64) = (-8.965784e+00, 0.0);
/// The curve's linear ProPhoto output at the knots, evenly spaced over [`STOPS`].
pub const KNOTS: [f64; 12] = [0.000000e+00, 8.133183e-04, 3.366455e-03, 7.031622e-03, 1.633029e-02, 3.958236e-02, 9.053161e-02, 1.968403e-01, 3.887485e-01, 6.233585e-01, 8.545124e-01, 1.000000e+00];
