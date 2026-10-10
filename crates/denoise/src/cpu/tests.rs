use super::*;
use crate::{
    net::{Conv, ConvT2},
    reference, synthetic,
};

fn random(n: usize, seed: u32) -> Vec<f32> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s >> 8) as f32 / (1u32 << 23) as f32 - 1.0
        })
        .collect()
}
fn net(tile: usize, ch: u64, depth: usize, seed: u64) -> Net {
    let p = std::env::temp_dir().join(format!("lc-purecpu-{}-{tile}-{ch}-{depth}-{seed}.onnx", std::process::id()));
    std::fs::write(&p, synthetic::unet_onnx(tile as u64, ch, depth, seed)).unwrap();
    let n = crate::onnx::read(&p).unwrap();
    std::fs::remove_file(p).unwrap();
    n
}
fn agree(a: &[f32], b: &[f32]) {
    assert_eq!(a.len(), b.len());
    assert!(a.iter().chain(b).all(|v| v.is_finite()));
    let scale = b.iter().fold(1e-6f32, |m, v| m.max(v.abs()));
    let error = a.iter().zip(b).fold(0f32, |m, (a, b)| m.max((a - b).abs())) / scale;
    assert!(error <= 1e-3, "relative maximum error {error:e}");
}
#[test]
fn all_operators_match_scalar_on_random_nets_and_odd_and_tiny_shapes() {
    for (t, ch, d) in [(1, 3, 0), (3, 5, 0), (5, 7, 0), (6, 3, 1), (20, 5, 2), (36, 9, 2), (64, 8, 3)] {
        for seed in [1, 73] {
            let n = net(t, ch, d, seed);
            let r = NetRunner::new(&n, t).unwrap();
            let x = random(4 * t * t, seed as u32);
            let want = reference::run(&n, t, &x).unwrap();
            agree(&r.run(&x).unwrap(), &want);
            assert_eq!(r.run(&x).unwrap(), r.run(&x).unwrap());
        }
    }
}

#[test]
fn malformed_values_fail_before_execution_and_workspace_sizes_are_bounded() {
    let mut n = net(4, 3, 1, 11);
    for t in [0, 1, 3, usize::MAX, 4096] {
        assert!(NetRunner::new(&n, t).is_err());
    }
    for ch in [0, 3, usize::MAX] {
        n.in_channels = ch;
        assert!(NetRunner::new(&n, 4).is_err());
    }
    let n = Net::new(
        4,
        vec![
            Op::Conv(Conv { src: 0, src2: None, cin: 4, cout: 4096, k: 1, weight: vec![0.1; 4 * 4096], bias: vec![0.0; 4096], leaky: None }),
            Op::Conv(Conv { src: 1, src2: None, cin: 4096, cout: 12, k: 1, weight: vec![0.1; 4096 * 12], bias: vec![0.0; 12], leaky: None }),
            Op::DepthToSpace2 { src: 2 },
        ],
    )
    .unwrap();
    assert!(NetRunner::new(&n, 2048).is_err());
    let ops = (0..100).map(|i| Op::MaxPool2 { src: i }).collect();
    let n = Net::new(4, ops).unwrap();
    assert!(NetRunner::new(&n, 16).is_err());
    let c = Conv { src: 0, src2: None, cin: 4, cout: 12, k: 1, weight: vec![0.25; 48], bias: vec![0.0; 12], leaky: None };
    for mode in 0..6 {
        let mut bad = c.clone();
        match mode {
            0 => bad.src = usize::MAX,
            1 => bad.weight.pop().map(|_| ()).unwrap(),
            2 => bad.bias[0] = f32::NAN,
            3 => bad.leaky = Some(f32::INFINITY),
            4 => bad.cout = usize::MAX,
            _ => bad.k = usize::MAX,
        }
        assert!(Net::new(4, vec![Op::Conv(bad), Op::DepthToSpace2 { src: 1 }]).is_err());
    }
}
#[test]
fn bad_tiles_and_arithmetic_overflow_are_errors_and_do_not_poison_reuse() {
    let c = Conv { src: 0, src2: None, cin: 4, cout: 12, k: 1, weight: vec![f32::MAX; 48], bias: vec![0.0; 12], leaky: None };
    let n = Net::new(4, vec![Op::Conv(c), Op::DepthToSpace2 { src: 1 }]).unwrap();
    let r = NetRunner::new(&n, 1).unwrap();
    assert!(r.run(&[]).is_err());
    assert!(r.run(&[f32::NAN; 4]).is_err());
    assert!(r.run(&[f32::INFINITY; 4]).is_err());
    assert_eq!(r.idle.lock().unwrap().len(), 0, "invalid input never allocates a workspace");
    assert!(r.run(&[2.0; 4]).is_err());
    assert_eq!(r.run(&[0.0; 4]).unwrap(), vec![0.0; 12]);
    // Matrix shape checks are the boundary in front of the dependency's asserting constructors.
    assert!(multiply(&[], &[], &mut [], usize::MAX, 2, 2).is_err());
}
#[test]
fn shared_runner_serves_concurrent_tiles_without_cross_contamination() {
    let n = net(12, 5, 1, 97);
    let r = NetRunner::new(&n, 12).unwrap();
    let inputs: Vec<_> = (0..8).map(|s| random(4 * 12 * 12, s)).collect();
    let answers: Vec<_> = inputs.iter().map(|x| reference::run(&n, 12, x).unwrap()).collect();
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for (x, want) in inputs.iter().zip(&answers) {
            let r = &r;
            handles.push(scope.spawn(move || {
                for _ in 0..3 {
                    agree(&r.run(x).unwrap(), want);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
    });
}
#[test]
fn join_may_read_the_same_tensor_twice_and_leaky_up_convolution_is_supported() {
    let c = ConvT2 { src: 0, cin: 4, cout: 3, weight: random(48, 2), bias: vec![0.1, -0.1, 0.0], leaky: Some(0.13) };
    let n = Net::new(
        4,
        vec![
            Op::ConvT2(c),
            Op::Conv(Conv { src: 1, src2: Some(1), cin: 6, cout: 3, k: 3, weight: random(162, 3), bias: vec![0.0; 3], leaky: Some(0.0) }),
        ],
    )
    .unwrap();
    let x = random(4 * 3 * 3, 7);
    agree(&NetRunner::new(&n, 3).unwrap().run(&x).unwrap(), &reference::run(&n, 3, &x).unwrap());
}

#[test]
fn reused_im2col_clears_all_edge_padding_between_images() {
    for t in [1, 3, 5, 36] {
        let n = net(t, 5, 0, 121);
        let r = NetRunner::new(&n, t).unwrap();
        for x in [vec![1.0; 4 * t * t], vec![0.0; 4 * t * t], random(4 * t * t, 9)] {
            agree(&r.run(&x).unwrap(), &reference::run(&n, t, &x).unwrap());
        }
    }
}
#[test]
#[ignore = "needs LC_DENOISE_MODEL; one tile with {ENV_PREFIX}_PROFILE=1"]
fn profile_one_real_tile() {
    let model = std::env::var_os("LC_DENOISE_MODEL").expect("set LC_DENOISE_MODEL");
    let m = crate::known::find("rawnind-bayer").unwrap().manifest;
    let r = crate::runtime::CpuRunner::load(std::path::Path::new(&model), &m).unwrap();
    println!("workspace {:.1} MB", r.bytes_per_tile() as f64 / 1e6);
    r.run(&crate::runtime::check_tile(512)).unwrap();
}
