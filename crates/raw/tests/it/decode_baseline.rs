//! The committed decode baseline (`docs/raw/decode-baseline.tsv`, #4577): one row per pinned CC0 corpus raw with what
//! `lightcraft_raw::decode` returns, craft-raw and LightCraft's adapter together.
//!
//! - `decode_baseline_matches_corpus` decodes every pinned corpus raw (the manifests of `cargo xtask corpus
//!   --download`: `RAW_SAMPLES` in `xtask/src/raw_samples.rs` and the D750 set in `xtask/src/corpus.rs`) and fails,
//!   naming the file and column, when a value differs from the baseline. Without the corpus it skips, as in CI. With
//!   part of it, it checks the files present and records a skip for the rest. Other files in the corpus directories
//!   are ignored.
//! - Two tests need no corpus and run in CI. `decode_baseline_file_is_well_formed` checks the committed file: its
//!   columns, one sorted row per pinned file and no others. `decode_baseline_matches_lockfile` compares the git
//!   revisions the baseline was written with (its `# lock` lines) against `Cargo.lock`, so a craft-libs pin bump
//!   fails until the baseline is regenerated.
//!
//! After a pin bump, a change to the corpus manifest or any other decode change, regenerate the baseline and review
//! the diff. Every changed row is a decode change the pull request should explain:
//! `LIGHTCRAFT_WRITE_DECODE_BASELINE=1 cargo test -p lightcraft-raw --test it decode_baseline_matches_corpus`
//! Writing needs every pinned file and never reads the old baseline, so a change of columns or files regenerates too.
//! To see which cells changed: `git diff --word-diff-regex='[^[:space:]]+' docs/raw/decode-baseline.tsv`.
//!
//! The explicit columns are the review surface. The `*_digest` columns are a safety net over whole structures
//! (`image_digest`: the whole `RawImage` but its samples), so a field no explicit column shows still changes a row.
//! Digests leave out `None` / null fields, so a new optional field that no file sets changes nothing. Floats keep 7
//! significant digits and zero has no sign, so the last-bit differences between platforms' maths libraries don't
//! show (Linux and Windows differ by about 3e-17 in some Nikon `WarpRectilinear` coefficients). Samples are hashed
//! bit for bit, `f32` ones included: today's one float raw is a 16-bit float DNG, decoded without arithmetic.

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
const WORKSPACE_MANIFEST: &str = include_str!("../../../../Cargo.toml");
const PACKAGE_MANIFEST: &str = include_str!("../../Cargo.toml");
const RAW_SAMPLES_SOURCE: &str = include_str!("../../../../xtask/src/raw_samples.rs");
const CORPUS_SOURCE: &str = include_str!("../../../../xtask/src/corpus.rs");
const WRITE_ENV: &str = "LIGHTCRAFT_WRITE_DECODE_BASELINE";
const REGENERATE: &str = "LIGHTCRAFT_WRITE_DECODE_BASELINE=1 cargo test -p lightcraft-raw --test it decode_baseline_matches_corpus";
const COLUMNS: [&str; 34] = [
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
    "kelvin_as_shot",
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
    "image_digest",
];

fn corpus_root() -> PathBuf {
    std::env::var_os("LIGHTCRAFT_CORPUS").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
}

fn baseline_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/raw/decode-baseline.tsv")
}

fn env_is_1(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| v == "1")
}

// ---- formatting -------------------------------------------------------------------------------------------------

/// A float with 7 significant digits, printed as the shortest decimal that reads back as the rounded value; zero
/// unsigned.
fn num(x: f64) -> String {
    if !x.is_finite() {
        return format!("{x}");
    }
    let rounded: f64 = format!("{x:.6e}").parse().unwrap_or(x);
    if rounded == 0.0 { "0".to_owned() } else { format!("{rounded}") }
}

fn nums(values: impl IntoIterator<Item = f64>) -> String {
    values.into_iter().map(num).collect::<Vec<_>>().join(",")
}

fn opt<T>(value: Option<T>, show: impl FnOnce(T) -> String) -> String {
    value.map_or_else(|| "-".to_owned(), show)
}

/// Canonical JSON: sorted keys, rounded floats, no null object entries.
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
            let sorted: BTreeMap<_, _> = map.iter().filter(|(_, item)| !item.is_null()).collect();
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

