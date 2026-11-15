## Development

- **Stamped plug-in manifests:** `plugin-stamp` (`sdk/stamp`), a small program for plug-in authors, runs a plug-in's `lcc_manifest` once in a sandbox and copies its answer into the `.wasm` as a `photo-plugin.manifest` custom section, so tools can read a plug-in's id, version and permissions without running it. `plugin-stamp --check` fails a build whose section is missing or stale. Nothing changes for plug-ins that aren't stamped. See "Stamping your plug-in" in `docs/plugins.md`.
