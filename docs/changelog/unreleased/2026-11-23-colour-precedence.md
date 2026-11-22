## Camera colour

- **Changed:** one ranked order decides where a raw's starting colour comes from (your default, your `.dcp`, a Lightroom calibration, the file's matrices, measured matrices, the camera-JPEG fit, estimated colour); `calibrate-lightroom fit` keeps a calibration that loses to the per-photo JPEG fit but beats the plain colour as the default for files with their own colour matrices only (`ownMatrixOnly`, converted DNGs) (#1119).
- **Fixed:** the Profile section's Calibrated camera colour row and the hot-pixel count line stay laid out (pending) while a photo is decoded, so the Develop panel no longer jumps between photos (#1272).
