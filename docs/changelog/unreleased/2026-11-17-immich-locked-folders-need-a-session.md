## Fixes

- **Immich locked folders:** Locked Folders… (and `immich.authStatus` / `immich.unlockSession`) no longer fail with a missing-permission message that a key can't fix. Immich keeps its locked folders for a signed-in session (PIN or password) and never opens them for an API key, so the error now says so (`error.kind` `sessionRequired`) in every language; a refused PIN or password is still reported as such. Issue #1253.
