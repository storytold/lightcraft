//! Opt-in local check of Sony's lens-correction records ([`lightcraft_raw::lens_metadata`]); it does nothing unless
//! `LIGHTCRAFT_SONY_LENS_DIR` names a directory of ARW files, so CI never runs it. The files there (searched
//! recursively) are only read, from their headers, and one line per file is printed: settings, where each table came
//! from, whether its raw-IFD and `SR2SubIFD` copies agree, sample ranges and status. Header and full decode are
//! compared too when `LIGHTCRAFT_SONY_LENS_FULL=1` (slow on large files).
//!
//! With `LIGHTCRAFT_SONY_LENS_INVENTORY` naming a CSV inventory of those files (comma-separated, no quoting, a header
//! row, one row per file name), every file must have a row, every row a file, and these columns must match where
//! present: `split` (the file's folder name), `raw_size` (`WxH`), `compression`, `crop_origin` / `crop_size` (Sony's
//! crop tags, `x y` / `w h`), and for each of `shading`, `ca` and `distortion`: `_entries` (TIFF count), `_bytes`,
//! `_samples` (the leading sample count), `_min` / `_max` (over the samples), `_sr2_equal` (`True` / `False`),
//! `_setting_raw` and `_setting` (a label: `Off`, `Auto`, `Auto (ILCE-1)`, `Auto fixed by lens`, `No correction
//! params available`). A file that doesn't decode must still have a row, and fails the check when it has one.
//!
//! Any decode error fails the check except an unsupported variant (`RawError::Unsupported`, e.g. the packed ARW of
//! some older bodies): those are counted, and the count must equal `LIGHTCRAFT_SONY_LENS_UNSUPPORTED` (default 0).
//!
//! `LIGHTCRAFT_SONY_LENS_DIR=… LIGHTCRAFT_SONY_LENS_INVENTORY=….csv cargo test -p lightcraft-raw --release --test
//! sony_lens_inventory -- --nocapture`

use lightcraft_raw::lens_metadata::{SonyCopy, SonyField, SonyLensCorrections, SonyLensStatus, SonySetting, SonySettingMeaning, SonySource};
use lightcraft_raw::{RawError, RawInfo, decode, probe_info};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn arw_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && depth < 8 {
            arw_files(&path, depth + 1, out);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("arw")) {
            out.push(path);
        }
    }
}

/// One component's columns, from the decoded record.
struct Component {
    setting: Option<(u16, SonySettingMeaning)>,
    /// Samples when the table was decoded.
    samples: Option<Vec<i16>>,
    source: Option<SonySource>,
    /// The raw-IFD and `SR2SubIFD` copies' states.
    copies: (SonyCopy, SonyCopy),
    agree: Option<bool>,
    status: String,
}

fn component<T>(setting: &SonyField<SonySetting>, table: &SonyField<T>, samples: Option<Vec<i16>>, status: SonyLensStatus) -> Component {
    Component {
        setting: setting.value.map(|s| (s.value, s.meaning)),
        samples,
        source: table.source,
        copies: (table.raw_ifd, table.sr2),
        agree: table.copies_agree,
        status: format!("{status:?}"),
    }
}

/// Shading, CA and distortion of a record, by CSV column prefix.
fn components(s: &SonyLensCorrections) -> [(&'static str, Component); 3] {
    let (sh, ca, d) = (&s.shading, &s.chromatic_aberration, &s.distortion);
    [
        ("shading", component(&sh.setting, &sh.table, sh.table.value.map(|v| v.to_vec()), sh.status)),
        ("ca", component(&ca.setting, &ca.table, ca.table.value.map(|v| v.concat()), ca.status)),
        ("distortion", component(&d.setting, &d.table, d.table.value.map(|v| v.to_vec()), d.status)),
    ]
}

fn label(meaning: SonySettingMeaning) -> &'static [&'static str] {
    match meaning {
        SonySettingMeaning::Off => &["Off"],
        SonySettingMeaning::Auto => &["Auto", "Auto (ILCE-1)"],
        SonySettingMeaning::ForcedAuto => &["Auto fixed by lens"],
        SonySettingMeaning::Unavailable => &["No correction params available"],
        SonySettingMeaning::Unknown(_) => &[],
    }
}

