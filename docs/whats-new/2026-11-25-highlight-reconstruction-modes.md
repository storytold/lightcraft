### Highlight reconstruction methods

- **Detail ▸ Sensor ▸ Highlights** chooses how a raw's clipped highlights are rebuilt: **Rebuild** (as before),
  **Inpaint Opposed** or **Guided Laplacian**, the last two ported from darktable.
- Inpaint Opposed is quick and suits small specular highlights and skies; Guided Laplacian is slower and keeps texture
  and colour gradients in larger clipped areas. Fully blown areas stay neutral with every method.
