// Packed RGBA8 -> four 256-bin histograms, sampled total, unwritten and visited counts.
// Only workgroup atomics are used. Each group writes its own partial; a second dispatch
// sums those partials without requiring optional GPU features or global atomic contention.
// P: width, pixel count, crop x/y/w/h, histogram sampling step, partial group count.
const SUMMARY_WORDS: u32 = 1027u;
var<workgroup> bins: array<atomic<u32>, 1024>;
var<workgroup> sampled: atomic<u32>;
var<workgroup> unwritten: atomic<u32>;
var<workgroup> visited: atomic<u32>;

@compute @workgroup_size(256)
fn summary_partial(@builtin(local_invocation_index) lane: u32, @builtin(workgroup_id) group: vec3<u32>) {
    // Count all pixels for validation; sample crop-anchored histogram bins within each group.
    for (var c = 0u; c < 4u; c++) {
        atomicStore(&bins[c * 256u + lane], 0u);
    }
    if (lane == 0u) {
        atomicStore(&sampled, 0u);
        atomicStore(&unwritten, 0u);
        atomicStore(&visited, 0u);
    }
    workgroupBarrier();
    var seen = 0u;
    var missing = 0u;
    var samples = 0u;
    for (var i = group.x * 256u + lane; i < pu(1u); i += pu(7u) * 256u) {
        let pixel = pixels[i];
        seen++;
        missing += select(0u, 1u, (pixel >> 24u) != 255u);
        let x = i % pu(0u);
        let y = i / pu(0u);
        // Sampling starts at the kept window's origin, exactly as Histogram::of_srgb8(crop).
        if (x >= pu(2u) && y >= pu(3u) && x - pu(2u) < pu(4u) && y - pu(3u) < pu(5u)) {
            if ((x - pu(2u)) % pu(6u) == 0u && (y - pu(3u)) % pu(6u) == 0u) {
                let r = pixel & 255u;
                let g = (pixel >> 8u) & 255u;
                let b = (pixel >> 16u) & 255u;
                let l = (r * 54u + g * 183u + b * 19u) >> 8u;
                atomicAdd(&bins[r], 1u);
                atomicAdd(&bins[256u + g], 1u);
                atomicAdd(&bins[512u + b], 1u);
                atomicAdd(&bins[768u + l], 1u);
                samples++;
            }
        }
    }
    atomicAdd(&sampled, samples);
    atomicAdd(&unwritten, missing);
    atomicAdd(&visited, seen);
    workgroupBarrier();
    let base = group.x * SUMMARY_WORDS;
    for (var c = 0u; c < 4u; c++) {
        partials[base + c * 256u + lane] = atomicLoad(&bins[c * 256u + lane]);
    }
    if (lane == 0u) {
        partials[base + 1024u] = atomicLoad(&sampled);
        partials[base + 1025u] = atomicLoad(&unwritten);
        partials[base + 1026u] = atomicLoad(&visited);
    }
}

@compute @workgroup_size(256)
fn summary_reduce(@builtin(global_invocation_id) g: vec3<u32>) {
    // Sum each bin/counter across the bounded partials into the compact readback buffer.
    let bin = g.x;
    if (bin >= SUMMARY_WORDS) {
        return;
    }
    var sum = 0u;
    for (var group = 0u; group < pu(7u); group++) {
        sum += partials[group * SUMMARY_WORDS + bin];
    }
    summary[bin] = sum;
}