/// The columns this check derives from a file, by CSV column name.
fn columns(path: &Path, info: &RawInfo) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut put = |k: &str, v: String| {
        out.insert(k.to_string(), v);
    };
    if let Some(split) = path.parent().and_then(|p| p.file_name()) {
        put("split", split.to_string_lossy().into_owned());
    }
    let Some(s) = &info.lens_metadata.sony else { return out };
    let g = &s.geometry;
    put("raw_size", format!("{}x{}", g.storage_width, g.storage_height));
    put("compression", g.compression.to_string());
    if let Some(c) = g.sony_crop {
        put("crop_origin", format!("{} {}", c.x, c.y));
        put("crop_size", format!("{} {}", c.width, c.height));
    }
    for (name, c) in components(s) {
        if let Some(v) = &c.samples {
            put(&format!("{name}_entries"), (v.len() + 1).to_string());
            put(&format!("{name}_bytes"), (2 * (v.len() + 1)).to_string());
            put(&format!("{name}_samples"), v.len().to_string());
            put(&format!("{name}_min"), v.iter().min().map_or(String::new(), i16::to_string));
            put(&format!("{name}_max"), v.iter().max().map_or(String::new(), i16::to_string));
        }
        put(&format!("{name}_sr2_equal"), c.agree.map_or("unknown", |a| if a { "True" } else { "False" }).to_string());
        if let Some((value, _)) = c.setting {
            put(&format!("{name}_setting_raw"), value.to_string());
        }
    }
    out
}

fn summary(info: &RawInfo) -> String {
    let Some(s) = &info.lens_metadata.sony else { return "no lens record".into() };
    let mut line = format!("sr2 {} |", if s.sr2_read { "read" } else { "absent" });
    for (name, c) in components(s) {
        let setting = c.setting.map_or("-".into(), |(v, m)| format!("{v} {m:?}"));
        let range = c
            .samples
            .as_ref()
            .map_or("no table".into(), |v| format!("{} samples {}..{}", v.len(), v.iter().min().unwrap_or(&0), v.iter().max().unwrap_or(&0)));
        line +=
            &format!(" {name}: {setting}, {range}, {:?} (copies {:?} / {:?}), agree {:?}, {} |", c.source, c.copies.0, c.copies.1, c.agree, c.status);
    }
    line + &format!(" crop {:?} reference {:?} 3:2 {}", s.geometry.image_crop, s.geometry.reference, s.geometry.native_3_2)
}

/// `(header, rows by file name)` of a simple CSV file.
fn read_csv(path: &Path) -> (Vec<String>, BTreeMap<String, Vec<String>>) {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header: Vec<String> = lines.next().expect("CSV header").split(',').map(|s| s.trim().to_string()).collect();
    let file = header.iter().position(|h| h == "file").expect("a `file` column");
    let mut rows = BTreeMap::new();
    for line in lines {
        assert!(!line.contains('"'), "quoted CSV fields are not supported: {line}");
        let row: Vec<String> = line.split(',').map(|s| s.trim().to_string()).collect();
        assert_eq!(row.len(), header.len(), "CSV row with {} fields, header has {}: {line}", row.len(), header.len());
        let name = row[file].clone();
        assert!(rows.insert(name.clone(), row).is_none(), "{name} listed twice");
    }
    (header, rows)
}

#[test]
fn sony_lens_records_reproduce_a_local_inventory() {
    let Some(dir) = std::env::var_os("LIGHTCRAFT_SONY_LENS_DIR").map(PathBuf::from) else {
        eprintln!("skip: LIGHTCRAFT_SONY_LENS_DIR unset");
        return;
    };
    let full = std::env::var_os("LIGHTCRAFT_SONY_LENS_FULL").is_some_and(|v| v == "1");
    let inventory = std::env::var_os("LIGHTCRAFT_SONY_LENS_INVENTORY").map(PathBuf::from);
    let unsupported = std::env::var("LIGHTCRAFT_SONY_LENS_UNSUPPORTED").map_or(0, |v| v.parse().expect("LIGHTCRAFT_SONY_LENS_UNSUPPORTED: a count"));
    let report = check(&dir, inventory.as_deref(), full, unsupported);
    assert!(report.problems.is_empty(), "{} problems", report.problems.len());
}

/// What [`check`] found.
#[derive(Debug, Default)]
struct Report {
    files: usize,
    records: usize,
    without_record: usize,
    unsupported: usize,
    problems: Vec<String>,
}

