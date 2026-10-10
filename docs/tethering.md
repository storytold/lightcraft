# Tethered capture

The app tethers in two ways (File ▸ Tethered Capture):

- **Studio capture** works with any camera: the camera's own app (EOS Utility, Nikon Webcam/NX Tether, Imaging Edge
  Remote, Fujifilm X Acquire, …) or a card reader writes shots into a folder; the app watches it and imports each new
  shot with the session's develop preset, metadata preset, keywords, naming and collection. The newest shot shows in
  the loupe.
- **Native tethering** talks to the camera directly over PTP (Picture Transfer Protocol): over USB, or over Wi-Fi
  with PTP/IP. Shots download into the session folder and are imported the same way.

Commands (also over the control channel and MCP): `tether.cameras`, `tether.connect {device}`, `tether.capture`,
`tether.camera`, `tether.camera.set {setting, value}`, `tether.liveView`, `tether.disconnect`. Devices are `usb` (the
first PTP camera), `usb:<bus>:<address>`, `ptpip:<host>[:port]` (port 15740 by default), and `sim` / `sim-nikon` (the
built-in simulated camera, for trying the workflow and for tests).

## What native tethering does

- Opens a PTP session and reads the camera's DeviceInfo (name, supported operations and properties).
- **Capture** (InitiateCapture) where the camera offers remote capture; shots taken with the camera's own shutter
  button download too (ObjectAdded events).
- **Download on capture** into the session folder; optional **delete from card** after download.
- **Tether bar readouts and settings**: shutter speed, aperture, ISO, white balance, exposure compensation and battery,
  as menus of the values the camera allows.
- **Develop settings "same as previous"**: each new shot gets the previous shot's develop settings.
- **Live view** where it is known (Nikon's live view operations, used only when the camera lists them).

Maker extensions are built from public PTP material only (the ISO 15740 vendor registry and the makers' published
operation codes). Canon EOS, Sony and Fujifilm bodies mostly need their maker's remote-control extensions for capture;
until those are verified on bodies, those cameras still download shots taken on the camera, and the bar says to use
studio capture when Capture is unavailable. Native capture has been verified only against the simulated camera so far.

## When a camera can't be tethered

Any connection failure (no camera found, access denied, the camera refuses the session) is reported with the reason
and the advice to use studio capture with the camera's own app. Studio capture never needs drivers or permissions.

## Platform notes

### Linux

USB device nodes belong to root. The packages install a udev rule
(`/usr/lib/udev/rules.d/70-<app_id>-ptp.rules`) that gives the logged-in user access to every PTP camera:

```
SUBSYSTEM=="usb", ENV{DEVTYPE}=="usb_device", ENV{ID_USB_INTERFACES}=="*:060101:*", TAG+="uaccess"
```

Built from source, copy `packaging/linux/70-{app_id}-ptp.rules.in` there (no substitutions are needed) and run
`sudo udevadm control --reload && sudo udevadm trigger`, then reconnect the camera. Desktop environments may
auto-mount the camera through gvfs (`gvfs-gphoto2-volume-monitor`); unmount it in the file manager if connecting
says the device is busy.

### macOS

macOS's `ptpcamerad` (Image Capture) claims cameras as they connect, so the app can't open them. Quit Photos, Image
Capture and similar apps, then release the camera, for example with `killall ptpcamerad` right before connecting
(it restarts on demand). In Image Capture, set "Connecting this camera opens" to "No application".

### Windows

Windows binds cameras to its WPD/MTP driver, which other programs can't use directly. For native tethering, bind the
camera's still-image interface to the WinUSB driver (for example with Zadig, choosing "WinUSB" for the camera), and
switch it back (Device Manager ▸ Update driver ▸ the original MTP/WPD driver) to use Explorer or the maker's tools
again. Wi-Fi (PTP/IP) needs no driver change. When in doubt, use studio capture.

### Wi-Fi (PTP/IP)

Put the camera in its "connect to computer" / PC remote Wi-Fi mode on the same network and connect to
`ptpip:<camera-ip>`. Many cameras ask to confirm the computer on their screen the first time.

## Sources

ISO 15740 / PIMA 15740:2000 (operation, response, event, property and data type codes; datasets), USB Still Image
Capture Device Definition 1.0 (containers), CIPA DC-005 (PTP/IP). No GPL code (libgphoto2 and others) was read.
