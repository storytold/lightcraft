## Import, export and formats

- **HDR gain map export for AVIF and JPEG XL:** with HDR output on, the new HDR mode choice (Gain map or HDR image, the default) writes an SDR image with an ISO 21496-1 gain map, or one PQ / HLG file as before. The AVIF follows the tone map derived item layout libavif writes; the JPEG XL carries libjxl's gain map box. `app.export` takes `hdrMode: "gainMap"` or `"hdr"`.
