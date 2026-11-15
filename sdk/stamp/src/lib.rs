//! Stamps a WebAssembly plug-in's manifest into the module as a custom section, so a tool can read
//! what a plug-in is and what it asks for without running it (`docs/plugins.md` → Stamping your
//! plug-in).
//!
//! The section is named [`SECTION`] and holds exactly the bytes the module's `lcc_manifest` export
//! returns. [`stamp`] gets them by running the module once in a sandbox of its own: every import is
//! defined as a trap, and fuel, memory and recursion are capped ([`Limits`]). So the section equals
//! `lcc_manifest` however the plug-in builds its manifest (a literal, a string filled at run time,
//! the SDK's `package:` form). `lcc_manifest` stays the authority: the section is a copy for tools
//! that must not run the module, and nothing has to trust this tool to have written it.
//!
//! Everything here reads hostile input: a malformed module or section is an [`Error`], never a
//! panic.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::collections::BTreeSet;
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use wasmi::{Config, EnforcedLimits, Engine, ExternType, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, TypedFunc};

/// The custom section's name. Neutral on purpose: it is stored in plug-in files, so it never follows
/// the product's name.
pub const SECTION: &str = "photo-plugin.manifest";
/// The plug-in ABI version this tool reads (`lcc_abi_version` must return it).
pub const ABI_VERSION: i32 = 1;
/// Largest module accepted, before and after stamping (the host's default limit).
pub const MAX_MODULE_BYTES: usize = 32 << 20;
/// Largest manifest accepted (the host's limit); the section is capped at the same size.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// The only imports a plug-in may have, all in module `host` (as the app's host requires).
const HOST_IMPORTS: &[&str] = &["call", "response", "log"];

/// `\0asm`, then binary format version 1.
const HEADER: &[u8] = b"\0asm\x01\0\0\0";

/// The sandbox `lcc_manifest` runs in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Instructions (fuel) instantiation and the two calls may use together.
    pub fuel: u64,
    /// Cap on the module's linear memory.
    pub max_memory_bytes: usize,
    /// Cap on nested calls.
    pub max_recursion_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { fuel: 2_000_000_000, max_memory_bytes: 256 << 20, max_recursion_depth: 1024 }
    }
}

/// Why a module couldn't be stamped or checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not a well-formed WebAssembly module: header, size, section framing or validation.
    Module(String),
    /// Running the module failed: a missing export, a trap, a host import called, the fuel used up.
    Run(String),
    /// `lcc_manifest`'s answer, or the section, can't be a manifest: too large, not UTF-8, not a JSON object.
    Manifest(String),
    /// The module has no [`SECTION`]: it wasn't stamped.
    Missing,
    /// The module has this many [`SECTION`]s; a stamped module has one.
    Duplicate(usize),
    /// The section differs from what `lcc_manifest` returns. Holds the top-level fields that differ
    /// (empty when the section isn't a JSON object).
    Stale(Vec<String>),
    /// Reading or writing a file.
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Module(m) => write!(f, "not a usable WebAssembly module: {m}"),
            Error::Run(m) => write!(f, "running lcc_manifest failed: {m}"),
            Error::Manifest(m) => write!(f, "not a usable manifest: {m}"),
            Error::Missing => write!(f, "no {SECTION} section: the plug-in wasn't stamped"),
            Error::Duplicate(n) => write!(f, "{n} {SECTION} sections; a stamped plug-in has exactly one (stamp it again)"),
            Error::Stale(fields) if fields.is_empty() => write!(f, "the {SECTION} section differs from what lcc_manifest returns (stamp it again)"),
            Error::Stale(fields) => {
                write!(f, "the {SECTION} section differs from what lcc_manifest returns in {} (stamp it again)", fields.join(", "))
            }
            Error::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Runs the module's `lcc_manifest` in the sandbox and returns its answer, checked to be a UTF-8
