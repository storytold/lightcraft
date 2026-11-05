// Block-matching collaborative filtering of one tile: the shared part of the three kernels. The reference is
// crates/denoise-core/src/bm3d.rs (the same steps in the same order, so the two agree to rounding): 4 x 4-cell blocks,
// groups of at most 16, planes of `t x t` cells with 4 planes one after the other.

struct P {
    t: u32,        // cells on a side of a plane
    nr: u32,       // blocks that start a group, along a side
    nb: u32,       // blocks along a side (t - 3)
    search: u32,   // how far a match is looked for, in cells
    cap: u32,      // most blocks in a group (at most 16)
    wiener: u32,   // 0: hard threshold, 1: Wiener shrinkage by the pilot
    pad0: u32,
    pad1: u32,
    tau: f32,      // how far above the noise two blocks may differ (mean square) and be grouped
    lambda: f32,   // hard threshold (already scaled by the noise's deviation)
    sigma: f32,    // the noise's deviation
    pad2: f32,
}

@group(0) @binding(0) var<uniform> p: P;
// [0, 16): the DCT matrix, [16, 32): the window (both f32 bits), [32, ..): where the blocks that start a group are
@group(0) @binding(1) var<storage, read> consts: array<u32>;

const N: u32 = 4u;
const NN: u32 = 16u;
const K: u32 = 16u;
// a group's entry in `members`: its size, then its blocks' top-left cells
const STRIDE: u32 = 17u;
const INV_SQRT2: f32 = 0.70710677;

fn axis(i: u32) -> u32 {
    return consts[32u + i];
}

fn dct(i: u32) -> f32 {
    return bitcast<f32>(consts[i]);
}

fn win(i: u32) -> f32 {
    return bitcast<f32>(consts[16u + i]);
}
