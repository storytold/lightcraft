//! The committed decode baseline (`docs/raw/decode-baseline.tsv`, #4577): one row per CC0 corpus raw with what
//! `lightcraft_raw::decode` returns, craft-raw and LightCraft's adapter together.
//!
//! - `decode_baseline_matches_corpus` decodes every corpus raw and fails, naming the file and column, when a value
//!   differs from the baseline. It needs the corpus (`cargo xtask corpus --download`, or `LIGHTCRAFT_CORPUS`) and
//!   skips without it, as in CI.
//! - `decode_baseline_matches_lockfile` needs no corpus and runs in CI. The baseline records the git revisions of
//!   `lightcraft-raw`'s dependencies from `Cargo.lock`, so a craft-libs pin bump fails here until the baseline is
//!   regenerated against the new revisions.
//!
//! After a pin bump (or any decode change), regenerate and review the diff. Every changed row is a decode change the
//! pull request should explain:
//! `LIGHTCRAFT_WRITE_DECODE_BASELINE=1 cargo test -p lightcraft-raw --test it decode_baseline_matches_corpus`
//!
//! The explicit columns are the review surface. The `*_digest` columns are a safety net over whole structures: they
//! also change when a dependency adds a field, which then shows as one column changing on every row. Floats keep 7
//! significant digits, so the last-bit differences between platforms' maths libraries don't show (Linux and Windows
//! differ by about 3e-17 in some Nikon `WarpRectilinear` coefficients).

use lightcraft_raw::{RawData, RawImage, decode};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

const BASELINE: &str = include_str!("../../../../docs/raw/decode-baseline.tsv");
const LOCKFILE: &str = include_str!("../../../../Cargo.lock");
const WRITE_ENV: &str = "LIGHTCRAFT_WRITE_DECODE_BASELINE";
const REGENERATE: &str = "LIGHTCRAFT_WRITE_DECODE_BASELINE=1 cargo test -p lightcraft-raw --test it decode_baseline_matches_corpus";
/// Corpus directories that `cargo xtask corpus --download` fills with raws, relative to the corpus root.
const CORPUS_DIRS: [&str; 2] = ["raw", "d750-colorchecker/raw"];
const COLUMNS: [&str; 32] = [
    "file",
    "file_sha256",
    "status",
    "format",
    "size",
    "bits",
    "samples",
    "cfa",
    "black",
    "white",
    "active_area",
    "crop",
    "orientation",
    "pixel_aspect",
    "linearized",
    "wb_multipliers",
    "as_shot_neutral",
    "as_shot_white_xy",
    "baseline_exposure",
    "illuminants",
    "kelvin_presets",
    "color_digest",
    "profile_digest",
    "opcodes",
    "opcodes_digest",
    "camera",
    "iso",
    "exposure",
    "lens",
    "capture_time",
    "metadata_digest",
    "lens_metadata_digest",
];

fn corpus_root() -> PathBuf {
    std::env::var_os("LIGHTCRAFT_CORPUS").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
}

fn baseline_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/raw/decode-baseline.tsv")
}

// ---- formatting -------------------------------------------------------------------------------------------------

/// A float with 7 significant digits, printed as the shortest decimal that reads back as the rounded value.
fn num(x: f64) -> String {
    if !x.is_finite() {
        return format!("{x}");
    }
    let rounded: f64 = format!("{x:.6e}").parse().unwrap_or(x);
    format!("{rounded}")
}

fn nums(values: impl IntoIterator<Item = f64>) -> String {
    values.into_iter().map(num).collect::<Vec<_>>().join(",")
}

fn opt<T>(value: Option<T>, show: impl FnOnce(T) -> String) -> String {
    value.map_or_else(|| "-".to_owned(), show)
}

/// Canonical JSON with sorted keys and rounded floats.
fn canonical(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => out.push_str(&i.to_string()),
            (None, Some(u)) => out.push_str(&u.to_string()),
            (None, None) => out.push_str(&num(n.as_f64().unwrap_or(f64::NAN))),
        },
        Value::String(s) => out.push_str(&Value::String(s.clone()).to_string()),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            let sorted: BTreeMap<_, _> = map.iter().collect();
            for (i, (key, item)) in sorted.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                canonical(item, out);
            }
            out.push('}');
        }
    }
}

