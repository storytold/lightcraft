use super::*;
use crate::graph::{Attr, Weight};

fn node(op: &str, inputs: &[&str], attrs: &[(&str, Attr)]) -> Node {
    Node {
        op: op.into(),
        inputs: inputs.iter().map(|s| s.to_string()).collect(),
        outputs: vec!["y".into()],
        attrs: attrs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
    }
}
fn graph(nodes: Vec<Node>, weights: Vec<(&str, Weight)>) -> Graph {
    Graph {
        nodes,
        weights: weights.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        inputs: vec![("x".into(), vec![1, 1, 3, 3])],
        outputs: vec!["y".into()],
    }
}
fn w(dims: &[usize], v: Vec<f32>) -> Weight {
    Weight { dims: dims.to_vec(), data: Data::F32(v) }
}
fn input(c: usize, h: usize, wd: usize, f: impl Fn(usize) -> f32) -> Tensor {
    Tensor::new(vec![1, c, h, wd], (0..c * h * wd).map(f).collect()).unwrap()
}
fn run(g: Graph, x: Tensor) -> Result<Tensor> {
    Ok(Net::new(g)?.run(x)?.remove(0).1)
}

/// A direct, slow reference convolution to check the fast one against.
fn reference_conv(x: &Tensor, wd: &[f32], dims: [usize; 4], bias: &[f32], s: usize, p: usize, groups: usize) -> Tensor {
    let (c, h, wi) = (x.shape[1], x.shape[2], x.shape[3]);
    let [co_n, cin_g, kh, kw] = dims;
    let (oh, ow) = ((h + 2 * p - kh) / s + 1, (wi + 2 * p - kw) / s + 1);
    let mut out = vec![0.0f32; co_n * oh * ow];
    for co in 0..co_n {
        let g = co / (co_n / groups);
        for oy in 0..oh {
            for ox in 0..ow {
                let mut acc = bias[co];
                for cl in 0..cin_g {
                    for ky in 0..kh {
                        for kx in 0..kw {
                            let (iy, ix) = ((oy * s + ky) as isize - p as isize, (ox * s + kx) as isize - p as isize);
                            if iy >= 0 && ix >= 0 && (iy as usize) < h && (ix as usize) < wi {
                                acc += x.data[((g * cin_g + cl) * h + iy as usize) * wi + ix as usize] * wd[((co * cin_g + cl) * kh + ky) * kw + kx];
                            }
                        }
                    }
                }
                out[(co * oh + oy) * ow + ox] = acc;
            }
        }
    }
    let _ = c;
    Tensor::new(vec![1, co_n, oh, ow], out).unwrap()
}

#[test]
fn convolutions_match_a_slow_reference() {
    // (cin, cout, groups, kernel, stride, pad, h, w)
    for (cin, cout, groups, k, s, p, h, wi) in [
        (3, 4, 1, 3, 1, 1, 7, 9),
        (3, 4, 1, 3, 2, 1, 8, 8),
        (4, 4, 4, 3, 1, 1, 6, 5),
        (2, 6, 1, 1, 1, 0, 5, 5),
        (4, 8, 2, 3, 1, 1, 5, 6),
        (1, 1, 1, 3, 1, 0, 3, 3),
    ] {
        let cin_g = cin / groups;
        let wd: Vec<f32> = (0..cout * cin_g * k * k).map(|i| ((i * 37 % 11) as f32 - 5.0) * 0.1).collect();
        let bias: Vec<f32> = (0..cout).map(|i| i as f32 * 0.25 - 0.5).collect();
        let x = input(cin, h, wi, |i| ((i * 13 % 17) as f32 - 8.0) * 0.2);
        let attrs = [("group", Attr::Int(groups as i64)), ("strides", Attr::Ints(vec![s as i64; 2])), ("pads", Attr::Ints(vec![p as i64; 4]))];
        let g =
            graph(vec![node("Conv", &["x", "w", "b"], &attrs)], vec![("w", w(&[cout, cin_g, k, k], wd.clone())), ("b", w(&[cout], bias.clone()))]);
        let got = run(g, x.clone()).unwrap();
        let want = reference_conv(&x, &wd, [cout, cin_g, k, k], &bias, s, p, groups);
        assert_eq!(got.shape, want.shape, "{cin} {cout} {groups} {k} {s} {p}");
        for (a, b) in got.data.iter().zip(&want.data) {
            assert!((a - b).abs() < 1e-4, "{cin} {cout} {groups} {k} {s} {p}: {a} vs {b}");
        }
    }
}