fn json(value: &impl serde::Serialize) -> Value {
    serde_json::to_value(value).unwrap_or_else(|e| panic!("{}: does not serialize: {e}", std::any::type_name_of_val(value)))
}

/// The first 12 hex digits of a SHA-256 over the value's canonical JSON; `-` for a `None` / null value.
fn digest(value: &impl serde::Serialize) -> String {
    let value = json(value);
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

fn samples(data: &RawData) -> String {
    let mut hash = Sha256::new();
    let (kind, count) = match data {
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

/// `KelvinPresets` keeps its points private; count them from its serialized form, and fail loudly if that changes.
fn kelvin(raw: &RawImage) -> String {
    opt(raw.color.kelvin_presets.as_ref(), |k| {
        let value = json(k);
        let points = value.get("points").and_then(Value::as_array).map(Vec::len);
        let points = points.unwrap_or_else(|| panic!("KelvinPresets serializes without a `points` array: update kelvin() in decode_baseline.rs"));
        let pair = if value.get("pair").is_some_and(|p| !p.is_null()) { "+pair" } else { "" };
        format!("{points}pts{pair} {}", digest(k))
    })
}

/// The as-shot temperature and tint the presets give the file's own multipliers: how a wrong Kelvin scale shows.
fn kelvin_as_shot(raw: &RawImage) -> String {
    let as_shot = raw.color.kelvin_presets.as_ref().zip(raw.wb_multipliers).and_then(|(k, wb)| k.temp_tint(wb));
    opt(as_shot, |(temp, tint)| format!("{}K {}", num(temp), num(tint)))
}

fn opcodes(raw: &RawImage) -> String {
    let lists = json(&raw.opcodes);
    let names = |list: &str| -> String {
        let items = lists.get(list).and_then(Value::as_array).unwrap_or_else(|| panic!("OpcodeLists serializes without `{list}`: update opcodes()"));
        let names: Vec<String> = items
            .iter()
            .map(|op| match op {
                Value::String(name) => name.clone(),
                Value::Object(map) if map.len() == 1 => map.keys().next().cloned().unwrap_or_default(),
                other => panic!("an opcode serializes as neither a name nor one tagged object: {other}; update opcodes()"),
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
    vec![
        opt(wb, nums),
        opt(c.as_shot_neutral, nums),
        opt(c.as_shot_white_xy, |xy| nums([xy.x, xy.y])),
        num(c.baseline_exposure),
        format!("{},{}", c.illuminant[0], c.illuminant[1]),
        kelvin(raw),
        kelvin_as_shot(raw),
        digest(c),
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
        opt(m.capture_time.as_ref(), |t| cell(&t.to_iso())),
        digest(m),
        digest(&raw.lens_metadata),
    ]
}

/// Every column after `status` for a decoded file. Takes the image to hash its samples once and then digest the
/// rest of it (`image_digest`) without them.
fn decoded_columns(mut raw: RawImage) -> Vec<String> {
    let samples = samples(&raw.data);
    raw.data = RawData::U16(Vec::new());
    let mut columns = vec![
        "ok".to_owned(),
        format!("{:?}", raw.format),
        format!("{}x{}x{}", raw.width, raw.height, raw.cpp),
        raw.bits.to_string(),
        samples,
        cfa(&raw),
        black(&raw),
        nums(raw.white.iter().map(|v| f64::from(*v))),
        rect(raw.active_area),
        rect(raw.crop),
        format!("{:?}", raw.orientation),
        num(f64::from(raw.pixel_aspect)),
        raw.linearized.to_string(),
    ];
    columns.extend(color_columns(&raw));
    columns.push(opcodes(&raw));
    columns.push(digest(&raw.opcodes));
    columns.extend(metadata_columns(&raw));
    columns.push(digest(&raw));
    columns
}

/// The row for one pinned file, after checking the bytes are the pinned file.
fn row(name: &str, pinned_sha256: &str, bytes: &[u8]) -> Vec<String> {
    let sha256 = Sha256::digest(bytes);
    assert_eq!(hex(&sha256, 64), pinned_sha256, "{name}: corpus file differs from its pinned SHA-256; run `cargo xtask corpus --download`");
    let mut columns = vec![name.to_owned(), hex(&sha256, 16)];
    match std::panic::catch_unwind(|| decode(bytes)) {
        Ok(Ok(raw)) => columns.extend(decoded_columns(raw)),
        Ok(Err(error)) => {
            columns.push(cell(&format!("error: {error}")));
            columns.resize(COLUMNS.len(), "-".to_owned());
        }
        Err(_) => panic!("{name}: decode panicked"),
    }
    assert_eq!(columns.len(), COLUMNS.len(), "{name}: column count");
    columns
}

// ---- the corpus manifest ----------------------------------------------------------------------------------------

/// The string literals of the `&[(&str, &str, &str)]` table that starts at `start` in `source`, comment lines skipped.
fn table_literals(source: &str, start: &str) -> Vec<String> {
    let from = source.find(start).unwrap_or_else(|| panic!("`{start}` not found: update decode_baseline.rs"));
    let body = &source[from..];
    let body = &body[..body.find("\n];").unwrap_or(body.len())];
    let mut literals = Vec::new();
    for line in body.lines().skip(1).filter(|line| !line.trim_start().starts_with("//")) {
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            if c != '"' {
                continue;
            }
            let mut literal = String::new();
            while let Some(c) = chars.next() {
                match c {
                    '"' => break,
                    '\\' => literal.extend(chars.next()),
                    _ => literal.push(c),
                }
            }
            literals.push(literal);
        }
    }
    assert_eq!(literals.len() % 3, 0, "`{start}`: expected (path, URL, SHA-256) triples");
    literals
}

/// The pinned corpus raws as `(path relative to the corpus root, SHA-256)`, sorted: what `cargo xtask corpus
/// --download` puts under `raw/` and `d750-colorchecker/raw/`.
fn manifest() -> Vec<(String, String)> {
    let raws = table_literals(RAW_SAMPLES_SOURCE, "pub(crate) const RAW_SAMPLES");
    let d750 = table_literals(CORPUS_SOURCE, "const D750_COLORCHECKER");
    let mut files: Vec<(String, String)> = raws.chunks(3).map(|t| (format!("raw/{}", t[0]), t[2].clone())).collect();
    files.extend(d750.chunks(3).filter_map(|t| t[0].strip_prefix("raw/").map(|name| (format!("d750-colorchecker/raw/{name}"), t[2].clone()))));
    files.sort();
    files
}

/// Decode the pinned `files` present under `root` on a few threads; a map from corpus-relative name to row.
fn decode_rows(root: &Path, files: &[&(String, String)]) -> BTreeMap<String, Vec<String>> {
    // Decodes peak near 700 MB each on the largest files; other corpus tests share the test binary.
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(4));
    let next = AtomicUsize::new(0);
    let rows = Mutex::new(BTreeMap::new());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                while let Some((name, sha256)) = files.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let bytes = std::fs::read(root.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
                    let row = row(name, sha256, &bytes);
                    rows.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(name.clone(), row);
                }
            });
        }
    });
    rows.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ---- Cargo.lock -------------------------------------------------------------------------------------------------

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