fn hex(bytes: &[u8], len: usize) -> String {
    bytes.iter().flat_map(|b| [b >> 4, b & 15]).take(len).map(|n| char::from_digit(u32::from(n), 16).unwrap_or('?')).collect()
}

/// The first 12 hex digits of a SHA-256 over the value's canonical JSON; `-` for a `None` / null value.
fn digest(value: &impl serde::Serialize) -> String {
    let Ok(value) = serde_json::to_value(value) else { return "unserializable".to_owned() };
    if value.is_null() {
        return "-".to_owned();
    }
    let mut text = String::new();
    canonical(&value, &mut text);
    hex(&Sha256::digest(text.as_bytes()), 12)
}

/// A value fit for one TSV cell.
fn cell(text: &str) -> String {
    text.replace(['\t', '\n', '\r'], " ")
}

// ---- one row ----------------------------------------------------------------------------------------------------

fn samples(raw: &RawImage) -> String {
    let mut hash = Sha256::new();
    let (kind, count) = match &raw.data {
        RawData::U16(samples) => {
            samples.iter().for_each(|s| hash.update(s.to_le_bytes()));
            ("u16", samples.len())
        }
        RawData::F32(samples) => {
            samples.iter().for_each(|s| hash.update(s.to_bits().to_le_bytes()));
            ("f32", samples.len())
        }
    };
    format!("{kind} {count} {}", hex(&hash.finalize(), 16))
}

fn cfa(raw: &RawImage) -> String {
    opt(raw.cfa.as_ref(), |c| {
        let pattern: String = c.pattern.iter().map(|p| p.to_string()).collect();
        format!("{}x{}@{},{}:{pattern}", c.width, c.height, c.origin_x, c.origin_y)
    })
}

fn black(raw: &RawImage) -> String {
    let b = &raw.black;
    let mut text = format!("{}x{}:{}", b.repeat_rows, b.repeat_cols, nums(b.values.iter().map(|v| f64::from(*v))));
    if !b.delta_h.is_empty() || !b.delta_v.is_empty() {
        let _ = write!(text, " dh{} dv{} {}", b.delta_h.len(), b.delta_v.len(), digest(&(&b.delta_h, &b.delta_v)));
    }
    text
}

fn rect(r: lightcraft_raw::Rect) -> String {
    format!("{},{},{},{}", r.x, r.y, r.width, r.height)
}

fn kelvin(raw: &RawImage) -> String {
    // `KelvinPresets` has no public accessors; count from its serialized form.
    opt(raw.color.kelvin_presets.as_ref(), |k| {
        let value = serde_json::to_value(k).unwrap_or(Value::Null);
        let points = value.get("points").and_then(Value::as_array).map_or(0, Vec::len);
        let pair = if value.get("pair").is_some_and(|p| !p.is_null()) { "+pair" } else { "" };
        format!("{points}pts{pair} {}", digest(k))
    })
}

fn opcodes(raw: &RawImage) -> String {
    let lists = serde_json::to_value(&raw.opcodes).unwrap_or(Value::Null);
    let names = |list: &str| -> String {
        let items = lists.get(list).and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        let names: Vec<String> = items
            .iter()
            .map(|op| match op {
                Value::String(name) => name.clone(),
                Value::Object(map) => map.keys().next().cloned().unwrap_or_default(),
                _ => "?".to_owned(),
            })
            .collect();
        names.join(",")
    };
    let text = format!("1:{};2:{};3:{}", names("list1"), names("list2"), names("list3"));
    if text == "1:;2:;3:" { "-".to_owned() } else { text }
}

fn color_columns(raw: &RawImage) -> Vec<String> {
    let c = &raw.color;
    let wb = raw.wb_multipliers.map(|m| m.map(f64::from));
    let matrices = (&c.color_matrix, &c.forward_matrix, &c.camera_calibration, &c.analog_balance, &c.baseline_sharpness);
    vec![
        opt(wb, nums),
        opt(c.as_shot_neutral, nums),
        opt(c.as_shot_white_xy, |xy| nums([xy.x, xy.y])),
        num(c.baseline_exposure),
        format!("{},{}", c.illuminant[0], c.illuminant[1]),
        kelvin(raw),
        digest(&matrices),
        digest(&c.profile),
    ]
}

