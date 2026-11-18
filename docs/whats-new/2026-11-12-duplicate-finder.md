### Duplicate Finder

- Library ▸ Find Duplicates… groups the photos that are copies of one another, the best copy of each group first (most pixels, then the largest file), the others marked: Remove sends them to Recently Deleted, Reject flags them, Stack Groups collapses each group into a stack. Copies with the same file bytes are found at once; Find Similar Copies also reads every photo's picture and finds copies that differ (resized, re-compressed, renamed) and raw + JPEG pairs of the same shot.
- The same scan is a command: `library.findDuplicates {similar}` returns the groups (with each photo's name, size and resolution) for the control channel and MCP.
