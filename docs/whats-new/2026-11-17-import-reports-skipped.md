### Scripted imports say what they skipped
- `library.import` now reports what its folder walk left out, `skipped: {unreadable, unsupported}`, as
  `library.importPreview` already did, so a script or MCP client can tell a skipped file from a missing one.
