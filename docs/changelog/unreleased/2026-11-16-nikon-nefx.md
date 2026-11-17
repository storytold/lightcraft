## Import, export and formats

- **Nikon NEFX:** the merged pixel-shift images NX Studio writes decode as raw files (three 14-bit samples per pixel, no demosaicing; black level, white balance and distortion correction as for a camera NEF), and the `nefx` extension is imported. Verified on 16-shot merges from one body, the Z 8; 4-, 8- and 32-shot merges and other bodies are unchecked. Previews and thumbnails are binned straight from the samples, so a 181 MP merge no longer needs a full-size picture for the loupe. PR #1267.
