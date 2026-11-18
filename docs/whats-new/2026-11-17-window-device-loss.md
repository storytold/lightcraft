### No more frozen window after a GPU device loss

- When the graphics driver loses the window's GPU device while the app runs, the app no longer freezes: it saves the library and settings, says what happened, and ends. From the next start the window uses another renderer (on Windows DX12 after Vulkan), and the lost one is tried last from then on. `<PREFIX>_GPU_BACKEND` still chooses one (issue #1194).
