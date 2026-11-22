//! Deterministic mutation fuzzing for never-crash tests (`CLAUDE.md` → Never crash).
//!
//! No cargo-fuzz, no nightly, no C: a seeded xorshift generator mutates valid seed inputs
//! (bit flips, byte overwrites, truncation, deletion, duplication, interesting tokens, extreme
//! numbers, splicing) so every harness runs in an ordinary `cargo test`, reproducibly, in bounded
//! time. A failing case prints the harness name, iteration and input on stderr; `LCC_FUZZ_ITERS`
//! raises the iteration count for longer local runs
//! (e.g. `LCC_FUZZ_ITERS=200000 cargo test -p lcc-geo --test robust`).
//!
//! Long runs (`cargo xtask fuzz`, nightly CI) use three more variables: `LCC_FUZZ_SECS` time-boxes
//! every harness (it runs until the deadline instead of a fixed count), `LCC_FUZZ_RNG` varies the
//! mutation stream between runs, and `LCC_FUZZ_CRASH_DIR` receives each failing input as
//! `<harness>-<hash>.bin`. `LCC_FUZZ_CORPUS` names a directory whose files [`corpus_seeds`] adds to a
//! harness's built-in seeds (the gitignored `corpus/`).
//!
//! Used from dev-dependencies only. Pure computation; builds everywhere.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

/// A small, fast, seeded PRNG (xorshift64*). Not for cryptography.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform-ish in `0..n` (0 when `n == 0`).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next_u64() % n as u64) as usize }
    }

    pub fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in.max(1)) == 0
    }

    pub fn byte(&mut self) -> u8 {
        (self.next_u64() >> 56) as u8
    }
}

const INTERESTING: &[&[u8]] = &[
    b"\0",
    b"\xff",
    b"\xff\xff\xff\xff",
    b"\x7f\xff\xff\xff",
    b"\x80\0\0\0",
    b"-1",
    b"0",
    b"1e309",
    b"-1e309",
    b"NaN",
    b"18446744073709551615",
    b"9223372036854775808",
    b"4294967296",
    b"null",
    b"true",
    b"\"\"",
    b"[]",
    b"{}",
    b"[[[[[[[[",
    b"{\"a\":{\"a\":{\"a\":",
    b"\"\\u0000\"",
    b"\"\\ud800\"",
    b"\xc3\x28",
    b"\xe2\x82",
    b"\n",
    b"<",
    b">",
    b"&#0;",
    b"&amp",
    b",",
    b":",
];

const NUMBERS: &[&[u8]] = &[b"0", b"4294967295", b"18446744073709551615", b"99999999999999999999999", b"1e308", b"-0", b"-5"];

/// One random mutation of `data` in place.
pub fn mutate_once(rng: &mut Rng, data: &mut Vec<u8>) {
    let len = data.len();
    match rng.below(10) {
        0 if len > 0 => {
            let i = rng.below(len);
            let bit = rng.below(8);
            if let Some(b) = data.get_mut(i) {
                *b ^= 1 << bit;
            }
        }
        1 if len > 0 => {
            let i = rng.below(len);
            let v = rng.byte();
            if let Some(b) = data.get_mut(i) {
                *b = v;
            }
        }
        2 => data.truncate(rng.below(len + 1)),
        3 if len > 0 => {
            let a = rng.below(len);
            let b = (a + 1 + rng.below(16)).min(len);
            data.drain(a..b);
        }
        4 if len > 0 => {
            let a = rng.below(len);
            let b = (a + 1 + rng.below(64)).min(len);
            let chunk: Vec<u8> = data.get(a..b).map(<[u8]>::to_vec).unwrap_or_default();
            let at = rng.below(data.len() + 1);
            data.splice(at..at, chunk);
        }
        5 => {
            let tok = INTERESTING.get(rng.below(INTERESTING.len())).copied().unwrap_or(b"\0");
            let at = rng.below(len + 1);
            if rng.chance(2) && at < len {
                let end = (at + tok.len()).min(len);
                data.splice(at..end, tok.iter().copied());
            } else {
                data.splice(at..at, tok.iter().copied());
            }
        }
        6 => {
            let found = (0..8).map(|_| rng.below(len.max(1))).find(|&i| data.get(i).is_some_and(u8::is_ascii_digit));
            if let Some(start) = found {
                let mut end = start;
                while data.get(end).is_some_and(u8::is_ascii_digit) {
                    end += 1;
                }
                let n = NUMBERS.get(rng.below(NUMBERS.len())).copied().unwrap_or(b"0");
                data.splice(start..end, n.iter().copied());
            }
        }
        7 => {
            for _ in 0..1 + rng.below(8) {
                let at = rng.below(data.len() + 1);
                let b = rng.byte();
                data.insert(at, b);
            }
        }
        8 if len > 1 => {
            let (a, b) = (rng.below(len), rng.below(len));
            data.swap(a, b);
        }
        _ if len > 0 => {
            let i = rng.below(len);
            if let Some(b) = data.get_mut(i) {
                *b = b'"';
            }
        }
        _ => data.push(rng.byte()),
    }
}

