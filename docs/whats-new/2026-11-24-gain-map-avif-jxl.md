### HDR gain map export for AVIF and JPEG XL

- Export ▸ File Settings has a new **HDR mode** choice for AVIF and JPEG XL with **HDR output** on: **Gain map** writes an ordinary SDR image plus an HDR gain map (ISO 21496-1), so the file looks right everywhere and shows brighter highlights where the viewer reads gain maps. The AVIF files are read by libavif tools; JPEG XL gain maps are a libjxl layout that few viewers show as HDR yet.
- **HDR image** stays the default: one Rec. 2020 PQ (or HLG) file, as before. JPEG always uses a gain map.
- `app.export` takes `hdrMode: "gainMap"` or `"hdr"` for the same choice.
