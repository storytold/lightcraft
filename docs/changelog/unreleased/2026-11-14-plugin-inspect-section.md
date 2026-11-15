## Development

- **Stamped manifests are read and checked:** `plugin.inspectFile` reads a stamped plug-in's `photo-plugin.manifest` section without running the module, and says so (`source: "section"`, `verified: false`); `verify: true`, and every file without a section, still read `lcc_manifest` (`source: "export"`, `verified: true`). Installing, updating or starting a plug-in whose section differs from `lcc_manifest` is refused, naming the fields that differ. The install dialog lists `lcc_manifest`'s permissions, as before.