#[test]
fn the_small_operators() {
    let x = input(1, 4, 4, |i| i as f32 - 6.0);
    // Relu, Sigmoid
    let r = run(graph(vec![node("Relu", &["x"], &[])], vec![]), x.clone()).unwrap();
    assert!(r.data.iter().all(|v| *v >= 0.0) && r.data.get(7) == Some(&1.0));
    let s = run(graph(vec![node("Sigmoid", &["x"], &[])], vec![]), x.clone()).unwrap();
    assert!((s.data[6] - 0.5).abs() < 1e-6 && s.data.iter().all(|v| (0.0..=1.0).contains(v)));
    // MaxPool 2x2/2
    let m = run(
        graph(vec![node("MaxPool", &["x"], &[("kernel_shape", Attr::Ints(vec![2, 2])), ("strides", Attr::Ints(vec![2, 2]))])], vec![]),
        x.clone(),
    )
    .unwrap();
    assert_eq!((m.shape.clone(), m.data.clone()), (vec![1, 1, 2, 2], vec![-1.0, 1.0, 7.0, 9.0]));
    // Resize x2 nearest
    let z =
        run(graph(vec![node("Resize", &["x", "", "s"], &[])], vec![("s", w(&[4], vec![1.0, 1.0, 2.0, 2.0]))]), input(1, 2, 2, |i| i as f32)).unwrap();
    assert_eq!(z.shape, vec![1, 1, 4, 4]);
    assert_eq!(&z.data[..8], &[0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0]);
    // Add
    let a = run(graph(vec![node("Add", &["x", "x"], &[])], vec![]), x.clone()).unwrap();
    assert_eq!(a.data[5], 2.0 * x.data[5]);
    // Transpose [0,2,3,1] then Reshape [1,-1,2]
    let t = run(graph(vec![node("Transpose", &["x"], &[("perm", Attr::Ints(vec![0, 2, 3, 1]))])], vec![]), input(2, 2, 3, |i| i as f32)).unwrap();
    assert_eq!(t.shape, vec![1, 2, 3, 2]);
    assert_eq!(t.data, vec![0.0, 6.0, 1.0, 7.0, 2.0, 8.0, 3.0, 9.0, 4.0, 10.0, 5.0, 11.0]);
    let rs = run(
        graph(vec![node("Reshape", &["x", "s"], &[])], vec![("s", Weight { dims: vec![3], data: Data::I64(vec![1, -1, 2]) })]),
        input(2, 2, 3, |i| i as f32),
    )
    .unwrap();
    assert_eq!(rs.shape, vec![1, 6, 2]);
}

/// An accelerator built on the slow reference convolution, optionally wrong or failing.
struct Fake {
    skew: f32,
    fail_after: std::sync::atomic::AtomicUsize,
}
impl Accelerator for Fake {
    fn device(&self) -> String {
        "test".into()
    }
    fn conv(&self, j: &ConvJob) -> std::result::Result<Option<Vec<f32>>, String> {
        if self.fail_after.fetch_sub(1, std::sync::atomic::Ordering::Relaxed) == 0 {
            return Err("device lost".into());
        }
        assert!(j.pt == j.pl && j.sh == j.sw);
        let x = Tensor::new(vec![1, j.cin, j.h, j.w], j.input.to_vec()).unwrap();
        let zero = vec![0.0; j.cout];
        let dims = [j.cout, j.cin / j.groups, j.kh, j.kw];
        let out = reference_conv(&x, j.weights, dims, j.bias.unwrap_or(&zero), j.sh, j.pt, j.groups);
        Ok(Some(out.data.iter().map(|v| v + self.skew).collect()))
    }
}

/// Wraps [`Fake`]: reports whether the net had published an accelerator at each convolution (what a scan running at
/// that moment would find), and can make the user choose the CPU (`Net::detach`) at the first one.
struct Spy {
    inner: Fake,
    net: std::sync::OnceLock<std::sync::Weak<Net>>,
    published_during: std::sync::Mutex<Vec<bool>>,
    detach_at_first: bool,
}
impl Accelerator for Spy {
    fn device(&self) -> String {
        "spy".into()
    }
    fn conv(&self, j: &ConvJob) -> std::result::Result<Option<Vec<f32>>, String> {
        let net = self.net.get().and_then(std::sync::Weak::upgrade).unwrap();
        let mut seen = self.published_during.lock().unwrap();
        if self.detach_at_first && seen.is_empty() {
            net.detach();
        }
        seen.push(net.accelerator().is_some());
        drop(seen);
        self.inner.conv(j)
    }
}

