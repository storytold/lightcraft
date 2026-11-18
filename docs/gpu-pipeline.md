# GPU develop pipeline (M5.2)

> Names in angle brackets (`<app>`, `<binary>`, `<cli>`, `<PREFIX>`, `<settings_dir>`, …) are the values set in
> [`brand.toml`](../brand.toml); see the README.

The CPU pipeline (`crates/pipeline`) is the reference ("oracle"). `lcc-gpu` (`crates/gpu`, L3)
evaluates the same stages with wgpu compute shaders (WGSL) on Metal / Vulkan / DX12. Everything is
pure Rust (wgpu, naga); the drivers are the system's. No GL backend is compiled in.

CPU fast guided-filter allocation measurements and reproduction commands are documented in
[guided-filter-memory.md](guided-filter-memory.md). That optimization retains the full-size base
and does not change the GPU's coefficient buffers.

Warped geometry uses a CPU-computed coverage bit mask for the source boundary.
The CPU reference evaluates that boundary in double precision; GPU float
rounding otherwise can turn a blank edge pixel into a photo pixel. The mask
uses one bit per output pixel (rows padded to 32 bits). Sampling and color
corrections remain on the GPU, and existing geometry stage caching reuses the
result. The mask is built per 32 × 16 block (`Warp::block_coverage`): the warp
formulas are written once, generic over `lcc_geom::Real`, and evaluated
with outward-rounded `Interval`s over the block. Those bounds enclose the f64
result of every pixel in the block, so a block whose bounds lie inside the
image edges (the same f64 comparisons, no margin), or beyond one of them, is
filled at once; only blocks across the edge, or whose bounds are undecided (a
perspective denominator that may vanish), evaluate each pixel (`Warp::covers`,
the framing chain `Warp::frame` the CPU resample uses too). The bits equal the
per-pixel decision. At 6000 × 4000 with lens warp + perspective the mask takes
~2.7 ms (32 threads; ~24.5 ms on one) instead of ~30 ms (~580 ms), and only
when warped geometry rebuilds.