/// 1..=`max` stacked mutations of a copy of `seed`.
pub fn mutate(rng: &mut Rng, seed: &[u8], max: usize) -> Vec<u8> {
    let mut v = seed.to_vec();
    for _ in 0..1 + rng.below(max.max(1)) {
        mutate_once(rng, &mut v);
    }
    v
}

/// Iterations per harness: `LCC_FUZZ_ITERS` or `default`.
pub fn iterations(default: usize) -> usize {
    std::env::var("LCC_FUZZ_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(default)
}

/// How long a harness runs: `LCC_FUZZ_SECS` (a deadline, for long runs) or a fixed count.
struct Budget {
    count: usize,
    deadline: Option<std::time::Instant>,
}

impl Budget {
    fn new(default: usize) -> Budget {
        Budget::share(default, 1)
    }

    /// `1/parts` of the `LCC_FUZZ_SECS` time box (harnesses with several phases split it, so the
    /// whole harness still ends at the deadline).
    fn share(default: usize, parts: u32) -> Budget {
        let secs = std::env::var("LCC_FUZZ_SECS").ok().and_then(|s| s.parse::<u64>().ok()).filter(|&s| s > 0);
        let deadline = secs.map(|s| std::time::Duration::from_secs(s) / parts.max(1)).and_then(|d| std::time::Instant::now().checked_add(d));
        Budget { count: iterations(default), deadline }
    }

    fn more(&self, i: usize) -> bool {
        match self.deadline {
            Some(d) => std::time::Instant::now() < d,
            None => i < self.count,
        }
    }
}

fn fnv(salt: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(salt, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3))
}

