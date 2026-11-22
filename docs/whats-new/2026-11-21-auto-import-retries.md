### Auto Import picks up photos whose copy wasn't finished
- A photo that Auto Import tried while it was still being copied into the watched folder (a slow or stalled copy, including one that sets the final size first and fills it in) is now tried again once the file changes, instead of waiting for a Synchronize Folder (issue #1302).
- An Auto Import that can't start, is cancelled, or finds another import already open leaves its photos for the next scan.
