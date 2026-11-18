## Updater and security

- **Plug-in author signatures:** `plugin-stamp keygen` and `plugin-stamp sign` add an ed25519 signature section to a stamped plug-in. The first signed install pins its key in `plugins.json`; a later file signed by another key, or unsigned, installs only on the user's yes (the install dialog's **Install Anyway**, or `acceptSigner` / `acceptUnsigned` on `plugin.install`), and an invalid signature is refused. At start-up a plug-in that no longer matches its pin isn't loaded and keeps its grants. `plugin.inspectFile` and `plugin.list` report the signer. Issue #1271.
