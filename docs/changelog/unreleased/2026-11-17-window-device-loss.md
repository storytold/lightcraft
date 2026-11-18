## Display

- **Window device loss no longer freezes the app:** when the window's GPU device is lost (seen with Vulkan on AMD integrated graphics under Windows), the library and settings are saved, a message explains it, and the next start draws the window with the next renderer, trying the lost one last from then on. Issue #1194.
