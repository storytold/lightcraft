//! Photo Merge ▸ Pixel Shift through the commands the menus run: the refusals and failures are said on the window, a
//! result that exists is not merged again, the question a series raises for the other merge kinds, Delete Merge
//! Sources, and the frames a merged file finds again when it is imported on its own. Synthetic frames only: their
//! headers carry the pixel-shift numbers, they have no pixels.

use std::time::Duration;

use lcc_catalog::{Op, PhotoId};
use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
/// The command Photo ▸ Photo Merge ▸ Delete Merge Sources runs.
const DELETE: &str = "merge.deleteSourcesNow";

/// A header-only synthetic NEF with the pixel-shift numbers of frame `index` (all values invented).
fn frame_bytes(index: u8, shutter: f64) -> Vec<u8> {
    use lcc_tiff::{ByteOrder, IfdBuilder, TiffWriter, Value, tags as t, writer::rational};
    let mut note = IfdBuilder::new();
    note.set(0x0034, Value::Short(vec![16]));
    let mut a = vec![0u8; 16];
    a[8] = index;
    note.set(0x0056, Value::Undefined(a));
    let mut b = vec![0u8; 28];
    b[5] = index;
    note.set(0x005e, Value::Undefined(b));
    note.set(0x00a7, Value::Long(vec![5000 + u32::from(index)]));
    let mut maker = b"Nikon\0\x02\0\0\0".to_vec();
    maker.extend(TiffWriter::new(ByteOrder::Little, false).write(&[note]).unwrap());
    let mut exif = IfdBuilder::new();
    exif.set(t::MAKER_NOTE, Value::Undefined(maker));
    exif.set(t::EXPOSURE_TIME, Value::Rational(vec![rational(shutter)]));
    exif.set(t::DATE_TIME_ORIGINAL, Value::Ascii("2030:05:06 07:08:09".into()));
    exif.set(0x9291, Value::Ascii(format!("{:02}", u32::from(index) * 6)));
    let mut ifd0 = IfdBuilder::new();
    ifd0.set(t::MAKE, Value::Ascii("NIKON CORPORATION".into()));
    ifd0.set(t::MODEL, Value::Ascii("NIKON Z 8".into()));
    ifd0.set_child(t::EXIF_IFD, exif);
    TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap()
}

fn captured(index: u8) -> String {
    format!("2030-05-06T07:08:09.{:03}+02:00", u32::from(index) * 6)
}