/// Check every ARW under `dir` (see the module docs). Any decode error is a problem except `Unsupported`, which
/// is counted and must come to exactly `expected_unsupported`; with an inventory, every file (decoded or not) must
/// have a row, and a file with a row must decode.
fn check(dir: &Path, inventory: Option<&Path>, full: bool, expected_unsupported: usize) -> Report {
    let inventory = inventory.map(read_csv);
    let mut files = Vec::new();
    arw_files(dir, 0, &mut files);
    files.sort();
    let mut report = Report { files: files.len(), ..Default::default() };
    if files.is_empty() {
        report.problems.push(format!("no ARW files under {}", dir.display()));
    }
    let (mut matched, mut mismatches, mut seen) = (0usize, Vec::new(), std::collections::BTreeSet::new());
    let compared: Vec<String> = ["split", "raw_size", "compression", "crop_origin", "crop_size"]
        .into_iter()
        .map(String::from)
        .chain(
            ["shading", "ca", "distortion"]
                .into_iter()
                .flat_map(|k| ["entries", "bytes", "samples", "min", "max", "sr2_equal", "setting_raw"].into_iter().map(move |c| format!("{k}_{c}"))),
        )
        .collect();
    for path in &files {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        seen.insert(name.clone());
        let info = match probe_info(&bytes) {
            Ok(info) => info,
            Err(e) => {
                eprintln!("{name}: {e}");
                match (&inventory, &e) {
                    (Some((_, rows)), _) if rows.contains_key(&name) => mismatches.push(format!("{name}: does not decode: {e}")),
                    (Some(_), _) => mismatches.push(format!("{name}: does not decode ({e}) and is not in the inventory")),
                    (None, RawError::Unsupported(_)) => {}
                    (None, _) => mismatches.push(format!("{name}: does not decode: {e}")),
                }
                report.unsupported += usize::from(matches!(e, RawError::Unsupported(_)));
                continue;
            }
        };
        if full {
            match decode(&bytes) {
                Ok(image) if image.info() == info => {}
                Ok(_) => mismatches.push(format!("{name}: header and full decode differ")),
                Err(e) => mismatches.push(format!("{name}: full decode fails: {e}")),
            }
        }
        if info.lens_metadata.sony.is_some() {
            report.records += 1;
        } else {
            report.without_record += 1;
        }
        eprintln!("{name}: {} {}", info.metadata.model.as_deref().unwrap_or("?"), summary(&info));
        let Some((header, rows)) = &inventory else { continue };
        let Some(row) = rows.get(&name) else {
            mismatches.push(format!("{name}: not in the inventory"));
            continue;
        };
        let got = columns(path, &info);
        let mut ok = true;
        for (i, column) in header.iter().enumerate() {
            let expected = &row[i];
            if let Some(setting) = column.strip_suffix("_setting")
                && let Some(s) = &info.lens_metadata.sony
            {
                let meaning = components(s).into_iter().find(|(n, _)| *n == setting).and_then(|(_, c)| c.setting).map(|(_, m)| m);
                if !meaning.is_some_and(|m| label(m).contains(&expected.as_str())) {
                    mismatches.push(format!("{name}: {column} {meaning:?}, inventory {expected}"));
                    ok = false;
                }
                continue;
            }
            if !compared.contains(column) {
                continue;
            }
            let value = got.get(column).map(String::as_str).unwrap_or("(none)");
            if value != expected {
                mismatches.push(format!("{name}: {column} {value}, inventory {expected}"));
                ok = false;
            }
        }
        matched += usize::from(ok);
    }
    if let Some((header, rows)) = &inventory {
        for name in rows.keys().filter(|n| !seen.contains(*n)) {
            mismatches.push(format!("{name}: in the inventory, no such file"));
        }
        let checked = header.iter().filter(|c| compared.contains(c) || c.ends_with("_setting")).count();
        assert!(checked > 0, "the inventory has none of the compared columns");
        eprintln!("{matched} of {} inventory rows reproduced from {} files ({checked} columns compared)", rows.len(), files.len());
    }
    if report.unsupported != expected_unsupported {
        mismatches.push(format!("{} files are unsupported variants, {expected_unsupported} expected", report.unsupported));
    }
    eprintln!(
        "{} files: {} with a lens record, {} decoded without one, {} unsupported ({expected_unsupported} expected)",
        report.files, report.records, report.without_record, report.unsupported
    );
    for m in &mismatches {
        eprintln!("problem: {m}");
    }
    report.problems.extend(mismatches);
    report
}