fn new_spy(net: &Arc<Net>, detach_at_first: bool) -> Arc<Spy> {
    let spy = Arc::new(Spy {
        inner: Fake { skew: 0.0, fail_after: usize::MAX.into() },
        net: std::sync::OnceLock::new(),
        published_during: std::sync::Mutex::new(Vec::new()),
        detach_at_first,
    });
    spy.net.set(Arc::downgrade(net)).unwrap();
    spy
}

/// A graph that takes any input size, and a portrait and a landscape input for it.
fn dynamic_two_convs() -> (Graph, Tensor, Tensor) {
    let (mut g, _) = two_convs();
    g.inputs = vec![("x".into(), vec![1, 3, 0, 0])];
    let make = |h: usize, w: usize| input(3, h, w, |i| ((i * 13 % 17) as f32 - 8.0) * 0.1);
    (g, make(8, 9), make(9, 8))
}

#[test]
fn the_accelerator_is_not_published_until_every_probe_agreed() {
    let (g, landscape, portrait) = dynamic_two_convs();
    let net = Arc::new(Net::new(g).unwrap());
    let spy = new_spy(&net, false);
    net.attach_checked(spy.clone(), &[landscape, portrait], 1e-4, false).unwrap();
    let seen = spy.published_during.lock().unwrap().clone();
    assert!(seen.len() >= 4, "the device ran both probes: {seen:?}");
    assert!(seen.iter().all(|published| !published), "a scan could use the device while it was still being checked: {seen:?}");
    assert!(net.accelerator().is_some(), "published once every probe agreed");
}

#[test]
fn a_device_wrong_at_a_later_probe_is_never_published() {
    let (g, landscape, portrait) = dynamic_two_convs();
    let net = Net::new(g).unwrap();
    // right on the first probe, wrong where the input is taller than wide
    struct PortraitWrong(Fake);
    impl Accelerator for PortraitWrong {
        fn device(&self) -> String {
            "portrait-wrong".into()
        }
        fn conv(&self, j: &ConvJob) -> std::result::Result<Option<Vec<f32>>, String> {
            self.0.conv(j).map(|o| o.map(|v| if j.h > j.w { v.iter().map(|x| x + 0.5).collect() } else { v }))
        }
    }
    let device = Arc::new(PortraitWrong(Fake { skew: 0.0, fail_after: usize::MAX.into() }));
    assert!(net.attach_checked(device.clone(), &[landscape.clone(), portrait.clone()], 1e-4, false).is_err());
    assert!(net.accelerator().is_none());
    assert!(net.attach_checked(device, &[landscape], 1e-4, false).is_ok(), "the first probe alone cannot see it");
}

#[test]
fn choosing_the_cpu_while_the_device_is_checked_wins_over_a_passing_check() {
    let (g, landscape, portrait) = dynamic_two_convs();
    let net = Arc::new(Net::new(g).unwrap());
    let cpu = net.run(portrait.clone()).unwrap();
    // the user switches to the CPU only during the first convolution of the check; the device agrees everywhere
    let spy = new_spy(&net, true);
    let why = net.attach_checked(spy.clone(), &[landscape, portrait.clone()], 1e-4, false).unwrap_err();
    assert!(why.contains("CPU was chosen"), "{why}");
    assert!(net.accelerator().is_none(), "the passing check re-attached the device");
    assert_eq!(net.run(portrait).unwrap(), cpu);
    // a later check, started after the switch, is a new decision
    net.attach_checked(new_spy(&net, false), &[input(3, 8, 9, |i| i as f32 * 0.01)], 1e-4, false).unwrap();
    assert!(net.accelerator().is_some());
}