/// The generator seed for harness `name`, varied by `LCC_FUZZ_RNG` for long runs.
fn rng_for(name: &str, salt: u64) -> Rng {
    let extra = std::env::var("LCC_FUZZ_RNG").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    Rng::new(fnv(salt, name.as_bytes()) ^ extra.wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

/// Files under `LCC_FUZZ_CORPUS` whose extension is one of `exts` (case-insensitive; `"*"` takes
/// any), smallest first: at most `max_files`, each at most `max_bytes`. Empty when the variable is
/// unset or the directory is unreadable, so harnesses always fall back on their built-in seeds.
pub fn corpus_seeds(exts: &[&str], max_files: usize, max_bytes: u64) -> Vec<Vec<u8>> {
    match std::env::var_os("LCC_FUZZ_CORPUS") {
        Some(dir) => seeds_in(std::path::Path::new(&dir), exts, max_files, max_bytes),
        None => Vec::new(),
    }
}

fn seeds_in(dir: &std::path::Path, exts: &[&str], max_files: usize, max_bytes: u64) -> Vec<Vec<u8>> {
    let mut found: Vec<(u64, std::path::PathBuf)> = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let path = e.path();
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                if depth < 8 {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("").to_ascii_lowercase();
            if meta.len() <= max_bytes && exts.iter().any(|w| *w == "*" || w.eq_ignore_ascii_case(&ext)) {
                found.push((meta.len(), path));
            }
        }
    }
    found.sort();
    found.into_iter().take(max_files).filter_map(|(_, p)| std::fs::read(p).ok()).collect()
}

/// [`run`] over `builtin` seeds plus [`corpus_seeds`] for `exts` (up to 32 files of at most 8 MB):
/// the shape of a `fuzz_target` harness that `cargo xtask fuzz` drives.
pub fn run_target(name: &str, builtin: &[Vec<u8>], exts: &[&str], default: usize, check: impl FnMut(&[u8])) {
    let mut all: Vec<Vec<u8>> = builtin.to_vec();
    all.extend(corpus_seeds(exts, 32, 8 << 20));
    let refs: Vec<&[u8]> = all.iter().map(Vec::as_slice).collect();
    run(name, &refs, default, check);
}

/// Writes a failing input to `dir` as `<harness>-<hash>.bin` and returns its path.
fn save_crash(dir: &std::path::Path, name: &str, input: &[u8]) -> std::io::Result<std::path::PathBuf> {
    let file: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    let path = dir.join(format!("{file}-{:016x}.bin", fnv(0xcbf2_9ce4_8422_2325, input)));
    std::fs::create_dir_all(dir)?;
    std::fs::write(&path, input)?;
    Ok(path)
}

/// Runs `check` on every seed, every prefix of the shortest seed, and `iterations(default)`
/// mutations of the seeds (or, with `LCC_FUZZ_SECS`, mutations until that deadline). A panic
/// inside `check` fails the test; the harness name, iteration and offending input are printed on
/// stderr first (and saved under `LCC_FUZZ_CRASH_DIR` when set).
pub fn run(name: &str, seeds: &[&[u8]], default: usize, check: impl FnMut(&[u8])) {
    run_in(name, seeds, Budget::new(default), check);
}

fn run_in(name: &str, seeds: &[&[u8]], budget: Budget, mut check: impl FnMut(&[u8])) {
    for s in seeds {
        let _report = Reporter { name, i: 0, input: s };
        check(s);
    }
    if let Some(short) = seeds.iter().min_by_key(|s| s.len()) {
        for n in 0..short.len().min(4096) {
            let input = short.get(..n).unwrap_or_default();
            let _report = Reporter { name, i: n, input };
            check(input);
        }
    }
    if seeds.is_empty() {
        return;
    }
    let watch = Watchdog::start(name);
    let mut rng = rng_for(name, 0xcbf2_9ce4_8422_2325);
    let mut i = 0usize;
    while budget.more(i) {
        i += 1;
        let seed = seeds.get(rng.below(seeds.len())).copied().unwrap_or_default();
        let mut input = mutate(&mut rng, seed, 6);
        if rng.chance(8) {
            let other = seeds.get(rng.below(seeds.len())).copied().unwrap_or_default();
            let at = rng.below(input.len() + 1);
            let from = rng.below(other.len() + 1);
            input.truncate(at);
            input.extend_from_slice(other.get(from..).unwrap_or_default());
        }
        watch.begin(i, &input);
        let _report = Reporter { name, i, input: &input };
        check(&input);
    }
    watch.stop();
}

/// Seconds one input may take before it counts as a hang: `LCC_FUZZ_HANG_SECS` (default 60).
fn hang_secs() -> u64 {
    std::env::var("LCC_FUZZ_HANG_SECS").ok().and_then(|s| s.parse().ok()).filter(|&s| s > 0).unwrap_or(60)
}

/// What the watchdog sees: the input in flight and when it started; `None` once the run ends.
type Current = std::sync::Arc<std::sync::Mutex<Option<(std::time::Instant, usize, Vec<u8>)>>>;

/// A wall-clock deadline per input. A blocked `check` would otherwise stall a time-boxed run forever
/// (the budget is only consulted between inputs); a hang is reported like a crash (stderr + saved
/// under `LCC_FUZZ_CRASH_DIR`) and the process exits with a failure.
struct Watchdog {
    current: Current,
}

impl Watchdog {
    fn start(name: &str) -> Watchdog {
        let current: Current = std::sync::Arc::new(std::sync::Mutex::new(None));
        let weak = std::sync::Arc::downgrade(&current);
        let name = name.to_string();
        let limit = std::time::Duration::from_secs(hang_secs());
        let spawned = std::thread::Builder::new().name("fuzz-watchdog".into()).spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(250));
                // The run dropped its `Watchdog`: done.
                let Some(strong) = weak.upgrade() else { return };
                let guard = strong.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some((since, i, input)) = guard.as_ref() else { continue };
                if since.elapsed() >= limit {
                    eprintln!("fuzz harness `{name}` HANG at iteration {i}: one input ran over {}s; input ({} bytes): {:02x?}", limit.as_secs(), input.len(), input.get(..input.len().min(256)).unwrap_or_default());
                    if let Some(dir) = std::env::var_os("LCC_FUZZ_CRASH_DIR") {
                        match save_crash(std::path::Path::new(&dir), &format!("{name}-hang"), input) {
                            Ok(p) => eprintln!("hang input saved to {}", p.display()),
                            Err(e) => eprintln!("could not save the hang input: {e}"),
                        }
                    }
                    std::process::exit(102);
                }
            }
        });
        if let Err(e) = spawned {
            eprintln!("fuzz harness: no hang watchdog ({e})");
        }
        Watchdog { current }
    }

    fn begin(&self, i: usize, input: &[u8]) {
        *self.current.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((std::time::Instant::now(), i, input.to_vec()));
    }

    fn stop(&self) {
        *self.current.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

/// [`run`] for text formats: each input is converted lossily to `&str`.
pub fn run_str(name: &str, seeds: &[&str], default: usize, mut check: impl FnMut(&str)) {
    let bytes: Vec<&[u8]> = seeds.iter().map(|s| s.as_bytes()).collect();
    run(name, &bytes, default, |b| check(&String::from_utf8_lossy(b)));
}

fn count_nodes(v: &serde_json::Value) -> usize {
    1 + match v {
        serde_json::Value::Array(a) => a.iter().map(count_nodes).sum(),
        serde_json::Value::Object(o) => o.values().map(count_nodes).sum(),
        _ => 0,
    }
}

/// Applies `f` to the `n`-th node in pre-order.
fn with_nth(v: &mut serde_json::Value, n: &mut usize, f: &mut dyn FnMut(&mut serde_json::Value)) -> bool {
    if *n == 0 {
        f(v);
        return true;
    }
    *n -= 1;
    match v {
        serde_json::Value::Array(a) => a.iter_mut().any(|c| with_nth(c, n, f)),
        serde_json::Value::Object(o) => o.values_mut().any(|c| with_nth(c, n, f)),
        _ => false,
    }
}

fn hostile_value(rng: &mut Rng, old: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    let numbers = [0.0, -1.0, -0.5, 0.5, 1e-300, 1e9, 4_294_967_296.0, 1e19, 1e300, -1e300, f64::from(u32::MAX), 255.0, 256.0, 65_536.0];
    // mostly keep the node's type, so mutants get past deserialization into the code behind it
    let pick = match old {
        Value::Number(_) if !rng.chance(4) => rng.below(3),
        Value::String(_) if !rng.chance(4) => 4,
        Value::Array(_) | Value::Object(_) if !rng.chance(3) => 7,
        _ => rng.below(9),
    };
    match pick {
        0 | 1 => numbers.get(rng.below(numbers.len())).and_then(|f| serde_json::Number::from_f64(*f)).map_or(Value::Null, Value::Number),
        2 => Value::from([u64::MAX, i64::MAX as u64, 0, 1, 2, 3, 1 << 32].get(rng.below(7)).copied().unwrap_or(0)),
        3 => Value::from(i64::MIN),
        4 => {
            let s = ["", "\0", "../../etc/passwd", "a\u{301}\u{301}\u{301}", "🦀", "\u{202e}x", "%s%n", "C:\\x", "/"];
            let mut t = s.get(rng.below(s.len())).copied().unwrap_or("").to_string();
            if rng.chance(4) {
                t = t.repeat(1 + rng.below(5000));
            }
            Value::from(t)
        }
        5 => Value::Null,
        6 => Value::Bool(rng.chance(2)),
        7 => match old {
            // grow a container (bounded)
            Value::Array(a) if !a.is_empty() => {
                let mut a = a.clone();
                let k = rng.below(a.len());
                let item = a.get(k).cloned().unwrap_or(Value::Null);
                for _ in 0..1 + rng.below(200) {
                    a.push(item.clone());
                }
                Value::Array(a)
            }
            Value::Array(_) => Value::Array(vec![Value::Null; 1 + rng.below(4)]),
            Value::Object(o) => {
                let mut o = o.clone();
                let keys: Vec<String> = o.keys().cloned().collect();
                if let Some(k) = keys.get(rng.below(keys.len().max(1))) {
                    o.remove(k);
                }
                Value::Object(o)
            }
            _ => Value::Array(vec![]),
        },
        _ => match old {
            Value::Array(_) => Value::Array(vec![]),
            Value::Object(_) => Value::Object(serde_json::Map::new()),
            Value::String(_) => Value::from(0),
            _ => Value::from("x"),
        },
    }
}

/// 1..=`max` structural mutations of a JSON value: a random node replaced with an extreme number,
/// a hostile string, `null`, a wrong-typed value, an emptied/grown container or a removed key.
pub fn mutate_json(rng: &mut Rng, seed: &serde_json::Value, max: usize) -> serde_json::Value {
    let mut v = seed.clone();
    for _ in 0..1 + rng.below(max.max(1)) {
        let mut n = rng.below(count_nodes(&v));
        let mut r = rng.clone();
        with_nth(&mut v, &mut n, &mut |node| {
            let new = hostile_value(&mut r, node);
            *node = new;
        });
        *rng = r;
        rng.next_u64();
    }
    v
}

/// Runs `check` on `iterations(default)` structural JSON mutations of the seeds (see
/// [`mutate_json`]), then on byte-level mutations of their text (see [`run`]). Seeds that aren't
/// JSON are only byte-mutated.
pub fn run_json(name: &str, seeds: &[&str], default: usize, mut check: impl FnMut(&str)) {
    let values: Vec<serde_json::Value> = seeds.iter().filter_map(|s| serde_json::from_str(s).ok()).collect();
    if !values.is_empty() {
        let mut rng = rng_for(name, 0x8422_2325_cbf2_9ce4);
        // half the time box here, half on byte mutations below
        let budget = Budget::share(default, 2);
        let mut i = 0usize;
        while budget.more(i) {
            i += 1;
            let seed = values.get(rng.below(values.len())).cloned().unwrap_or_default();
            let v = mutate_json(&mut rng, &seed, 4);
            let text = v.to_string();
            let _report = Reporter { name, i, input: text.as_bytes() };
            check(&text);
        }
    }
    let bytes: Vec<&[u8]> = seeds.iter().map(|s| s.as_bytes()).collect();
    run_in(name, &bytes, Budget::share(default / 2, 2), |b| check(&String::from_utf8_lossy(b)));
}

struct Reporter<'a> {
    name: &'a str,
    i: usize,
    input: &'a [u8],
}