/// The quoted entries of a dependency list, inline (`["a", "b"]`) or one per line.
fn quoted_entries(text: &str) -> impl Iterator<Item = &str> {
    text.split(',').map(|entry| entry.trim().trim_start_matches('[').trim_end_matches(']').trim().trim_matches('"')).filter(|entry| !entry.is_empty())
}

/// The `[[package]]` tables of a `Cargo.lock`; every other table (`[metadata]`, `[[patch.unused]]`) is skipped.
fn lock_packages(lockfile: &str) -> Vec<LockPackage<'_>> {
    let mut packages: Vec<LockPackage<'_>> = Vec::new();
    let (mut in_package, mut in_dependencies) = (false, false);
    for line in lockfile.lines().map(str::trim) {
        if in_dependencies {
            in_dependencies = !line.starts_with(']');
            if let Some(package) = packages.last_mut().filter(|_| in_dependencies) {
                package.dependencies.extend(quoted_entries(line));
            }
            continue;
        }
        if line.starts_with('[') {
            in_package = line == "[[package]]";
            if in_package {
                packages.push(LockPackage::default());
            }
            continue;
        }
        let Some(package) = packages.last_mut().filter(|_| in_package) else { continue };
        if line.starts_with("name = ") {
            package.name = lock_value(line).unwrap_or_default();
        } else if line.starts_with("version = ") {
            package.version = lock_value(line).unwrap_or_default();
        } else if line.starts_with("source = ") {
            package.source = lock_value(line);
        } else if let Some(list) = line.strip_prefix("dependencies = ") {
            package.dependencies.extend(quoted_entries(list));
            in_dependencies = !list.trim_end().ends_with(']');
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

/// The packages a manifest lists under `[dev-dependencies]` and not as a normal or build dependency (by package
/// name, following `package = "..."` renames).
fn dev_only_dependencies(manifest: &str) -> BTreeSet<String> {
    let (mut dev, mut other) = (BTreeSet::new(), BTreeSet::new());
    let mut section = "";
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line;
            continue;
        }
        let Some((key, value)) = line.split_once('=').filter(|_| !line.starts_with('#')) else { continue };
        let key = key.trim().split('.').next().unwrap_or_default().trim_matches('"');
        let package = value.split("package = \"").nth(1).and_then(|rest| rest.split('"').next()).unwrap_or(key).to_owned();
        if section == "[dev-dependencies]" {
            dev.insert(package);
        } else if section.ends_with("dependencies]") {
            other.insert(package);
        }
    }
    dev.difference(&other).cloned().collect()
}

