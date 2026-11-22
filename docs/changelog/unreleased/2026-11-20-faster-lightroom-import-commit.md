## Performance

- **Faster end of a Lightroom import:** looking a camera up by name in the camera database and the spectral-sensitivity table no longer normalises every row on every call; both use a hash index built once, with identical answers (a test compares the index with the old scan on every row, alias, make and model spelling it can build). The window freeze while an imported catalog is added to the library drops by about 75% (400 Nikon Z9 photos: 2.8 s to 0.7 s, measured alternating with the previous build on one machine). Part of issue #1303.