/// JSON object within [`MAX_MANIFEST_BYTES`].
pub fn manifest(wasm: &[u8], limits: &Limits) -> Result<Vec<u8>> {
    check_size(wasm)?;
    let mut config = Config::default();
    config.consume_fuel(true).enforced_limits(EnforcedLimits::strict()).set_max_recursion_depth(limits.max_recursion_depth.max(16));
    config.compilation_mode(wasmi::CompilationMode::Eager);
    let engine = Engine::new(&config);
    let module = Module::new(&engine, wasm).map_err(|e| Error::Module(e.to_string()))?;
    let store_limits =
        StoreLimitsBuilder::new().memory_size(limits.max_memory_bytes).memories(1).tables(4).table_elements(100_000).instances(1).build();
    let mut store = Store::new(&engine, store_limits);
    store.limiter(|l| l);
    store.set_fuel(limits.fuel).map_err(run_err)?;
    // the host's three functions are there, and trap when called; anything else is refused, as the host refuses it
    let mut linker = Linker::<StoreLimits>::new(&engine);
    let mut defined = BTreeSet::new();
    for import in module.imports() {
        let name = format!("{}.{}", import.module(), import.name());
        if import.module() != "host" || !HOST_IMPORTS.contains(&import.name()) {
            return Err(Error::Module(format!("it imports `{name}`; plug-ins may import only host.call, host.response and host.log (no WASI)")));
        }
        let ExternType::Func(ty) = import.ty() else {
            return Err(Error::Module(format!("it imports `{name}`, which isn't a function; plug-ins import only host functions")));
        };
        if !defined.insert(name.clone()) {
            continue;
        }
        linker
            .func_new(import.module(), import.name(), ty.clone(), move |_, _, _| Err(wasmi::Error::host(ImportCalled(name.clone()))))
            .map_err(|e| Error::Module(e.to_string()))?;
    }
    let instance = linker.instantiate_and_start(&mut store, &module).map_err(|e| {
        // a trap (the start function, the fuel) is the module running; anything else is a module that can't be set up
        if e.as_trap_code().is_some() || e.downcast_ref::<ImportCalled>().is_some() {
            run_err(e)
        } else {
            Error::Module(format!("it can't be instantiated: {e}"))
        }
    })?;
    let version: TypedFunc<(), i32> = instance
        .get_typed_func(&store, "lcc_abi_version")
        .map_err(|_| Error::Run("the module exports no `lcc_abi_version() -> i32`: is it a plug-in?".into()))?;
    let v = version.call(&mut store, ()).map_err(run_err)?;
    if v != ABI_VERSION {
        return Err(Error::Run(format!("lcc_abi_version returned {v}; this tool reads version {ABI_VERSION}")));
    }
    let lcc_manifest: TypedFunc<(), i64> =
        instance.get_typed_func(&store, "lcc_manifest").map_err(|_| Error::Run("the module exports no `lcc_manifest() -> i64`".into()))?;
    let packed = lcc_manifest.call(&mut store, ()).map_err(run_err)? as u64;
    let (ptr, len) = ((packed & 0xffff_ffff) as usize, (packed >> 32) as usize);
    if len > MAX_MANIFEST_BYTES {
        return Err(Error::Manifest(format!("lcc_manifest returned {len} bytes (limit {MAX_MANIFEST_BYTES})")));
    }
    let memory = instance.get_memory(&store, "memory").ok_or_else(|| Error::Run("the module must export its linear memory as `memory`".into()))?;
    let bytes =
        memory.data(&store).get(ptr..ptr.saturating_add(len)).ok_or_else(|| Error::Manifest("lcc_manifest points outside memory".into()))?.to_vec();
    parse_manifest(&bytes)?;
    // the rest of the ABI, so a module stamped here is one the host can load
    instance.get_typed_func::<i32, i32>(&store, "lcc_alloc").map_err(|_| Error::Run("the module exports no `lcc_alloc(i32) -> i32`".into()))?;
    instance
        .get_typed_func::<(i32, i32), i64>(&store, "lcc_handle")
        .map_err(|_| Error::Run("the module exports no `lcc_handle(i32, i32) -> i64`".into()))?;
    Ok(bytes)
}