/// The `# lock` header lines: every git package `Cargo.lock` reaches from `lightcraft-raw` (leaving out its
/// dev-only dependencies), as `# lock<TAB>name<TAB>commit`, sorted. The lockfile also records optional dependencies
/// whose features are off, so the set can be larger than what compiles.
fn lock_lines(lockfile: &str, dev_only: &BTreeSet<String>) -> Vec<String> {
    let packages = lock_packages(lockfile);
    let roots: Vec<usize> = packages.iter().enumerate().filter(|(_, p)| p.name == "lightcraft-raw" && p.source.is_none()).map(|(i, _)| i).collect();
    let mut seen = BTreeSet::new();
    let mut queue = Vec::new();
    for &root in &roots {
        seen.insert(root);
        let normal = packages[root].dependencies.iter().filter(|spec| !dev_only.contains(spec.split(' ').next().unwrap_or_default()));
        queue.extend(normal.flat_map(|spec| resolve(&packages, spec)));
    }
    while let Some(index) = queue.pop() {
        if seen.insert(index) {
            queue.extend(packages[index].dependencies.iter().flat_map(|spec| resolve(&packages, spec)));
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

fn current_lock_lines() -> Vec<String> {
    lock_lines(LOCKFILE, &dev_only_dependencies(PACKAGE_MANIFEST))
}

// ---- the committed file -----------------------------------------------------------------------------------------

/// The baseline's rows by file name. Fails on a changed column header, a row of the wrong width, an empty cell
/// or rows out of order (duplicates included), which is what a hand-resolved merge conflict leaves.
fn baseline_rows(text: &str) -> BTreeMap<String, Vec<String>> {
    let mut lines = text.lines().filter(|line| !line.starts_with('#'));
    let header = lines.next().unwrap_or_default();
    assert_eq!(header, COLUMNS.join("\t"), "docs/raw/decode-baseline.tsv: the columns changed; regenerate it with\n  {REGENERATE}");
    let mut rows = BTreeMap::new();
    let mut previous = String::new();
    for line in lines {
        let columns: Vec<String> = line.split('\t').map(str::to_owned).collect();
        let fix = format!("regenerate it rather than editing it:\n  {REGENERATE}");
        assert!(
            columns.len() == COLUMNS.len() && columns.iter().all(|c| !c.is_empty()),
            "docs/raw/decode-baseline.tsv: malformed row {line:?}; {fix}"
        );
        assert!(columns[0] > previous, "docs/raw/decode-baseline.tsv: {} is out of order or repeated; {fix}", columns[0]);
        previous = columns[0].clone();
        rows.insert(columns[0].clone(), columns);
    }
    rows
}

fn write_baseline(rows: &BTreeMap<String, Vec<String>>) {
    let mut text = String::new();
    text.push_str("# LightCraft decode baseline (#4577): each pinned CC0 corpus raw as lightcraft_raw::decode returns it.\n");
    text.push_str("# Written by crates/raw/tests/it/decode_baseline.rs; never edit by hand. After a pin bump or a decode change:\n");
    let _ = writeln!(text, "#   {REGENERATE}");
    text.push_str("# and explain every changed row in the pull request. Floats keep 7 significant digits; *_digest columns are the\n");
    text.push_str("# first 12 hex digits of a SHA-256 over canonical JSON. `# lock` lines: the git packages Cargo.lock reaches\n");
    text.push_str("# from lightcraft-raw (dev-only dependencies left out).\n");
    for line in current_lock_lines() {
        let _ = writeln!(text, "{line}");
    }
    let _ = writeln!(text, "{}", COLUMNS.join("\t"));
    for row in rows.values() {
        let _ = writeln!(text, "{}", row.join("\t"));
    }
    std::fs::write(baseline_path(), text).unwrap_or_else(|e| panic!("write {}: {e}", baseline_path().display()));
}

/// Every difference between the baseline and the decoded rows, one line each. Files the baseline has and the
/// corpus run didn't decode are left out (absent files are reported as a skip).
fn differences(baseline: &BTreeMap<String, Vec<String>>, decoded: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    let mut out = Vec::new();
    for (name, row) in decoded {
        let Some(expected) = baseline.get(name) else {
            out.push(format!("{name}: pinned in the corpus manifest but not in the baseline"));
            continue;
        };
        // A file that starts or stops decoding: the status says it, the other columns would only repeat it.
        let decodes = |row: &[String]| row.get(2).is_some_and(|status| status == "ok");
        let status_only = decodes(expected) != decodes(row);
        for ((column, want), got) in COLUMNS.iter().zip(expected).zip(row) {
            if want != got && (!status_only || *column == "status") {
                out.push(format!("{name}: {column}: baseline {want}, decoded {got}"));
            }
        }
    }
    out
}

/// How many files each column changed on, most first: `iso 13, metadata_digest 13`.
fn per_column(differences: &[String]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for line in differences {
        let column = line.split(": ").nth(1).filter(|c| COLUMNS.contains(c)).unwrap_or("rows");
        *counts.entry(column).or_default() += 1;
    }
    let mut counts: Vec<_> = counts.into_iter().collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    counts.iter().map(|(column, n)| format!("{column} {n}")).collect::<Vec<_>>().join(", ")
}

#[test]
fn decode_baseline_matches_corpus() {
    let root = corpus_root();
    let manifest = manifest();
    let (present, absent): (Vec<_>, Vec<_>) = manifest.iter().partition(|(name, _)| root.join(name).is_file());
    let writing = env_is_1(WRITE_ENV);
    let required = env_is_1("LIGHTCRAFT_CORPUS_REQUIRED");
    if writing || required {
        let first = absent.first().map(|(name, _)| name.as_str());
        let mode = if writing { WRITE_ENV } else { "LIGHTCRAFT_CORPUS_REQUIRED" };
        assert!(
            absent.is_empty(),
            "{mode}=1 needs every pinned file: {} of {} absent under {} (e.g. {first:?}); run `cargo xtask corpus --download`",
            absent.len(),
            manifest.len(),
            root.display()
        );
    } else if present.is_empty() {
        lightcraft_testkit::skip!(Corpus, "no pinned corpus raws under {}", root.display());
        return;
    }
    let decoded = decode_rows(&root, &present);
    if writing {
        write_baseline(&decoded);
        eprintln!("wrote {} rows to {}", decoded.len(), baseline_path().display());
        return;
    }
    if !absent.is_empty() {
        lightcraft_testkit::skip!(
            Corpus,
            "{} of {} pinned files absent under {}; checked the {} present",
            absent.len(),
            manifest.len(),
            root.display(),
            present.len()
        );
    }
    let differences = differences(&baseline_rows(BASELINE), &decoded);
    let list = std::env::temp_dir().join("lightcraft-decode-baseline-differences.txt");
    if !differences.is_empty() {
        let _ = std::fs::write(&list, differences.join("\n") + "\n");
    }
    let shown: Vec<&str> = differences.iter().take(40).map(String::as_str).collect();
    assert!(
        differences.is_empty(),
        "{} decode difference(s) from docs/raw/decode-baseline.tsv (files per column: {}); all of them in {}:\n  {}\nIf they \
         are intended, regenerate the baseline and explain them in the pull request:\n  {REGENERATE}",
        differences.len(),
        per_column(&differences),
        list.display(),
        shown.join("\n  ")
    );
    eprintln!("decode baseline: {} files match", decoded.len());
}

#[test]
fn decode_baseline_file_is_well_formed() {
    let rows = baseline_rows(BASELINE);
    let manifest = manifest();
    let pinned: Vec<&str> = manifest.iter().map(|(name, _)| name.as_str()).collect();
    let recorded: Vec<&str> = rows.keys().map(String::as_str).collect();
    assert!(!pinned.is_empty(), "no pinned corpus raws found in xtask's manifests");
    assert_eq!(
        recorded, pinned,
        "docs/raw/decode-baseline.tsv lists other files than the corpus manifest pins (left: baseline, right: manifest); \
         after changing the manifest, download the corpus and regenerate:\n  {REGENERATE}"
    );
}

#[test]
fn decode_baseline_matches_lockfile() {
    let recorded: BTreeSet<&str> = BASELINE.lines().filter(|line| line.starts_with("# lock\t")).collect();
    let current = current_lock_lines();
    let current: BTreeSet<&str> = current.iter().map(String::as_str).collect();
    let stale: Vec<&&str> = recorded.difference(&current).collect();
    let new: Vec<&&str> = current.difference(&recorded).collect();
    assert!(
        stale.is_empty() && new.is_empty(),
        "Cargo.lock's git dependencies of lightcraft-raw changed since docs/raw/decode-baseline.tsv was written.\n  \
         recorded, now gone: {stale:?}\n  in Cargo.lock, not recorded: {new:?}\nRun the corpus check against the new \
         revisions and commit the regenerated baseline, explaining every changed row in the pull request:\n  {REGENERATE}"
    );
}

/// `lightcraft-raw`'s own git dependencies with the revision the workspace pins each at, as `(package, rev)`:
/// what `decode_baseline_matches_lockfile` must at least gate on, read from the manifests rather than the lockfile.
fn pinned_git_dependencies() -> Vec<(String, String)> {
    let mut section = "";
    let mut normal = BTreeSet::new();
    for line in PACKAGE_MANIFEST.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line;
        } else if section == "[dependencies]" && !line.starts_with('#') {
            normal.extend(line.split_once('=').map(|(key, _)| key.trim().split('.').next().unwrap_or_default().to_owned()));
        }
    }
    let quoted = |line: &str, key: &str| line.split(&format!("{key} = \"")).nth(1).and_then(|rest| rest.split('"').next()).map(str::to_owned);
    WORKSPACE_MANIFEST
        .lines()
        .filter_map(|line| {
            let key = line.split_once(" = ")?.0.trim();
            let rev = quoted(line, "rev").filter(|_| normal.contains(key) && line.contains("git = "))?;
            Some((quoted(line, "package").unwrap_or_else(|| key.to_owned()), rev))
        })
        .collect()
}

#[test]
fn lock_lines_include_every_pinned_git_dependency() {
    let pins = pinned_git_dependencies();
    assert!(pins.iter().any(|(package, _)| package == "craft-raw"), "craft-raw not found among {pins:?}");
    let lines = current_lock_lines();
    for (package, rev) in pins {
        let expected = format!("# lock\t{package}\t{rev}");
        assert!(lines.contains(&expected), "{expected:?} missing from {lines:?}");
    }
}

#[test]
fn lock_lines_follow_dependencies_and_skip_dev_only_and_other_tables() {
    let lock = r#"
[[package]]
name = "lightcraft-raw"
version = "0.1.0"
dependencies = [
 "craft-raw",
 "craft-tiff 0.1.0 (git+https://example.org/libs?rev=bbbb)",
 "craft-jxl",
 "serde",
]

[[package]]
name = "craft-raw"
version = "0.1.0"
source = "git+https://example.org/libs?rev=aaaa#aaaa"
dependencies = ["craft-tiff 0.1.0 (git+https://example.org/libs?rev=aaaa)", "craft-crw"]

[[package]]
name = "craft-crw"
version = "0.1.0"
source = "git+https://example.org/libs?rev=aaaa#aaaa"

[[package]]
name = "craft-tiff"
version = "0.1.0"
source = "git+https://example.org/libs?rev=aaaa#aaaa"

[[package]]
name = "craft-tiff"
version = "0.1.0"
source = "git+https://example.org/libs?rev=bbbb#bbbb"

[[package]]
name = "craft-jxl"
version = "0.1.0"
source = "git+https://example.org/libs?rev=dddd#dddd"

[[package]]
name = "craft-zip"
version = "0.1.0"
source = "git+https://example.org/libs?rev=cccc#cccc"

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[patch.unused]]
name = "craft-raw"
version = "0.2.0"
source = "git+https://example.org/libs?rev=eeee#eeee"
"#;
    let manifest = "[dependencies]\ncraft-raw = { workspace = true }\n\n[dev-dependencies]\ncraft-jxl = { workspace = true }\n";
    assert_eq!(dev_only_dependencies(manifest), BTreeSet::from(["craft-jxl".to_owned()]));
    assert_eq!(
        lock_lines(lock, &dev_only_dependencies(manifest)),
        ["# lock\tcraft-crw\taaaa", "# lock\tcraft-raw\taaaa", "# lock\tcraft-tiff\taaaa", "# lock\tcraft-tiff\tbbbb"]
    );
}