## Backends, environment variables and troubleshooting (issue #136)
wgpu loads the driver of **every** backend in an instance's set while it enumerates adapters — even
when it then picks another one. A Vulkan driver that crashes there (issue #136: an access violation
in Intel's `igvk64.dll` on a UHD 630 under Windows 11) takes the process down before any window
appears, and a native crash cannot be caught. So the app only lets wgpu touch the backends it
means to use (`lcc_gpu::backend`), for its compute device *and* for the desktop window
(eframe/egui-wgpu, `window_wgpu_options` in `apps/app/src/main.rs`):

| platform | default (window and compute) |
|---|---|
| Windows | Vulkan, DX12 as the fallback (each tried on its own; issue #934: DX12's FXC took 31 s to build the kernels on a cold shader cache). Compute follows the window's backend, else the first with a graphics card |
| macOS | Metal |
| Linux / BSD | Vulkan (the window also lists GL, eframe's default; no GL backend is compiled in) |

Overrides (read once at launch):
- `<PREFIX>_GPU_BACKEND=dx12 | vulkan | metal | gl | auto | off` (comma lists allowed, e.g.
  `vulkan,dx12`): the backends for both the window and GPU rendering; `off` turns GPU rendering off
  (CPU pipeline) — the window still needs a backend and keeps the platform default. A backend this
  build doesn't contain (`gl`) is ignored with a warning.
- else `WGPU_BACKEND` (wgpu's own variable, same names) — before #136 only the window honoured it,
  the compute device always used Vulkan + DX12 (+ Metal).
- `<PREFIX>_GPU=0`: GPU rendering off for the process (the window is unaffected).
- `<PREFIX>_GPU_ADAPTER=<part of a card's name>` (any case, e.g. `4070`, `radeon`, `"rtx 4070 super"`):
  the graphics card for GPU rendering, AI Denoise and the face models, on a machine with more than one
  (`backend::pick_adapter`). The first hardware card whose name contains it wins, in wgpu's order;
  software adapters (WARP, llvmpipe) never. A name that matches no card is logged
  (`gpu: GPU_ADAPTER=… names none of the graphics cards (…); using the default choice`) and the default
  high-performance card is used. The window stays on the card that drives the screen. The log's
  `gpu: GPU_ADAPTER=…: using …` line names the card used. Handy to keep test runs or a long export off
  the card the screens hang on.
- Both `<PREFIX>_GPU=0` and `<PREFIX>_GPU_BACKEND=off` also keep AI Denoise, the face models
  and the AI mask model (SAM 3 on Metal) on the CPU.
- `<PREFIX>_GPU_INIT_TIMEOUT=<seconds>` (whole seconds, 1–3600; default 30): how long creating a
  compute device (rendering, AI Denoise, faces) and compiling its kernels may go without building a
  kernel (ten times that in all). See *Device creation time limit* below.

**DX12 shader compiler (issue #471).** Every instance (window and compute) compiles DX12 shaders with
FXC (`d3dcompiler_47.dll`, part of Windows; `lcc_gpu::backend::backend_options`). wgpu's
default, `Auto`, loads whichever `dxcompiler.dll` the DLL search path finds first. The app ships
none, so that copy belongs to another program (a Windows SDK, a folder on `PATH`). An older copy without
`dxil.dll` beside it (e.g. DXC 1.7) warns that the DXIL is unsigned, wgpu treats the warning as a
compile error, its indirect-validation pipelines fail, the device is lost, and the window never opened
(`WGPU error: Parent device is lost`). `WGPU_DX12_COMPILER=dxc | auto | fxc` (wgpu's variable) still
chooses; with `dxc`, put `dxcompiler.dll` *and* `dxil.dll` (DXC ≥ 1.8.2502) next to `<binary>.exe`.
FXC builds the compute kernels somewhat slower than DXC (≈ 9 s vs 7.5 s for the GPU test suite on an
RTX 4080), off the startup path.

FXC merges stores to neighbouring elements of a storage array into one wider store. NVIDIA's DX12 driver ran
the 16-byte store it made of a denoise convolution's output wrong on a GTX 1070 (every second pixel's channels
shifted; DXC and Vulkan were right), so `nn_conv.wgsl` steps those stores by `1 + zero` from its uniform and FXC
leaves them apart. The transposed convolution's merged store ran right on that card, but it steps the same way.
`nn::tests::a_layer_stores_every_pixels_channels_where_the_next_reads_them` catches the fault on such a card;
`a_transposed_convolution_stores_every_pixels_channels_where_the_next_reads_them` checks the other store exactly.

FXC also cannot address two things the WGSL looks harmless with, and answers with `X3550 … forcing loop to unroll`
and then `X3511 unable to unroll loop` (the shader module fails, so the pipeline is never made): an array handed to a
function through a pointer (`ptr<function, array<…>>`) that the function then indexes with a loop variable, and a vector
indexed with a loop variable on the left of an assignment (`acc[ch] = …` with `acc: vec4<f32>`). The block-matching
kernels (`wgsl/bm3d_*.wgsl`) keep their working arrays at module scope (`var<private>`, one copy of the transforms
generated per array, `bm3d_transform.wgsl`) and accumulate in `array<f32, 4>` instead. A GPU test that only records a
skip when the device cannot build the kernels hides this, so read the skip note (`cargo test … -- --nocapture`).

**Off the startup path.** The compute device is created on a background thread once the window is
up (`gpu::warm_up` from the first frame's settings), and never while GPU rendering is off: with
Settings ▸ Performance ▸ *Use the GPU for rendering* unchecked (applied before the window opens),
`<PREFIX>_GPU=0` or `<PREFIX>_GPU_BACKEND=off`, no GPU driver is loaded for rendering at all.

**Device creation time limit (issue #3068).** Creating the wgpu instance, adapter and device calls
into the driver, which on Linux may first talk to the display server; with one that accepts the
connection and never answers (a crash-looping WSLg compositor, a stale `WAYLAND_DISPLAY` socket) those
calls never returned, and every render waiting for the device, and every test that creates one, hung
for good. wgpu's native calls are synchronous, so each compute device (rendering, denoise, faces) is
now created, kernels included, on a thread of its own (`lc-gpu-device`, `crates/gpu/src/init.rs`), and
the caller waits until `<PREFIX>_GPU_INIT_TIMEOUT` seconds (default 30) pass with no progress. Each
module prepared and each kernel built is progress (`init::Progress`, ticked by `ctx::build_kernels`),
so a first start after an install or a driver update, when the driver's shader cache is empty and
compiling every kernel takes 30 s on an RTX 4070 SUPER and an RTX 5070 under DX12, still ends on the
GPU instead of falling back to the CPU; a creation is given up on at ten times the limit in all
(`did not finish in 300 s (10 times …)`), or a limit after its last kernel (`stopped making progress
for 30 s … after N kernels`). Past it that device is unavailable for the process, with the
reason `GPU device creation did not finish in 30 s (<PREFIX>_GPU_INIT_TIMEOUT): …` reported like any
other creation failure (`perf.gpuReason`, the no-GPU notice, the log), and the blocked thread is left
behind. That thread, not the caller, holds what must last as long as the driver work does: its share
of the GPU work in flight, so a quit meanwhile finds the GPU busy (`gpu::wait_idle` returns `false` at
its own deadline, *Quitting* below), and the crash sentinel, so a quit meanwhile leaves the marker and
the next launch starts on the CPU (*Crash sentinel* below). While it is left behind, creating another
kind of device fails at once with the same reason rather than leave a second thread behind; if the
driver ever answers, the marker is removed, its late device is leaked, not dropped, and later
creations are tried again. The desktop window's own device is not covered (#410).

**Quitting.** The devices are never dropped, and renders run on worker threads: a render still inside
the driver while the process exits crashes there (a SIGSEGV in the NVIDIA Vulkan driver on Linux,
issue #620), which no panic hook catches. So every entry into a device counts as work in flight
(`crates/gpu/src/exit.rs`), and the desktop app ends with `LccApp::shutdown` (from `on_exit`,
after settings and library are saved): `gpu::begin_shutdown()` — from then on `gpu::render` returns
`None`, no device is created, denoise tiles fail over and a render job gives up instead of rendering
on the CPU — then the render pool stops (queued jobs dropped, running ones waited for) and
`gpu::wait_idle` waits for GPU work on other threads (export, denoise, the device warm-up). One
deadline of 2 s covers it all; past it the app quits anyway. A job pool that is dropped (headless
hosts, tests) waits for its running jobs the same way (`JobPool::shutdown`, `crates/preview/src/pool.rs`).

**Crash sentinel.** The desktop app writes `gpu-init.marker` into its settings folder (next to
`ui.json`: `%APPDATA%\<settings_dir>`, `~/Library/Application Support/<settings_dir>`,
`~/.config/<settings_dir>`) just before the compute device is created and its kernels are compiled (and, on the first
AI denoise, before that network's kernels are) and removes it as soon as that returns, successfully or
not (issue #250: Apple's Metal compiler, `MTLCompilerService`, aborting on a kernel on an older Intel
Mac). A kernel the driver refuses without crashing is caught by wgpu error scopes: the log says why
(`gpu: rendering on the CPU: …`) and renders stay on the CPU. `<PREFIX>_GPU=0` (or Settings ▸
uncheck *Use the GPU for rendering*) forces the CPU path from the start. If the marker is still there at the next launch, the process died inside
the driver: the app starts with GPU rendering off (the preference is saved unchecked), removes the
marker and says so in a notice. The same happens after a quit while a device creation that passed its
time limit was still inside the driver (above): the driver hung rather than crashed, but it would
likely hang again, so the next launch starts on the CPU too. Checking *Use the GPU for rendering* again tries the GPU once more
(and re-arms the sentinel). Not with `<PREFIX>_NO_PREFS` (tests, scripts), and not in a `--memory` session,
which writes nothing: it neither arms the sentinel nor removes a marker it finds (it still starts with GPU
rendering off when one is there, and the next ordinary launch reports and clears it). Quitting the app
normally removes the marker even when device creation has not returned yet (a launch closed a few
seconds in, while a large library was still opening): an orderly quit proves the process was not killed
inside the driver (`backend::release_init_marker_on_quit`, called in `App::on_exit` after the shutdown wait). Killing the app during
the ~0.3 s of device creation, or two instances starting at the same moment, can trip it falsely —
harmless: rendering is then on the CPU until the box is checked again. The sentinel covers the compute
device and every kernel build; a crash while the window's renderer starts is avoided by the backend defaults above
or worked around with `<PREFIX>_GPU_BACKEND`.

**The window's renderer and its fallback (Windows).** Before the window opens, each renderer is probed in
order: Vulkan, DX12, GL, then eframe's OpenGL renderer (glow). A broken Vulkan driver can crash the process before
any window (#136): the marker below then puts Vulkan last. A probe creates a device and builds egui's
pipeline inside wgpu error scopes, so a driver that cannot compile egui's shaders ("The parameter is
incorrect. (0x80070057)" in `egui_pipeline`) is logged and skipped instead of killing the app; no pop-up
unless that renderer was forced (#1624). The renderer
that worked is remembered in the settings folder (`window-renderer-2`); a start that dies before the window is up
leaves the renderer's name in `window-init.marker` (emptied once the window is up), and the next start
(relaunched automatically, at most 4 times) tries that renderer last. Where the file system supports locks, the
starting copy holds an operating-system lock on the marker until its window is up, so a second copy started on
the same settings folder while the first is still starting leaves the first's marker alone and does not read it
as a failed start (no "could not start with its … renderer last time" notice, and Windows does not remember a
weaker renderer because of it). That second copy writes no marker while the first holds it, so if its own start
dies it is not relaunched. A crash or kill releases the lock, so a marker with no live owner (also one an older
version left) still counts as a failed start; a copy that hangs inside the driver without dying keeps the lock
until it is ended. An empty or unreadable marker names no failed start. `<PREFIX>_GPU_BACKEND=dx12 | vulkan | gl | glow` (or `WGPU_BACKEND`) forces one; DX12 stays
first because a broken Vulkan driver can crash the process uncatchably (#136). Compute keeps its own
device and falls back to the CPU as before.

**A window device lost while running (#1194).** egui-wgpu skips a frame it cannot draw (#1098), so a
window whose device was lost would skip every frame forever (frozen, "Application Hang"). eframe cannot
rebuild a window's device in place, so `window_gpu::LossWatch` watches the frames instead (a vendored
egui-wgpu hook, `on_frame_acquired`, reports each frame that got its texture): an unbroken run of failed
frames — 30 over at least 2 s after the GPU reported a lost device, 600 over 20 s otherwise — means the
device is gone. A single skipped frame, a hidden or resizing window never counts. The renderer is then written
to `window-lost` in the settings folder (at once, so it survives the process being killed) and dropped from
`window-renderer-2`; every later start tries it last (unless `<PREFIX>_GPU_BACKEND` forces it). The library and
settings are saved, a native message box says what happened, and the app ends.

**The window's surface: one frame in flight.** The window's wgpu options start from eframe's defaults
(`NativeOptions::default().wgpu_options`, `window_gpu::base_options`), whose surface is egui-wgpu's
`SurfaceConfig::LOW_LATENCY`: vsync, at most one frame queued ahead of the display (two drawables on
Metal). egui-wgpu's bare `WgpuConfiguration::default()` is `HIGH_THROUGHPUT` (two frames queued, three
drawables), and the window used it from #136 until this change, so a drag reached the screen one frame
(8.3 ms at 120 Hz, 16.7 ms at 60 Hz) later. The window's own GPU work is small (egui's meshes and
texture uploads; renders run on the compute device), which is what `LOW_LATENCY` is meant for.
During a live window resize egui-wgpu still uses three drawables on macOS.

**Troubleshooting a crash at startup (Windows).** Start the app from a `.cmd` file or a terminal
with `set <PREFIX>_GPU_BACKEND=dx12` (the default since #136), or `=off` to keep the GPU out of
rendering; `set VK_LOADER_DRIVERS_DISABLE=*igvk64*` (Vulkan loader) hides a specific Vulkan driver
from every program started with it. Help ▸ System Info and `app.gpu` show the adapter and backend in
use (e.g. `Intel(R) UHD Graphics 630 (Dx12)`).

**The app stops during an export or a render; the log says "device lost" (Windows, issue #1337).**
When the graphics driver resets the GPU (the Windows watchdog, a driver hang: DXGI `0x887A0005`, "GPU
device instance has been suspended"), renders fall back to the CPU, but the window's renderer (egui-wgpu)
panics on the lost device and the app stops. It says so ("<app> stopped unexpectedly", not "could
not open its window") and records it in `gpu-lost.marker` in the settings folder: the next start turns
*Use the GPU for rendering* off and says why once. Turn it back on in Settings ▸ Performance after a
driver update.

**Window never appears, "not responding" (Windows, NVIDIA; issue #374).** With *Shader Cache Size*
set to **Disabled** in NVIDIA Control Panel (Manage 3D settings → Global Settings), NVIDIA's driver
walks the whole system drive while the window's GPU device is created (`D3D12CreateDevice` →
`nvwgf2umx.dll` → `NvMemMapStoragex.dll!TotalDiskUsage`), before the first frame: a black,
unresponsive window for minutes. The Vulkan driver shares that code, so another backend doesn't
help. Set *Shader Cache Size* back to **Driver Default** (or any size); the setting is global, there
is no per-program override. Other programs that create a DX12 or Vulkan device at startup hang the
same way (PhotoCraft does). It is a driver issue: nothing in the app itself
walks the drive.

## Where it is used
- `lcc_engine::media::develop` (called by every `RenderJob`): loupe / before / compare views,
  `render_now` (CLI, MCP, control channel renders) and exports render on the GPU when one is
  available; grid/filmstrip thumbnails (many small jobs in parallel) stay on the CPU.
- Anything the GPU path cannot do returns `None` and the CPU renders instead: no adapter (CI
  machines, software-only adapters), `<PREFIX>_GPU=0` or `<PREFIX>_GPU_BACKEND=off` (whole
  process; see [Backends](#backends-environment-variables-and-troubleshooting-issue-136)), the `app.gpu {enabled}`
  command (runtime preference; `ui.inspect` → `perf.gpu` shows the adapter), a buffer larger than
  the device's storage-buffer limit, or a render the device did not complete correctly (see
  [Limits, failures and the CPU fallback](#limits-failures-and-the-cpu-fallback)).
- wasm32: the crate compiles to the CPU fallback (WebGPU needs asynchronous device creation and a
  device shared with the canvas — open item below).

## Structure
- Shared parameters, so the two implementations cannot drift apart: `pipeline::plan` (effective
  settings, frame, output size, stage-cache keys), `Frame::sample_plan`, `local::{wb_matrix_for,
  nr_params, plane_sigmas, guided_fast_step, airlight_of}`, `finish::{FinishParams, mask_terms}`,
  `masks::brush_dabs`; exact tables (tone LUT, sRGB LUT, curve LUTs, the masks' local Curves from
  `finish::local_curve_luts`, resample taps) and the OkLab
  matrices are uploaded / generated into the WGSL prelude from the CPU values.
- Buffers, not textures: images are `array<f32>` with the CPU's interleaved layout (RGB, 1–3
  channels), so upload / readback are plain copies of `Rgb32f` / `Plane` / `Rgba8` data.
- Kernels (`crates/gpu/src/wgsl/`): `geom` (orientation pixel map, bilinear through the
  crop/straighten/flip affine or the full lens + perspective warp), `resize` (separable resample
  with the CPU's taps; Mitchell prefilter, box/bilinear for the fast guided filter), `blur` (box
  passes, three each way = the CPU's Gaussian: horizontal ones sum each pixel's window, so adjacent
  threads read adjacent pixels; vertical ones slide running sums down chunks of a column), `map`
  (log luminance, dark channel, guided-filter steps, white balance, luminance / colour NR, airlight
  sampling), `mask` (linear / radial / luminance / colour range / brush shapes, combine, finalize),
  `finish` (the whole per-pixel stage), `fit` / `fit_pack` (the loupe's display fit: the 8-bit
  output unpacked for `resize`'s Lanczos taps and packed back with the CPU's rounding) — each
  mirrors a named CPU function.
- Soft proofing against an output space (`RenderRequest::proof`, with its destination and display gamut
  warnings) runs in `finish`; a proof against an output-device profile (its 3D table) renders on the CPU.
- Display fit (`lcc_gpu::try_render_fitted`, used by `RenderJob::run` for jobs with a
  `display_fit`): the render is shrunk to the size it is drawn on the device, after its histogram
  is binned from the whole frame, and only the fitted image is read back (2560 → 1728 px: 11.4 →
  3.7 ms with the fit, against the CPU's `fit_rgba8` after a full readback). It matches `fit_rgba8`
  within 1 LSB (`the_display_fit_on_the_device_matches_the_cpus`). Overlays, a window that spots
  grew, HDR renders and a monitor profile's conversion keep the CPU fit. A render at rest reads the full
  image back with the fitted one, for the photo's view preview; a drag's updates cache the fitted one.
- Smooth Skin (`skin` module: `skin_lab`, `skin_reduce`, `skin_combine`, with the box Gaussian on
  4-channel bands) runs on the device from the CPU's plan (`skin::plan`: the coverage over the
  skin's box, the faces grouped by size and each group's region), the same fast log / exp as the
  CPU (`crates/gpu/tests/skin_gpu.rs`: max 1 LSB). Before, a photo with Smooth Skin had its whole
  linear image read back, smoothed on the processor and uploaded again (a 45 MP export spent ~0.8 s
  of its GPU turn on that). The retouch of people's parts still runs on the CPU (read back, uploaded),
  and so does Smooth Skin when a region is over the device's buffer limit.
- Per-stage hybrid: defringe and spot removal (rare, CPU-only for now) download the resampled image,
  run on the CPU and upload; heuristic mask shapes (Sky, Subject, Background, …), brushes with pen pressure and
  brushes of more than 65 536 dabs (or whose tile lists would pass 8 M words) are evaluated on the CPU and uploaded. The
  brush kernel bins each stroke's dabs into tiles of about a quarter radius, so a pixel looks only at the dabs that reach
  its tile instead of every dab of the stroke (a dress brushed over, zoomed to 100 %: tens of seconds before); sources over the buffer limit are resampled
  on the CPU. Retouch ▸ Teeth (`lcc_pipeline::retouch::apply`) reads the
  noise-reduced scene-linear image back, runs the CPU's code and uploads it again, so the two renderers
  match by construction (`gpu_matches_cpu` case `retouch teeth (cpu stage)`); it is per-pixel inside
  small masks, so a kernel of its own was not worth a second implementation yet.
- Stage cache: `GpuStages` mirrors `StageCache` (same keys: source identity + `geo`, `lin_key`,
  per-plane radii) and lives in it as an extension, so the UI needs no change. The uploaded source is
  kept per view. A tone / colour / exposure drag re-runs one dispatch of `finish` plus the readback.
- One command encoder per render; the only mid-render readback is the airlight samples (dehaze, when
  the dark channel is recomputed). Dropped buffers are recycled (exact size, ≤ 2 GB pool) once the
  thread that dropped them has submitted — allocating and zero-filling fresh 100–300 MB buffers
  per pass cost as much as the passes.
- `<PREFIX>_PROFILE=1` prints GPU stage timings (each stage is then submitted and waited for).

## Limits, failures and the CPU fallback
Issue #78: on an Intel UHD (ICL GT1) iGPU with Mesa/Vulkan, exports from the desktop app came out
as valid JPEGs that were entirely black, while previews, `<cli> render` and
`<PREFIX>_GPU=0` were fine — and the GPU export took ~15 s against ~3 s on the CPU. The readback
returned a buffer the per-pixel stage had never written, with no error. The likeliest cause: the
whole export was recorded as one command submission lasting seconds on that GPU, and the driver
reset it (GPU hang check / preemption timeout — the app's own window keeps the GPU busy, the CLI
doesn't) without the error reaching wgpu. wgpu also reports a lost device only through the
device-lost callback, which was not set. So a GPU render now refuses to hand back anything it
cannot vouch for:

- **Short submissions.** A render submits after every stage and every 16 M kernel invocations
  (`FLUSH_INVOCATIONS`); the per-pixel stage runs in bands of ~4 MP. Every submission on a slow
  iGPU stays short enough to be preempted and never trips a watchdog (Windows TDR ~2 s, i915).
  Buffers released by earlier stages are reused by later ones, lowering peak device memory.
- **Error scopes.** Each render runs inside out-of-memory, validation and internal error scopes
  (thread-local in wgpu): errors fail that render instead of disappearing into the uncaptured
  handler. The device-lost callback and the uncaptured handler (other threads) stop GPU use.
- **Readback.** The wait has a timeout (60 s); a failed wait, map error or missing map callback
  fails the render (before, a lost device left the readback waiting forever).
- **Limits.** The device is created with the adapter's limits; every buffer is checked against
  `min(max_storage_buffer_binding_size, max_buffer_size)` when it is created. A render with a
  buffer over the limit (24 MP RGB f32 = 275 MiB; a 128 MiB WebGPU-default device; many masks)
  falls back for that render, without a device error. Dispatches stay under 65535 workgroups per
  dimension (`groups1` folds 1-D kernels onto two axes). Exports over the limit are not tiled:
  most stages need neighbourhoods tens to hundreds of pixels wide (guided filters at clarity /
  base radius, dehaze dark channel) and some need global values (airlight percentile), so tiles
  would need wide overlaps and a two-pass plan — the CPU renders those exports instead.
- **Sanity checks on the result.** The output buffer is cleared before the per-pixel stage, which
  writes alpha 255 everywhere: any pixel without it means work did not run, and the GPU is not used
  again (`incomplete image`). An entirely black result (n ≥ 4096 pixels) is re-rendered on the CPU
  for that render only — a really black photo costs one extra CPU render, nothing else; there are
  no false positives because the CPU result is what is returned.

| failure | this render | later renders |
|---|---|---|
| buffer over the device limit | CPU | GPU |
| out of device memory | CPU (buffer pool emptied) | GPU |
| entirely black result | CPU | GPU |
| validation / internal error, device lost, readback failure or timeout, incomplete image, panic | CPU | CPU (whole process) |

**Diagnosing.** `ui.inspect` → `perf.gpu` (adapter in use), `perf.gpuReason` (why renders don't use
the GPU, e.g. `"disabled by <PREFIX>_GPU=0"`, `"software adapter (llvmpipe (LLVM 21.1.8, 256 bits))
skipped: …"`, `"stopped after a GPU failure: device lost …"`), `perf.gpuFallback` (the latest render
redone on the CPU and why, e.g. `"6000×4000: the GPU returned an incomplete image (24000000 of
24000000 pixels unwritten); the GPU is not used again"`). The same in `app.gpu` (`reason`,
`lastFallback`), Help ▸ System Info and Settings ▸ Performance. `<PREFIX>_PROFILE=1` prints the
fallback with the stage timings; `RUST_LOG=warn` logs it.
`perf.gpuFallback` is one value for the whole process, overwritten by the latest render on any
thread. A render's *own* reason comes from `lcc_gpu::try_render` (`Err(Fallback::Failed(why))`;
`Fallback::NotTried` when the GPU was not used at all), which `lcc_engine::media::develop_reporting`
passes on as `RenderResult::gpu_fallback`; an export carries it as `Exported::gpu_fallback`, and
each file of an `app.export` batch result has `gpuFallback` when its render fell back (issue #675).
**Reproducing a smaller GPU:** `<PREFIX>_GPU_LIMITS=webgpu` (or `downlevel`) creates the device
with 128 MiB storage bindings / 256 MiB buffers, `<PREFIX>_GPU_LIMITS=<n>` with n MiB / 2n MiB.
Tests inject failures with `lcc_gpu::inject_fault` (`crates/gpu/tests/it/fallback.rs`,
`export_falls_back_to_the_cpu_when_gpu_work_is_lost`). The fault is thread-local, and so are its
consequences: a simulated fatal failure stops GPU use on the injecting thread only (`available()`
false, `unavailable_reason()` set there), until `reset_failures()`, which undoes nothing else — a
real failure still stops the whole process. Such a test asserts on its own render's reason, not on
`last_fallback()`, so it can run beside other tests rendering on the GPU (issue #675).

## Correctness: CPU oracle and equivalence tests
`crates/gpu/tests/it/equivalence.rs` renders the same settings on both and compares 8-bit sRGB:
bounds **mean |Δ| < 0.5 LSB and max |Δ| ≤ 3 LSB** per channel. Measured (Apple M4 Pro, Metal):
**max 1 LSB, mean ≤ 0.0003 LSB** for every case — 26 settings cases (all tone/colour/effects tools,
3 vignette styles, grain, curves, grading, B&W, WB, NR, crop/straighten/flip, lens + perspective,
three mask sets incl. CPU-evaluated shapes, spots + defringe), all 8 orientations ± crop, embedded
DNG lens data, display-referred sources, draft quality, full-size renders; the 24 MP export in
`render_bench` also differs by max 1 LSB. Cached (slider-drag) GPU renders are bit-identical to
fresh ones. `windows_match` runs the settings cases again as a window of a 3000 × 2000 frame
(`RenderRequest::window`, the zoomed loupe, issue #323) plus a vignette in a corner and a spot reading
from outside the window: the vignette geometry arrives as the `VIG_VIEW` parameter, the airlight and
noise-reduction size come from the whole frame, and a window that spots or Auto Mask strokes grew is
cut back after the readback. The tests skip (pass with a note) when no adapter exists.

**AMD Radeon 8060S (RADV STRIX_HALO, Vulkan 1.4, Mesa 26.2.4, Linux 7.2), 2026-10-12**, the first
non-Apple GPU verified: all 16 tests pass, 135 compared cases. 124 differ by max 1 LSB, 10 are
bit-identical, and one (the point colour range overlay) has max 2 LSB on 0.0002% of pixels. Worst
mean 0.0003 LSB. That is the same picture as on Metal, inside the bounds
(`cargo test -p lcc-gpu --release --test it equivalence:: -- --nocapture`).

**HDR renders** (`OutputDepth::F32Hdr`: the HDR views of an HDR display, HDR exports, gain maps) run on
the GPU too: the per-pixel kernel takes the HDR edit's peak (`PEAK`; the tone LUT is `ToneMap::hdr` /
`camera_hdr` / `display_hdr` from `finish::base_tone`, shared with the CPU) and, with `DEEP`, encodes with
the exact sRGB curve unbounded above 1, continues the curves past SDR white with slope 1, gamut-maps into
`0..peak` and writes linear float RGB (`f32`, output primaries) after the 8-bit image, which is its
reduction (`DeepImage::to_rgba8`), so the histogram is still binned on the GPU. `hdr_renders_match`
compares the float samples (sRGB-encoded past 1, ×255) with the CPU's: max < 0.01, mean ≤ 0.0001 steps
on the AMD Radeon 8060S across tone-map arms, light edits, vignette, curves, grain and an HDR request of
an SDR edit. 16-bit and SDR float exports, soft proofs against an output-device profile and Visualize HDR stay on the CPU (`handles`); matrix soft proofs of HDR renders run on the GPU, mapped to the HDR peak.
An HDR view on an HDR display that takes texels (`HdrOutput::texel_headroom`) has its
`Rgba16Float` texture made on the device (`lcc_gpu::try_render_hdr_texels`, `hdr_texels.wgsl`, the
same transform as `lcc_engine::hdr_texture::encode_pixel`): 8 bytes a pixel come back instead of 12,
and the CPU neither unpacks the floats nor encodes them (`the_gpus_hdr_texels_match_the_cpu_encoder`:
within 2e-3 relative). HDR exports, gain maps and the CPU path keep the floats.
3000 × 2000 HDR render (release, shared machine): CPU ~200–575 ms, GPU ~50–70 ms
(`cargo test -p lcc-gpu --release --test it equivalence::hdr_render_time -- --ignored --nocapture`).

Remaining differences come from f32 vs f64 coordinate math, fast-math transcendental functions on
Metal and summation order in the box filters — all far below one 8-bit step.

## Measurements
**In the app:** Help ▸ Debug ▸ Frame Rate (`view.frameRate`, `{show?}`; `<PREFIX>_FPS=1`
starts with it on, never saved) draws frames per second, each frame's UI CPU time against its
time spent waiting (for vsync, or for the GPU), the latest whole-frame and zoom-window render
times, the render queue, renders thrown away as superseded, the GPU adapter and its memory, and a
graph of the last frames with a tick where a render arrived. Once a second it logs the same
numbers (`fps-hud:`), so a scripted drag (`ui.dragWidget`) can be measured from the log.

**Present mode** (issue #633): the window presents with FIFO (vsync), except with HDR output on
Wayland with an NVIDIA GPU, where it uses Mailbox when the surface offers it (FIFO there missed
vblank at idle clocks: ~95 instead of 144 fps). `<PREFIX>_PRESENT=fifo` forces FIFO,
`<PREFIX>_PRESENT=mailbox` asks for Mailbox anywhere it is offered; a mode the surface doesn't
offer always falls back to FIFO.

`render_bench` on `corpus/raw/arw-sony-a7m3-compressed.arw` (24 MP; loupe = 2560 px preview →
1920×1280), min of 7 runs, wall clock, on a heavily shared machine (load average 60–110 on 14 cores,
so CPU numbers are pessimistic; the GPU numbers are less affected):

| scenario | CPU | GPU |
|---|---|---|
| loupe 1920×1280 cold (new photo, incl. upload) | 415 ms | 32 ms |
| loupe draft 1152×768 cold | 169 ms | 21 ms |
| exposure drag 1920×1280 (warm) | 22–42 ms | 3.9 ms |
| highlights drag, draft (warm) | 14 ms | 3.2 ms |
| clarity drag, draft (warm) | 16–23 ms | 3.0 ms |
| NR drag, draft / 1920×1280 (warm) | 186 / 301–471 ms | 10 / 18 ms |
| per-pixel stage 1920×1280, + vibrance/saturation | 53–65 ms | 5 ms |
| export render 6000×4000 | 1037–1414 ms | 291–330 ms |

On Windows (Radeon 780M iGPU, DX12), a full-size JPEG export of the same raw with heavy edits (NR 30
/ 25, clarity, texture, dehaze, sharpening, highlights / shadows) took 3.6–4.0 s on the GPU against
1.9–2.0 s on the CPU: the horizontal box pass had each thread slide a running sum over 16 pixels, so
the threads of a workgroup read pixels 16 apart, and it took 9–15× as long as the vertical pass.
Summing each pixel's window instead (adjacent threads, adjacent pixels): 1.9–2.0 s, with the GPU's
NR stage 1.5 → 0.4 s and spatial planes 0.89 → 0.29 s (`<PREFIX>_PROFILE`). Per-pass timings come from `<PREFIX>_PROFILE` on
`cargo xtask bench` (24 MP source).

GPU timings include the readback of the 8-bit result and the histogram. The histogram (channels, chroma, vectorscope and waveform grids) is binned on the GPU by `hist_k` (`wgsl/hist.wgsl`: workgroup-local atomics for the 1D bins, global atomics for the grids) over the same ~1 MP sample grid as `Histogram::of_srgb8_covered`, the CPU oracle, and read back with the image in one transfer; `gpu_histogram_matches_the_cpu_oracle` checks they agree exactly. At 24 MP it takes the ~7 ms CPU pass off each render (75 → 68 ms on this machine's GPU). Device creation + kernel
compilation: ~0.4 s once per process — the desktop app starts it on a background thread at launch
(`lcc_gpu::warm_up`), so the first loupe render doesn't wait for it; other processes create
the device on their first GPU render. `lcc_gpu::ready()` asks without blocking.

**Colour NR on the CPU (gap 26, 2026-10-13).** From a blur radius of `HALF_RES_NR_SIGMA` (4 px)
on, a CPU *draft* render (slider drag) blurs the chromaticity at half resolution (2×2 box down, half the radius, bilinear back)
instead of at full resolution; the luminance and the per-pixel mix stay at full resolution, and
smaller radii and every full-quality render keep the full-resolution blur, so final renders match the GPU
and the look existing edits always had (half-res on final renders broke that; fixed 2026-10-13). A test bounds the difference (mean < 0.002, worst
< 0.02 linear). Measured by `colour_nr_timing` (`cargo test --release -p lcc-pipeline
colour_nr_timing -- --ignored --nocapture`), both paths interleaved in one run on a machine at load
average ≈ 105 on 32 threads, so compare the ratio, not the absolute numbers: 1936×1290 140 → 78 ms,
6000×4000 670 → 336 ms (σ 6.8, best of 5). Full-resolution colour NR on the CPU is still over the
16 ms slider budget at loupe size; the GPU path (unchanged, full-resolution blur, 10–18 ms for the
whole NR drag) is the one that meets it.

On Windows / DX12 the kernels cost more: FXC compiles each pipeline single-threaded inside
`create_compute_pipeline` (issue #471). Built one after another, the 27 took ~2.2 s on an Intel Core
Ultra 7 165U iGPU (`app.gpu` 2.4 s median, up to 7 s on the first run after an install), so
`ctx::build_kernels` shares them out to worker threads (issue #2936): ~1.05 s there (`app.gpu` 1.2 s),
most likely the largest kernel (`finish.wgsl`) alone (not timed per kernel). wgpu's error scopes are per thread, so every worker
opens its own; a refused kernel still means no GPU. The `gpu:` info log line gives the kernel count
and build time.

## Opening a photo (M5.4)
- The loupe shows a stand-in at once (`media::QuickJob`): the photo's cached view render for its
  current settings, else (raws with their import look) the embedded camera JPEG, else a
  thumbnail-level render; the full render replaces it in place.
- Raw sources for previews (2560 px) and thumbnails are binned straight from the mosaic
  (`RawImage::develop_binned`, 2× for a 24 MP preview); only exports / 1:1 demosaic at full size.
- The next and previous photos in filmstrip order are prepared in the background once the current
  one is rendered (decoded source kept, view render cached): stepping through photos shows the
  developed image in ~50 ms.

## Memory (M5.6)
A view's stages keep an uploaded source only up to 96 MB (a Preview-level source); a bigger one (the original a zoom window is cut from) is kept once, outside the views' stages, for all views of the photo (the Before and After windows and every pan share it; the Before window of a Before/After waits at first open until the After's job has handed the original over, or each would decode and upload its own copy) and given back when the app idles with no zoom window on screen (a zoomed view keeps it for its next pan: a 64 MP original is 800 MB to upload again; issue #323: kept per view it counted against the stage budget on every tick of a drag, and not kept every new window uploaded 288 MB again).
- `library.memory` reports what the engine's caches hold (decoded thumbnail / preview / full-size
  sources, rendered previews) and the GPU renderer's device buffers (allocated, of which pooled
  and retired); `ui.inspect` → `memory` adds the loupe's stage caches (CPU images, GPU buffers)
  and the textures.
- Heap profile: build `<cli>` with `--features dhat-heap`; `library.memory` then also
  reports live/peak heap bytes and the run writes `dhat-heap.json` (`<PREFIX>_DHAT_FILE`), whose
  allocation sites at the peak (`t-gmax`) show who holds the memory.
- Measuring the scenario (import 14 raws from `corpus/raw`, open the loupe, step 12 times):
  `/usr/bin/time -l <cli> snapshot <files> --script steps.jsonl -o out.png` → "maximum
  resident set size" and "peak memory footprint". Run it several times: the high-water mark is
  noisy (allocator caching, scheduling). On Apple silicon GPU buffers count in the footprint.
- Imports read raw headers only (`lcc_raw::probe_info`): no pixel data is decompressed,
  except the first rows a few vendor formats need for facts their headers lack (Nikon NEF: the
  first 128 rows for the masked trailing columns, issue #708).
- One budget (`memory::budget`, default min(25 % of RAM, 16 GiB); by hand at most 75 % of RAM; `<PREFIX>_MEMORY_MB` or
  `app.memoryBudget {mb}`): half for the engine caches (decoded thumbnail / preview / full
  sources and rendered previews, evicted least recently used *across* them; the photo on screen
  stays), a quarter for decodes in flight (`memory::work_gate`: grid thumbnails, neighbour
  prefetch and import probes wait for room, loupe and export loads never wait), an eighth for the
  GPU buffer pool (reuse by best fit within 25 %; freed after 3 s idle). "RAM" honours container
  and cgroup limits: on Linux it is the smaller of `MemTotal` and the process's cgroup memory limit
  (`memory.max`, set on its cgroup or an ancestor; v1 `memory.limit_in_bytes`), as is merge's 75 %.
- Decodes free the file's bytes and the raw samples as soon as they are consumed and colour /
  crop / orient in place. macOS keeps freed large blocks resident ("reusable") until the system
  is short of memory, so the resident size climbed with every photo decoded; the apps install
  `malloc_zone_pressure_relief` as `memory::set_release_hook`, called after each decode, import
  and idle trim.
- Result (M4 Pro, 14 raws, loupe, 12 steps): peak RSS 1.9–2.3 GB → 0.83–0.99 GB, peak footprint
  2.5–2.6 GB → 0.91–0.96 GB, heap peak 930 → ~600 MB; stepping median ~13 ms (unchanged).

## Drawing drafts straight from the GPU
The desktop app creates the window's wgpu device with the adapter's limits and offers it to the GPU
renderer (`lcc_gpu::offer_window_device`; the kernels build on it when the renderer warms up, off
the UI thread, under the crash sentinel and the creation deadline), so rendering and drawing share
one device. A slider drag's drafts of the views (Detail, Before, hover, Compare, the second window)
are then `RenderJob::on_screen`: the render ends in an `Rgba8Unorm` texture
(`lcc_gpu::try_render_display`) that egui-wgpu draws as a native texture (`ScreenTextures` in
`ui-egui`, implemented by the app): no readback, no conversion to egui's pixels, no re-upload. The
per-render checks run on the device instead: every pixel written (an atomic count), not entirely
black (the device histogram). Rows are re-laid at a 256-byte stride for the texture copy when the
width needs it (`present.wgsl`).

Renders that need pixels keep the readback: the settled full-quality render (the cached view
preview, the colour readout, screenshots), high quality adjustment previews (their Lanczos display
fit), zoomed windows and tiles, thumbnails, exports, overlays, windows that spots grew, a monitor
profile, HDR views (their own texels path) and anything the GPU path declines. A device that
doesn't suit (software adapter, too few storage buffers, kernels that don't build there) is
declined and the renderer makes one of its own, as before; a device that replaces a lost one is
always its own. A headless `ui.screenshot` reads a GPU-drawn view back (`lcc_gpu::read_texture`).

- Not offered on an HDR window for now: with the renderer on the window's device, turning an
  edit's HDR on stopped the Wayland window drawing (Radeon 8060S / RADV, 2026-11-03; no error
  logged; fine with a device of its own). `<PREFIX>_GPU_WINDOW=1` offers it anyway.
- `<PREFIX>_GPU_WINDOW=0`: never offer the window's device (a device of its own, renders read
  back): to compare, or for a driver that misbehaves with compute and drawing on one device.
  `<PREFIX>_GPU_LIMITS` caps the adopted device as it caps one of its own.
- `ui.inspect` → `perf.gpuWindowDevice` (renders run on the window's device), `perf.screenTextures`
  (the UI can draw GPU-resident renders), `perf.mainOnGpu` (the loupe shows one now).
- With the renderer on the window's device, errors the device raises outside a render (the window's
  own included) are logged and replace the render device (one of its own) instead of wgpu's
  default panic.

AI models (the denoise network in `nn.rs` and `bm3d.rs`, the face models in `faces.rs`)
make their device on the second graphics card when one is chosen or found, else on the editing
card (`lcc_gpu::ai_adapter_on`; `ai_card()` says which card the first goes to), and keep a device
of their own on each card (`lcc_gpu::ai::Card`; below). Each AI device is made once and given up
on alone. Models that arrive later (SAM 2.1 / EoMT masks, MediaPipe people) should make their
devices per card the same way. (The AI masks and the MediaPipe face finder run through candle or
on the processor, with no wgpu device.)

## Two graphics cards: who renders batch work

Settings ▸ Performance ▸ Second graphics card (`app.gpu {helper}`; `<PREFIX>_GPU_HELPER` wins) sets
up a device on another card (`helper.rs`). Thumbnails sent there (`lcc_gpu::prefer_helper`) and AI
models (`ai_adapter`) use it whenever it is ready. Batch photos (exports, Build Previews, the
background preparation, the import's previews) take a **turn per photo** on either card
(`lcc_gpu::batch_turn_any` / `wait_batch_turn_any`), and the photo's renders run on that card:

- Each card's gate (`gate.rs`) measures its batch speed (bytes of finished turns per busy second,
  the recent past first), and a photo goes to the card whose running photos plus this one would
  finish soonest, so both cards work in proportion to their speed (the second card on a tie).
  Before this, every batch render went to the second card while it was ready and the editing card
  sat idle.
- The editing card yields to the interface once the desktop app tracks it
  (`lcc_gpu::track_interaction`, `note_interaction` on every frame with input): batch turns there
  only after ~1 s with no input and no view render, one job at a time while the second card
  works, and a batch render already running there pauses between its submissions the moment a
  slider, brush, pan or photo change starts (`batch.pauses` in `app.gpu`). The second card draws no
  window: it never waits for the interface, and its default batch budget is 80 % of its memory
  (55 % on the editing card).
- 1:1 Build Previews runs two photos per card (`batch_lanes_hint(2)`); exports' automatic lanes
  grow by the second card's jobs, and a lanes setting made on one card is raised to both cards'
  turns plus one (`<PREFIX>_EXPORT_LANES` still means exactly that many). The CLI, MCP and tests
  don't track the interface: there both cards simply share photos by speed.
- `app.gpu` → `batch.cards`, `batch.rate {editing, helper}`, `batch.renders` (GPU renders so far by
  card and kind of work) and `batch.interfaceBusy` show it working.
- Exports follow Settings ▸ Performance ▸ Exports ▸ Graphics cards for exports
  (`lcc_engine::export::ExportCards`, `app.exportTuning {cards}`): both cards (above), the card for
  editing only (`try_batch_turn_on(false, …)`), or the second card only, to measure it (the card
  for editing takes the photo when the second card isn't ready or is lost).
- AI Denoise keeps a denoise device on each card (`nn::Card`: `Second`, `Editing`; `nn::dev_on`,
  `nn::runner_on`, `bm3d::Bm3dGpu::new_on`), each set up, checked against the processor and given
  up on alone (`nn::broken_reason_on`). A batch makes one photo on each card at once, the second
  card first (`Model::lanes`; one photo at a light pace), and one photo's tiles stay on its card.
  While the second card works too, a tile for the editing card waits for an idle interface
  (`nn::background_turn`); with one card nothing waits. The second card's set-up marker is
  `gpu-setup-helper.marker` beside the editing card's.
- The face recognition models keep a face device on each card too (`faces.rs`; `ai::Card`, shared
  with denoise). A new model goes to the second card when there is one that works, else to the
  card for editing (`faces::accelerator`; `accelerator_on` names a card), and keeps one copy of its
  weights there: a face scan is mostly the processor's work (decoding, finding faces with the CPU
  MediaPipe model), so a copy on each card would gain little. Each card is given up on alone
  (`faces::broken_reason_on`); when the card a model ran on stops working, the scan offers the
  model to the other card (checked against the CPU there as at first), else it stays on the CPU
  (`face_detect::move_off_lost_card`; `faces.gpu` names the card as `recognition.card`). The
  scan's pace holds the second card back only at a light pace (`faces::set_pace` gives it its own
  share: all of its time at a normal or full pace); a model on the card for editing doesn't wait
  for the interface, since the second card never has the same model's work.

## Open items
- WebGPU in the browser build (async init, single device per canvas).
- Defringe and spot removal kernels; Sky/Subject heuristics (replaced by the segmenter in M12).
- Tiling for sources / outputs beyond the buffer limit (24 MP+ on WebGPU-default limits, 60 MP+
  on most desktop adapters; these render on the CPU now): tile with overlap equal to the largest
  filter radius, with global values (airlight) from a first pass.
- Integrated GPUs much slower than the CPU (issue #78's GT1): a measured per-device choice of the
  faster path for exports.
- Large-radius blurs at full size (dehaze dark channel at 24 MP) dominate the export: a summed-area
  table or a downsampled dark channel would cut them further.
