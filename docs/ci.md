# CI and landing changes

> **Last updated:** 2026-10-15 · **Change:** minor (release jobs upload straight to the draft release, not via Actions artifacts); major (every workflow on self-hosted runners, containers on gpu0, fork PRs approved after review); minor (test retries, flaky reports and the test quarantine for the full run); major (fast PR gate; full test runs only before a release); major (created: PR-only main, self-hosted runner, local status)

## How changes reach `main`

`main` on the fork is protected by two rulesets:

- **No force-push, no deletion** — nobody bypasses this.
- **Pull request required** (0 approvals, merge commits only), the **"Linux (cargo xtask ci)"** check must be
  green and the branch up to date with `main`. Repository admins can bypass it for emergencies only.

Every change goes on a branch, opens a PR and is set to auto-merge
(`gh pr merge <n> --repo <owner>/<repo> --auto --merge`); it lands when the check passes.

## Fast gate on PRs, full CI before a release

| Trigger | Linux ("Linux (cargo xtask ci)") | macOS / Windows |
|---|---|---|
| Pull request to `main`, push to `main` | `cargo xtask ci --fast` + SIL suite | build + clippy |
| Push to `release`, manual dispatch (Actions → CI → Run workflow) | full `cargo xtask ci` + SIL suite | build + clippy + `cargo test --workspace` |

`cargo xtask ci --fast` runs fmt, brand, docs, notes (release-note entries, `docs/whats-new/README.md`), clippy
`-D warnings` (incl. the HEIF feature), parity refs, shared-check, layers, assets, deny and the wasm check; it skips
the workspace and HEIF test runs. The SIL step runs `cargo xtask sil` (docs/testing-sil.md) once that command
exists and is skipped until then. Locally, `cargo xtask ci` stays the full run; use `--fast` per commit and run the targeted tests of what you touched.

Before cutting a release, dispatch CI on the commit (or push it to `release`, which also starts
`release.yml`) and make sure all three OSes are green.

## Where the jobs run

GitHub-hosted minutes are used up, so **every job of every workflow runs on the maintainer's self-hosted
runners** (all carry the label `lcc`):

| Runner | Labels | Machine | Runs |
|---|---|---|---|
| `gpu0`, `gpu0-b` | `self-hosted, Linux, X64, lcc` | CachyOS, two runner instances (`~/actions-runner`, `~/actions-runner-b`) | CI Linux, Brand, Immich, Release version/web/draft; in Docker containers: Packaging lint, Print, Release Linux and Flatpak |
| `mac0` | `self-hosted, macOS, ARM64, lcc` | Apple silicon, LaunchAgent | CI macOS, Release macOS (signing, notarization) |
| `win0` | `self-hosted, Windows, X64, lcc` | Windows 11 x64, service as NETWORK SERVICE | CI Windows, Packaging lint's MSI probe, Release Windows x64 |
| `SNAPDRAGON` | `self-hosted, Windows, ARM64, lcc` | Windows 11 on ARM, service as NETWORK SERVICE | Windows ARM64 (native build + install test), Release Windows ARM64 |

`freebsd.yml` stays disabled; the disabled riscv64 and FreeBSD release jobs point at gpu0 but need work
(a container, KVM) before they are re-enabled.

Release platform jobs upload their files straight to the draft GitHub release with `gh release upload`
(`.github/scripts/install-gh.sh` fetches the CLI into `$RUNNER_TEMP` where a runner lacks it), not through
Actions artifacts: from these runners `actions/download-artifact` ran at about 0.3 MB/s (the 0.8.2 release
job spent over an hour downloading), `gh release` at about 11 MB/s. Only a few-byte checksum file per platform
(and, on a dry run, the Linux tarball for the Flatpak job) still goes through artifacts. See
[releasing.md](releasing.md).

Jobs run at low priority, since the machines are also used interactively: Linux steps run under
`nice -n 19` (job `defaults.run.shell`), macOS steps under `nice -n 19` (not `taskpolicy -b`: background QoS confines builds to the efficiency cores), and Windows
jobs set the runner worker's priority class to BelowNormal in their first step (later steps inherit it).
The Windows ARM64 builds on SNAPDRAGON (12 cores) set `CARGO_BUILD_JOBS: 10` in the job, overriding the runner's own
limit of 4 and leaving two cores for the desktop; the x64 Windows release job keeps cargo's default.