#[test]
fn the_manifest_reader_finds_every_pinned_raw() {
    let manifest = manifest();
    let names: BTreeSet<&str> = manifest.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names.len(), manifest.len(), "a corpus file is pinned twice");
    assert!(manifest.iter().all(|(_, sha)| sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit())), "a pin is not a SHA-256");
    assert!(names.contains("raw/coverage-915.dng") && names.contains("d750-colorchecker/raw/chart-indoor.NEF"));
    assert!(!names.iter().any(|name| name.ends_with(".dcp") || name.ends_with(".jpg")), "only the D750 raws are corpus raws");
}

#[test]
fn floats_keep_seven_significant_digits() {
    assert_eq!(num(0.47), "0.47");
    assert_eq!(num(0.015579343267122363), num(0.015579343267122337));
    assert_ne!(num(0.0155793), num(0.0155794));
    assert_eq!(num(f64::from(0.1f32)), "0.1");
    assert_eq!(num(-0.0), "0");
}

#[test]
fn digests_ignore_absent_optional_fields() {
    #[derive(serde::Serialize)]
    struct Before {
        a: f64,
    }
    #[derive(serde::Serialize)]
    struct After {
        a: f64,
        added: Option<u32>,
    }
    assert_eq!(digest(&Before { a: 1.5 }), digest(&After { a: 1.5, added: None }));
    assert_ne!(digest(&Before { a: 1.5 }), digest(&After { a: 1.5, added: Some(1) }));
}