fn two_convs() -> (Graph, Tensor) {
    let attrs = [("strides", Attr::Ints(vec![2, 2])), ("pads", Attr::Ints(vec![1; 4]))];
    let mut a = node("Conv", &["x", "w1", "b1"], &attrs);
    a.outputs = vec!["h".into()];
    let b = node("Conv", &["h", "w2"], &[("group", Attr::Int(2)), ("pads", Attr::Ints(vec![1; 4]))]);
    let mut g = graph(
        vec![a, b],
        vec![
            ("w1", w(&[4, 3, 3, 3], (0..108).map(|i| ((i * 7 % 13) as f32 - 6.0) * 0.05).collect())),
            ("b1", w(&[4], vec![0.1, -0.2, 0.3, 0.0])),
            ("w2", w(&[4, 2, 3, 3], (0..72).map(|i| ((i * 5 % 11) as f32 - 5.0) * 0.05).collect())),
        ],
    );
    g.inputs = vec![("x".into(), vec![1, 3, 9, 8])];
    (g, input(3, 9, 8, |i| ((i * 13 % 17) as f32 - 8.0) * 0.1))
}

#[test]
fn an_accelerator_is_used_only_when_it_agrees_with_the_cpu() {
    let (g, x) = two_convs();
    let net = Net::new(g).unwrap();
    let cpu = net.run(x.clone()).unwrap();
    let good = Arc::new(Fake { skew: 0.0, fail_after: usize::MAX.into() });
    net.attach_checked(good, std::slice::from_ref(&x), 1e-4, false).unwrap();
    assert!(net.accelerator().is_some());
    assert_eq!(net.run(x.clone()).unwrap()[0].1.shape, cpu[0].1.shape);
    let wrong = Arc::new(Fake { skew: 0.5, fail_after: usize::MAX.into() });
    assert!(net.attach_checked(wrong, std::slice::from_ref(&x), 1e-4, false).is_err());
    assert!(net.accelerator().is_none(), "a device that disagrees is not used");
    assert_eq!(net.run(x).unwrap(), cpu);
}

#[test]
fn a_failing_accelerator_falls_back_to_the_cpu_mid_run() {
    let (g, x) = two_convs();
    let net = Net::new(g).unwrap();
    let cpu = net.run(x.clone()).unwrap();
    // agrees on the probe (two convs), then fails on the first conv of the next run, so the second is left to the CPU
    // (`a_device_that_failed_is_not_asked_again_in_the_run` counts the calls it must not get)
    net.attach_checked(Arc::new(Fake { skew: 0.0, fail_after: 2.into() }), std::slice::from_ref(&x), 1e-4, false).unwrap();
    let out = net.run(x).unwrap();
    assert!(net.accelerator().is_none());
    for (a, b) in out[0].1.data.iter().zip(&cpu[0].1.data) {
        assert!((a - b).abs() < 1e-4);
    }
}

/// A device that works until told to misbehave (`fail_from_now`), then fails every convolution it is given (or, with
/// `nan`, answers with values that are not finite), counting the calls made after the first bad one.
struct Counting {
    calls: std::sync::atomic::AtomicUsize,
    bad_from: std::sync::atomic::AtomicUsize,
    after_failure: std::sync::atomic::AtomicUsize,
    nan: bool,
}
impl Counting {
    fn new(nan: bool) -> Arc<Counting> {
        Arc::new(Counting { calls: 0.into(), bad_from: usize::MAX.into(), after_failure: 0.into(), nan })
    }
    fn fail_from_now(&self) {
        self.bad_from.store(self.calls.load(std::sync::atomic::Ordering::SeqCst), std::sync::atomic::Ordering::SeqCst);
    }
    fn calls_after_the_first_bad_one(&self) -> usize {
        self.after_failure.load(std::sync::atomic::Ordering::SeqCst)
    }
}
impl Accelerator for Counting {
    fn device(&self) -> String {
        "counting".into()
    }
    fn conv(&self, j: &ConvJob) -> std::result::Result<Option<Vec<f32>>, String> {
        use std::sync::atomic::Ordering::SeqCst;
        let n = self.calls.fetch_add(1, SeqCst);
        let bad_from = self.bad_from.load(SeqCst);
        if n > bad_from {
            self.after_failure.fetch_add(1, SeqCst);
        }
        if n >= bad_from {
            return if self.nan { Ok(Some(vec![f32::NAN; j.cout * j.oh * j.ow])) } else { Err("device lost".into()) };
        }
        let x = Tensor::new(vec![1, j.cin, j.h, j.w], j.input.to_vec()).unwrap();
        let zero = vec![0.0; j.cout];
        let dims = [j.cout, j.cin / j.groups, j.kh, j.kw];
        Ok(Some(reference_conv(&x, j.weights, dims, j.bias.unwrap_or(&zero), j.sh, j.pt, j.groups).data))
    }
}

