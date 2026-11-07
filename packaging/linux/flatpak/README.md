# From-source Flatpak packaging

`ai.storyteller.lightcraft.yml` builds LightCraft and `lightcraft-cli` from this
repository inside the Flatpak SDK. Flatpak builders have no network access, so
`cargo-sources.json` lists every crate and git source with a fixed checksum or
commit. Regenerate it after changing `Cargo.lock`:

```sh
./packaging/linux/flatpak/generate-cargo-sources.py
flatpak-builder --user --install --force-clean build-flatpak packaging/linux/flatpak/ai.storyteller.lightcraft.yml
flatpak run ai.storyteller.lightcraft
```

## Regeneration wrapper

`generate-cargo-sources.py` uses the upstream
[`flatpak-cargo-generator.py`](https://github.com/flatpak/flatpak-builder-tools/blob/9d86d336a39b9af38b86f114b6ce5820fe260f91/cargo/flatpak-cargo-generator.py)
pinned to commit `9d86d336a39b9af38b86f114b6ce5820fe260f91` with SHA-256
`0a2db6be87d75910facef28ab46d4d6460802e8419ab850d0caa6a364d26b380`. It downloads
the script, verifies that hash, applies the two reviewed patches below, and
invokes it with the current Python interpreter. The upstream script is not
vendored; it is MIT-licensed
([upstream licence](https://github.com/flatpak/flatpak-builder-tools/blob/9d86d336a39b9af38b86f114b6ce5820fe260f91/LICENSE)).
The patched script's hash is also checked before execution.

Requirements: Python 3.9+ with `aiohttp` and `tomlkit` (`PyYAML` is optional and
only needed for upstream's YAML output), plus `git` and network access to fetch
the generator and git dependencies. Crate archive URLs and checksums are read
from Cargo.lock; Flatpak downloads those archives when materializing the sources. The generator clones git
repositories into `$XDG_CACHE_HOME/lightcraft-flatpak-cargo` by default, or the
directory passed with `--cache-dir`; the wrapper refuses a cache inside the
checkout or `CARGO_TARGET_DIR`. `cargo-sources.json` is written to a temporary
file and atomically renamed only after the generator succeeds, so a failed run
leaves the previous inventory untouched.

The regenerated inventory keeps the temporary file’s owner-only permissions (0600 on POSIX).
Run the generator and Flatpak builder as the same user; a cross-user packaging workflow
should explicitly install the inventory with its required ownership and permissions.

Generation is capped at 30 minutes and the generator's stdin is `/dev/null`, so
an interactive credential prompt fails instead of hanging. On POSIX systems
(Linux is the supported Flatpak packaging platform) the generator starts in its
own session; at the cap, the wrapper kills that generator's process group,
including git clone/fetch descendants, and reaps its leader, so no orphan keeps
writing into the cache. On Windows only the direct generator process is
terminated; its git descendants are not tree-killed, and the timeout is
otherwise the same. The timeout path runs before the atomic rename, so the
existing inventory and the temporary-file cleanup behave exactly like any other
generation failure.

Because the generator no longer shares the wrapper's terminal session, the
wrapper also handles Ctrl-C (`SIGINT`) and `SIGTERM` while it runs: the signal
is recorded, the generator's process group is killed and reaped like the timeout
path, the temporary file and script directory are removed, and only then is the
signal replayed with the handler that was installed before generation started
(`KeyboardInterrupt` for `SIGINT`, the previous or default action for
`SIGTERM`). A cancelled `flatpak-builder` run or an interactive Ctrl-C therefore
leaves neither a detached generator or git clone nor a modified inventory. The
temporary handlers are installed before the generator is spawned, so a signal
that arrives while the child is being created cannot strand the new session
either. A cancellation delivered while those handlers are installed is recorded
once (the first signal wins) and replayed after cleanup with the caller's
restored handler. Signals that cannot be handled by this process at all
(`SIGKILL`, an OOM kill, or a wrapper crash) cannot be cleaned up: the generator
runs in its own session, so it may survive the wrapper, and any temporary output
or script directory it had created remains. A hard termination between creating
the temporary directory or output file and installing the handlers, or after
`run_generator` returns while the output is being validated and renamed, can
likewise leave those temporary paths behind (the complete temporary output in
the latter case), because the default signal action performs no cleanup.

## Why two patches

1. **Keep pinned revisions distinct.** Upstream keys `[source]` replacement
   entries by the canonical repository URL and then adds `rev`/`tag`/`branch`.
   Two different revisions of the same repository therefore overwrite each
   other. The patch keys the entry by the full source URL without its `#`
   fragment, so `https://github.com/storytold/craft-libs?rev=...` remains a
   separate source for each craft-libs revision.
2. **Include `[patch.unused]`.** `Cargo.lock` records patched packages that are
   not in the resolved graph under `patch.unused`. Upstream iterates only
   `package`, which omits crates such as `egui_demo_lib`; `--locked` offline
   dependency resolution then fails because Cargo still loads those patched manifests. The
   patch also walks `patch.unused` entries.

No third patch is needed: upstream's `git_repo_name()` already appends the
first seven characters of the commit, so the two craft-libs checkouts get
different destinations.

## Offline validation performed

The committed inventory was generated from the current `Cargo.lock` and then
validated without contacting the network: every git/archive/shell/inline entry
was materialized as Flatpak would, crate archives were checked against their
recorded SHA-256 values, and the generated Cargo config was pointed at the
materialized vendor directory. With an isolated `CARGO_HOME` and
`CARGO_NET_OFFLINE=true`, `cargo --config <generated-config> metadata --offline
--locked --format-version 1` exited 0. The two craft-libs revisions
(`82a24b006eefa036b43c09a67fb9a3fbd17ffe63` and
`25da1314d443171b4adeaad2cd9454b273f35c6f`) are present as separate git sources
and `[source]` entries. The checked-in `cargo-sources.json` has SHA-256
`c7f515d1178bb79b2cabdfd419ad375a04ff5d84ba9d08b1b34970a460cd0f2c`.

This validates the source inventory and offline metadata resolution only. A
full `flatpak-builder` build against the `org.freedesktop.Sdk` 25.08 runtime and
Rust extension is still required before release; it was not performed here.

Run the wrapper failure-path and cancellation regression tests (the real
process-group cases, including Ctrl-C/`SIGTERM` cancellation of a live
generator session, run on Linux only and are skipped elsewhere) with:

```sh
python3 -B packaging/linux/flatpak/test_generate_cargo_sources.py
```

The cancellation and timeout tests check process liveness and a frozen
heartbeat before their teardown kills anything. With
`LIGHTCRAFT_FLATPAK_TEST_BREAK_KILL=1` the group kill is a deliberate no-op and
those tests must fail; run that variant to confirm they do not silently pass on
a broken kill path.
