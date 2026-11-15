## Performance

- **Plug-in filter stacks no longer slow down with every filter added:** the renders of one edit (the loupe, the preview, the grid's cell, an export) wait for the one full-resolution run of a stack whose filter reads neighbouring pixels (a degrain, a sharpen) instead of each running it at once, and a render that is replaced (a slider drag starting, the loupe moving to another photo) stops its plug-in filters where they are instead of finishing beside the new one. Before, each render ran the whole stack itself, all at once, so every added filter made every edit slower.