impl Drop for Reporter<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let shown: String = String::from_utf8_lossy(self.input).chars().take(2000).collect();
            eprintln!("fuzz `{}` failed at iteration {} on input ({} bytes):\n{shown:?}", self.name, self.i, self.input.len());
            if let Some(dir) = std::env::var_os("LCC_FUZZ_CRASH_DIR") {
                match save_crash(std::path::Path::new(&dir), self.name, self.input) {
                    Ok(path) => eprintln!("fuzz: crash input saved to {}", path.display()),
                    Err(e) => eprintln!("fuzz: could not save the crash input under {}: {e}", std::path::Path::new(&dir).display()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_bounded() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        assert_eq!(mutate(&mut a, b"hello world", 6), mutate(&mut b, b"hello world", 6));
        let mut n = 0;
        run("t", &[b"abc", b"{\"x\":1}"], 100, |_| n += 1);
        assert!(n >= 100);
    }

    #[test]
    fn a_crash_input_is_saved_and_corpus_files_become_seeds() {
        let dir = std::env::temp_dir().join(format!("lcc-fuzzkit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let saved = save_crash(&dir.join("crashes"), "raw/dng", b"boom").unwrap();
        assert_eq!(std::fs::read(&saved).unwrap(), b"boom");
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::write(dir.join("a/b/x.DNG"), b"dng").unwrap();
        std::fs::write(dir.join("a/y.jpg"), b"jpeg").unwrap();
        let seeds = seeds_in(&dir, &["dng"], 8, 1 << 20);
        assert_eq!(seeds, vec![b"dng".to_vec()]);
        assert!(seeds_in(&dir, &["dng"], 8, 1).is_empty(), "files over the size cap are skipped");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
