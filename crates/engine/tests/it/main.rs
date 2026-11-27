//! The integration tests, one module per file, linked into one test binary.

#[cfg(feature = "denoise")]
mod denoise_cpu;
#[cfg(feature = "denoise")]
mod denoise_real;
mod orf_withdrawal;
mod preview_read;

#[cfg(test)]
mod collapsed_stack_hdr_panorama {
    #[test]
    fn collapsed_stack_expands_in_capture_order_for_hdr_panorama() {
        let mut s = lightcraft_engine::Session::new();
        let mut ids = Vec::new();
        for i in 0..6 {
            let id = s.catalog.alloc_photo_id();
            let name = format!("IMG_{i}.dng");
            let p = lightcraft_engine::catalog::Photo::new(
                id,
                lightcraft_engine::catalog::Source::File { path: name.clone() },
                &name,
                "DNG",
                96,
                64,
                "2026-01-01T00:00:00",
            );
            s.catalog.apply(lightcraft_engine::catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
            ids.push(id);
        }
        // Capture order is the import order above; the stack top is not the earliest capture.
        let op = s.catalog.group_ops(ids[3], &ids, true).unwrap();
        s.catalog.apply(op).unwrap();
        let stack = s.catalog.stack_of(ids[3]).unwrap().clone();
        assert!(stack.collapsed);
        assert_eq!(stack.top(), ids[3]);
        let (kind, finish) = lightcraft_engine::merge::parse("merge.hdrPanorama", &serde_json::json!({})).unwrap();
        // Naming every member in capture order must not re-expand the stack in stack order.
        let job = s.plan_merge(kind, finish, &ids, true).unwrap();
        assert_eq!(
            job.sources.iter().map(|(p, _)| p.0).collect::<Vec<_>>(),
            ids.iter().map(|p| p.0).collect::<Vec<_>>(),
            "collapsed stack should expand in capture order, not stack order (top first)"
        );
    }
}