fn metadata_columns(raw: &RawImage) -> Vec<String> {
    let m = &raw.metadata;
    let text = |s: &Option<String>| opt(s.as_deref(), cell);
    let exposure = format!("{} f{} {}mm", opt(m.exposure_time, num), opt(m.f_number, num), opt(m.focal_length, num));
    vec![
        format!("{}|{}", text(&m.make), text(&m.model)),
        opt(m.iso, |iso| iso.to_string()),
        exposure,
        text(&m.lens_model),
        opt(m.capture_time.as_ref(), |t| cell(&format!("{t:?}"))),
        digest(m),
        digest(&raw.lens_metadata),
    ]
}

fn decoded_columns(raw: &RawImage) -> Vec<String> {
    let mut columns = vec![
        "ok".to_owned(),
        format!("{:?}", raw.format),
        format!("{}x{}x{}", raw.width, raw.height, raw.cpp),
        raw.bits.to_string(),
        samples(raw),
        cfa(raw),
        black(raw),
        nums(raw.white.iter().map(|v| f64::from(*v))),
        rect(raw.active_area),
        rect(raw.crop),
        format!("{:?}", raw.orientation),
        num(f64::from(raw.pixel_aspect)),
        raw.linearized.to_string(),
    ];
    columns.extend(color_columns(raw));
    columns.push(opcodes(raw));
    columns.push(digest(&raw.opcodes));
    columns.extend(metadata_columns(raw));
    columns
}

fn row(name: &str, bytes: &[u8]) -> Vec<String> {
    let mut columns = vec![name.to_owned(), hex(&Sha256::digest(bytes), 16)];
    match std::panic::catch_unwind(|| decode(bytes)) {
        Ok(Ok(raw)) => columns.extend(decoded_columns(&raw)),
        Ok(Err(error)) => {
            columns.push(cell(&format!("error: {error}")));
            columns.resize(COLUMNS.len(), "-".to_owned());
        }
        Err(_) => panic!("{name}: decode panicked"),
    }
    assert_eq!(columns.len(), COLUMNS.len(), "{name}: column count");
    columns
}

// ---- the corpus and the committed file --------------------------------------------------------------------------

/// Every file in the corpus raw directories, as `(path relative to the corpus root, absolute path)`, sorted.
fn corpus_files(root: &Path) -> Vec<(String, PathBuf)> {
    let mut files = Vec::new();
    for dir in CORPUS_DIRS {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else { continue };
        for path in entries.flatten().map(|e| e.path()).filter(|p| p.is_file()) {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if !name.ends_with(".part") {
                files.push((format!("{dir}/{name}"), path));
            }
        }
    }
    files.sort();
    files
}

/// Decode `files` on a few threads; a map from corpus-relative name to row.
fn decode_rows(files: &[(String, PathBuf)]) -> BTreeMap<String, Vec<String>> {
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(6));
    let next = AtomicUsize::new(0);
    let rows = Mutex::new(BTreeMap::new());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                while let Some((name, path)) = files.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{name}: {e}"));
                    let row = row(name, &bytes);
                    rows.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(name.clone(), row);
                }
            });
        }
    });
    rows.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The `# lock` header lines for `lightcraft-raw`'s git dependencies in a `Cargo.lock`, sorted.
fn lock_lines(lockfile: &str) -> Vec<String> {
    let packages = lock_packages(lockfile);
    let mut seen = BTreeSet::new();
    let mut queue: Vec<usize> =
        packages.iter().enumerate().filter(|(_, p)| p.name == "lightcraft-raw" && p.source.is_none()).map(|(i, _)| i).collect();
    while let Some(index) = queue.pop() {
        if !seen.insert(index) {
            continue;
        }
        for spec in &packages[index].dependencies {
            queue.extend(resolve(&packages, spec));
        }
    }
    let lines: BTreeSet<String> = seen
        .iter()
        .filter_map(|&i| {
            let p = &packages[i];
            let source = p.source.filter(|s| s.starts_with("git+"))?;
            let rev = source.rsplit_once('#').map_or(source, |(_, rev)| rev);
            Some(format!("# lock\t{}\t{rev}", p.name))
        })
        .collect();
    lines.into_iter().collect()
}

#[derive(Default)]
struct LockPackage<'a> {
    name: &'a str,
    version: &'a str,
    source: Option<&'a str>,
    dependencies: Vec<&'a str>,
}