fn two_convs_inputs(n: usize) -> Vec<Tensor> {
    (0..n).map(|s| input(3, 9, 8, |i| (((i * 13 + s * 5) % 17) as f32 - 8.0) * 0.1)).collect()
}

#[test]
fn a_device_that_failed_is_not_asked_again_in_the_run() {
    let (g, x) = two_convs();
    let net = Net::new(g).unwrap();
    let cpu = net.run(x.clone()).unwrap();
    let device = Counting::new(false);
    net.attach_checked(device.clone(), std::slice::from_ref(&x), 1e-4, false).unwrap();
    // it fails on the first of the two convolutions: the second must not go to it
    device.fail_from_now();
    let out = net.run(x).unwrap();
    assert_eq!(device.calls_after_the_first_bad_one(), 0);
    assert!(net.accelerator().is_none());
    agree(&cpu, &out, 1e-4).unwrap();
}

#[test]
fn a_device_that_failed_on_the_first_input_is_not_asked_for_the_rest_of_the_batch() {
    let (g, x) = two_convs();
    let net = Net::new(g).unwrap();
    let inputs = two_convs_inputs(3);
    let cpu: Vec<_> = inputs.iter().map(|t| net.run(t.clone()).unwrap()).collect();
    let device = Counting::new(false);
    net.attach_checked(device.clone(), std::slice::from_ref(&x), 1e-4, false).unwrap();
    device.fail_from_now();
    let out = net.run_batch(inputs).unwrap();
    assert_eq!(device.calls_after_the_first_bad_one(), 0);
    for (want, got) in cpu.iter().zip(&out) {
        agree(want, got, 1e-4).unwrap();
    }
}

#[test]
fn a_failing_device_is_taken_off_the_net_without_counting_as_a_cpu_choice() {
    let (g, x) = two_convs();
    let net = Net::new(g).unwrap();
    let device = Counting::new(false);
    net.attach_checked(device.clone(), std::slice::from_ref(&x), 1e-4, false).unwrap();
    device.fail_from_now();
    net.run_batch(two_convs_inputs(2)).unwrap();
    assert!(net.accelerator().is_none(), "the failed device stays published");
    assert_eq!(net.detaches.load(std::sync::atomic::Ordering::SeqCst), 0, "a device failure is not the user choosing the CPU");
}

fn good_fake() -> Arc<Fake> {
    Arc::new(Fake { skew: 0.0, fail_after: usize::MAX.into() })
}

/// A scan keeps the device it started with. When that device fails after a re-check has published another one, only
/// the scan's own device goes: the new one stays.
#[test]
fn an_old_scans_failing_device_leaves_a_newer_device_published() {
    let (g, x) = two_convs();
    let net = Net::new(g).unwrap();
    net.attach_checked(good_fake(), std::slice::from_ref(&x), 1e-4, false).unwrap();
    let old_scan = Lane::new(net.accelerator(), true);
    let newer: Arc<dyn Accelerator> = good_fake();
    net.attach_checked(newer.clone(), std::slice::from_ref(&x), 1e-4, false).unwrap();
    old_scan.fail(&net);
    assert!(net.accelerator().is_some_and(|now| Arc::ptr_eq(&now, &newer)), "the old scan's failure dropped the newer device");
}

/// The same scan failing while the re-check runs does not refuse the device being checked, as the user choosing the
/// CPU would.
#[test]
fn an_old_scans_failing_device_during_a_check_does_not_refuse_the_checked_device() {
    let (g, x) = two_convs();
    let net = Arc::new(Net::new(g).unwrap());
    net.attach_checked(good_fake(), std::slice::from_ref(&x), 1e-4, false).unwrap();
    let old_scan = Arc::new(Lane::new(net.accelerator(), true));
    // a device that agrees everywhere; at its first convolution the old scan's device fails
    struct FailsOldScanFirst {
        inner: Fake,
        old: std::sync::Mutex<Option<(Arc<Net>, Arc<Lane>)>>,
    }
    impl Accelerator for FailsOldScanFirst {
        fn device(&self) -> String {
            "fails-old-scan".into()
        }
        fn conv(&self, j: &ConvJob) -> std::result::Result<Option<Vec<f32>>, String> {
            if let Some((net, lane)) = self.old.lock().unwrap().take() {
                lane.fail(&net);
            }
            self.inner.conv(j)
        }
    }
    let checked: Arc<dyn Accelerator> = Arc::new(FailsOldScanFirst {
        inner: Fake { skew: 0.0, fail_after: usize::MAX.into() },
        old: std::sync::Mutex::new(Some((net.clone(), old_scan.clone()))),
    });
    net.attach_checked(checked.clone(), std::slice::from_ref(&x), 1e-4, false).unwrap();
    assert!(old_scan.has_failed());
    assert!(net.accelerator().is_some_and(|now| Arc::ptr_eq(&now, &checked)), "the checked device was refused");
}

