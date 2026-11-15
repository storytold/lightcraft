## Library

- **Sharp thumbnails of tight crops:** a cropped photo's grid thumbnail was cut from the 512 px thumbnail source and stretched, so tight crops came out blurry. Past a 2x stretch it now reads the preview-level source at the size the crop needs (never the original), without pushing the loupe's preview out of memory; blurry thumbnails cached before render again. Issue #1197.