/// The quoted value of a `key = "value"` line.
fn lock_value(line: &str) -> Option<&str> {
    line.split_once('=').map(|(_, v)| v.trim().trim_matches('"'))
}

fn lock_packages(lockfile: &str) -> Vec<LockPackage<'_>> {
    let mut packages: Vec<LockPackage<'_>> = Vec::new();
    let mut in_dependencies = false;
    for line in lockfile.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            packages.push(LockPackage::default());
            in_dependencies = false;
            continue;
        }
        let Some(package) = packages.last_mut() else { continue };
        if in_dependencies {
            if line.starts_with(']') {
                in_dependencies = false;
            } else {
                package.dependencies.push(line.trim_end_matches(',').trim_matches('"'));
            }
        } else if line.starts_with("name = ") {
            package.name = lock_value(line).unwrap_or_default();
        } else if line.starts_with("version = ") {
            package.version = lock_value(line).unwrap_or_default();
        } else if line.starts_with("source = ") {
            package.source = lock_value(line);
        } else if line.starts_with("dependencies = [") {
            in_dependencies = !line.ends_with(']');
        }
    }
    packages
}

/// A lockfile source without its `#<commit>` suffix, which dependency entries leave out.
fn without_fragment(source: &str) -> &str {
    source.split_once('#').map_or(source, |(url, _)| url)
}

/// The packages a `Cargo.lock` dependency entry (`name`, `name version` or `name version (source)`) names.
fn resolve(packages: &[LockPackage<'_>], spec: &str) -> Vec<usize> {
    let mut parts = spec.splitn(3, ' ');
    let name = parts.next().unwrap_or_default();
    let version = parts.next();
    let source = parts.next().map(|s| s.trim_start_matches('(').trim_end_matches(')'));
    packages
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            p.name == name
                && version.is_none_or(|v| p.version == v)
                && source.is_none_or(|s| p.source.map(without_fragment) == Some(without_fragment(s)))
        })
        .map(|(i, _)| i)
        .collect()
}

/// The baseline's rows by file name, after checking its column header.
fn baseline_rows(text: &str) -> BTreeMap<String, Vec<String>> {
    let mut lines = text.lines().filter(|line| !line.starts_with('#'));
    let header = lines.next().unwrap_or_default();
    assert_eq!(header, COLUMNS.join("\t"), "docs/raw/decode-baseline.tsv: the columns changed; regenerate it with\n  {REGENERATE}");
    lines
        .map(|line| {
            let columns: Vec<String> = line.split('\t').map(str::to_owned).collect();
            assert_eq!(columns.len(), COLUMNS.len(), "docs/raw/decode-baseline.tsv: bad row {line}");
            (columns[0].clone(), columns)
        })
        .collect()
}

fn write_baseline(rows: &BTreeMap<String, Vec<String>>) {
    let mut text = String::new();
    text.push_str("# LightCraft decode baseline (#4577): each CC0 corpus raw as lightcraft_raw::decode returns it.\n");
    text.push_str("# Written by crates/raw/tests/it/decode_baseline.rs; never edit by hand. After a pin bump or a decode change:\n");
    let _ = writeln!(text, "#   {REGENERATE}");
    text.push_str("# and explain every changed row in the pull request. Floats keep 7 significant digits; *_digest columns are the\n");
    text.push_str("# first 12 hex digits of a SHA-256 over canonical JSON. `# lock` lines: lightcraft-raw's git dependencies in Cargo.lock.\n");
    for line in lock_lines(LOCKFILE) {
        let _ = writeln!(text, "{line}");
    }
    let _ = writeln!(text, "{}", COLUMNS.join("\t"));
    for row in rows.values() {
        let _ = writeln!(text, "{}", row.join("\t"));
    }
    std::fs::write(baseline_path(), text).unwrap_or_else(|e| panic!("write {}: {e}", baseline_path().display()));
}

/// Every difference between the baseline and the decoded rows, one line each.
fn differences(baseline: &BTreeMap<String, Vec<String>>, decoded: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    let mut out = Vec::new();
    for (name, row) in decoded {
        let Some(expected) = baseline.get(name) else {
            out.push(format!("{name}: in the corpus but not in the baseline"));
            continue;
        };
        for ((column, want), got) in COLUMNS.iter().zip(expected).zip(row) {
            if want != got {
                out.push(format!("{name}: {column}: baseline {want}, decoded {got}"));
            }
        }
    }
    out
}