#[test]
fn a_device_that_answers_with_values_that_are_not_finite_mid_run_gives_the_cpu_result() {
    let (g, x) = two_convs();
    let net = Net::new(g).unwrap();
    let cpu = net.run(x.clone()).unwrap();
    let device = Counting::new(true);
    net.attach_checked(device.clone(), std::slice::from_ref(&x), 1e-4, false).unwrap();
    device.fail_from_now();
    let out = net.run(x).unwrap();
    assert_eq!(device.calls_after_the_first_bad_one(), 0);
    assert!(net.accelerator().is_none());
    agree(&cpu, &out, 1e-4).unwrap();
}

/// Runs whole programs correctly until `poison` is set, then answers with a value that is not finite; any single
/// convolution it is given is answered with values that are not finite too, and counted.
struct LateNaN {
    poison: std::sync::atomic::AtomicBool,
    conv_calls: std::sync::atomic::AtomicUsize,
}
impl Accelerator for LateNaN {
    fn device(&self) -> String {
        "late nan".into()
    }
    fn conv(&self, j: &ConvJob) -> std::result::Result<Option<Vec<f32>>, String> {
        self.conv_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Some(vec![f32::NAN; j.cout * j.oh * j.ow]))
    }
    fn run_program(&self, p: &program::Program, inputs: &[&[f32]]) -> std::result::Result<Option<Vec<Vec<Vec<f32>>>>, String> {
        let mut out: Vec<Vec<Vec<f32>>> = inputs.iter().map(|x| run_steps(p, x)).collect();
        if self.poison.load(std::sync::atomic::Ordering::SeqCst) {
            out[0][0][0] = f32::NAN;
        }
        Ok(Some(out))
    }
}

#[test]
fn a_program_answer_that_is_not_finite_falls_back_to_the_cpu_without_asking_the_device_again() {
    let net = Net::new(crate::synthetic::every_operator_graph()).unwrap();
    let x = every_operator_input(2);
    let cpu = net.run(x.clone()).unwrap();
    let device = Arc::new(LateNaN { poison: false.into(), conv_calls: 0.into() });
    net.attach_checked(device.clone(), std::slice::from_ref(&x), 1e-5, false).unwrap();
    device.poison.store(true, std::sync::atomic::Ordering::SeqCst);
    let out = net.run(x).unwrap();
    assert_eq!(device.conv_calls.load(std::sync::atomic::Ordering::SeqCst), 0, "the convolutions of the CPU fallback went to the device");
    assert!(net.accelerator().is_none());
    assert_eq!(out, cpu);
}

/// An accelerator that runs whole programs with plain loops (the definition of each step), counting its calls;
/// `poison` makes it give a value that is not finite.
struct ProgramFake {
    calls: std::sync::atomic::AtomicUsize,
    poison: bool,
}

