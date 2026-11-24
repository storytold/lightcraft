### Smoother Luminance noise reduction drags on the GPU

- Dragging the Luminance noise reduction sliders on the GPU keeps the colour-noise-reduced image between frames and reruns only luminance noise reduction, as the CPU already did: about 3.5× faster draft frames. The image after you let go is unchanged.
