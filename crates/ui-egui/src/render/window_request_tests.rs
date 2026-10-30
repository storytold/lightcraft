use super::*;

struct Blocked;
impl RenderOffload for Blocked {
    fn try_start(&mut self, _: Slot, job: RenderJob) -> Option<RenderJob> {
        Some(job)
    }
    fn finished(&mut self) -> Vec<(Slot, RenderResult, f64)> {
        vec![]
    }
}

#[test]
fn exact_replacements_wait_100_ms_and_failures_do_not_restart_the_wait() {
    let mut session = Session::with_demo();
    let photo = session.visible_cloned()[0];
    let win = lightcraft_pipeline::PixelWindow { x: 100, y: 100, w: 121, h: 83 };
    let job = session.region_job(photo, 1201, 803, win, true).unwrap();
    for slot in [Slot::Region, Slot::RegionBefore, Slot::CompareRegion(0)] {
        let ctx = egui::Context::default();
        let mut r = Renderer::default();
        r.set_offload(Box::new(Blocked));
        let tex = ctx.load_texture("bounded", egui::ColorImage::filled([1, 1], egui::Color32::BLACK), egui::TextureOptions::LINEAR);
        r.textures.insert(
            slot,
            Tex { look: None, key: job.clone().bounded_window().key, photo, tex, size: [1, 1], histogram: None, ms: 0.0, quick: None, pixels: None },
        );
        let request = |r: &mut Renderer, time| {
            let input = crate::headless::HeadlessView::raw_input(egui::vec2(300.0, 200.0), 1.0, time, vec![]);
            let mut output = ctx.run_ui(input, |_| r.request_window(&ctx, slot, job.clone(), 99));
            output.textures_delta.clear();
            output
        };
        request(&mut r, 1.0);
        assert!(!r.is_pending(slot));
        assert_eq!(r.in_flight(), 1);
        let output = request(&mut r, 1.099);
        assert!(output.viewport_output.values().any(|v| v.repaint_delay <= std::time::Duration::from_millis(16)), "the wait must wake an idle app");
        assert_eq!(r.queued(), 0);
        request(&mut r, 1.101);
        assert_eq!(r.wanted(slot), Some(job.key));
        assert_eq!(r.queued(), 1);
        assert!(r.exact_waits.is_empty());
        r.pending.clear();
        r.queue.clear();
        r.failed.insert(slot, (job.key, "render failed".into(), r.catalog_rev, photo));
        request(&mut r, 1.2);
        assert_eq!(r.in_flight(), 0, "a failed exact job must not keep headless settling busy");
        r.failed.clear();
        request(&mut r, 1.3);
        assert_eq!(r.in_flight(), 1);
        r.release(slot);
        assert_eq!(r.in_flight(), 0, "closing the pane cancels its deferred replacement");
    }
}
