### Windows on ARM uses DirectX 12 first

- On Snapdragon (Windows on ARM) laptops the window and GPU rendering now start on DirectX 12, with Vulkan as the fallback. The Adreno Vulkan driver could lose its device while scrolling the library. `<PREFIX>_GPU_BACKEND=vulkan` still forces Vulkan.