fn run_steps(p: &program::Program, input: &[f32]) -> Vec<Vec<f32>> {
    use program::{Arg, Binary, Op, Unary};
    let mut values: Vec<Vec<f32>> = vec![Vec::new(); p.lens.len()];
    values[program::INPUT] = input.to_vec();
    for step in &p.steps {
        let arg = |i: usize| -> Vec<f32> {
            match &step.args[i] {
                Arg::Value(v) => values[*v].clone(),
                Arg::Const { data, .. } => data.to_vec(),
            }
        };
        let len = p.lens[step.out];
        let x = arg(0);
        let out: Vec<f32> = match &step.op {
            Op::Conv(j) => {
                let (m, cg) = (j.cout / j.groups, j.cin / j.groups);
                let mut out = vec![0f32; len];
                for co in 0..j.cout {
                    for oy in 0..j.oh {
                        for ox in 0..j.ow {
                            let mut v = j.bias.map_or(0.0, |b| b[co]);
                            for ci in 0..cg {
                                for ky in 0..j.kh {
                                    for kx in 0..j.kw {
                                        let (iy, ix) = ((oy * j.sh + ky) as isize - j.pt as isize, (ox * j.sw + kx) as isize - j.pl as isize);
                                        if iy >= 0 && ix >= 0 && (iy as usize) < j.h && (ix as usize) < j.w {
                                            v += j.weights[((co * cg + ci) * j.kh + ky) * j.kw + kx]
                                                * x[((co / m * cg + ci) * j.h + iy as usize) * j.w + ix as usize];
                                        }
                                    }
                                }
                            }
                            out[(co * j.oh + oy) * j.ow + ox] = v;
                        }
                    }
                }
                out
            }
            Op::Unary(Unary::Relu) => x.iter().map(|v| v.max(0.0)).collect(),
            Op::Unary(Unary::Sigmoid) => x.iter().map(|v| 1.0 / (1.0 + (-v).exp())).collect(),
            Op::ChannelAffine { channels, plane } => {
                let (s, o) = (arg(1), arg(2));
                x.iter().enumerate().map(|(i, v)| v * s[(i / plane) % channels] + o[(i / plane) % channels]).collect()
            }
            Op::ChannelPrelu { channels, plane } => {
                let a = arg(1);
                x.iter().enumerate().map(|(i, v)| if *v < 0.0 { v * a[(i / plane) % channels] } else { *v }).collect()
            }
            Op::Binary { op, dims, a, b } => {
                let y = arg(1);
                (0..len)
                    .map(|i| {
                        let (mut rest, mut ia, mut ib) = (i, 0, 0);
                        for d in (0..4).rev() {
                            let at = rest % dims[d];
                            rest /= dims[d];
                            ia += at * a[d];
                            ib += at * b[d];
                        }
                        match op {
                            Binary::Add => x[ia] + y[ib],
                            Binary::Sub => x[ia] - y[ib],
                            Binary::Mul => x[ia] * y[ib],
                        }
                    })
                    .collect()
            }
            Op::Transpose { dims, strides } => (0..len)
                .map(|i| {
                    let (mut rest, mut from) = (i, 0);
                    for d in (0..4).rev() {
                        from += (rest % dims[d]) * strides[d];
                        rest /= dims[d];
                    }
                    x[from]
                })
                .collect(),
            Op::MaxPool { h, w, kh, kw, sh, sw, oh, ow, .. } => (0..len)
                .map(|i| {
                    let (ox, oy, c) = (i % ow, (i / ow) % oh, i / (ow * oh));
                    let mut m = f32::NEG_INFINITY;
                    for ky in 0..*kh {
                        for kx in 0..*kw {
                            m = m.max(x[(c * h + oy * sh + ky) * w + ox * sw + kx]);
                        }
                    }
                    m
                })
                .collect(),
            Op::Resize { h, w, sh, sw, .. } => (0..len)
                .map(|i| {
                    let (ow, oh) = (w * sw, h * sh);
                    let (ox, oy, c) = (i % ow, (i / ow) % oh, i / (ow * oh));
                    x[(c * h + oy / sh) * w + ox / sw]
                })
                .collect(),
            Op::GlobalAverage { plane, .. } => x.chunks(*plane).map(|c| c.iter().sum::<f32>() / *plane as f32).collect(),
            Op::Gemv { k, n, trans_b, alpha } => {
                let (wt, bias) = (arg(1), arg(2));
                (0..*n).map(|o| alpha * (0..*k).map(|j| x[j] * wt[if *trans_b { o * k + j } else { j * n + o }]).sum::<f32>() + bias[o]).collect()
            }
        };
        assert_eq!(out.len(), len, "{:?}", step.op);
        values[step.out] = out;
        for f in &step.free {
            values[*f] = Vec::new();
        }
    }
    p.outputs.iter().map(|(_, v, _)| values[*v].clone()).collect()
}

impl Accelerator for ProgramFake {
    fn device(&self) -> String {
        "program test".into()
    }
    fn conv(&self, _: &ConvJob) -> std::result::Result<Option<Vec<f32>>, String> {
        Ok(None)
    }
    fn run_program(&self, p: &program::Program, inputs: &[&[f32]]) -> std::result::Result<Option<Vec<Vec<Vec<f32>>>>, String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut out: Vec<Vec<Vec<f32>>> = inputs.iter().map(|x| run_steps(p, x)).collect();
        if self.poison {
            out[0][0][0] = f32::NAN;
        }
        Ok(Some(out))
    }
}

