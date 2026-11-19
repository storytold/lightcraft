//! The built-in denoiser's block-matching filter ([`lightcraft_denoise::bm3d`], which is the reference) on the GPU:
//! three WGSL kernels (`wgsl/bm3d_*.wgsl`) that group alike blocks, filter each group's blocks together and put them
//! back, run twice (a hard threshold, then a Wiener filter built from its result) in one command buffer per tile.
//!
//! Only the filter stage runs here. Measuring the noise, the variance-stabilising transform and its inverse and the
//! demosaic stay on the processor ([`Builtin::run_with`]), so a tile differs from the processor's only by the rounding
//! of the filter itself. Like [`crate::nn`], the runner has a device of its own, respects the same switches and the same
//! crash sentinel, never panics, and a failure is an `Err` that makes the caller run that tile on the CPU.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use bytemuck::{Pod, Zeroable};
use lightcraft_denoise::bm3d::{self, Params};
use lightcraft_denoise::builtin::Builtin;
use lightcraft_denoise::nlm::hadamard;
use lightcraft_denoise::run::{Error, TileRunner};
use wgpu::util::DeviceExt;

use crate::nn::{CLOSING, Dev, TIMEOUT, dev, entry, mark_broken, pipeline, storage};

/// Tiles that can be on the GPU at once (each has buffers of its own).
const WORKSPACES: usize = 2;
/// The largest tile side (in cells) the buffers and dispatch sizes are made for.
const MAX_TILE: usize = 512;
/// The block side and the group size the kernels are written for.
const BLOCK: usize = 4;
const GROUP: usize = 16;
/// Invocations in a workgroup (the kernels' `@workgroup_size`).
const WORKGROUP: u32 = 64;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    t: u32,
    nr: u32,
    nb: u32,
    search: u32,
    cap: u32,
    wiener: u32,
    pad0: u32,
    pad1: u32,
    tau: f32,
    lambda: f32,
    sigma: f32,
    pad2: f32,
}

struct Pipes {
    matching: wgpu::ComputePipeline,
    matching_layout: wgpu::BindGroupLayout,
    collab: wgpu::ComputePipeline,
    collab_layout: wgpu::BindGroupLayout,
    agg: wgpu::ComputePipeline,
    agg_layout: wgpu::BindGroupLayout,
    /// How long each kernel took to build (the driver compiles them), for the log and the tests.
    built: Vec<(String, f64)>,
}

/// Compile the three kernels (match, filter, put back) and make their bind-group layouts, timing each build.
fn build_pipes(d: &Dev) -> Pipes {
    use wgpu::BufferBindingType::{Storage, Uniform};
    let (ro, rw) = (Storage { read_only: true }, Storage { read_only: false });
    let layout =
        |entries: &[wgpu::BindGroupLayoutEntry]| d.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None, entries });
    let matching_layout = layout(&[entry(0, Uniform), entry(1, ro), entry(2, ro), entry(3, rw)]);
    let collab_layout = layout(&[entry(0, Uniform), entry(1, ro), entry(2, ro), entry(3, ro), entry(4, ro), entry(5, rw), entry(6, rw)]);
    let agg_layout = layout(&[entry(0, Uniform), entry(1, ro), entry(2, ro), entry(3, ro), entry(4, ro), entry(5, ro), entry(6, rw)]);
    let common = include_str!("wgsl/bm3d_common.wgsl");
    let transform = include_str!("wgsl/bm3d_transform.wgsl");
    // the group's transforms, for the noisy blocks (`buf`) and for the pilot's (`pb`)
    let collab_source = format!(
        "{}\n{}\n{}",
        transform.replace("{{A}}", "buf").replace("{{S}}", ""),
        transform.replace("{{A}}", "pb").replace("{{S}}", "_p"),
        include_str!("wgsl/bm3d_collab.wgsl")
    );
    let mut built = Vec::new();
    let mut make = |name: &str, kernel: &str, layout: &wgpu::BindGroupLayout| {
        let started = std::time::Instant::now();
        let p = pipeline(d, name, &format!("{common}\n{kernel}"), layout);
        built.push((name.to_string(), started.elapsed().as_secs_f64() * 1000.0));
        p
    };
    let matching = make("bm3d match", include_str!("wgsl/bm3d_match.wgsl"), &matching_layout);
    let collab = make("bm3d filter", &collab_source, &collab_layout);
    let agg = make("bm3d put back", include_str!("wgsl/bm3d_agg.wgsl"), &agg_layout);
    Pipes { matching, matching_layout, collab, collab_layout, agg, agg_layout, built }
}

