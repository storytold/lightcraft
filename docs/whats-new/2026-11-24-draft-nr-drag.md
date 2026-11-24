### Smoother noise reduction sliders without a GPU

- Dragging the Luminance or Detail noise reduction slider on the CPU renderer now reuses the colour noise reduction instead of redoing it every frame, so the preview keeps up about twice as well. The final image when you let go, and every export, are unchanged.