Each runner instance has its own build cache (`CARGO_TARGET_DIR` from the runner's `.env`, else
`~/.cache/lcc-ci-target-<runner name>`), so several instances on one machine (`gpu0` and `gpu0-b`) can run
jobs at once without fighting over one build directory. Release jobs use a separate
`lcc-release-target-<runner name>` directory (`C:\lcc-ci\release-<runner name>` on Windows: a short path, since `rc.exe` fails on long ones) so release builds don't evict the
CI cache. Only the Linux check blocks merging.

### Containers on gpu0

Jobs that need apt packages or an old glibc run in Docker containers on gpu0 (`container:` in the job):
Packaging lint (`ubuntu:24.04`), Print (`rust:1-bookworm`, which starts its own `cupsd`), Release Linux
(`ubuntu:22.04`, so the binaries need only glibc 2.35) and Release Flatpak (`ubuntu:24.04`, `--privileged`
because flatpak-builder and `flatpak run` sandbox with bubblewrap). Immich runs cargo on the host and its
server with the host's `docker compose`. Caches live in named Docker volumes, never in the runners' own
target dirs: `lcc-ci-cargo-registry`, `lcc-ci-target-print`, `lcc-release-cargo-jammy`,
`lcc-release-rustup-jammy`, `lcc-release-target-linux`, `lcc-release-flatpak-user`
(`docker volume ls | grep lcc-`; `docker volume rm` one to start it clean). Containers run as root, so each
container job ends by handing the workspace back to the runner user (`chown`), or the next checkout on the
host would fail on root-owned files.

Docker setup (once, by the maintainer): `sudo pacman -S docker docker-compose`, `sudo systemctl enable --now
docker`, `sudo usermod -aG docker <runner user>`, then restart both runner services (`sudo ./svc.sh stop && sudo
./svc.sh start` in each runner directory) so they pick up the group. `docker info` must work without sudo.

### Outside contributors' PRs

PRs from forks run on the same self-hosted runners. Settings → Actions → General → *Approval for running
fork pull request workflows from contributors* is **Require approval for all external contributors**
(`gh api repos/<owner>/<repo>/actions/permissions/fork-pr-contributor-approval` →
`all_external_contributors`). Before approving a run, the maintainer reads the PR's whole diff, workflows
and build scripts (`build.rs`, `xtask/`, `packaging/`) included, since the code runs on their machines;
after any new push the run needs approving again. Re-review before every approval.

### Windows on ARM runner notes

SNAPDRAGON has Visual Studio 2022 Build Tools with the ARM64-hosted ARM64 MSVC tools and the Windows 11 SDK
(10.0.26100), the machine-wide Rust in `C:\rust` (host `aarch64-pc-windows-msvc`), the .NET 8 SDK and WiX,
so Windows ARM64 builds natively there. win0 has no ARM64 MSVC tools, so it doesn't cross-compile. Like
win0, the runner account can't write to Program Files: workflows install the .NET SDK
(`actions/setup-dotnet` with `DOTNET_INSTALL_DIR`) and WiX into the runner's tool cache.

### macOS runner notes

The runner is a LaunchAgent of the logged-in user (`./svc.sh install` in `~/actions-runner`, no sudo), using
that user's `rustup` in `~/.cargo/bin` (set in the runner's `.path`) and Xcode's toolchain. `.env` sets
`CARGO_TARGET_DIR=~/.cache/lcc-ci-target`. A LaunchAgent runs only while that user is logged in.

### Windows runner notes

