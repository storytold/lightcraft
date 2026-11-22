use super::*;
use lightcraft_pipeline::curve_input::{CurveInput, EDGE};
use serde_json::json;

#[test]
fn curve_input_jobs_never_read_or_write_photo_caches_and_key_on_preceding_edits() {
    let mut s = crate::Session::with_demo();
    let id = s.active().unwrap();
    let normal = s.thumb_job(id, EDGE).unwrap();
    let (cache, key) = normal.cache.clone().unwrap();
    let sentinel = Arc::new(Rgba8::from_fn(3, 3, |_, _| [255, 0, 255, 255]));
    cache.put_at(normal.cache_generation, key, sentinel.clone());
    let input = normal.with_curve_input(CurveInput::Master);
    let input_key = input.key;
    let source_key = input.source_key;
    assert!(input.cache.is_none() && input.view_cache.is_none());
    assert_eq!(input.level, SourceLevel::Thumb);
    assert_eq!(input.clone().with_curve_input(CurveInput::Master).key, input_key, "idempotent mode selection");
    let r = input.run();
    assert_eq!(r.source_key, source_key);
    assert!(r.gpu_fallback.is_none(), "diagnostics use the CPU reference intentionally");
    let image = &r.rendered.as_ref().unwrap().image;
    assert!(image.width <= EDGE && image.height <= EDGE);
    assert_ne!(image, &*sentinel, "a cached output thumbnail is never input data");
    assert_eq!(&*cache.get_at(s.media.rendered.generation(), key).unwrap(), &*sentinel);
    s.accept(&r);
    s.execute("develop.curve", &json!({"channel":"master", "points":[[0,0],[0.25,0.7],[1,1]]})).unwrap();
    let same = s.thumb_job(id, EDGE).unwrap().with_curve_input(CurveInput::Master);
    assert_eq!(same.key, input_key, "moving this curve needs no extra diagnostic render");
    s.execute("develop.set", &json!({"control":"curve.lights", "value":40})).unwrap();
    let changed = s.thumb_job(id, EDGE).unwrap().with_curve_input(CurveInput::Master);
    assert_ne!(changed.key, input_key, "the preceding parametric curve invalidates master input");
}
