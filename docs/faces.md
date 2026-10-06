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

Adding a model is how you say you want it: once it is installed it is **chosen and recognition is switched on**, with no
further step. The one question is its licence, and that comes first.

1. **Settings ▸ Faces ▸ Download** on SFace or AuraFace (the models LightCraft has a pinned address for), or **Add a
   model file…** (or drop a `.onnx` file on the window) for any other.
2. A dialog shows the licence, whether commercial use is allowed, what the model was trained on (or that this is not
   known) and, for a file LightCraft does not know, what it assumed (112 × 112 aligned faces, RGB, `(x − 127.5) / 127.5`,
   one vector per face: the ArcFace convention that InsightFace models also use). The button (**Accept & Download**, or
   **Install** for a file) stays disabled until you tick "I have read these terms and accept them for my own use".
3. A download runs in the background with a progress bar and a Cancel button. When it has arrived and matched its recorded
   size and SHA-256, the model is installed, tested, chosen and recognition is turned on; the status line says so. A file
   goes through the same steps without the download.

The newest model is the one in use. An earlier one stays installed, **Use** switches back, and each model keeps its own
cache of embeddings (`face-embeddings-<model id>.bin` in the library folder), so going back does not start over. Removing
the model in use hands over to another installed one. Results of the model that was running when you switched are
discarded, never mixed into the new one's index.

LightCraft fetches a model only when you accept a **Download**, and only from the address pinned in its code to a commit of
the model's own repository (github.com for SFace, huggingface.co for AuraFace); nothing about you or your photos is
sent. It has no HTTP stack of its own (the usual TLS crates bring in C or assembly), so the transfer is done by the computer's own `curl` (Windows 10
and later, macOS and most Linux have it), started hidden. A file that does not match its recorded size and SHA-256 is
thrown away; one that does waits in `<models folder>/.downloads` for the moment it takes to install, then is moved into
place. Quitting stops a download and its `curl`. Without `curl`, or offline, the row says so; **Open page** opens the
model's own page in your browser, and you add the file as above. A build without the recognition runtime offers no
download.

### Bring your own models: `catalog.json`

LightCraft's built-in list only holds models whose terms let the project point at them. For anything else (a stronger
recogniser whose weights are for research use, a model you trained, a mirror you trust) put a `catalog.json` in the models
folder. Each entry is a model manifest plus the address to fetch it from, and it then gets the same **Download** button as
a built-in model, with its licence notice shown before anything is fetched. LightCraft ships and links to none of them: what
the file says, and whether you may use the weights, is yours to check (the dialog shows the notice you wrote, in a warning
colour unless you marked the model `"commercial": "yes"`).

```json
{ "models": [ {
    "id": "my-recogniser", "name": "My recogniser (R50)", "version": "1", "role": "embedder",
    "url": "https://example.org/weights/recogniser.onnx",
    "sha256": "(64 lowercase hex digits)", "sizeBytes": 166000000,
    "licence": { "name": "Research use only", "commercial": "no", "notice": "Not for commercial use." },
    "provenance": "Trained on ...",
    "output": { "kind": "embedding", "dim": 512 },
    "thresholds": { "matchCosine": 0.4 },
    "speed": 1.7
} ] }
```

`speed` is optional: how many times faster the model is than a ResNet-100 (1.7 for a ResNet-50, 7 for a MobileFaceNet; see the
next section). Settings ▸ Faces shows it as "1.7× faster than a ResNet-100 model", also before the model is downloaded; leave it
out if you have not measured it and nothing is shown. `input` may be left out (112 × 112 RGB, `(x − 127.5) / 127.5`, the ArcFace convention that InsightFace-style models use); say
otherwise with `"input": {"width": 112, "height": 112, "colour": "bgr", "mean": [0,0,0], "std": [1,1,1]}`. `sha256` and
`sizeBytes` are required (a download is checked against them), the address must be `https`, and the model must give one
vector per face. A bad entry is reported in Settings ▸ Faces and skipped; the rest still load. The same file by hand
(Add a model file…) is recognised by its hash and gets the terms you wrote. Models you cannot or do not want to list can
still be added by file; non-commercial models are never offered by the built-in list.

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

