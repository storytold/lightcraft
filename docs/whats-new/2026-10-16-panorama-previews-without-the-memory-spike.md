### Panorama previews without the memory spike
- The Photo Merge dialog no longer decodes every photo at full size for its preview: a dozen 48 MP drone DNGs took
  40 GB of memory and minutes before the preview appeared; now 2 GB and 2 seconds. Changing an option (projection,
  deghost amount) reuses the photos already decoded, so the preview updates in about a second.
- HDR panoramas are merged one bracket at a time, so only one bracket's photos are held at full size.
- DNGs that correct lens shading with a gain map (DJI drones and others) show their previews and thumbnails sooner.
