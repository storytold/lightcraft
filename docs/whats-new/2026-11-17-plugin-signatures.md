### Signed plug-ins

- The plug-in install dialog now says who signed a plug-in, by its author key's fingerprint, or that nobody did. When an update is signed by another key than the installed version, or not signed, the dialog names both and installs only on your **Install Anyway** (issue #1271).
- Plug-in authors sign a stamped plug-in with `plugin-stamp keygen` and `plugin-stamp sign`; the signature travels inside the `.wasm` file.
