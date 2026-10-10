# Tethered capture

> Names in angle brackets are the values set in [`brand.toml`](../../brand.toml); see the [manual index](README.md).

Tethered capture today is **studio capture from a watched folder**: the camera maker's own tethering software (or any
tool that saves shots to disk) writes into a folder, and <app> imports each new shot as it lands, applies your
settings and shows it in the loupe.

## Start a session

File → Tethered Capture → Start Tethered Capture… (`tether.start`):

- **Folder:** where the camera software saves shots.
- **Session name** (default "Studio Session"; a new name starts a new session, numbering restarts).
- **Copy** shots into `Originals/<session>/`, renamed with a template using `{session}` and the rename tokens
  (`{seq:4}` …); otherwise they are added in place.
- A **develop preset**, a **metadata preset** and **keywords** applied to every shot.
- The **collection** (album) the shots go into (default: the session name).
- Shots already in the folder are skipped unless you turn that off.

A file is imported once its size has held still between two scans, so a shot still being written waits. The newest
shot becomes the selection. Change settings mid-session with `tether.settings`; File → Tethered Capture → Stop
Tethered Capture ends it (`tether.stop`, `end=true` forgets the session). The session is saved in `tether.json` in
the library, so it resumes after a restart.

```sh
<cli> run --library DIR tether.start folder=/home/me/Capture session="Shoot 42" copy=true keywords="studio, red"
```

## Native camera control

Direct USB control of cameras (PTP: live view, triggering, camera settings) is not available yet. Linux packages will
need a udev rule granting the user access to PTP devices once it lands; see [packaging.md](../packaging.md).