The runner runs as a service under NETWORK SERVICE, so Rust is installed machine-wide
(`RUSTUP_HOME=C:\rust\rustup`, `CARGO_HOME=C:\rust\cargo`, with `C:\rust\cargo\bin` on the machine
`Path`) and NETWORK SERVICE may modify `C:\rust`. The runner's `.env` file sets those two variables and
`CARGO_TARGET_DIR=C:\lcc-ci-target` (the persistent build cache); its `.path` file holds the machine
`Path`. Visual Studio Build Tools (C++) provide the linker. The runner reads `PATH` from `.env` (its
`.path` file is not what step shells search), which puts `C:\Program Files\Git\bin` first, so workflow
`bash` steps get Git Bash rather than the WSL launcher in `System32` (which fails under the service
account). PowerShell 7 (`pwsh`, machine-wide MSI with "add to PATH") is required: actions such as
`taiki-e/install-action` run their Windows steps with `shell: pwsh`, which the built-in Windows PowerShell
5.1 doesn't provide. Windows PowerShell's machine execution policy is `RemoteSigned`, so the runner's
temporary step scripts (`shell: powershell`) may run. After changing `.env`, restart the service
(`Restart-Service actions.runner.*`).

### Setting up the runner machine

1. Install the system libraries the build needs (Wayland, X11, xkbcommon, OpenGL, udev development packages;
   on Debian/Ubuntu: `libxkbcommon-dev libwayland-dev libx11-dev libxrandr-dev libxi-dev libgl1-mesa-dev
   libudev-dev`), `git`, a C linker, and
   `rustup` (the workflow installs the stable toolchain, `wasm32-unknown-unknown`, rustfmt and clippy).
2. Repository → Settings → Actions → Runners → **New self-hosted runner** (Linux x64); follow the commands,
   and when `config.sh` asks for labels add **`lcc`**.
3. Run it as a service (`sudo ./svc.sh install && sudo ./svc.sh start`) under a normal user, not root.
4. Settings → Actions → General → *Fork pull request workflows from outside collaborators*:
   **Require approval for all outside collaborators** (see above).
5. For the container jobs: Docker (see *Containers on gpu0*).

If the runner is offline, jobs for our own branches wait in the queue; post a local result instead (below).

## Retries, flaky tests and the quarantine

The full pre-release run (`cargo xtask ci` without `--fast`, on a push to `release` or a manual dispatch) runs the tests with [cargo-nextest](https://nexte.st) (installed by `taiki-e/install-action`, into the
runner's cargo home), profile `ci` in `.config/nextest.toml`:

- **Retries:** a failing test is retried up to 2 times. One that then passes is reported as **FLAKY** in the
  log and the final summary; the run stays green, but a flaky test is a bug: fix it.
- **Hangs:** a test is reported slow after 2 minutes and killed after 10, so a hang fails in minutes.
- **Doctests:** nextest doesn't run them; `cargo test --workspace --doc` runs as a separate step.

On Linux, `cargo xtask ci` uses nextest when `CI=true` (GitHub sets it) or `XTASK_NEXTEST=1` and nextest is
installed; locally it stays a strict `cargo test --workspace`. In the full run the macOS and Windows jobs run
`cargo xtask quarantine test`; the fast PR gate runs no tests, so retries and the quarantine don't apply to it. Without nextest both fall back to plain `cargo test --workspace`.

### Quarantine

`ci/quarantine.toml` lists known-flaky tests per platform. On those platforms the gating run excludes them;
each then runs on its own, and a failure shows as a `Quarantined test failed` warning annotation on the run
(naming the test, platform, issue and reason) instead of failing the job. Rules:

- **Robustness fixes first.** Quarantine is the last resort for a test that fails for environmental reasons
  while the fix is underway, never a way to hide a real regression.
- **Every entry needs a tracking issue (`issue = "#N"`) and an expiry (`until`, at most a few weeks out).** After the
  expiry the test gates again and every run warns `Quarantine entry expired`; fix it or renew the entry
  with a reason.
- The entry comes out in the same PR as the fix, which closes the issue.
- `cargo xtask quarantine check` (a step of both `cargo xtask ci` and `--fast`) rejects malformed entries; `cargo xtask quarantine
  filter --platform macos` prints the nextest filter a platform uses.

## Posting a local result

When `cargo xtask ci` (the full run) has to run anyway (or the runner is offline), run it locally on the pushed branch
and post the result as the required check:

```
tools/post-local-ci.sh
```

It refuses with uncommitted changes or an unpushed HEAD, runs `cargo xtask ci`, and posts a commit status
named "Linux (cargo xtask ci)" (success or failure) for HEAD with `gh`. Auto-merge then proceeds. The
status is only as trustworthy as the run behind it: post it only from a real, complete run.