/// Returns `wasm` with exactly one [`SECTION`], holding what `lcc_manifest` returns. Every other
/// section is kept as it was, in its place; an earlier [`SECTION`] is dropped and the new one goes
/// last. The same input always gives the same output.
pub fn stamp(wasm: &[u8], limits: &Limits) -> Result<Vec<u8>> {
    stamp_and_manifest(wasm, limits).map(|(stamped, _)| stamped)
}

/// [`stamp`], also returning the manifest it stamped.
fn stamp_and_manifest(wasm: &[u8], limits: &Limits) -> Result<(Vec<u8>, Vec<u8>)> {
    check_size(wasm)?;
    // walk the sections before running anything: a module whose framing is broken is refused as such
    for s in sections(wasm)? {
        s?;
    }
    let manifest = manifest(wasm, limits)?;
    let mut out = Vec::with_capacity(wasm.len().saturating_add(manifest.len()).saturating_add(32));
    out.extend_from_slice(HEADER);
    for s in sections(wasm)? {
        let s = s?;
        if s.name != Some(SECTION) {
            out.extend_from_slice(s.raw);
        }
    }
    let mut payload = Vec::with_capacity(manifest.len().saturating_add(SECTION.len()).saturating_add(5));
    leb128(&mut payload, SECTION.len());
    payload.extend_from_slice(SECTION.as_bytes());
    payload.extend_from_slice(&manifest);
    out.push(0);
    leb128(&mut out, payload.len());
    out.extend_from_slice(&payload);
    if out.len() > MAX_MODULE_BYTES {
        return Err(Error::Module(format!("stamped, it would be {} bytes (limit {MAX_MODULE_BYTES})", out.len())));
    }
    Ok((out, manifest))
}

/// The module's [`SECTION`], read without running anything: `None` when it has none (it wasn't
/// stamped). Two or more are an error, as is a section over [`MAX_MANIFEST_BYTES`] or a module
/// whose sections can't be walked.
pub fn read_section(wasm: &[u8]) -> Result<Option<&[u8]>> {
    check_size(wasm)?;
    let mut found = None;
    let mut count = 0usize;
    for s in sections(wasm)? {
        let s = s?;
        if s.name == Some(SECTION) {
            count = count.saturating_add(1);
            found.get_or_insert(s.data);
        }
    }
    if count > 1 {
        return Err(Error::Duplicate(count));
    }
    if let Some(data) = found
        && data.len() > MAX_MANIFEST_BYTES
    {
        return Err(Error::Manifest(format!("the {SECTION} section is {} bytes (limit {MAX_MANIFEST_BYTES})", data.len())));
    }
    Ok(found)
}

/// Checks that the module has exactly one [`SECTION`] and that it equals what `lcc_manifest`
/// returns; returns the manifest when it does.
pub fn check(wasm: &[u8], limits: &Limits) -> Result<Vec<u8>> {
    let section = read_section(wasm)?.ok_or(Error::Missing)?;
    let manifest = manifest(wasm, limits)?;
    if section == manifest.as_slice() { Ok(manifest) } else { Err(Error::Stale(differing_fields(section, &manifest))) }
}

/// Stamps the file `input` and writes the result to `output` (`input` itself when `None`). The
/// file is written to a temporary name beside it and renamed, so on any error both paths are left
/// as they were. It keeps the permissions of the file it replaces (of `input` for a new file).
/// Returns the manifest.
pub fn stamp_file(input: &Path, output: Option<&Path>, limits: &Limits) -> Result<Vec<u8>> {
    let wasm = read_file(input)?;
    let (stamped, manifest) = stamp_and_manifest(&wasm, limits)?;
    let target = output.unwrap_or(input);
    let file = |p: &Path| std::fs::metadata(p).ok().filter(std::fs::Metadata::is_file);
    let permissions = file(target).or_else(|| file(input)).map(|m| m.permissions());
    write_file(target, &stamped, permissions)?;
    Ok(manifest)
}

