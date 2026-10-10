# Tethered capture

> Names in angle brackets are the values set in [`brand.toml`](../../brand.toml); see the [manual index](README.md).

File → Tethered Capture offers two ways to shoot straight into the library. The full reference (commands, devices,
what each camera brand supports, platform setup) is [docs/tethering.md](../tethering.md); this page is the quick
start.

- **Studio capture** works with any camera: the maker's own tethering app (or any tool) saves shots into a folder,
  and <app> imports each one as it lands.
- **Native tethering** talks to the camera over PTP: by USB, or over Wi-Fi with PTP/IP (`ptpip:<host>`). Capture,
  download, shutter/aperture/ISO/white balance settings and, where the camera offers it, live view. A simulated
  camera (`sim`, `sim-nikon`) lets you try the workflow without one. Cameras that need a maker's remote-control
  extension for capture (many Canon, Sony and Fujifilm bodies) still download shots taken on the camera; use studio
  capture to trigger them.

## A studio session

File → Tethered Capture → Start Tethered Capture… (`tether.start`) asks for the folder the camera software saves to,
a session name, whether to copy shots into `Originals/<session>/` (renamed with `{session}` and the rename tokens),
and a develop preset, metadata preset, keywords and collection for every shot. A file is imported once its size
holds still between two scans, so a shot still being written waits; the newest shot becomes the selection. The
session is saved in the library (`tether.json`) and resumes after a restart.

```sh
<cli> run --library DIR tether.start folder=/home/me/Capture session="Shoot 42" copy=true keywords="studio, red"
```

## A native session

`tether.cameras` lists cameras, `tether.connect device=usb` (or `ptpip:192.168.1.20`, `sim`) connects,
`tether.capture` fires, `tether.camera.set` changes a setting, `tether.disconnect` ends it.

On Linux the packages install a udev rule so your user can open USB cameras; built from source, install it by hand,
and on macOS quit Image Capture/`ptpcamerad` first. Details: [docs/tethering.md](../tethering.md) → Platform notes.