/// A library of the 16 frames of a series (`frame-01.nef` …) on disk, in the catalog and not yet selected.
fn series(tag: &str) -> (lcc_testkit::TempDir, Headless, Vec<PhotoId>) {
    let dir = lcc_testkit::TempDir::new(&format!("lc-pixelshift-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut session = lcc_engine::Session::new().with_fs();
    let mut ids = Vec::new();
    for i in 1..=16u8 {
        let name = format!("frame-{i:02}.nef");
        let path = dir.join(&name).to_string_lossy().to_string();
        std::fs::write(&path, frame_bytes(i, 0.01)).unwrap();
        ids.push(add_photo(&mut session, &path, &name, "NEF", Some(captured(i))));
    }
    let app = LccApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.settle(T);
    (dir, h, ids)
}

fn add_photo(session: &mut lcc_engine::Session, path: &str, name: &str, format: &str, captured: Option<String>) -> PhotoId {
    let id = session.catalog.alloc_photo_id();
    let mut photo = lcc_catalog::Photo::new(id, lcc_catalog::Source::File { path: path.into() }, name, format, 8256, 5504, "2031-01-01T00:00:00");
    photo.captured = captured;
    session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    id
}

fn select(h: &mut Headless, ids: &[PhotoId]) {
    h.app.session.execute("library.select", &json!({"ids": ids.iter().map(|i| i.0).collect::<Vec<_>>()})).unwrap();
}

/// The text of the message the window shows (the latest).
fn said(h: &Headless) -> String {
    h.app.ui.toast().map(|t| t.0.clone()).unwrap_or_default()
}

#[test]
fn a_selection_that_is_not_one_series_says_why_and_merges_nothing() {
    let (dir, mut h, ids) = series("refusals");
    // a frame is missing
    select(&mut h, &ids[..15]);
    assert!(h.app.run("merge.pixelShiftNow", json!({})).is_err());
    assert_eq!(said(&h), "Pixel Shift: Frame 16 of the series is missing");
    // a file that is not a pixel-shift frame
    h.app.ui.toasts.clear();
    let plain = dir.join("plain.nef");
    std::fs::write(&plain, b"not a tiff").unwrap();
    let plain_id = add_photo(&mut h.app.session, &plain.to_string_lossy(), "plain.nef", "NEF", None);
    let mut with_plain = ids[..15].to_vec();
    with_plain.push(plain_id);
    select(&mut h, &with_plain);
    assert!(h.app.run("merge.pixelShiftNow", json!({})).is_err());
    assert_eq!(said(&h), "Pixel Shift: plain.nef is not a pixel-shift frame");
    // another exposure among the frames
    h.app.ui.toasts.clear();
    let other = dir.join("frame-05.nef");
    std::fs::write(&other, frame_bytes(5, 0.02)).unwrap();
    select(&mut h, &ids);
    assert!(h.app.run("merge.pixelShiftNow", json!({})).is_err());
    assert_eq!(said(&h), "Pixel Shift: The photos have different exposure settings");
    // one photo
    h.app.ui.toasts.clear();
    select(&mut h, &ids[..1]);
    assert!(h.app.run("merge.pixelShiftNow", json!({})).is_err());
    assert_eq!(said(&h), "Pixel Shift: Select the frames of a pixel-shift series");
    assert!(h.app.merge.final_task.is_none(), "nothing started");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_merge_whose_result_exists_is_refused_before_anything_is_decoded() {
    let (dir, mut h, ids) = series("exists");
    std::fs::write(dir.join("frame-01-PixelShift.dng"), b"an earlier result").unwrap();
    select(&mut h, &ids);
    assert!(h.app.run("merge.pixelShiftNow", json!({})).is_err());
    assert_eq!(said(&h), "A pixel-shift merge of this series already exists (frame-01-PixelShift.dng)");
    assert!(h.app.merge.final_task.is_none() && h.app.session.activity.list().is_empty(), "nothing started");
    assert_eq!(std::fs::read(dir.join("frame-01-PixelShift.dng")).unwrap(), b"an earlier result", "not replaced");
    assert!(!dir.join("frame-01-PixelShift-2.dng").exists(), "no other name");
    assert_eq!(h.app.session.catalog.len(), 16, "no photo added");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_merge_that_fails_while_running_says_so() {
    // the frames have no pixels: the merge starts (the headers are fine) and fails when it decodes
    let (dir, mut h, ids) = series("fails");
    select(&mut h, &ids);
    h.app.run("merge.pixelShiftNow", json!({})).unwrap();
    assert!(h.step_until(T, |h| h.app.merge.final_task.is_none()));
    assert!(said(&h).starts_with("Pixel shift merge failed: "), "{}", said(&h));
    assert_eq!(h.app.session.catalog.len(), 16, "no photo added");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_question_for_another_kind_of_merge_on_a_series_has_three_answers() {
    let (dir, mut h, ids) = series("offer");
    // exactly one complete series: asked, nothing opened or started
    select(&mut h, &ids);
    h.app.run("dialog.mergeHdr", json!({})).unwrap();
    assert!(format!("{:?}", h.app.ui.dialog).contains("PixelShiftOffer"), "{:?}", h.app.ui.dialog);
    assert!(h.app.merge.final_task.is_none());
    // Cancel: nothing
    h.step();
    assert!(h.click_as_user("button:dialogCancel"), "Cancel is there");
    assert!(h.app.ui.dialog.is_none() && h.app.merge.final_task.is_none() && h.app.merge.preview_task.is_none());
    // Continue: the HDR dialog, as without the question
    h.app.run("dialog.mergeFocusStack", json!({})).unwrap();
    h.step();
    assert!(h.click_as_user("button:dialogContinueMerge"), "Continue is there");
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Merge { opts }) if opts.command == "merge.focusStack"), "{:?}", h.app.ui.dialog);
    h.app.ui.dialog = None;
    // Pixel Shift: the pixel-shift merge starts
    h.app.run("dialog.mergePanorama", json!({})).unwrap();
    h.step();
    assert!(h.click_as_user("button:dialogOk"), "Pixel Shift is there");
    // (the frames have no pixels: it starts and fails at once, and says which merge it was)
    assert!(h.step_until(T, |h| h.app.merge.final_task.is_none()));
    assert!(said(&h).starts_with("Pixel shift merge failed: "), "{}", said(&h));
    assert!(!h.app.session.last_merge.contains_key("merge.panorama"), "with its own options, not the panorama's");
    assert!(h.step_until(T, |h| h.app.merge.final_task.is_none()));
    // another selection: no question, the dialog of the kind asked for
    h.app.ui.dialog = None;
    select(&mut h, &ids[..6]);
    h.app.run("dialog.mergeHdr", json!({})).unwrap();
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Merge { .. })), "{:?}", h.app.ui.dialog);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The merged photo of a series: frame 01 and it stacked, its sources remembered (as `finish_merge` leaves them).
fn merged(h: &mut Headless, dir: &std::path::Path, frames: &[PhotoId]) -> PhotoId {
    let path = dir.join("frame-01-PixelShift.dng").to_string_lossy().to_string();
    let id = add_photo(&mut h.app.session, &path, "frame-01-PixelShift.dng", "DNG", Some(captured(1)));
    let sources = lcc_catalog::MergeSources {
        engine: 1,
        shot_count: 16,
        reference: 1,
        scale: 2,
        frames: frames
            .iter()
            .enumerate()
            .map(|(k, f)| lcc_catalog::MergeSource {
                photo: Some(*f),
                index: k as u16 + 1,
                file_name: format!("frame-{:02}.nef", k + 1),
                captured: captured(k as u8 + 1),
                counter: 5000 + k as u32 + 1,
            })
            .collect(),
    };
    let stack = h.app.session.catalog.stack_with_ops(id, frames).unwrap();
    h.app.session.catalog.apply(stack).unwrap();
    h.app.session.catalog.apply(Op::SetMergeSources { id, sources: Some(Box::new(sources)) }).unwrap();
    select(h, &[id]);
    id
}

