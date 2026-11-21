## Performance

- **High Efficiency NEF import:** the header probe used by import and Reload reads only the HE codestream headers (frame size, black level) instead of decoding the whole wavelet image, so importing HE / HE★ NEFs costs about the same as other raws (#1304).