/// One tile's buffers and bind groups.
struct Workspace {
    y: wgpu::Buffer,
    basic: wgpu::Buffer,
    out: wgpu::Buffer,
    /// The two steps' uniforms (their match tolerance and the filter they use differ).
    first: wgpu::Buffer,
    second: wgpu::Buffer,
    staging: wgpu::Buffer,
    /// Match, filter and put back, for the first step then the second.
    bound: [wgpu::BindGroup; 6],
    _slots: [wgpu::Buffer; 3],
}

/// The filter on the GPU for one tile size and one set of parameters. Cheap to share between threads: each call takes a
/// free workspace.
pub struct Bm3dGpu {
    dev: &'static Dev,
    pipes: Pipes,
    /// The parameters it was made for, as they were given.
    requested: Params,
    t: usize,
    /// Blocks that start a group, along a side, and how many blocks fit along a side.
    nr: usize,
    nb: usize,
    consts: wgpu::Buffer,
    free: Mutex<Vec<Workspace>>,
    /// Workspaces made so far (they are made when a call finds none free, up to [`WORKSPACES`]).
    made: Mutex<usize>,
    wake: Condvar,
}

/// An error for something the kernels do not do, with `why` as its text.
fn unsupported<T>(why: impl Into<String>) -> Result<T, String> {
    Err(why.into())
}