/// [`check`] on a file; writes nothing.
pub fn check_file(path: &Path, limits: &Limits) -> Result<Vec<u8>> {
    check(&read_file(path)?, limits)
}

fn read_file(path: &Path) -> Result<Vec<u8>> {
    let io = |e: std::io::Error| Error::Io(format!("reading {}: {e}", path.display()));
    let len = std::fs::metadata(path).map_err(io)?.len();
    if len > MAX_MODULE_BYTES as u64 {
        return Err(Error::Module(format!("{} is {len} bytes (limit {MAX_MODULE_BYTES})", path.display())));
    }
    std::fs::read(path).map_err(io)
}

fn write_file(path: &Path, bytes: &[u8], permissions: Option<std::fs::Permissions>) -> Result<()> {
    let io = |e: std::io::Error| Error::Io(format!("writing {}: {e}", path.display()));
    let name = path.file_name().ok_or_else(|| Error::Io(format!("writing {}: not a file name", path.display())))?;
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".plugin-stamp-{}.tmp", std::process::id()));
    let tmp = dir.join(tmp_name);
    let written = std::fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(bytes)?;
        f.sync_all()
    });
    let kept = written.and_then(|()| permissions.map_or(Ok(()), |p| std::fs::set_permissions(&tmp, p)));
    let renamed = kept.and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = renamed {
        // a read-only temporary file (kept permissions) can't be deleted on Windows until it is writable again
        if let Ok(m) = std::fs::metadata(&tmp) {
            let mut p = m.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            p.set_readonly(false);
            let _ = std::fs::set_permissions(&tmp, p);
        }
        let _ = std::fs::remove_file(&tmp);
        return Err(io(e));
    }
    Ok(())
}

fn check_size(wasm: &[u8]) -> Result<()> {
    if wasm.len() > MAX_MODULE_BYTES {
        return Err(Error::Module(format!("the module is {} bytes (limit {MAX_MODULE_BYTES})", wasm.len())));
    }
    Ok(())
}

fn parse_manifest(bytes: &[u8]) -> Result<Map<String, Value>> {
    let text = std::str::from_utf8(bytes).map_err(|e| Error::Manifest(format!("lcc_manifest's answer isn't UTF-8 ({e})")))?;
    match serde_json::from_str(text) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(Error::Manifest("lcc_manifest's answer isn't a JSON object".into())),
        Err(e) => Err(Error::Manifest(format!("lcc_manifest's answer isn't JSON ({e})"))),
    }
}

/// The top-level fields whose values differ between the section and the manifest, sorted.
fn differing_fields(section: &[u8], manifest: &[u8]) -> Vec<String> {
    let (Ok(a), Ok(b)) = (parse_manifest(section), parse_manifest(manifest)) else { return Vec::new() };
    let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    keys.into_iter().filter(|k| a.get(*k) != b.get(*k)).cloned().collect()
}

fn run_err(e: wasmi::Error) -> Error {
    if e.as_trap_code() == Some(wasmi::TrapCode::OutOfFuel) {
        Error::Run("it ran out of fuel (instruction budget)".into())
    } else if let Some(ImportCalled(name)) = e.downcast_ref::<ImportCalled>() {
        Error::Run(format!("it called the host import `{name}`; the manifest must not need the host"))
    } else {
        Error::Run(format!("it trapped: {e}"))
    }
}

/// A call to one of the module's imports, which the sandbox turns into a trap.
#[derive(Debug)]
struct ImportCalled(String);