#[test]
fn differences_name_the_file_and_column_and_collapse_a_status_change() {
    let ok = |name: &str, iso: &str| -> Vec<String> {
        COLUMNS
            .iter()
            .map(|column| match *column {
                "file" => name.to_owned(),
                "status" => "ok".to_owned(),
                "iso" => iso.to_owned(),
                _ => "v".to_owned(),
            })
            .collect()
    };
    let failed = |name: &str| -> Vec<String> {
        let mut row = vec![name.to_owned(), "v".to_owned(), "error: corrupt".to_owned()];
        row.resize(COLUMNS.len(), "-".to_owned());
        row
    };
    let baseline = BTreeMap::from([("a".to_owned(), ok("a", "160")), ("b".to_owned(), ok("b", "100")), ("c".to_owned(), ok("c", "100"))]);
    let decoded = BTreeMap::from([
        ("a".to_owned(), ok("a", "-")),
        ("b".to_owned(), failed("b")),
        ("c".to_owned(), ok("c", "100")),
        ("d".to_owned(), ok("d", "100")),
    ]);
    let found = differences(&baseline, &decoded);
    assert_eq!(
        found,
        [
            "a: iso: baseline 160, decoded -",
            "b: status: baseline ok, decoded error: corrupt",
            "d: pinned in the corpus manifest but not in the baseline"
        ]
    );
    assert_eq!(per_column(&found), "iso 1, rows 1, status 1");
}

#[test]
fn a_hand_merged_baseline_fails_to_parse() {
    let header = COLUMNS.join("\t");
    let row =
        |name: &str| std::iter::once(name.to_owned()).chain(std::iter::repeat_n("v".to_owned(), COLUMNS.len() - 1)).collect::<Vec<_>>().join("\t");
    let parses = |text: String| std::panic::catch_unwind(|| baseline_rows(&text)).is_ok();
    assert!(parses(format!("{header}\n{}\n{}\n", row("a"), row("b"))));
    assert!(!parses(format!("{header}\n{}\n{}\n", row("a"), row("a"))), "a repeated row");
    assert!(!parses(format!("{header}\n{}\n{}\n", row("b"), row("a"))), "rows out of order");
    assert!(!parses(format!("{header}\n{}\n<<<<<<< HEAD\n", row("a"))), "a conflict marker");
}