impl Bm3dGpu {
    /// The filter with `params` for tiles of `t × t` cells on the GPU, or why it cannot be (no GPU, parameters or a size
    /// the kernels do not do, the device refusing the buffers). Creates the device on first use and compiles the kernels.
    pub fn new(params: &Params, t: usize) -> Result<Bm3dGpu, String> {
        let _work = crate::exit::enter().ok_or(CLOSING)?;
        let Some(plan) = bm3d::plan(params, t, 0.0) else { return unsupported("the tile is smaller than a block") };
        let p = plan.params;
        if p.block != BLOCK || p.group > GROUP || p.step == 0 {
            return unsupported(format!("the kernels do {BLOCK} x {BLOCK} blocks in groups of at most {GROUP}"));
        }
        if t > MAX_TILE {
            return unsupported(format!("tiles of more than {MAX_TILE} cells on a side are not done on the GPU"));
        }
        let d = dev()?;
        let nb = t + 1 - BLOCK;
        let axis = bm3d::starts(nb, p.step);
        let nr = axis.len();
        let blocks_bytes = (nr * nr * 4 * GROUP * BLOCK * BLOCK * 4) as u64;
        if blocks_bytes > d.limits.max_storage_buffer_binding_size.min(d.limits.max_buffer_size) {
            return unsupported(format!("{} cannot hold a tile's filtered blocks ({} MB)", d.info.name, blocks_bytes >> 20));
        }
        let (dct, window) = bm3d::constants(BLOCK);
        let consts: Vec<u32> = dct.iter().chain(&window).map(|v| v.to_bits()).chain(axis.iter().map(|&a| a as u32)).collect();
        let scope = [
            d.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory),
            d.device.push_error_scope(wgpu::ErrorFilter::Validation),
            d.device.push_error_scope(wgpu::ErrorFilter::Internal),
        ];
        // the driver compiles the kernels here: a crash in it is reported at the next launch (issue #250)
        let pipes = crate::backend::with_marker("denoise kernel compilation", || build_pipes(d));
        let consts = d.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("bm3d consts"),
            contents: bytemuck::cast_slice(&consts),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let mut me =
            Bm3dGpu { dev: d, pipes, requested: *params, t, nr, nb, consts, free: Mutex::new(Vec::new()), made: Mutex::new(1), wake: Condvar::new() };
        let first = me.workspace();
        me.free = Mutex::new(vec![first]);
        let [oom, validation, internal] = scope;
        let errors = [pollster::block_on(internal.pop()), pollster::block_on(validation.pop()), pollster::block_on(oom.pop())];
        if let Some(e) = errors.into_iter().flatten().next() {
            return Err(format!("{}: the denoise kernels could not be set up ({e})", d.info.name));
        }
        Ok(me)
    }

    /// The adapter this runs on.
    pub fn adapter(&self) -> String {
        format!("{} ({:?})", self.dev.info.name, self.dev.info.backend)
    }

    /// The parameters it was made for.
    pub fn params(&self) -> Params {
        self.requested
    }

    /// How long each kernel took to build, e.g. `bm3d match: 812 ms`.
    pub fn build_report(&self) -> String {
        self.pipes.built.iter().map(|(k, ms)| format!("{k}: {ms:.0} ms")).collect::<Vec<_>>().join(", ")
    }

    /// A new workspace for this runner's tile size: the plane, scratch and staging buffers and the six bind groups.
    fn workspace(&self) -> Workspace {
        let d = self.dev;
        let (t, groups) = (self.t, self.nr * self.nr);
        let planes = (4 * t * t * 4) as u64;
        let (y, basic, out) = (
            storage(d, "bm3d y", planes, wgpu::BufferUsages::COPY_DST),
            storage(d, "bm3d basic", planes, wgpu::BufferUsages::COPY_SRC),
            storage(d, "bm3d out", planes, wgpu::BufferUsages::COPY_SRC),
        );
        let members = storage(d, "bm3d members", (groups * (GROUP + 1) * 4) as u64, wgpu::BufferUsages::empty());
        let blocks = storage(d, "bm3d blocks", (groups * 4 * GROUP * BLOCK * BLOCK * 4) as u64, wgpu::BufferUsages::empty());
        let weights = storage(d, "bm3d weights", (groups * 4 * 4) as u64, wgpu::BufferUsages::empty());
        let uniform = |label| {
            d.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: std::mem::size_of::<Uniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let (first, second) = (uniform("bm3d first step"), uniform("bm3d second step"));
        let staging = d.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bm3d readback"),
            size: planes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = |layout: &wgpu::BindGroupLayout, buffers: &[&wgpu::Buffer]| {
            let entries: Vec<wgpu::BindGroupEntry> =
                buffers.iter().enumerate().map(|(i, b)| wgpu::BindGroupEntry { binding: i as u32, resource: b.as_entire_binding() }).collect();
            d.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout, entries: &entries })
        };
        let (m, c, a) = (&self.pipes.matching_layout, &self.pipes.collab_layout, &self.pipes.agg_layout);
        let k = &self.consts;
        let bound = [
            bind(m, &[&first, k, &y, &members]),
            // the first step's filter has no pilot: it is given a buffer it does not read
            bind(c, &[&first, k, &y, &basic, &members, &blocks, &weights]),
            bind(a, &[&first, k, &y, &members, &blocks, &weights, &basic]),
            bind(m, &[&second, k, &basic, &members]),
            bind(c, &[&second, k, &y, &basic, &members, &blocks, &weights]),
            bind(a, &[&second, k, &y, &members, &blocks, &weights, &out]),
        ];
        Workspace { y, basic, out, first, second, staging, bound, _slots: [members, blocks, weights] }
    }

    /// A free workspace, a new one when fewer than [`WORKSPACES`] exist, else wait for one to come back.
    fn take(&self) -> Result<Workspace, Error> {
        let mut free = self.free.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(w) = free.pop() {
                return Ok(w);
            }
            {
                let mut made = self.made.lock().unwrap_or_else(|e| e.into_inner());
                if *made < WORKSPACES {
                    *made += 1;
                    drop(made);
                    drop(free);
                    let d = self.dev;
                    let scopes =
                        [d.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory), d.device.push_error_scope(wgpu::ErrorFilter::Validation)];
                    let w = self.workspace();
                    let [oom, validation] = scopes;
                    let (validation, oom) = (pollster::block_on(validation.pop()), pollster::block_on(oom.pop()));
                    if let Some(e) = oom.or(validation) {
                        self.forget();
                        return Err(Error::Runtime(format!("the GPU could not make room for another tile: {e}")));
                    }
                    return Ok(w);
                }
            }
            free = self.wake.wait(free).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// A workspace comes back after a good run: it goes on the free list and one waiting caller is woken.
    fn give(&self, w: Workspace) {
        self.free.lock().unwrap_or_else(|e| e.into_inner()).push(w);
        self.wake.notify_one();
    }

    /// A workspace is gone (dropped after a failure): room for a new one, and a waiting caller may make it.
    fn forget(&self) {
        // under `free`, as `take` is when it reads `made` and goes to wait: a wake-up sent between the two would be lost
        let free = self.free.lock().unwrap_or_else(|e| e.into_inner());
        let mut made = self.made.lock().unwrap_or_else(|e| e.into_inner());
        *made = made.saturating_sub(1);
        drop(made);
        drop(free);
        self.wake.notify_one();
    }

    /// Denoise four unit-noise planes of `t × t` cells as [`bm3d::filter`] does; `noise` as there.
    pub fn filter(&self, planes: &[Vec<f32>], t: usize, noise: f32) -> Result<Vec<Vec<f32>>, Error> {
        if let Some(why) = crate::nn::broken_reason() {
            return Err(Error::Runtime(format!("the GPU is not used any more: {why}")));
        }
        let Some(plan) = bm3d::plan(&self.requested, t, noise).filter(|_| t == self.t) else {
            return Err(Error::Input("the tile is not the size the filter was made for".into()));
        };
        if planes.len() != 4 || planes.iter().any(|p| p.len() != t * t) {
            return Err(Error::Input("the tile is not four square planes".into()));
        }
        let y = hadamard(planes);
        // one tile is one piece of GPU work: a photo's worth would outlast any wait at exit
        let Some(_work) = crate::exit::enter() else { return Err(Error::Runtime(CLOSING.into())) };
        let w = self.take()?;
        let flat: Vec<f32> = y.concat();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.execute(&w, &flat, &plan))) {
            Ok(Ok(out)) => {
                self.give(w);
                Ok(hadamard(&out.chunks_exact(t * t).map(<[f32]>::to_vec).collect::<Vec<_>>()))
            }
            Ok(Err(e)) => {
                // its readback buffer may be left half-mapped: a fresh workspace is made when one is next needed
                drop(w);
                self.forget();
                Err(e)
            }
            Err(_) => {
                // a panic inside wgpu: this workspace is not given back, and the GPU is not used again
                mark_broken("a denoise tile panicked");
                drop(w);
                self.forget();
                Err(Error::Runtime("the GPU runner gave up".into()))
            }
        }
    }

    /// Run the filter's steps in `w` on the rotated planes `y` (one plane after another) and read the result back; a
    /// device that runs out of memory or fails validation gives an error.
    fn execute(&self, w: &Workspace, y: &[f32], plan: &bm3d::Plan) -> Result<Vec<f32>, Error> {
        let d = self.dev;
        let p = &plan.params;
        let sigma = p.sigma.max(1e-3);
        let var = sigma * sigma;
        let uniforms = |tau: f32, wiener: u32| Uniforms {
            t: self.t as u32,
            nr: self.nr as u32,
            nb: self.nb as u32,
            search: p.search as u32,
            cap: p.group as u32,
            wiener,
            pad0: 0,
            pad1: 0,
            tau,
            lambda: plan.lambda * sigma,
            sigma,
            pad2: 0.0,
        };
        let groups = (self.nr * self.nr) as u32;
        let cells = (self.t * self.t) as u32;
        let scopes = [
            d.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory),
            d.device.push_error_scope(wgpu::ErrorFilter::Validation),
            d.device.push_error_scope(wgpu::ErrorFilter::Internal),
        ];
        d.queue.write_buffer(&w.y, 0, bytemuck::cast_slice(y));
        d.queue.write_buffer(&w.first, 0, bytemuck::bytes_of(&uniforms(p.tau1 * var, 0)));
        d.queue.write_buffer(&w.second, 0, bytemuck::bytes_of(&uniforms(p.tau2 * var, 1)));
        let mut enc = d.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("bm3d tile"), timestamp_writes: None });
            let steps = if p.wiener { 2 } else { 1 };
            for step in 0..steps {
                for (i, (pipe, count)) in
                    [(&self.pipes.matching, groups), (&self.pipes.collab, groups * 4), (&self.pipes.agg, cells)].into_iter().enumerate()
                {
                    pass.set_pipeline(pipe);
                    pass.set_bind_group(0, w.bound.get(step * 3 + i), &[]);
                    pass.dispatch_workgroups(count.div_ceil(WORKGROUP), 1, 1);
                }
            }
        }
        let result = if p.wiener { &w.out } else { &w.basic };
        let bytes = (y.len() * 4) as u64;
        enc.copy_buffer_to_buffer(result, 0, &w.staging, 0, bytes);
        d.queue.submit([enc.finish()]);
        let slice = w.staging.slice(..bytes);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let waited = d.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(TIMEOUT) });
        let [oom, validation, internal] = scopes;
        let errors = [pollster::block_on(internal.pop()), pollster::block_on(validation.pop()), pollster::block_on(oom.pop())];
        let [internal, validation, oom] = errors;
        if let Some(e) = oom {
            // not a broken device: this tile runs on the CPU, and the next may fit
            return Err(Error::Runtime(format!("the GPU ran out of memory: {e}")));
        }
        if let Some(e) = internal.or(validation) {
            mark_broken(&format!("device error: {e}"));
            return Err(Error::Runtime(format!("the GPU reported an error: {e}")));
        }
        if let Err(e) = waited {
            mark_broken(&format!("the GPU did not finish a tile: {e}"));
            return Err(Error::Runtime(format!("the GPU did not finish: {e}")));
        }
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(Error::Runtime(format!("reading the result back failed: {e}"))),
            Err(e) => return Err(Error::Runtime(format!("reading the result back failed: {e}"))),
        }
        let out = {
            let data = slice.get_mapped_range().map_err(|e| Error::Runtime(format!("reading the result back failed: {e:?}")))?;
            data.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect::<Vec<f32>>()
        };
        w.staging.unmap();
        Ok(out)
    }
}