#[test]
fn decode_baseline_matches_corpus() {
    let root = corpus_root();
    let files = corpus_files(&root);
    if files.is_empty() {
        lightcraft_testkit::skip!(Corpus, "no raws under {}/{{{}}}", root.display(), CORPUS_DIRS.join(","));
        return;
    }
    let baseline = baseline_rows(BASELINE);
    let present: BTreeSet<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    let absent: Vec<&String> = baseline.keys().filter(|name| !present.contains(name.as_str())).collect();
    let writing = std::env::var_os(WRITE_ENV).is_some_and(|v| v == "1");
    let required = std::env::var_os("LIGHTCRAFT_CORPUS_REQUIRED").is_some_and(|v| v == "1");
    assert!(
        !(writing || required) || absent.is_empty(),
        "the corpus lacks {} baseline file(s), e.g. {:?}; run `cargo xtask corpus --download`",
        absent.len(),
        absent.first()
    );
    let decoded = decode_rows(&files);
    if writing {
        write_baseline(&decoded);
        eprintln!("wrote {} rows to {}", decoded.len(), baseline_path().display());
        return;
    }
    if !absent.is_empty() {
        lightcraft_testkit::skip!(Corpus, "{} of {} baseline files absent under {}", absent.len(), baseline.len(), root.display());
    }
    let differences = differences(&baseline, &decoded);
    let shown: Vec<&String> = differences.iter().take(40).collect();
    assert!(
        differences.is_empty(),
        "{} decode difference(s) from docs/raw/decode-baseline.tsv:\n  {}\nIf they are intended, regenerate the baseline and explain them in the pull request:\n  {REGENERATE}",
        differences.len(),
        shown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n  ")
    );
    eprintln!("decode baseline: {} files match", decoded.len());
}

#[test]
fn decode_baseline_matches_lockfile() {
    let recorded: Vec<&str> = BASELINE.lines().filter(|line| line.starts_with("# lock\t")).collect();
    let current = lock_lines(LOCKFILE);
    assert!(!current.is_empty(), "no git dependencies of lightcraft-raw found in Cargo.lock");
    assert_eq!(
        recorded,
        current.iter().map(String::as_str).collect::<Vec<_>>(),
        "Cargo.lock's git dependencies of lightcraft-raw (right) differ from those docs/raw/decode-baseline.tsv was \
         written with (left). After a pin bump, run the corpus check and commit the regenerated baseline, explaining \
         every changed row in the pull request:\n  {REGENERATE}"
    );
}

#[test]
fn lock_lines_follow_dependencies_and_ignore_unrelated_packages() {
    let lock = r#"
[[package]]
name = "lightcraft-raw"
version = "0.1.0"
dependencies = [
 "craft-raw",
 "craft-tiff 0.1.0 (git+https://example.org/libs?rev=bbbb)",
 "serde",
]

[[package]]
name = "craft-raw"
version = "0.1.0"
source = "git+https://example.org/libs?rev=aaaa#aaaa"
dependencies = [
 "craft-tiff 0.1.0 (git+https://example.org/libs?rev=aaaa)",
]

[[package]]
name = "craft-tiff"
version = "0.1.0"
source = "git+https://example.org/libs?rev=aaaa#aaaa"

[[package]]
name = "craft-tiff"
version = "0.1.0"
source = "git+https://example.org/libs?rev=bbbb#bbbb"

[[package]]
name = "craft-zip"
version = "0.1.0"
source = "git+https://example.org/libs?rev=cccc#cccc"

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;
    assert_eq!(lock_lines(lock), ["# lock\tcraft-raw\taaaa", "# lock\tcraft-tiff\taaaa", "# lock\tcraft-tiff\tbbbb"]);
}

#[test]
fn floats_keep_seven_significant_digits() {
    assert_eq!(num(0.47), "0.47");
    assert_eq!(num(0.015579343267122363), num(0.015579343267122337));
    assert_ne!(num(0.0155793), num(0.0155794));
    assert_eq!(num(f64::from(0.1f32)), "0.1");
    assert_eq!(num(-0.0), "-0");
}