## How fast are the models

Settings ▸ Faces shows these ratios beside each model (before it is downloaded too), not milliseconds. Milliseconds depend
on your computer, so the speed is a ratio to a **ResNet-100 model = 1×**: a bigger number is faster. Measured with
LightCraft's runtime (tract on the CPU, one thread per face) by timing each model's self-test, median of ten runs, on a
32-thread desktop; the ratios should carry over to other machines, the milliseconds will not. A model with no known ratio
shows none.

| Model | Size | Speed (ResNet-100 = 1×) | Here |
| --- | --- | --- | --- |
| MobileFaceNet (InsightFace buffalo_sc) | 14 MB | **7× faster** | 31 ms per face |
| SFace (OpenCV Zoo) | 39 MB | **4.6× faster** | 47 ms |
| ResNet-50 class (InsightFace buffalo_l `w600k_r50`) | 174 MB | **1.7× faster** | 129 ms |
| ResNet-100 class (AuraFace) | 261 MB | **1×** (the reference) | 218 ms |

Models of the same architecture run at the same speed whoever trained them, so the ResNet-50 row stands for any R50
recogniser (TopoFR R50, AdaFace R50…) and the ResNet-100 row for any R100. A ResNet-200 would be about **half as fast** as the
R100 (0.5×, estimated from the compute, not measured). Only the recognition step is timed here: scanning a photo also decodes
it and finds its faces (about as much work as one SFace face), so a whole-photo scan with an R100 takes roughly 2.5 to 3
times as long as with SFace, not 4.6 times.

### And how well do they recognise?

Published numbers only, each copied from the model's own page (we have not re-run them). Higher is better. They are
standard face-verification benchmarks: **IJB-C** (hard, mixed-quality photos; true-accept rate at one false accept in
10,000), **CFP-FP** (frontal against profile) and **AgeDB-30** (years apart), both in percent.

| Model | Speed (R100 = 1×) | IJB-C | CFP-FP | AgeDB-30 | Source |
| --- | --- | --- | --- | --- | --- |
| MobileFaceNet (InsightFace buffalo_s / buffalo_sc) | 7× faster | 95.02 | 98.00 | 96.58 | InsightFace model zoo |
| SFace (OpenCV Zoo) | 4.6× faster | not given | not given | not given | OpenCV Zoo says 0.9940 on a set it does not name |
| ResNet-50 (InsightFace buffalo_l) | 1.7× faster | 97.25 | 99.33 | 98.23 | InsightFace model zoo |
| ResNet-100 (AuraFace v1) | 1× | not given | 95.19 | 96.10 | its Hugging Face page |
| TopoFR R50 / R100 / R200 (Glint360K) | 1.7× / 1× / 0.5× | 97.27 / 97.60 / 97.84 | not given | not given | TopoFR repository |

Read with care: the sources report different sets, so the blanks are not zeros and the rows are not all on the same
yardstick. What the numbers do say: a ResNet-50 recogniser is about two points above a MobileFaceNet on IJB-C, a ResNet-100
or 200 adds only a few tenths more, and AuraFace's published CFP-FP and AgeDB-30 are below buffalo_l's ResNet-50 despite being
a ResNet-100 (so it is not the accuracy choice its size suggests). InsightFace's pretrained weights are, in its own words,
"available for non-commercial research purposes only"; TopoFR's page states no licence for its weights.

## Finding faces yourself

**Photo ▸ Detect Faces** (`faces.detect`) runs the bundled detector on the selected photos and adds what it finds as
unnamed face boxes, in one undo step. A new run replaces earlier detections; boxes that came from XMP, or that you drew or
named, are never touched (and a face that already has one is not boxed a second time), and no sidecar is written. It looks at the photo upright and uncropped with default settings,
so your edits and crops do not matter. `apply: false` only reports. It finds faces of about 10 pixels and up in a
640-pixel version of the photo (so very small faces in a large group photo can be missed; looking at tiles is planned).
The detector's output matches OpenCV's own YuNet on a 45-photo public-domain test set (97 of 99 faces found by both,
mean box overlap 0.97), including marble busts and paintings, with no false boxes on the landscape and architecture
photos in the set.