/// The built-in denoiser with its filter on the GPU: a [`TileRunner`] for one photo (its noise level is in `builtin`).
pub struct BuiltinRunner {
    gpu: Arc<Bm3dGpu>,
    builtin: Builtin,
}

/// How close the card's filter is to the processor's on a tile, in the noise's deviations (the filter's own domain, where
/// the noise has deviation 1): the largest difference at one place and the mean one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Agreement {
    pub worst: f32,
    pub mean: f32,
}

impl BuiltinRunner {
    /// `builtin` must use the block-matching filter with the parameters `gpu` was made for.
    pub fn new(gpu: Arc<Bm3dGpu>, builtin: Builtin) -> Result<Self, String> {
        match builtin.bm3d_params() {
            Some(p) if p == gpu.params() => Ok(BuiltinRunner { gpu, builtin }),
            _ => Err("the settings of the built-in denoiser are not the ones the graphics card was set up for".into()),
        }
    }

    /// The adapter it runs on.
    pub fn adapter(&self) -> String {
        self.gpu.adapter()
    }

    /// Run the filter of the tile `input` on the card and on the processor and say how far apart they are. The two
    /// differ by the rounding of the card's arithmetic: a few blocks whose match or threshold falls either side of a
    /// tie change a little (well under half a deviation), and a broken kernel by far more. (The final picture is not
    /// compared: the demosaic's choice of direction at a pixel can flip on a tiny difference.)
    pub fn agreement(&self, input: &[f32]) -> Result<Agreement, Error> {
        let seen = std::cell::Cell::new(None);
        self.builtin.run_with(input, &|planes, t, noise| {
            let want = bm3d::filter(planes, t, &self.gpu.params(), noise);
            let got = self.gpu.filter(planes, t, noise)?;
            let (mut worst, mut sum, mut n) = (0f32, 0f64, 0usize);
            for (a, b) in want.iter().flatten().zip(got.iter().flatten()) {
                let d = (a - b).abs();
                worst = if d.is_nan() { f32::INFINITY } else { worst.max(d) };
                sum += f64::from(d);
                n += 1;
            }
            seen.set(Some(Agreement { worst, mean: (sum / n.max(1) as f64) as f32 }));
            Ok(got)
        })?;
        seen.get().ok_or_else(|| Error::Runtime("the filter was not run".into()))
    }
}

