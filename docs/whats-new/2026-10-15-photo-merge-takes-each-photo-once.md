### Photo Merge takes each photo once
- A merge that names the same photo more than once (through `ids` on the control channel, MCP or the CLI) now merges
  it once (issue #1947). Naming one photo three times no longer "merges" it with itself, and a long repeated list no
  longer decodes the photo again for every entry.