impl fmt::Display for ImportCalled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "called {}", self.0)
    }
}

impl wasmi::errors::HostError for ImportCalled {}

/// One section of a module, as it is in the file.
struct Section<'a> {
    /// The whole section: id, size and payload.
    raw: &'a [u8],
    /// A custom section's name; `None` for the other sections.
    name: Option<&'a str>,
    /// A custom section's data after its name.
    data: &'a [u8],
}

/// Walks the module's sections without collecting them (a hostile module may hold millions).
fn sections(wasm: &[u8]) -> Result<impl Iterator<Item = Result<Section<'_>>>> {
    let mut rest = wasm.strip_prefix(HEADER).ok_or_else(|| Error::Module("no WebAssembly version 1 header (\\0asm, 1)".into()))?;
    let mut index = 0usize;
    let mut failed = false;
    Ok(std::iter::from_fn(move || {
        if failed || rest.is_empty() {
            return None;
        }
        let section = next_section(rest, index);
        index = index.saturating_add(1);
        match section {
            Ok((s, after)) => {
                rest = after;
                Some(Ok(s))
            }
            Err(e) => {
                failed = true;
                Some(Err(e))
            }
        }
    }))
}

fn next_section(rest: &[u8], index: usize) -> Result<(Section<'_>, &[u8])> {
    let bad = |what: &str| Error::Module(format!("section {index}: {what}"));
    let (&id, after_id) = rest.split_first().ok_or_else(|| bad("missing"))?;
    let (size, after_size) = read_leb128(after_id).ok_or_else(|| bad("bad length"))?;
    let payload = after_size.get(..size).ok_or_else(|| bad("its length runs past the end of the module"))?;
    let after = after_size.get(size..).ok_or_else(|| bad("its length runs past the end of the module"))?;
    let raw = rest.get(..rest.len().saturating_sub(after.len())).ok_or_else(|| bad("bad length"))?;
    if id != 0 {
        return Ok((Section { raw, name: None, data: &[] }, after));
    }
    let (name_len, after_len) = read_leb128(payload).ok_or_else(|| bad("a custom section with a bad name length"))?;
    let name = after_len.get(..name_len).ok_or_else(|| bad("a custom section whose name runs past its end"))?;
    let name = std::str::from_utf8(name).map_err(|_| bad("a custom section whose name isn't UTF-8"))?;
    let data = after_len.get(name_len..).ok_or_else(|| bad("a custom section whose name runs past its end"))?;
    Ok((Section { raw, name: Some(name), data }, after))
}

/// An unsigned LEB128 `u32` (at most five bytes) and the bytes after it.
fn read_leb128(bytes: &[u8]) -> Option<(usize, &[u8])> {
    let mut value = 0u32;
    for (i, &b) in bytes.iter().enumerate().take(5) {
        let part = u32::from(b & 0x7f);
        // the fifth byte holds only the top four bits
        if i == 4 && (b & 0x80 != 0 || part > 0x0f) {
            return None;
        }
        value |= part << (7 * i);
        if b & 0x80 == 0 {
            return Some((usize::try_from(value).ok()?, bytes.get(i + 1..)?));
        }
    }
    None
}

fn leb128(out: &mut Vec<u8>, mut value: usize) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leb128_round_trips_and_refuses_overlong_input() {
        for v in [0usize, 1, 127, 128, 300, 65_535, 1 << 21, u32::MAX as usize] {
            let mut b = Vec::new();
            leb128(&mut b, v);
            assert_eq!(read_leb128(&b), Some((v, &[][..])), "{v}");
        }
        assert_eq!(read_leb128(&[0x80, 0x80, 0x80, 0x80, 0x10]), None, "more than 32 bits");
        assert_eq!(read_leb128(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x00]), None, "more than five bytes");
        assert_eq!(read_leb128(&[0x80]), None, "unterminated");
        assert_eq!(read_leb128(&[]), None, "empty");
    }
}
