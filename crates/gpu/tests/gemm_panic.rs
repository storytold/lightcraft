//! A panic inside a matrix product on the GPU (wgpu panics in `Device::poll` once a device is lost)
//! is caught: the product falls back to the CPU (`None`) and that card's device is given up on.
//! Its own test binary: giving up is process-wide.
#![cfg(not(target_arch = "wasm32"))]

use lightcraft_gpu::{Role, ai, gemm};

#[test]
fn a_panic_in_a_product_falls_back_and_stops_the_gpu() {
    if let Err(e) = gemm::adapter_on(Role::Primary) {
        lightcraft_testkit::skip!(Gpu, "no matrix device ({e})");
        return;
    }
    // (on the editing card: with a second card, background products would go there first)
    ai::on_card(Role::Primary, || {
        let a = vec![1.0f32; 4];
        let r = gemm::matmul(&a, 2, 2, 2, 1, &|| panic!("the product failed on the device"));
        assert!(r.is_none(), "the CPU computes it");
        assert!(gemm::broken_reason().is_some_and(|w| w.contains("panicked")), "{:?}", gemm::broken_reason());
        assert!(gemm::matmul(&a, 2, 2, 2, 2, &|| Some(vec![1.0; 4])).is_none(), "later products go to the CPU");
    });
    assert_eq!(gemm::broken_reason_on(Role::Helper), None, "the other card is left alone");
}
