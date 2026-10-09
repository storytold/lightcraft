# Merging upstream (LightCraft)

This repository is a fork of LightCraft. The fork point is tagged `fork-base`, so

```sh
git log --oneline fork-base..upstream/main
```

always lists what upstream has done since. Merge it in **weekly** at first (more often while the code bases are still
close, since small merges are cheap and big ones are not).

## Remotes (one-time)

```sh
git remote -v                    # upstream = the LightCraft repository, origin = this fork
git remote add upstream ../lightcraft   # only if it is missing
```

## The routine

1. **Fetch.**
   ```sh
   git fetch upstream
   git log --oneline fork-base..upstream/main | wc -l   # how much is new
   ```
2. **Merge on a branch** named for the day:
   ```sh
   git switch -c merge/upstream-$(date +%Y%m%d) main
   git merge --no-ff upstream/main
   ```
   Git's rename detection maps upstream's `apps/lightcraft`, `apps/lightcraft-cli` and `apps/lightcraft-web` onto
   `apps/app`, `apps/cli` and `apps/web`. Resolve content conflicts as usual, but **never by hand-editing names**
   (step 3 does that).
3. **Map upstream crate names to ours.** Upstream calls its crates `lightcraft-*` (paths `lightcraft_*`); ours use
   the internal prefix (`dac-*`). Rewrite every incoming reference in the files the merge touched:
   ```sh
   cargo xtask rename-crates --upstream            # files changed since fork-base, plus conflicted files
   cargo xtask rename-crates --upstream --since HEAD~1   # narrower: only what this merge brought
   ```
   Only names of crates this workspace has are rewritten (`lightcraft-raw` → `dac-raw`, `lightcraft_engine::` →
   `dac_engine::`, `-p lightcraft` → `-p dac-app`). A crate upstream **added** is not known yet: rename its package
   to `dac-<name>` in its `Cargo.toml`, register it in `xtask/src/layers.rs`, and run the command again.
4. **Remove upstream's product name** everywhere else. Upstream writes "LightCraft", `lightcraft`, `LIGHTCRAFT_*`
   and its app id into code, docs and packaging; here every name comes from `brand.toml`:
   ```sh
   cargo xtask brand check
   ```
   lists each `file:line`. Fix them with the brand constants (`dac_brand::DISPLAY_NAME`, `dac_brand::env("LOG")`,
   `dac_brand::fill("… {app} …")`), `{{app}}`/`{{binary}}` placeholders in `*.md.in` docs and `packaging/**/*.in`
   templates, or neutral wording ("the app"). New legacy handling (code that must name an old identity to migrate
   from it) goes in `LEGACY_FILES` in `xtask/src/brand.rs`, and the old values in `[legacy]` of `brand.toml`.
5. **Regenerate and verify.**
   ```sh
   cargo xtask docs          # re-render *.md.in after upstream doc changes
   cargo xtask ci            # fmt, brand check, docs --check, clippy, tests, parity, layers, assets, deny, wasm
   cargo xtask brand test    # optional locally; CI runs it: the throw-away brand shows nowhere else
   ```
6. **Commit, review, merge.** Commit the merge (one commit for the merge itself, separate commits for the
   rename and brand fixes keep the history readable), open a PR from `merge/upstream-YYYYMMDD`, and merge it into
   `main` once CI is green.

## Notes

- Upstream renames are resolved by the brand tooling (`rename-crates --upstream`, `brand check`), never by
  hand-editing names; if a pattern keeps coming back, teach the tooling instead.
- Licence: upstream code arrives under MIT OR Apache-2.0; we use it under MIT and keep upstream's copyright lines in
  `LICENSE`/`NOTICE`. `crates/segment` and `crates/fetch` stay Apache-2.0. `cargo deny check licenses` (part of
  `cargo xtask ci`) catches a dependency upstream adds under a licence `deny.toml` does not allow.
- To change our own crate prefix (an internal codename users never see), run `cargo xtask rename-crates <prefix>` in
  a commit of its own, then re-check that `--upstream` still maps cleanly.