/// A directory under the system temporary directory, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lightcraft-sony-lens-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A small Sony CFA file with a distortion setting (a lens record), or, `unsupported`, the packed 12-bit ARW variant
/// the decoder refuses as unsupported.
fn synthetic_arw(unsupported: bool) -> Vec<u8> {
    use lightcraft_tiff::{IfdBuilder, ImageData, TiffWriter, Value, tags as t};
    let mut raw = IfdBuilder::new();
    raw.set(t::MAKE, Value::Ascii("SONY".into()));
    raw.set(t::MODEL, Value::Ascii("ILCE-7CR".into()));
    raw.set(t::IMAGE_WIDTH, Value::Long(vec![32]));
    raw.set(t::IMAGE_LENGTH, Value::Long(vec![4]));
    raw.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![1]));
    raw.set(t::PHOTOMETRIC, Value::Short(vec![t::photometric::CFA]));
    raw.set(0x7036, Value::Short(vec![0]));
    if unsupported {
        raw.set(t::BITS_PER_SAMPLE, Value::Short(vec![12]));
        raw.set(t::COMPRESSION, Value::Short(vec![32767]));
        raw.set(0x7010, Value::Short(vec![8000, 10400, 12900, 14100]));
        raw.set_image(ImageData::Strips { rows_per_strip: 4, strips: vec![vec![0; 32 * 4 + 8]] });
    } else {
        raw.set(t::BITS_PER_SAMPLE, Value::Short(vec![16]));
        raw.set(t::COMPRESSION, Value::Short(vec![1]));
        raw.set(t::WHITE_LEVEL, Value::Long(vec![16383]));
        raw.set_image(ImageData::Strips { rows_per_strip: 4, strips: vec![vec![0; 32 * 4 * 2]] });
    }
    TiffWriter::default().write(&[raw]).unwrap()
}

#[test]
fn files_that_fail_to_decode_fail_the_check() {
    // a file that is no raw at all fails the check, whatever is expected
    let dir = Scratch::new("decode-error");
    std::fs::write(dir.0.join("not-a-raw.arw"), b"not a raw file").unwrap();
    let report = check(&dir.0, None, false, 0);
    assert!(!report.problems.is_empty(), "an ARW that does not decode passed the check");
    assert!(!check(&dir.0, None, false, 1).problems.is_empty(), "a decode error counted as an unsupported variant");
    // an unsupported variant is counted, and must be expected
    let dir = Scratch::new("unsupported");
    std::fs::write(dir.0.join("good.arw"), synthetic_arw(false)).unwrap();
    std::fs::write(dir.0.join("packed.arw"), synthetic_arw(true)).unwrap();
    let report = check(&dir.0, None, false, 0);
    assert_eq!((report.files, report.records, report.unsupported), (2, 1, 1));
    assert_eq!(report.problems.len(), 1, "{:?}", report.problems);
    assert!(check(&dir.0, None, true, 1).problems.is_empty());
    // with an inventory, a file that fails must still be listed: here it is not
    let csv = dir.0.join("inventory.csv");
    std::fs::write(&csv, "file,distortion_setting_raw\ngood.arw,0\n").unwrap();
    // (the inventory itself sits in the directory, but only .arw files are checked)
    let mut report = check(&dir.0, Some(&csv), false, 1);
    report.problems.sort();
    assert_eq!(report.problems.len(), 1, "{:?}", report.problems);
    assert!(report.problems[0].contains("packed.arw") && report.problems[0].contains("not in the inventory"), "{:?}", report.problems);
    // listed but failing, and listed and decoding
    std::fs::write(&csv, "file,distortion_setting_raw\ngood.arw,0\npacked.arw,0\n").unwrap();
    let report = check(&dir.0, Some(&csv), false, 1);
    assert!(report.problems.len() == 1 && report.problems[0].contains("packed.arw: does not decode"), "{:?}", report.problems);
    std::fs::remove_file(dir.0.join("packed.arw")).unwrap();
    std::fs::write(&csv, "file,distortion_setting_raw\ngood.arw,0\n").unwrap();
    assert!(check(&dir.0, Some(&csv), false, 0).problems.is_empty());
    std::fs::write(&csv, "file,distortion_setting_raw\ngood.arw,17\n").unwrap();
    assert!(!check(&dir.0, Some(&csv), false, 0).problems.is_empty(), "a wrong value passed");
}
