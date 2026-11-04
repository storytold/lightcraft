# Slideshow

LightCraft's slideshow follows Lightroom Classic's Slideshow module (no Adobe assets, templates or designs): the photos
in view played full screen with a layout, overlays, intro and ending screens and fades, and exported as PDF, JPEG or
video. There is no Slideshow module view yet: the settings live in Settings ▸ Slideshow (View ▸ Slideshow Options…)
next to a live preview of the selected photo's slide, and View ▸ Slideshow (⌥⌘↩) plays.

| Part | Commands | Code |
|---|---|---|
| Playback: slide and fade durations, fade colour, random order, repeat, preparing slides ahead, manual slideshow | `slideshow.playback`, `view.slideshow` | `crates/engine/src/slideshow.rs` (the timeline), `crates/ui-egui/src/panels/slideshow.rs` |
| Layout, options, backdrop: margins, aspect preview, zoom to fill, stroke, shadow, colour, colour wash, background photo | `slideshow.layout` | `crates/slideshow` (the composer, no UI), `crates/engine/src/slide.rs` |
| Overlays: identity plate, rating stars, text with tokens, watermark | `slideshow.overlays` | `crates/engine/src/slide_overlay.rs` |
| Intro and ending screens | `slideshow.titles` | `crates/engine/src/slide_title.rs` |
| Templates: built-ins of our own and the user's (`<settings>/slideshow-templates/*.json`) | `slideshow.templates.*` | `crates/engine/src/slide_template.rs` |
| One slide or screen as a PNG | `slideshow.preview` | `crates/engine/src/cmd/slideshow.rs` |
| Export: PDF, JPEG sequence, AV1 video | `slideshow.export`, `slideshow.exportStatus` | `crates/engine/src/slideshow_export.rs`, `crates/codecs/src/video.rs` |

All settings are library preferences (`prefs.json`); sizes and offsets are fractions of the slide's shorter side, so a
slide looks the same on any screen and at any export size. Playback, the preview, `slideshow.preview` and export draw
the same pixels through the same composer.

## Export

View ▸ Export Slideshow… (or Export PDF / JPEG / Video at the end of Settings ▸ Slideshow) chooses a format and size,
then asks where to write. The slides are the photos in view, in view order, between the intro and ending screens that
are on; random order, repeat and manual slideshow are playback choices and don't apply. The slide aspect is the Layout's aspect preview
(Screen exports 16:9).

- **PDF** — one page per slide, its picture a JPEG at the chosen quality, the page as many points as the slide has
  pixels (1024 / 1920 / 3840 on the long side; 64–8192 by command). Each page has the slide duration (`/Dur`) and,
  after the first, a fade transition (`/Trans`), which PDF viewers use in presentation mode; "Automatically show full
  screen" sets `/PageMode /FullScreen`. Written by `crates/engine/src/pdf.rs` (shared with the contact sheet), streamed
  to disk.
- **JPEG** — `<name>-001.jpg`, `-002.jpg`… in the chosen folder (created; `name` defaults to the folder's name).
- **Video** — AV1 in MP4 (default) or WebM, 720p or 1080p (64–2160 by command), 24 or 30 fps (25 / 60 by command), no
  audio (music is a later item, LRC-SS-MUSIC). The timeline is sampled at the frame rate: a slide's fade-in is rendered
  frame by frame (a crossfade, or out to the fade colour and in from it), then the slide is held for the rest of its
  duration as **one** sample lasting that long (variable frame rate), so a still slide costs one encoded frame. That
  frame is a key frame: seeking lands on slides. Encoded by `rav1e` (pure Rust, BSD-2-Clause, already used for AVIF) at
  speed 10, 8-bit 4:2:0 BT.709 limited range, in order (low latency, no frame reordering); at most 2 hours.

  The muxers are our own (`crates/codecs/src/video.rs`, from the public ISO/IEC 14496-12 + AV1-ISOBMFF and
  Matroska/WebM specifications): MP4 is `ftyp`, one `mdat` (64-bit size patched at the end) and `moov` with an `av01`
  sample entry (`av1C` holding the sequence header OBU, `colr nclx`, `pasp`), `stts` durations, `stss` key frames and a
  single chunk; WebM is the EBML header and a Segment with SeekHead, Info (duration), Tracks (`V_AV1`, `CodecPrivate` =
  `av1C`), Clusters of SimpleBlocks (the last frame a BlockGroup with its duration) and Cues. Samples are temporal units
  without temporal delimiters. No AV1 decoder is in the product, so the tests check the container structure, sample
  counts and durations; ffmpeg / browsers play the files (checked by hand when the muxers change).

The export runs on a worker thread with a row in the activity stack ("Exporting slideshow": slides done, or frames for
video, and the slide being rendered; ✕ or `activity.cancel {id: taskId}` stops it). Every output is written as
`<file>.part` and renamed when complete, so a cancelled, failed or interrupted export leaves nothing behind (for a JPEG
sequence, all files are renamed at the end). A photo that can't be rendered (its file gone or unreadable) is skipped
and named in the result's `warnings`; a full disk or an unwritable folder is an error. An original in the library is
never overwritten (`check_write_target` before and after).

Encoding speed (rav1e speed 10, measured 2026-10-11 on a 32-thread Linux machine shared with other work): one minute of
slideshow (15 slides of 4 s with 1 s fades, 30 fps: 435 encoded frames) took 35 s at 720p and 74 s at 1080p, slide
rendering included. The cost is in the fades (one encoded frame per frame of fade); held slides cost one frame each.

## Not yet

The Slideshow module view (left-panel template browser, slide editor with draggable guides and overlays), music and
sync to music, pan & zoom, H.264 video, saving a slideshow with an album, export presets.