## Suggesting who is in a photo

With **Recognise faces** switched on in Settings ▸ Faces and a recognition model chosen (**Use**), LightCraft works out, in
the background, what each face in your library looks like to the model, and uses the faces you have already named to
suggest names for the ones you have not:

- An unnamed face with a good match gets a dim label such as **Jane Doe?** in the loupe. Click it and the name box opens
  with the guess filled in; Enter confirms. Any other unnamed face shows **Add name** when you point at it. Clicking a name
  lets you change it, and clearing the box removes it. Typing completes from the people you have already named.
- **Nothing is ever named for you.** A suggestion is only a label until you confirm it, and a suggestion appears only when
  the match is strong *and* clearly ahead of the next person: it is better to leave a face unnamed than to name it wrongly.
  Naming a face also makes it one of the faces the others are compared with, so the suggestions improve as you go.
- Faces come from the names other apps wrote into your photos (read from XMP), from Photo ▸ Detect Faces, or both. A face is
  aligned using the detector's five landmarks (eyes, nose, mouth corners) when it finds the same face, and cut out by its box
  otherwise.
- Everything stays on your computer. The embeddings (one short list of numbers per face) are cached in the library folder in
  `face-embeddings-<model id>.bin`, one file per model, so switching models back and forth does not start over; they are
  not part of the catalog and not written to XMP.

### The background scan

Turning recognition on starts a scan of the whole library, once, in the background:

- A photo that already has face boxes (from XMP, or drawn, or named) has its faces embedded.
- A photo with **no** face boxes at all is searched with the detector, and what it is sure of becomes that photo's unnamed
  face boxes (marked "Detected by YuNet", so a manual Detect Faces replaces them), embedded in the same pass. Photos are
  searched once: `face-scanned.bin` in the library folder remembers which, so a photo whose boxes you remove is not boxed
  again. These boxes are LightCraft's own bookkeeping: not an undo step, never written to a sidecar.
- Photos you have named faces in go first, then the rest. Raw files are read through the camera's embedded preview (much
  faster than decoding the raw); everything else is rendered at 2048 pixels.
- **How hard it works follows what you are doing.** The app tells the engine on every call to `faces.pump`, which is made
  about 20 times a second while there is work and a few times a minute otherwise (the window is not redrawn for it at any
  other time). Dragging, typing or scrolling: nothing new is started. The pointer moving, or the window minimized:
  **light**, one photo at a time on two threads. Idle for three seconds, or the window still on screen but with another
  app in front (you are working elsewhere): **normal**, half of the processor's threads. Idle and looking at the progress
  (Settings ▸ Faces, or the People view): **full**, four fifths. Photos already running are never interrupted, and a
  change of pace takes effect at once.
- **Threads and photos.** Most of a photo's cost is its parallel work (decoding, developing the picture), so the pace sets the
  size of a pool of threads for that work: two, half the machine, four fifths. The scan has pools of its own, not the one the
  loupe and exports use (which has no priorities), so a slider drag never queues behind a scan. Measured on a 32-thread
  desktop with 4 photos at once: about 6.4 photos a second at full pace (a pool of 25 threads), 6.0 at normal (16) and 1.8
  when light (2 threads).
- **Memory** limits the number of photos at once as much as the processor does: a photo in progress holds about 180 MB
  (peak memory grew by that much per extra worker), so at most half of the memory budget (a quarter of the RAM, at most
  1.5 GiB, unless `LIGHTCRAFT_MEMORY_MB` says otherwise) is given to them: about four photos at once on a default setup,
  whatever the core count. Eight at once was not much faster and needed about twice the memory (a 2.0 GB peak against
  1.1 GB). `LIGHTCRAFT_FACE_THREADS` replaces these limits with a number of your own. A couple more photos wait behind
  the running ones, so a worker that finishes has its next photo at once.