impl TileRunner for BuiltinRunner {
    /// Clean one tile with the built-in pipeline, its block-matching filter running on the card.
    fn run(&self, input: &[f32]) -> Result<Vec<f32>, Error> {
        self.builtin.run_with(input, &|planes, t, noise| self.gpu.filter(planes, t, noise))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Four planes of `t × t` cells: edges, a ramp and some texture, plus unit noise (an LCG and Box-Muller).
    fn scene(t: usize, seed: u64) -> Vec<Vec<f32>> {
        let mut state = seed;
        let mut uniform = move || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((state >> 40) as f32 + 0.5) / (1u64 << 24) as f32
        };
        (0..4)
            .map(|c| {
                (0..t * t)
                    .map(|i| {
                        let (x, y) = ((i % t) as f32, (i / t) as f32);
                        let signal = 20.0
                            + 0.1 * x
                            + if (x as usize / 17 + y as usize / 23).is_multiple_of(2) { 6.0 } else { 0.0 }
                            + 2.0 * ((x + 3.0 * c as f32) * 0.9).sin();
                        let (u, v) = (uniform(), uniform());
                        signal + (-2.0 * u.ln()).sqrt() * (std::f32::consts::TAU * v).cos()
                    })
                    .collect()
            })
            .collect()
    }

