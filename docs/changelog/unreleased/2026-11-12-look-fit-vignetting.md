## Develop

- **Camera look fitted on the vignetting the camera JPEG shows:** the per-file look fitted to the camera JPEG now measures how much of the lens database's vignetting correction that JPEG applied and fits on that share, instead of assuming all of it. Canon EOS 6D photos whose JPEGs keep their dark corners rendered 5 to 11 L* under the camera JPEG; they now land within about 1 L* (lens correction off). Photos with a calibration file and cameras with no vignetting data are unchanged. The look version and the thumbnail cache version are bumped.