- **Speed.** A mixed raw and JPEG library of 184 photos (the raws read through their embedded previews, 73 photos searched
  for faces) took about 29 seconds on that machine at full pace: roughly 6 photos a second, so the first scan of 10,000 photos
  is a matter of half an hour; after that only new photos are looked at. Settings ▸ Faces shows how many are left, with a progress bar. After you accept a model's terms
  you are returned to that tab, so the download and then the scan can be watched there; the main window has no status bar.
- Opening another library starts a fresh scan state; nothing learned about one library is used in another.

### The People view

**People** shows a card for each named person (their face, with the number of photos they are in on the picture) and, below
a line, the **Unnamed faces**: every face nobody has named, as cropped pictures. With recognition running, faces that look
alike are next to each other (put in order by looking at every pair, for up to the first 1,500 embedded faces), and a face
the named ones recognise carries the name they suggest along its bottom edge; click that name to accept it for that face.
To name a group: click faces to select them (Shift-click selects a range, **Select all** takes every one listed), type a
name in the bar that appears (people already named complete as you type), press Enter: all of them are named at once, as
one undo step. Selecting names nothing.

The thumbnail-size slider in the bottom bar sizes the faces here too, with limits of their own. Every face is shown by the
same kind of box: the detector's own box once the scan has looked at it (kept beside its embedding), so a loosely drawn box
from another tool does not make one face look farther away than the next; before the scan has looked at a face it is shown
by its own box.

### A person's page

In **People**, a click on a person opens their page: **only cropped faces**, never whole photos. First the faces named
with their name (a click opens that photo), then **More**: the unnamed faces that look like them, most alike first. Click one
to confirm it (that names the face, as one undo step, and it moves up into their faces), or × to hide it for this session.
**Show photos** puts their photos in the grid; **‹ People** or Escape goes back. "More" only holds faces the scan has
already embedded, so it fills in as the scan goes; it shows faces scoring above four fifths of the model's suggestion bar,
since you look at each one before anything is named.

How well it works: on 755 named faces of marble busts from one museum folder (each face hidden in turn and matched against
shots taken more than five seconds apart), SFace named the right person first 95.5% of the time and AuraFace 94.7%; at
the starting thresholds LightCraft suggests (SFace 0.55, AuraFace 0.40) about 97% of the suggestions were right. Busts are
a hard case in some ways (no skin or hair to go by) and an easy one in others (the same sculpture looks the same in every
shot), so check a model on your own photos before trusting it:

`faces.evaluate` tests a model on *your* photos: it hides each named face in turn, asks who it looks like from the others
(ignoring shots taken within a few seconds of it, which would make it too easy) and reports, for each threshold, how many
suggestions it would make and how many were right.

## Commands

All of this is reachable from the control channel, the CLI and MCP: `faces.models.list`, `faces.models.inspect {path}`,
`faces.models.install {path, acknowledged: true, activate?}`, `faces.models.download {id, acknowledged: true}` (then `faces.models.downloads`, which also installs what has arrived, and `faces.models.downloadCancel {id}`), `faces.models.remove {id}`, `faces.models.select {id}`,
`faces.enable {enabled?}`, `faces.detect {ids?, apply?}`, `faces.index {budgetMs?, ids?}`, `faces.pump` (what the app calls every frame), `faces.suggest {ids?, threshold?, margin?}`, `faces.person {name, more?}` (a person's faces and the unnamed faces that look like them), `faces.unnamed {limit?}` (every unnamed face, look-alikes together, with suggested names), `faces.setName {id?, index, name}`, `faces.nameFaces {faces: [{photo, index}], name}` (name many at once, one undo step) and `faces.evaluate`. `acknowledged` must be `true`: the caller has shown the user the terms and the user agreed. Installing makes the model the one in use and switches recognition on unless `activate` is `false`.
