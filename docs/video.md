# Video

> **Last updated:** 2026-10-13 · **Change:** minor (clip sound on Linux/BSD through PulseAudio / PipeWire in every build)

> **Last updated:** 2026-10-12 · **Change:** minor (timelapse video from photos, upstream #497)
> **Last updated:** 2026-10-12 · **Change:** minor (GPU playback LUT: frames graded on the window GPU)
> **Last updated:** 2026-10-12 · **Change:** minor (clip audio: PCM in MP4/MOV/MKV/TS/MXF, MP3; FLAC/ALAC skipped)
> **Last updated:** 2026-10-12 · **Change:** minor (faster clip export: pipelined decode/grade/encode, GPU grade for H.264)
> **Last updated:** 2026-10-12 · **Change:** minor (export audio from any decodable codec; PCM in ProRes MOV)
> **Last updated:** 2026-10-12 · **Change:** minor (CLI `--trim`, end-to-end CLI/MCP tests, per-`ids` gating)
> **Last updated:** 2026-10-12 · **Change:** minor (sequential `next_frame` / `seek`, pixel aspect, MPEG-2 deinterlace, decode cost)
> **Last updated:** 2026-10-12 · **Change:** minor (M14.1: Matroska/WebM, MPEG-TS, MXF; VP9, AV1, ProRes, DNxHR, APV, MPEG-2)
> **Last updated:** 2026-10-12 · **Change:** M14.8: video export wired through `lcc_engine::export`
> **Last updated:** 2026-10-12 · **Change:** minor (M14.2 import side, M14.3 posters)

Video support is milestone M14 (`plan/video-filmcraft.md`, gaps.md row 18, parity rows LR-VID-*). Containers and
codecs come from [FilmCraft](https://github.com/storytold/filmcraft) (pure Rust, clean-room, `MIT OR Apache-2.0`),
used as git dependencies pinned to one full revision. The app writes no codecs or containers of its own.

## Using video

- **Import** clips like photos (MP4, MOV, MKV, WebM, MPEG-TS, MXF). The grid shows a poster frame; the Info panel
  shows duration, frame rate, codec, resolution, audio and trim. `video:yes` in the search bar finds clips.
- **Play** in the loupe: <kbd>Space</kbd> plays and pauses, <kbd>←</kbd> / <kbd>→</kbd> step one frame, drag the
  playhead to scrub. Clip sound plays on Linux and the BSDs (PulseAudio / PipeWire) in every build, on macOS/Windows only with `audio-out` (see [Audio](#audio-decode-always-sound-on-linuxbsd-elsewhere-with-audio-out)).
- **Trim**: drag the start and end handles on the bar; the drag is one undo step. Photo ▸ Video ▸ Reset Trim plays
  the whole clip again.
- **Poster frame**: Set Poster Frame (bar button or Photo ▸ Video) uses the current frame for the grid.
- **Capture Frame** (Photo ▸ Video) saves the current frame, graded, as a JPEG next to the clip and stacks it with it.
- **Edit** with the Light and Color controls, tone curve, Color Mixer and Color Grading; presets apply only those.
  Masks, spot removal, red eye, lens corrections, denoise and crop are greyed out ("Not available for video").
- **Export**: put clips in an export batch; the dialog's Video section picks H.264 (MP4) or ProRes (MOV), quality and
  long edge. The movie keeps the trim, the grade and the sound.
- **CLI**: `<cli> render clip.mov -o out.mp4 --trim 2,8` (seconds; `--trim 2,` = to the end). MCP agents use the
  `video.*` commands and `export`.

## `crates/video` (`lcc-video`, L3, native only)

| API | What it does |
|---|---|
| `probe(path) -> Result<VideoInfo, VideoError>` | Container and stream headers, no decoding: `duration` (s), `fps_num`/`fps_den`, display `width`/`height` (after rotation), `codec` (`h264`, `hevc`, `vp9`, `av1`, `prores`, `dnxhd`, `apv`, `mpeg2`, or the container's name for codecs we don't decode), `has_audio`, `created` (QuickTime `creationdate`/`©day` as written; Matroska `DateUTC` as RFC 3339), `gps` (ISO 6709), `rotation`, `hdr`, `decodable`, `coded_width`/`coded_height`, `par_num`/`par_den` (pixel aspect), `interlaced` (MPEG-2). `width`/`height` are the display size after the pixel aspect. |
| `FrameReader::open(path)` | Opens a clip with a decodable video track ([supported formats](#supported-formats)); a recognised but undecodable stream is `Unsupported` with the reason. |
| `FrameReader::frame_at(t, max_side) -> Result<Frame, VideoError>` | The frame displayed at `t` seconds (clamped to the clip; NaN = 0), box-decimated so the longer side is at most `max_side`. `Frame { image: lcc_raster::Rgb32f, time }`: linear Rec.709 RGB, the pipeline's JPEG/TIFF input type; `time` is the frame's presentation time. |
| `FrameReader::seek(t)` / `next_frame(max_side) -> Result<Option<Frame>>` / `frames_from(t, max_side)` | Sequential playback for a decode-ahead worker: frames in presentation order from the cursor (`seek`, or just after the last `frame_at`/`next_frame`), decoding forward across GOPs without re-seeking; `None` at the end. `frames_from` is `seek` + an iterator that stops after the first error. |
| `FrameReader::frame_times()` | Every frame's presentation time, sorted (frame stepping). |
| `poster(path, t, max_side)` | One frame for thumbnails; `t = None` means `default_poster_time(duration)` = 10 % in. |

**Seeking.** Each container is reduced to one sample table (decode order, presentation times, sync flags), so seeking
is container-independent: `frame_at` finds the sample presented at `t` (MP4 edit lists applied; frame times count
from the first shown frame), decodes from the preceding sync sample (one GOP further back for an open GOP's leading
pictures), and keeps the decoder while later times stay in the same GOP, so playback and frame stepping don't
re-decode from the keyframe. B-frame reordering is handled by matching the decoder's output pts. MPEG-TS/PS and MXF
times are display positions at the stream's frame rate (TS: the coded rate unless the PTS say otherwise).

**Colour.** The stream's colour comes from size-based defaults (BT.601 matrix up to 1024×576, else BT.709, limited
range), overridden by the bitstream (H.264/HEVC VUI, AV1 sequence header, VP9 colour space, APV/ProRes/MPEG-2 headers),
overridden by the container (`colr` box, `vpcC`, Matroska `Colour`, MXF full-range coding). FilmCraft's converter turns Y'CbCr
into linear light; the crate then converts BT.2020/P3 primaries to Rec.709 and, for HLG (BT.2100 OOTF, 1000 cd/m²) and
PQ, tone-maps to SDR with the BT.2390 EETF, reference white (203 cd/m²) at 1.0. True HDR output waits for gap Q.

**Errors, never panics.** `VideoError::{Io, Invalid, Unsupported, Decode, Internal}`. Every call into FilmCraft runs
under `catch_unwind` (the safety net the never-crash standard allows), and a failed decoder is discarded. Tests sweep
truncations, random byte corruption and garbage files.

**Web build.** On wasm32 the API exists and returns `Unsupported`; the FilmCraft crates aren't linked.

## `crates/video-export` (`lcc-video-export`, L3, native only)

`export_clip(src, (trim_in, trim_out), grade, &ClipExportOptions { format, quality, max_side, out }, cancel, progress)
-> Result<ClipReport, ExportClipError>`: decodes the trimmed range with `lcc_video::FrameReader` (first frame = the one
displayed at the in point, decoded from the previous sync sample), runs `grade(&mut lcc_raster::Rgb32f)` on every
frame (linear Rec.709, display-referred), converts to BT.709 limited-range Y'CbCr and encodes H.264 High 4:2:0
(`filmcraft-h264enc`, CRF from quality) into MP4 or ProRes 422 10-bit (`filmcraft-prores`, HQ/Standard/LT) into MOV.
Audio (`encode_audio_range(src, start, end, format, cancel) -> Result<Option<EncodedAudioTrack>>`, called once per
clip): the first track `AudioReader` decodes (AAC, Opus, AC-3 / E-AC-3 from MP4/MOV, Matroska, TS) for the trimmed
range, placed by block time relative to the first exported frame (gaps → silence), folded to mono/stereo (−3 dB
centre/surround), then AAC-LC for MP4 (encoder delay hidden by an edit list; resampled to 48 kHz if the rate is not an
AAC rate) or 16-bit little-endian PCM (`sowt`) for ProRes MOV. Clips with no decodable audio export silent. Output
goes to `<out>.partial` and is renamed on success; errors, panics and cancel remove it.

**Spike S3 (export option A vs B):** option A. `filmcraft-export`'s `FrameRenderer` hook is a global registry of
plain `fn()` factories rendering a `filmcraft-project` sequence, so a per-export grade closure cannot be passed
without global state, and the crate pulls `project`/`render`/`text`/`audio-dsp` plus `filmcraft-media`/`-codecs`
(symphonia, MPL-2.0: rejected by `cargo deny`). Option A is ~600 lines over isobmff, h264enc, prores and aac.

## Engine: playback, trim, poster, Capture Frame (M14.4 / M14.6)

`crates/engine/src/video/` holds the state, `crates/engine/src/cmd/video.rs` the commands. The engine decodes through
a `VideoBackend` (`probe`, `frame_at`) in `session.video.backend`: native builds install `video::native::LccVideo`
(`lcc-video`); the web build has none, and every `video.*` command is then disabled with "video playback isn't
available in this build". Commands act on the active photo and are enabled only when it is a video; a clip without
`Photo::video` is probed on first use (`Op::SetVideoMeta`, not an undo step).

| Command | Params | Result |
|---|---|---|
| `video.get` | — | `{id, duration, fps, frameRate, width, height, codec, hasAudio, canPlayAudio, current, frame, playing, looping, trimIn, trimOut, trimmed, poster, posterSet}` |
| `video.play` | `{loop?}` | plays from the current position (from the trim start when at the end) |
| `video.pause`, `video.togglePlay` | — | |
| `video.seek` | `{time}` | snaps to the frame start, clamped into the trim |
| `video.stepFrame` | `{delta?}` (whole frames, default 1, at most ±1 000 000) | pauses, moves by frames inside the trim |
| `video.setTrim` | `{in, out?}` | `0 ≤ in < out ≤ duration`, otherwise an error; one undo step (`Op::SetVideoTrim`) |
| `video.resetTrim` | — | undoable; no step when the clip isn't trimmed |
| `video.setPoster` | `{time}` or `{time: null}` | snapped to a frame; undoable (`Op::SetVideoPoster`); `null` = default (10 % in) |
| `video.captureFrame` | `{time?}` (default: current position) | JPEG next to the clip (`<name>-<m>m<ss>s<mmm>.jpg`, never overwriting), imported and stacked under the clip, one undo step, an activity row while it runs → `{id, path, time, clip}` |

The position follows `session.video.clock` (monotonic seconds, injectable in tests): it advances while playing, stops
at the trim end, or wraps to the trim start with `loop`. `session.video.follow_audio_clock(AudioClock)` makes the
position follow an audio device instead (no jump), `follow_system_clock()` goes back. Playback has sound on Linux
and the BSDs in every build (pure-Rust PulseAudio client) and elsewhere only with `audio-out` (`canPlayAudio`: true
then, and only for clips with decodable sound; see Audio below). Every command
answers the `video.get` state, so UI, CLI and MCP see the same thing. Captured frames are not graded yet (M14.7).
## Loupe playback (M14.4)

`lcc_engine::video::playback`: `Playback::start(backend, path, t, max_side, &develop)` runs a worker that decodes
sequentially (`VideoBackend::frames_from`, native: `FrameReader::seek` + `next_frame`) at ≤ 960 px, grades each frame
with a `GradeLut` baked from `video_settings(develop)` (rebaked only when that changes: `set_settings`), and sends
it over an 8-frame bounded queue; frames more than a frame behind `set_target(t)` are skipped before grading.
`poll(now, slack)` returns the newest due frame (earlier ones are dropped as late). `spawn_still` renders one
full-quality frame (`grade_frame`) on its own thread. The UI (`crates/ui-egui/src/video_view.rs`) polls once per frame:
playing, it shows the worker's frames by `video.get.current` (restarting the worker after a seek or loop), requests
repaints, and on a decode error pauses (`video.pause`) with one toast (and a log line); paused, it shows the still for
(time, look, size), so develop edits re-render it. Trim handles drag a preview and send one `video.setTrim` on release.
`ui.inspect` → `loupe.source` reads `video` / `video-still`. With `canPlayAudio` (Linux/BSD always, elsewhere with `audio-out`), playing also starts `playback::AudioPlayback` (an `AudioReader` feeder thread, resampled to `default_rate()`, stereo, ~1 s ahead) and the player follows its `AudioClock` (`follow_audio_clock`); pausing, seeking or leaving the clip drops it and returns to the system clock. Builds without a sound path (default macOS/Windows) stay on the system clock, muted; so does playback when no sound server is running. Neither is an activity row (interactive, long-lived).

**GPU playback grade.** When the window renders with wgpu (`Services::video_gpu`, desktop
`apps/app/src/video_gpu.rs`) and Settings ▸ GPU is on, the loupe starts the worker with `Playback::start_linear`: it
decodes and bakes the LUT as before but skips the CPU grade, handing `FramePixels::Linear { image, lut, key }`. The
UI thread uploads the linear `f32` frame as-is and `lcc_gpu::video_lut::VideoLut::grade` runs one compute shader
(`wgsl/video_lut.wgsl`, the tetrahedral twin of `GradeLut::lookup`, LUT re-uploaded only when its key changes) into
an `Rgba8Unorm` texture registered with egui-wgpu through an `Rgba8UnormSrgb` view, so there is no CPU grade,
RGBA conversion or egui texture copy. Any GPU error falls back to the CPU LUT for the rest of the session (logged
once). Paused stills keep the full CPU pipeline (`spawn_still`). Equivalence: GPU vs CPU LUT ≤ 2 codes
(`lcc-gpu` `video_lut::tests::gpu_lut_matches_cpu_lut`, includes NaN / out-of-range / lattice edges).

Per playing frame (`cargo test -p lcc-engine --release --lib video::bench_playback -- --ignored --nocapture`,
2026-10-12, Radeon / Vulkan, **load average ≈ 150–170 from other jobs**, so upper bounds; 47 frames each):

| Clip (decoded at ≤ 960 px) | decode | CPU LUT | RGBA conv. | GPU LUT (upload + shader, waited) |
|---|---|---|---|---|
| H.264 1080p BBB → 960×540 | 41.6 ms | 24.9 ms | 0.19 ms | 3.5 ms |
| HEVC 1080p → 960×540 | 47.2 ms | 29.9 ms | 0.26 ms | 3.9 ms |
| VP9 1080p → 960×540 | 79.2 ms | 23.9 ms | 0.26 ms | 4.1 ms |
| AV1 1080p → 960×540 | 130.9 ms | 24.0 ms | 0.21 ms | 3.6 ms |
| H.264 720p NPS → 640×360 | 25.4 ms | 20.6 ms | 0.28 ms | 2.8 ms |
| synthetic H.264 2160p → 960×540 | 82.0 ms | 23.1 ms | 0.22 ms | 3.2 ms |

The GPU path removes ~85 % of the per-frame grade cost from the CPU (and the shader time itself is a fraction of the
3–4 ms, which is mostly the 6 MB upload and the wait the benchmark adds). Decode now dominates: under this load no
codec reaches 30 fps; on an idle machine sequential decode is 5.7–25 ms (see Cost). 4K and high-frame-rate clips
are bounded by the decoder, not the grade; the CPU LUT has a fixed ~20 ms per frame even at 640×360, worth a look.

## Supported formats
Probe works for every container below; frames decode when the codec cell says so. "Unsupported" streams probe with
`decodable: false`, and `FrameReader::open` returns `VideoError::Unsupported(reason)`.
| Container | Extensions | Decoded video |
| MP4 / QuickTime | `.mp4` `.m4v` `.mov` | H.264, HEVC, VP9, AV1, ProRes, DNxHD/DNxHR, APV, MPEG-1/2 (XDCAM, IMX, HDV fourccs) |
| Matroska / WebM | `.mkv` `.webm` | H.264, HEVC, VP9, AV1, ProRes, APV, MPEG-1/2 |
| MPEG-TS / M2TS | `.ts` `.m2ts` `.mts` | H.264, HEVC, MPEG-1/2 (Annex B, in-band parameter sets) |
| MPEG program / elementary stream | `.mpg` `.vob` `.m2v` | MPEG-1/2 |
| MXF (OP1a, OP-Atom) | `.mxf` | H.264, DNxHD/DNxHR (VC-3), ProRes, MPEG-2 (D-10/IMX, XDCAM) |
| Codec | FilmCraft crate | Notes |
| H.264 | `filmcraft-h264` | progressive 8-bit 4:2:0 only. **Interlaced** (field / MBAFF: AVCHD 1080i, broadcast) is detected from the SPS and reported as `Unsupported("interlaced H.264 …")`; High 4:2:2 / 10-bit likewise unsupported |
| HEVC | `filmcraft-hevc` | Main, Main 10 |
| VP9 | `filmcraft-vp9` | profiles 0–3, 8/10/12-bit; transfer/primaries from `vpcC` / Matroska `Colour` |
| AV1 | `filmcraft-av1` | slowest decoder: ~50 ms per 1080p frame on the reference machine |
| ProRes | `filmcraft-prores` | all 422/4444 profiles (alpha ignored) |
| DNxHD / DNxHR | `filmcraft-dnx` | incl. RGB 4:4:4 |
| APV | `filmcraft-apv` | |
| MPEG-1/2 | `filmcraft-mpeg2v` | interlaced pictures are deinterlaced (linear: the first field kept, the other rebuilt from its neighbours; an even box decimation blends the fields instead) |
| VP8, MJPEG, others | – | probed, unsupported |
**Pixel aspect.** Read from the container (MP4 `pasp`, Matroska `DisplayWidth`/`DisplayHeight`, MXF aspect ratio),
else the bitstream (H.264 / HEVC VUI SAR, MPEG-2 sequence header); frames come out at display aspect by stretching the
wider axis (HDV / DNxHD 1440×1080 at 4:3 → 1920×1080; DV NTSC 720×480 at 10:11 → 720×528), bilinear after the box
decimation. Only the first video track
is read; the first decodable audio track is decoded by `AudioReader` (see Audio below).
## Export through the engine (M14.8)
`lcc_engine::export` is the one export path for app, CLI and MCP (`crates/engine/src/export_video.rs`). A clip
(`Photo.video` set) in a batch exported to a rendered format becomes a movie; `Original` copies its file. It keeps
the clip's trim (`trim_in`/`trim_out`) and its develop settings reduced by `video_settings`, graded on every frame at
full quality by `lcc_pipeline::grade::grade_frame_linear` (float, linear Rec.709 in and out, so the export crate's
clamp + sRGB encode after the grade is the only encode; an 8-bit round trip would band ProRes). A default grade is the
identity and skips the pipeline. Options (`ExportOptions`, validated like every export key): `videoFormat`
(`h264` default = H.264 + AAC MP4, `prores` = ProRes 422 + AAC MOV), `videoQuality` 1–100 (default 80), `videoMaxSide`
16–8192 (0 = the clip's size). The file name keeps the naming template with `.mp4` / `.mov`.
The movie is written to a temporary file on the worker (`PreparedExport::run`, beside the batch's other items) and
moved into place in batch order (`Exported::video_file`, `Exported::save`); a skipped, failed or cancelled clip
leaves nothing behind. `set_video_hooks(items, &VideoHooks { cancel, progress })` gives clips the batch's
activity-row flag (checked once per frame) and a per-clip progress callback: the desktop background export and the
headless MCP export show it as the row's detail (`clip.mp4 42%`). The Export dialog shows a Video section (format,
quality, long edge) when the selection holds clips. CLI: `<cli> render clip.mp4 -o out.mp4` (or `.mov`), `--trim IN,OUT` (seconds; OUT empty = to the end) exports a range.
**Speed.** `export_clip` runs decode (one thread, in order), grading (`ceil(cores/8)` threads, 1–4, each frame's grade
is itself parallel) and encode + mux (the calling thread, which reorders frames by index) as a pipeline joined by
bounded queues (4 frames each), so memory stays at ~a dozen frames. Cancel is checked between frames in every stage;
on cancel or error the stages are halted and drained, the partial file removed. H.264 exports grade on the GPU when
one is available (`export_video::grade_gpu`: the 8-bit sRGB GPU render decoded back to linear; within a few 8-bit
codes of the CPU float grade, test `gpu_grade_matches_cpu_grade`; CPU fallback per frame); ProRes keeps the float
CPU grade (10-bit). The settings are reduced to `video_settings` once per clip; a default grade skips the pipeline.
Bench (`VIDEO_BENCH_CLIP=corpus/video/h264-1080p-bbb.mp4 cargo test --release -p lcc-engine --lib export_speed_1080p
-- --ignored --nocapture`; 10 s 1080p24 = 240 frames, exposure/contrast/vibrance grade, 32-core Linux shared with
other builds, load 30–110, so numbers are noisy):

| | H.264 | ProRes |
|---|---|---|
| before (sequential, CPU) | 243 s | 343 s |
| pipelined, GPU grade (H.264) | 45–52 s | 55 s |
| pipelined, `<PREFIX>_GPU=0`, load ~110 | 232 s | 397 s |

**Stage profile** (`<PREFIX>_PROFILE=1` prints per-frame busy time per stage). The Y'CbCr conversion and plane
packing used to run on the encoder thread; they now run on the grade workers, and ProRes frames (intra-only, the
encoder keeps no state between frames) are encoded there too, in parallel, and muxed in index order. Both encoders
are already slice-parallel (filmcraft `threads` feature, on by default). Output is byte-identical to before (same
sizes, 38 211 291 B H.264 / 550 802 254 B ProRes for 600 frames). Measured 2026-10-12 on the same 32-core machine at
load 115–185 (heavily shared, so noisy), same session, interleaved after/before/after:

| | H.264 | ProRes |
|---|---|---|
| before (conversion on the encoder thread) | 264 s | 378 s |
| after, run 1 / run 2 | 75 s / 94 s | 224 s / 180 s |

Per frame after (busy time summed over threads; H.264 run 1): decode 78 ms, grade 47 ms (GPU), conversion 280 ms
(on 4 workers), H.264 encode + mux 124 ms on the calling thread with ~0 ms waiting: the H.264 encoder is now the
bottleneck (124 ms × 600 ≈ the 75 s wall). For ProRes the workers are (CPU grade 430–530 ms + conversion and encode
760–960 ms per frame over 4 workers) and the muxer waits. Next steps: more ProRes workers, a faster (parallel or
SIMD) conversion, and H.264 over independent GOPs.

The 55 s ProRes run (same CPU grade path) shows the pipeline's gain on a less loaded machine; the `GPU=0` run was
taken at load ~110 and is CPU-starved. Output is byte-identical in size for ProRes before/after (the same frames).
Limits: the import probe that fills `Photo.video` lands with the engine wiring of M14.2; no "export current frame as
JPEG" command yet.

## Timelapse from photos (upstream #497)
`export.timelapse {path, ids?, fps?, size?, fit?, hold?, crossfade?, format?, quality?}` (`crates/engine/src/timelapse.rs`;
UI: File ▸ Export ▸ Timelapse Video…, `crates/ui-egui/src/timelapse_ui.rs`) turns stills into a silent movie. Photos: `ids`
in the given order, else the selection in the shown order (the current sort, Custom Order in an album); video clips are
left out (`skipped`). Each photo is rendered by the export render job with its own develop settings (sRGB, 8-bit) and
placed in one `size` frame (`"WxH"` or `[w, h]`, 16–8192, rounded down to even; default 1920x1080): `fit` letterboxes in
black, `fill` crops the centre. Each photo is held `hold` frames (1–1000, default 1) and `crossfade` extra frames
(0–1000) blend it into the next, so a movie has `photos × hold + (photos − 1) × crossfade` frames at `fps` (1–120,
default 24). `format` `h264` (MP4) or `prores` (MOV; the default follows the extension), `quality` 1–100 (80). The
frames go to `lcc_video_export::SequenceWriter` (same encoders and muxer as clip export; `push_encoded` takes the
sRGB render values as BT.709 video), written to `<path>.partial` and renamed when done; an error or a cancel (activity
row ✕, checked once per frame) leaves no file. The desktop app runs it on a worker; CLI/MCP run it in the call with its
own activity row. Limits: no audio/music, no deflicker or pan/zoom, integer frame rates only, native only.

## Audio (decode always; sound on Linux/BSD, elsewhere with `audio-out`)

**Decode** (`lcc-video`, every native build): `AudioReader::open(path)` takes the first decodable audio track and
returns interleaved f32 PCM at the source rate: `info()` → `AudioInfo {codec, sample_rate, channels}`; `seek(t)` +
`next_block()` → `AudioBlock {time, samples}`; `samples_from(t)` iterates blocks; `read_range(start, end)` returns a
trimmed range (capped at `MAX_RANGE_SECONDS`); `resample(samples, channels, from, to)` converts rates (linear, own
code). Times count from the clip's first shown frame, like `FrameReader`'s (MP4 edit lists, Matroska `CodecDelay`
and encoder priming are honoured; Matroska timestamps are whole milliseconds, so it is within 1 ms there).

| Container | AAC-LC | Opus | AC-3 / E-AC-3 | MP3 | PCM |
|---|---|---|---|---|---|
| MP4 / MOV | ✅ | ✅ (`dOps`) | ✅ | ✅ | ✅ `lpcm`, `sowt`, `twos`, `in24`, `in32`, `fl32`, `fl64`, `raw ` |
| Matroska / WebM | ✅ | ✅ | AC-3 | ✅ `A_MPEG/L3` | ✅ `A_PCM/INT/LIT`, `/INT/BIG`, `/FLOAT/IEEE` |
| MPEG-TS / PS | ✅ (ADTS; not LATM) | — | ✅ | ✅ | ✅ Blu-ray / AVCHD LPCM; DVD LPCM 16-bit only |
| MXF | — | — | — | — | ✅ PCM (BWF) and AES3 (ST 331) |

PCM is 8-bit (unsigned where the container says so) to 32-bit integer and 32/64-bit float, either byte order, up to
`MAX_CHANNELS`; 20-bit samples are read left-justified in 3 bytes. HE-AAC plays as its AAC-LC core (FilmCraft doesn't
rebuild the SBR band). MP3 is MPEG-1/2/2.5 Layer III only (Layer I/II are refused per packet). Not read
(`AudioReader::open` → `Unsupported` when no other track decodes): **FLAC** — the one pure-Rust decoder, `claxon`
(Apache-2.0), uses `unsafe` with uninitialised-buffer `set_len` on metadata reads, so it stays out until a safe
decoder exists; **ALAC** — the only pure-Rust decoder is symphonia's (MPL-2.0); MPEG-1 Layer II, DTS, TrueHD, LATM AAC;
20/24-bit DVD LPCM (grouped sample layout). Decoders: `filmcraft-aac`, `filmcraft-opus`, `filmcraft-ac3` (std-only,
MIT OR Apache-2.0), `puremp3` (MIT OR CC0-1.0, no `unsafe`; panics are caught per packet), PCM unpacking in
`audio_native.rs` (own code; MXF through `filmcraft-mxf::read_pcm`); never `filmcraft-codecs` (symphonia). A bad
packet is skipped; 32 bad in a row is an error.
Tests: `crates/video/tests/audio.rs` (a synthetic AAC tone in MP4, Matroska and TS, phase-checked; resampling;
damaged copies; the corpus clip `h264-aac-720p-nps-elk.mp4` when present) and `crates/video/tests/audio_pcm.rs`
(sample-exact PCM tones in MP4/MOV under each FourCC, Matroska and OP1a MXF; synthetic silent MP3 frames in
Matroska; damaged copies).

**Output** (`crates/audio-out` = `lcc-audio-out`, L0): the one sound-card path, shared with slideshow music.
`Output::start(rate, channels, fill)` plays what `fill` writes; `output.clock()` is an `AudioClock` (frames the device
pulled, as seconds) that the engine player can follow. `CAN_PLAY` says whether the build has sound.

**Linux and the BSDs: always on.** `crates/audio-out/src/pulse.rs` speaks the PulseAudio native protocol (also served
by PipeWire's `pipewire-pulse`) through the pure-Rust `pulseaudio` crate (MIT): no C library, so every build on those
systems has sound (`CAN_PLAY` true, `canPlayAudio` true for clips with decodable sound). With no server running,
`Output::start` errors and playback stays silent on the system clock.

**macOS / Windows: the `audio-out` feature.** There sound needs the cargo feature `audio-out`
(`cargo build -p lcc-app --features audio-out`; it forwards `lcc-engine/audio-out` → `lcc-audio-out/audio-out`, and
`lcc-slideshow/audio-out` is the same switch). It brings in `cpal` (Apache-2.0), which on Linux links the system C
library **libasound** (ALSA, through `alsa-sys` and pkg-config): a C dependency at build and run time, which the
pure-Rust rule forbids by default. macOS (CoreAudio) and Windows (WASAPI) need no extra library. Linux doesn't need it (the PulseAudio path above). Without
it, macOS and Windows builds report `canPlayAudio: false`, follow the system clock and play slideshows silently.

Checked 2026-10-12: with default features no package (`lcc-app`, `lcc-cli`, `lcc-web`, `lcc-engine`, `lcc-slideshow`)
has `cpal` in its tree (`cargo tree -p <pkg> -i cpal` finds none); `cargo deny check licenses` passes with default
features and with `--features lcc-app/audio-out`.

## Licences: why not `filmcraft-codecs`

The plan named `filmcraft-codecs` (the container/codec hub with GOP caching). At rev 156086dc it depends
unconditionally on `symphonia` (MPL-2.0, audio decoding), which `deny.toml` rejects (`cargo deny check licenses`
fails). `lcc-video` therefore uses the crates below it directly: the demuxers `filmcraft-isobmff`, `-matroska`,
`-mpegts`, `-mxf`; the decoders `filmcraft-h264`, `-hevc`, `-vp9`, `-av1`, `-prores`, `-dnx`, `-apv`, `-mpeg2v`;
and `filmcraft-bitstream`, `-frame`, `-color`, `-time`. Each is std-only (plus `thiserror` and `rayon`), `MIT OR
Apache-2.0`, with no `unsafe` and no C; `cargo deny check licenses` passes. The per-codec frame conversions follow
`filmcraft-codecs` (same licence) with bounds-checked plane access. `filmcraft-h264enc` is a dev-dependency only:
the tests generate their clips in-test, so no media is committed.

## Cost (2026-10-12, 32-core Linux)

- Cold release build of `lcc-video` and its FilmCraft dependencies: ~8 s wall with MP4 + H.264/HEVC only (spike S1
  measured ~20 s with the example), ~15 s with every container and codec above.
- A stripped binary using the API (the `clips` test binary): 2.6 MB → 5.2 MB with every container and codec.
- Decoding vs the `ffmpeg` oracle (dev only, not in CI), frame at 4 s, PSNR in sRGB: Big Buck Bunny 1080p excerpts
  H.264 38.6 dB, HEVC 38.6 dB, VP9 38.5 dB, AV1 38.5 dB (the four agree with each other at 32–34 dB, the excerpts
  being separate encodes of one source); the NPS 720p H.264 clip 47.3 dB. Spike S1: a 1080p libx264 clip with
  B-frames, 48.9 dB.
- First frame 2 s into a 1080p clip (decodes from the keyframe, single reader, no cache): H.264 ~0.6 s, HEVC ~0.5 s,
  VP9 ~0.7 s, AV1 ~3.1 s.

- Sequential decode with `next_frame` (`examples/seq_bench`, 300 frames from the start of each Big Buck Bunny 1080p
  excerpt, release build; FilmCraft's decoders use several threads), ms per frame at full size → at 960 px:
  H.264 8.8 → 5.7 (114 → 175 fps), HEVC 10.1 → 6.7 (99 → 150 fps), VP9 13.6 → 11.3 (73 → 88 fps), AV1 27.8 → 25.2
  (36 → 40 fps). The 960 px output is box-decimated from the decoder's Y'CbCr planes before Y'CbCr → RGB, so it costs
  less than full size; the decode itself dominates. The ≥ 30 fps target for 1080p H.264 at 960 px is met ~6×; AV1
  is the only codec near the limit.

**Tests.** `tests/clips.rs` (synthetic H.264 MP4: colour, seeking, `next_frame`/`frames_from`, VUI SAR 4:3 shown at display aspect, fuzz), `tests/formats.rs` (synthetic ProRes,
DNxHR, APV in MOV/MP4, ProRes and DNxHR in MXF, H.264 with B-frames in Matroska and in a hand-muxed transport stream:
probe fields, frame times, seeking, truncation and corruption sweeps), `tests/corpus.rs` (the clips from `cargo xtask
corpus --download`, skipped when absent: probe fields, neighbour/far-frame PSNR, the four Big Buck Bunny decoders
against each other, AVCHD 1080i reported interlaced, truncated and corrupted copies of each). There is no synthetic
MPEG-2, VP9 or AV1 clip (FilmCraft has no encoder for them); VP9 and AV1 are covered by the corpus clips, MPEG-2
only by FilmCraft's own tests.

```sh
cargo run --release -p lcc-video --example frame_png -- clip.mp4 1.5 out.png [max_side]
cargo run --release -p lcc-video --example seq_bench -- clip.mp4 [frames] [max_side]
```

## Revision history

- 2026-10-12: `export.timelapse` + File ▸ Export ▸ Timelapse Video… (upstream #497), `lcc_video_export::SequenceWriter`.
- 2026-10-12: GPU playback LUT (`lcc_gpu::video_lut`, `Playback::start_linear`, `Services::video_gpu`), per-frame cost measured.
- 2026-10-12: M14.4 loupe playback (decode-ahead worker, LUT grade, paused still), one-step trim drag.
- 2026-10-12: clip audio decode (`AudioReader`: AAC, Opus, AC-3 in MP4 / Matroska / TS; `resample`), `lcc-audio-out` (shared with slideshows) with `AudioClock`, engine clock hook, `canPlayAudio`.
- 2026-10-12: CLI `render --trim`; photo-only commands naming a clip in `ids` are refused even with a still
  active (`cmd::video_gate_call`); end-to-end tests (`apps/cli/tests/video.rs`, `crates/mcp/tests/e2e.rs`
  `video_clip_over_mcp`) on clips from `lcc_video::test_util::write_clip` (feature `test-util`, dev-only).
- 2026-10-12: sequential `seek`/`next_frame`/`frames_from`, pixel aspect ratio, MPEG-2 deinterlace, sequential decode cost.
- 2026-10-12: M14.4 / M14.6 engine: `video.*` commands, player state machine, `lcc-video` backend.
- 2026-10-12: M14.8 core, `lcc-video-export` (S3: option A).
- 2026-10-12: M14.8 export through the engine (options, dialog, CLI, MCP, activity row).
## Import and posters (M14.2 import side, M14.3)
- **Import** (`lcc_engine::files::probe_video_file`, used by the native file probe for video extensions): the clip is
  hashed in 1 MiB chunks (content hash + SHA-1 of the whole file, equal to hashing its bytes at once, so duplicate
  detection and Import → Copy verification work as for photos; never the whole clip in memory), then
  `lcc_video::probe` fills `Photo.video` (`VideoMeta`), the display width/height, `Photo.duration`, the capture date
  (QuickTime `creationdate` / `©day`, normalised to the Exif-date form `YYYY-MM-DDThh:mm:ss[±hh:mm]`) and GPS
  (ISO 6709). All of it lands in the photo record, so it is one undo step with the import. A clip whose container
  can't be read still imports (`video: None`, 1×1).
- **Posters** (`SourceRef::Video`, `media::video_poster`): a video's thumbnail, loupe and preview renders start from
  its poster frame (`lcc_video::poster` at `VideoMeta::poster_time`, default 10 % in) at the level's size, graded
  with `lcc_pipeline::grade::video_settings` of its develop settings on the display-referred path (exactly what
  `grade_frame` does, but through the ordinary render jobs, so thumbnails use the preview cache, its workers and
  the activity rows of preview builds). The render cache keys (`media::pixels_key`) include the poster time, and
  `Op::SetVideoPoster` / `Op::SetVideoMeta` drop the clip's decoded sources: a new poster renders afresh.
- A clip we can't decode (unsupported codec, damaged stream, the web build) gets `media::video_placeholder`, a dark
  film-strip tile drawn in code, instead of an error (logged at debug); an unreadable file (offline drive) stays an
  error.

## Next

Catalog format 10 + import (M14.2), grid posters (M14.3), player (M14.4), trim/poster (M14.6), develop on video
(M14.7), export (M14.8). See `plan/video-filmcraft.md` §9.
## Grading performance
A clip has one `DevelopSettings`, applied to every frame through the display-referred (JPEG/TIFF) input path of the
photo pipeline: `lcc_pipeline::grade::grade_frame(&Rgb32f, &DevelopSettings, Quality) -> Rgba8` (CPU) and
`lcc_gpu::grade_frame(&Arc<Rgb32f>, &DevelopSettings, Quality) -> (Rgba8, bool)` (GPU when available, CPU fallback;
the bool says whether the GPU rendered it). `Quality::Draft` fits 960 px wide, `Quality::Full` keeps the frame size.
`video_settings` keeps only the video controls: white balance (relative), Light, tone curve, colour (vibrance,
saturation, HSL), mixer / B&W mix, colour grading, profile and treatment. Everything else (texture, clarity, dehaze,
vignette, grain, detail, optics, geometry, crop, masks, spots, red eye, lens blur, enhance, point colour, calibration,
HDR) is dropped.
Measured 2026-10-12 on a 32-thread Linux box **under heavy load from other jobs (load average ≈ 90)**, so treat
these as upper bounds; synthetic `lcc-scenes` frames, exposure / contrast / highlights / shadows / vibrance / WB set.
Draft rows take the full-size frame and resample it; playback should decode at display size instead.
| Frame | Quality | CPU pipeline | GPU pipeline | CPU 3D LUT |
|---|---|---|---|---|
| 960×540 input | Draft/Full | — | — | 13–18 ms (56–79 fps) |
| 1920×1080 | Draft (960 wide) | 440 ms | 35 ms (29 fps) | 69 ms (resample dominates) |
| 1920×1080 | Full | 664 ms | 41 ms (24 fps) | 38 ms (27 fps) |
| 3840×2160 | Draft (960 wide) | 530 ms | 177 ms | 70 ms |
| 3840×2160 | Full | 1515 ms | 157 ms (6 fps) | 99 ms (10 fps) |
`<PREFIX>_PROFILE=1` on the 1080p CPU draft: sample 60–110 ms, white balance 15–25 ms, log luminance 12–40 ms,
the edge-aware Highlights/Shadows base 175–275 ms, finish 46–60 ms. The CPU pipeline is far from 30 fps, so playback
uses a baked LUT:
**3D LUT playback path** (`lcc_pipeline::grade::GradeLut`): `GradeLut::bake(&settings)` evaluates the video
controls on a 33³ lattice (sRGB-curve spacing of the linear input, ~30 ms) and `apply(&frame, quality)` /
`lookup(rgb)` interpolate it tetrahedrally (one thread: ~107 ns/px, 55 ms per 960×540 frame; parallel as above).
Highlights/Shadows enter it point-wise (their base taken as the pixel's own luminance); texture, clarity and dehaze
never do (stripped for video anyway). Accuracy vs the real grade on a 480×270 frame (CIE76 ΔE, tested): point-wise
controls mean 0.34 / p99 1.26; with Highlights/Shadows mean 0.61 / p99 2.24. Use the LUT while playing and the real
pipeline (`grade_frame`) on pause and export.
Reproduce: `cargo test --release -p lcc-gpu --test grade -- --ignored --nocapture` and
`cargo test --release -p lcc-pipeline grade::tests::lut_timings -- --ignored --nocapture`.
## Revision history
| Date | Change | Summary |
|---|---|---|
| 2026-10-13 | minor | Clip sound on Linux/BSD through the pure-Rust PulseAudio / PipeWire client in every build; `audio-out` only for macOS/Windows |
| 2026-10-12 | minor | Collapsed the two revision histories and the stacked Last updated lines into one |
| 2026-10-12 | minor | "Using video" section (M14.9). |
| 2026-10-12 | minor | M14.4 loupe playback (decode-ahead worker, LUT grade, paused still), one-step trim drag. |
| 2026-10-12 | minor | clip audio decode (`AudioReader`: AAC, Opus, AC-3 in MP4 / Matroska / TS; `resample`), `lcc-audio-out` (shared with slideshows) with `AudioClock`, engine clock hook, `canPlayAudio`. |
| 2026-10-12 | minor | CLI `render --trim`; photo-only commands naming a clip in `ids` are refused even with a still active (`cmd::video_gate_call`); end-to-end tests (`apps/cli/tests/video.rs`, `crates/mcp/tests/e2e.rs` `video_clip_over_mcp`) on clips from `lcc_video::test_util::write_clip` (feature `test-util`, dev-only). |
| 2026-10-12 | minor | sequential `seek`/`next_frame`/`frames_from`, pixel aspect ratio, MPEG-2 deinterlace, sequential decode cost. |
| 2026-10-12 | minor | M14.4 / M14.6 engine: `video.*` commands, player state machine, `lcc-video` backend. |
| 2026-10-12 | minor | M14.8 core, `lcc-video-export` (S3: option A). |
| 2026-10-12 | minor | M14.8 export through the engine (options, dialog, CLI, MCP, activity row). |
| 2026-10-12 | minor | faster clip export (pipelined stages, parallel grading, GPU grade for H.264). |
| 2026-10-12 | minor | Import fills `VideoMeta`, size, duration, date, GPS (streamed hash); graded poster thumbnails and loupe, placeholder tile (M14.2/M14.3) |
| 2026-10-12 | minor | Grading performance: CPU / GPU / LUT timings at 1080p and 4K |
| 2026-10-12 | minor | Clip audio decode: PCM (MP4/MOV FourCCs, Matroska, Blu-ray/DVD LPCM, MXF PCM/AES3), MP3 (`puremp3`); FLAC and ALAC documented as skipped |
| 2026-10-12 | minor | Export audio from any decodable source codec (AAC/Opus/AC-3/E-AC-3, MP4/MOV/MKV/TS); PCM in ProRes MOV (M14.8) |
