# Faces

LightCraft reads the face names other apps (Lightroom, digiKam…) write into your photos, shows them in the loupe
(boxes you can switch off, resize and remove) and groups photos by person in the People view. This page is about the
*models* that find and recognise faces by themselves.

## What ships, what is opt-in

- **Face detection is bundled.** [YuNet](https://github.com/opencv/opencv_zoo/tree/main/models/face_detection_yunet)
  (232 KB, MIT, `assets/models/face_detection_yunet_2023mar.onnx`) is part of LightCraft and runs in LightCraft's own
  code, with no extra runtime. It was trained on the WIDER FACE dataset; an MIT licence on weights does not settle that
  dataset's terms, so this is flagged for the maintainer. It finds faces; it does not identify anyone.
- **Face recognition models are never bundled.** They are large (AuraFace is 261 MB), and their licences and training
  data deserve a decision by the person installing them. Everything works without one: names from XMP, the People
  view, the face boxes.
- **Recognition runs on [tract](https://github.com/sonos/tract)** (Apache-2.0 or MIT), an ONNX runtime written in
  Rust, in the desktop app and the CLI. Its build is not only Rust: it assembles hand-written assembly kernels for
  speed, and on Linux ARM machines (aarch64) it may also compile a few small C ones, skipped when the compiler
  cannot. A build with `--no-default-features` leaves tract, and so recognition, out (the desktop executable is
  about 24 MB smaller on Windows); detection with YuNet and everything else stay.

## Adding a recognition model

1. **Settings ▸ Faces ▸ Add a model file…**, or drop a `.onnx` file on the window.
2. LightCraft looks at the file (it never runs it at this point): it recognises models it knows by their SHA-256, and
   for any other file it reads the input and output shapes and describes what it assumed (112 × 112 aligned faces,
   RGB, `(x − 127.5) / 127.5`, one vector per face: the ArcFace convention that InsightFace models also use).
3. A dialog shows the licence, whether commercial use is allowed, what the model was trained on (or that this is not
   known), and **Install stays disabled until you tick "I have read these terms and accept them for my own use"**.
4. The model is copied into LightCraft's models folder and checked against the original by hash.

LightCraft never downloads a model by itself. **Get…** opens the model's own page in your browser; you download the file
there and add it as above. Non-commercial models (InsightFace, for example) can be added the same way for your own
use; LightCraft never bundles, hosts or links them from a picker, and the dialog says so.

The models folder is `<config>/models` (`%APPDATA%\LightCraft\models` on Windows, `~/Library/Application Support/LightCraft/models`
on macOS, `~/.config/lightcraft/models` on Linux), or `$LIGHTCRAFT_FACE_MODELS`. The desktop app, the CLI and the MCP
server share it.

## What is known about the models LightCraft recognises

| Model | Licence of the weights | Trained on | Notes |
| --- | --- | --- | --- |
| YuNet 2023mar (bundled detector) | MIT | WIDER FACE | 232 KB |
| AuraFace v1 | Apache-2.0 | "a commercial dataset", undisclosed | 261 MB; the best measured on sculpted busts |
| SFace 2021dec | labelled Apache-2.0 | undocumented (the upstream repository mentions CASIA-WebFace, VGGFace2, MS1MV2) | 39 MB; two questions about commercial use are unanswered upstream |

"Commercial use allowed" in the dialog is the weights' licence; it says nothing about the training data.

## Finding faces yourself

**Photo ▸ Detect Faces** (`faces.detect`) runs the bundled detector on the selected photos and adds what it finds as
unnamed face boxes, in one undo step. A new run replaces earlier detections; boxes that came from XMP, or that you drew or
named, are never touched (and a face that already has one is not boxed a second time), and no sidecar is written. It looks at the photo upright and uncropped with default settings,
so your edits and crops do not matter. `apply: false` only reports. It finds faces of about 10 pixels and up in a
640-pixel version of the photo (so very small faces in a large group photo can be missed; looking at tiles is planned).
The detector's output matches OpenCV's own YuNet on a 45-photo public-domain test set (97 of 99 faces found by both,
mean box overlap 0.97), including marble busts and paintings, with no false boxes on the landscape and architecture
photos in the set.

## Commands

All of this is reachable from the control channel, the CLI and MCP: `faces.models.list`, `faces.models.inspect {path}`,
`faces.models.install {path, acknowledged: true}`, `faces.models.remove {id}`, `faces.models.select {id}`,
`faces.enable {enabled?}`, and `faces.detect {ids?, apply?}`. `acknowledged` must be `true`: the caller has shown the user the terms and the user agreed.