    /// The filter on the GPU, or `None` (with a note) on a machine with no usable GPU.
    fn gpu(p: &Params, t: usize) -> Option<Bm3dGpu> {
        match Bm3dGpu::new(p, t) {
            Ok(g) => Some(g),
            Err(e) => {
                lightcraft_testkit::skip!(Gpu, "no GPU runner here ({e})");
                None
            }
        }
    }

    /// The largest and the mean difference between two sets of planes.
    fn differ(a: &[Vec<f32>], b: &[Vec<f32>]) -> (f32, f32) {
        let (mut worst, mut sum, mut n) = (0f32, 0f64, 0usize);
        for (x, y) in a.iter().flatten().zip(b.iter().flatten()) {
            worst = worst.max((x - y).abs());
            sum += f64::from((x - y).abs());
            n += 1;
        }
        (worst, (sum / n.max(1) as f64) as f32)
    }

    /// At four tile sizes the card's result is finite and within 0.1 noise deviations of the processor's at worst, and
    /// 1e-3 on average.
    #[test]
    fn the_gpu_gives_what_the_cpu_gives() {
        let p = Params::default();
        for (t, seed, noise) in [(32usize, 1u64, 0.003f32), (64, 2, 0.01), (100, 3, 0.04), (256, 4, 0.02)] {
            let Some(g) = gpu(&p, t) else { return };
            let planes = scene(t, seed);
            let want = bm3d::filter(&planes, t, &p, noise);
            let got = g.filter(&planes, t, noise).unwrap();
            let (worst, mean) = differ(&got, &want);
            eprintln!("{}: tile {t}, noise {noise}: worst {worst:e}, mean {mean:e} (noise deviation 1); {}", g.adapter(), g.build_report());
            assert!(got.iter().flatten().all(|v| v.is_finite()));
            assert!(mean < 1e-3 && worst < 0.1, "tile {t}: the GPU is off by {worst:e} at most and {mean:e} on average");
        }
    }