fn every_operator_input(seed: usize) -> Tensor {
    input(4, 8, 8, |i| (((i * 31 + seed * 7) % 37) as f32 - 18.0) * 0.05)
}

#[test]
fn a_program_of_every_operator_gives_what_the_interpreter_gives() {
    let net = Net::new(crate::synthetic::every_operator_graph()).unwrap();
    let x = every_operator_input(0);
    let cpu = net.run(x.clone()).unwrap();
    let fake = Arc::new(ProgramFake { calls: 0.into(), poison: false });
    net.attach_checked(fake.clone(), std::slice::from_ref(&x), 1e-5, false).unwrap();
    let calls = fake.calls.load(std::sync::atomic::Ordering::Relaxed);
    assert!(calls > 0, "the whole network ran as one program");
    // several inputs go to the device together, and each gets its own outputs
    let inputs: Vec<Tensor> = (0..3).map(every_operator_input).collect();
    let together = net.run_batch(inputs.clone()).unwrap();
    assert_eq!(fake.calls.load(std::sync::atomic::Ordering::Relaxed), calls + 1, "one dispatch for the batch");
    net.detach();
    for (x, got) in inputs.into_iter().zip(&together) {
        let want = net.run(x).unwrap();
        agree(&want, got, 1e-5).unwrap();
    }
    assert_eq!(cpu.len(), 2);
}

#[test]
fn a_device_that_gives_values_that_are_not_finite_is_dropped() {
    let net = Net::new(crate::synthetic::every_operator_graph()).unwrap();
    let x = every_operator_input(1);
    let cpu = net.run(x.clone()).unwrap();
    assert!(net.attach_checked(Arc::new(ProgramFake { calls: 0.into(), poison: true }), std::slice::from_ref(&x), 1e-5, false).is_err());
    assert!(net.accelerator().is_none());
    assert_eq!(net.run(x).unwrap(), cpu);
}

#[test]
fn unsupported_and_inconsistent_models_are_errors() {
    let x = input(1, 3, 3, |i| i as f32);
    for (what, g) in [
        ("unknown op", graph(vec![node("Softmax", &["x"], &[])], vec![])),
        ("dilated conv", graph(vec![node("Conv", &["x", "w"], &[("dilations", Attr::Ints(vec![2, 2]))])], vec![("w", w(&[1, 1, 1, 1], vec![1.0]))])),
        ("conv without weights", graph(vec![node("Conv", &["x", "w"], &[])], vec![])),
        ("channels do not match", graph(vec![node("Conv", &["x", "w"], &[])], vec![("w", w(&[1, 5, 1, 1], vec![1.0; 5]))])),
        ("kernel too big", graph(vec![node("Conv", &["x", "w"], &[])], vec![("w", w(&[1, 1, 5, 5], vec![1.0; 25]))])),
        ("zero group", graph(vec![node("Conv", &["x", "w"], &[("group", Attr::Int(0))])], vec![("w", w(&[1, 1, 1, 1], vec![1.0]))])),
        ("zero stride", graph(vec![node("Conv", &["x", "w"], &[("strides", Attr::Ints(vec![0, 0]))])], vec![("w", w(&[1, 1, 1, 1], vec![1.0]))])),
        (
            "negative pad",
            graph(vec![node("Conv", &["x", "w"], &[("pads", Attr::Ints(vec![-1, 0, 0, 0]))])], vec![("w", w(&[1, 1, 1, 1], vec![1.0]))]),
        ),
        ("bad perm", graph(vec![node("Transpose", &["x"], &[("perm", Attr::Ints(vec![0, 0, 1, 2]))])], vec![])),
        ("huge reshape", graph(vec![node("Reshape", &["x", "s"], &[])], vec![("s", Weight { dims: vec![2], data: Data::I64(vec![i64::MAX, 1]) })])),
        ("reshape mismatch", graph(vec![node("Reshape", &["x", "s"], &[])], vec![("s", Weight { dims: vec![2], data: Data::I64(vec![4, 4]) })])),
        ("non-integer resize", graph(vec![node("Resize", &["x", "", "s"], &[])], vec![("s", w(&[4], vec![1.0, 1.0, 1.5, 1.5]))])),
        ("add that broadcasts", graph(vec![node("Add", &["x", "c"], &[])], vec![])),
    ] {
        assert!(run(g, x.clone()).is_err(), "{what}");
    }
}