#[test]
fn delete_merge_sources_moves_frames_two_to_sixteen_to_recently_deleted() {
    for confirm in [false, true] {
        let (dir, mut h, ids) = series(&format!("delete-{confirm}"));
        let id = merged(&mut h, &dir, &ids);
        h.app.ui.settings.confirm_delete = confirm;
        assert!(crate::menus::ui_enabled(&h.app, DELETE), "enabled on the merged photo");
        h.app.run(DELETE, json!({})).unwrap();
        if confirm {
            // asked first, nothing deleted until the question is answered
            assert!(format!("{:?}", h.app.ui.dialog).contains("ConfirmDeleteSources"), "{:?}", h.app.ui.dialog);
            assert!(ids.iter().all(|i| !h.app.session.catalog.photo(*i).unwrap().deleted));
            h.step();
            assert!(h.click_as_user("button:dialogOk"));
        } else {
            assert!(h.app.ui.dialog.is_none(), "not asked with the setting off");
        }
        let deleted = |i: &PhotoId| h.app.session.catalog.photo(*i).unwrap().deleted;
        assert!(!deleted(&ids[0]), "frame 01 stays");
        assert!(ids[1..].iter().all(deleted), "frames 02–16 are in Recently Deleted (confirm: {confirm})");
        assert!(!h.app.session.catalog.photo(id).unwrap().deleted, "the merged photo stays");
        let stack = h.app.session.catalog.stack_of(id).expect("still stacked");
        let live: Vec<PhotoId> = stack.photos.iter().copied().filter(|p| !h.app.session.catalog.photo(*p).unwrap().deleted).collect();
        assert_eq!(live.len(), 2, "the merged photo and frame 01: {live:?}");
        assert!(live.contains(&id) && live.contains(&ids[0]));
        assert_eq!(said(&h), "Frames moved to Recently Deleted: 15");
        // nothing but frame 01 is left: greyed out
        assert!(!crate::menus::ui_enabled(&h.app, DELETE));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn delete_merge_sources_with_some_frames_gone_says_how_many() {
    let (dir, mut h, ids) = series("partial");
    let id = merged(&mut h, &dir, &ids);
    h.app.ui.settings.confirm_delete = false;
    // three frames are already in Recently Deleted or no longer in the library
    h.app.session.execute("photo.delete", &json!({"ids": [ids[2].0, ids[3].0]})).unwrap();
    h.app.session.catalog.apply(h.app.session.catalog.delete_photos_permanently_ops(&[ids[9]])).unwrap();
    select(&mut h, &[id]);
    h.app.run(DELETE, json!({})).unwrap();
    assert!(ids[1..].iter().all(|i| h.app.session.catalog.photo(*i).is_none_or(|p| p.deleted)), "every frame is gone now");
    assert!(!h.app.session.catalog.photo(ids[0]).unwrap().deleted);
    assert_eq!(said(&h), "Frames moved to Recently Deleted: 12; already gone: 3");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn delete_merge_sources_is_greyed_out_for_other_photos() {
    let (dir, mut h, ids) = series("greyed");
    select(&mut h, &ids[..1]);
    assert!(!crate::menus::ui_enabled(&h.app, DELETE), "a plain photo");
    let err = h.app.run(DELETE, json!({})).unwrap_err();
    assert_eq!(err, "This photo is not a pixel-shift merge");
    assert_eq!(said(&h), err, "said on the window too");
    // a merged photo whose frames other than 01 are all gone
    let id = merged(&mut h, &dir, &ids);
    h.app.ui.settings.confirm_delete = false;
    h.app.run(DELETE, json!({})).unwrap();
    assert!(!crate::menus::ui_enabled(&h.app, DELETE));
    select(&mut h, &[id]);
    assert!(h.app.run(DELETE, json!({})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_merged_file_imported_on_its_own_finds_its_frames_again() {
    use lcc_geom::Orientation;
    use lcc_merge::pixelshift::{DngPacking, DngSource, Merger, SHOT_16, write_dng};
    use lcc_raw::{BlackLevel, Cfa, ColorData, RawData, RawFormat, RawImage, Rect};
    let (dir, mut h, ids) = series("provenance");
    // the library has all frames but 3 and 7 (index 3 and 7)
    for gone in [ids[2], ids[6]] {
        h.app.session.catalog.apply(h.app.session.catalog.delete_photos_permanently_ops(&[gone])).unwrap();
    }
    let (w, ht) = (24usize, 24usize);
    let frame = |k: usize| RawImage {
        format: RawFormat::Nef,
        width: w,
        height: ht,
        cpp: 1,
        data: RawData::U16((0..w * ht).map(|i| 1100 + ((i * 7 + k * 13) % 3000) as u16).collect()),
        cfa: Cfa::bayer("RGGB"),
        bits: 14,
        black: BlackLevel::uniform(1008.0),
        white: vec![16383.0],
        active_area: Rect::new(0, 0, w, ht),
        crop: Rect::new(2, 2, 20, 20),
        orientation: Orientation::Normal,
        color: ColorData::default(),
        wb_multipliers: None,
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: Default::default(),
        metadata: Default::default(),
        lens_metadata: Default::default(),
    };
    let mut merger = Merger::new(&SHOT_16, &frame(0)).unwrap();
    for k in 0..16 {
        merger.add_frame(k, &frame(k)).unwrap();
    }
    let metadata = lcc_meta::Metadata {
        make: Some("NIKON CORPORATION".into()),
        model: Some("NIKON Z 8".into()),
        capture_time: lcc_meta::DateTime::parse_iso(&captured(1)),
        ..Default::default()
    };
    let provenance = lcc_raw::merge_provenance::Provenance {
        engine: 1,
        shot_count: 16,
        reference: 1,
        scale: 2,
        window: None,
        frames: (1..=16u8)
            .map(|i| lcc_raw::merge_provenance::SourceFrame {
                index: u16::from(i),
                file_name: format!("frame-{i:02}.nef"),
                captured: captured(i),
                counter: 5000 + u32::from(i),
            })
            .collect(),
    };
    let dng = write_dng(
        merger.finish().unwrap(),
        DngSource { metadata, color: ColorData::default(), orientation: Orientation::Normal, lens: vec![], provenance, preview: None },
        DngPacking::Deflate,
    )
    .unwrap();
    let merged_dir = dir.join("merged");
    std::fs::create_dir_all(&merged_dir).unwrap();
    std::fs::write(merged_dir.join("frame-01-PixelShift.dng"), &dng).unwrap();
    let r = h.app.session.execute("library.import", &json!({"paths": [merged_dir.to_string_lossy()]})).unwrap();
    let id = PhotoId(r["imported"][0].as_u64().expect("imported"));
    let sources = h.app.session.catalog.photo(id).unwrap().merge_sources.clone().expect("its sources are restored");
    assert_eq!((sources.shot_count, sources.reference, sources.frames.len()), (16, 1, 16));
    for (k, f) in sources.frames.iter().enumerate() {
        let expected = if k == 2 || k == 6 { None } else { Some(ids[k]) };
        assert_eq!(f.photo, expected, "frame {}", k + 1);
        assert_eq!((f.index, f.counter), (k as u16 + 1, 5000 + k as u32 + 1));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
