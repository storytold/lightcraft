### Untouched raws keep their camera preview
- A raw you haven't edited shows its camera preview at once, even when its as-shot white balance hasn't been read from the file yet or was stored on the older relative scale; it no longer counts as edited or waits for a full decode.
- Such a raw still starts from a raw's defaults (sharpening, colour noise reduction, Remove Hot Pixels), so Reset gives it the same look import did.