    /// `forget` must change `made` under the `free` lock: a waiting `take` holds it from reading `made` until it sleeps, so
    /// a slot given up in between cannot be announced to nobody (the waiter would sleep on a free slot for good).
    #[test]
    fn forgetting_a_workspace_waits_for_a_taker_that_is_about_to_sleep() {
        let Some(g) = gpu(&Params::default(), 32) else { return };
        let g = Arc::new(g);
        let before = *g.made.lock().unwrap();
        let held = g.free.lock().unwrap();
        let other = g.clone();
        let forgetter = std::thread::spawn(move || other.forget());
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(*g.made.lock().unwrap(), before, "forget changed the count while a taker held the free list");
        drop(held);
        forgetter.join().unwrap();
        assert_eq!(*g.made.lock().unwrap(), before.saturating_sub(1));
    }

    /// One runner shared by four threads, each filtering several tiles, agrees with the processor on every one.
    #[test]
    fn a_runner_is_reusable_and_shared_between_threads() {
        let p = Params::default();
        let t = 48;
        let Some(g) = gpu(&p, t) else { return };
        let g = Arc::new(g);
        let handles: Vec<_> = (0..4u64)
            .map(|k| {
                let g = g.clone();
                std::thread::spawn(move || {
                    for round in 0..3 {
                        let planes = scene(t, 10 + k * 7 + round);
                        let want = bm3d::filter(&planes, t, &Params::default(), 0.01);
                        let got = g.filter(&planes, t, 0.01).unwrap();
                        let (worst, mean) = differ(&got, &want);
                        assert!(mean < 1e-3 && worst < 0.1, "thread {k}, round {round}: off by {worst:e} / {mean:e}");
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }

    /// Block or group sizes the kernels are not written for, and tiles too small or too big, are errors; so are a tile
    /// or planes of the wrong size. NaN input does not panic, and a flat tile stays flat.
    #[test]
    fn what_the_kernels_do_not_do_is_an_error_and_odd_inputs_never_panic() {
        // not the block or group size the kernels are written for, or too big a tile
        assert!(Bm3dGpu::new(&Params { block: 6, ..Params::default() }, 64).is_err());
        assert!(Bm3dGpu::new(&Params { group: 32, ..Params::default() }, 64).is_err());
        assert!(Bm3dGpu::new(&Params::default(), 2).is_err());
        assert!(Bm3dGpu::new(&Params::default(), 4096).is_err());
        let Some(g) = gpu(&Params::default(), 32) else { return };
        // a tile of another size, planes of the wrong size, numbers that are not finite
        assert!(g.filter(&scene(40, 1), 40, 0.5).is_err());
        assert!(g.filter(&scene(32, 1).into_iter().take(3).collect::<Vec<_>>(), 32, 0.5).is_err());
        let nan = vec![vec![f32::NAN; 32 * 32]; 4];
        let _ = g.filter(&nan, 32, f32::NAN);
        let flat = vec![vec![5.0f32; 32 * 32]; 4];
        let out = g.filter(&flat, 32, 0.5).unwrap();
        assert!(out.iter().flatten().all(|v| (v - 5.0).abs() < 1e-3));
    }
}
