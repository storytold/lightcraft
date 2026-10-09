use dac_segment::remote::{Operation, Request, read_frame};
use std::io::Cursor;

#[test]
fn rejects_unbounded_and_truncated_frames() {
    assert!(read_frame(&mut Cursor::new(u32::MAX.to_be_bytes()), 1024).is_err());
    assert!(read_frame(&mut Cursor::new([0, 0, 0, 3, 1]), 1024).is_err());
}

#[test]
fn validates_images_and_prompt_boundaries() {
    let request = |width, height, operation| Request { width, height, temporary: false, operation };
    assert!(request(1009, 1, Operation::Prepare).validate().is_err());
    assert!(request(0, 1, Operation::Prepare).validate().is_err());
    assert!(request(0, 0, Operation::Text("dog".into())).validate().is_ok());
    assert!(request(4, 3, Operation::Clicks(vec![[0.5, 0.5, 1.0]])).validate().is_ok());
    assert!(request(4, 3, Operation::Clicks(vec![[1.5, 0.5, 1.0]])).validate().is_err());
    assert!(request(4, 3, Operation::Clicks(vec![[0.5, f32::NAN, 1.0]])).validate().is_err());
    assert!(request(4, 3, Operation::Clicks(vec![[0.5, 0.5, 0.0]])).validate().is_err());
    assert!(request(4, 3, Operation::Text(" ".into())).validate().is_err());
}
