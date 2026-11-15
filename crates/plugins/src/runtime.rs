//! The wasmi host: loading, instantiating and calling a plug-in under resource limits.
//!
//! ABI v1 (see `docs/plugins.md`). The module exports `memory`, `lcc_abi_version() -> i32`,
//! `lcc_manifest() -> i64`, `lcc_alloc(len: i32) -> i32` and `lcc_handle(ptr: i32, len: i32) -> i64`
//! (`i64` results pack `len << 32 | ptr` of a UTF-8 JSON buffer in the module's memory). It may
//! import only:
//!
//! - `host.call(ptr: i32, len: i32) -> i32`: send a JSON request `{method, params}`; returns the
//!   length of the JSON reply (`{"ok": value}` or `{"error": message}`), which
//! - `host.response(ptr: i32)` copies into the module's memory at `ptr` (a buffer of that length);
//! - `host.log(ptr: i32, len: i32)`: one line for the plug-in's log.
//!
//! `host.call` leaves the interpreter (a resumable host trap), so the request is served outside
//! wasmi with the app's state, then the call resumes; fuel and the deadline keep running.

use std::fmt;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use wasmi::{
    Caller, Config, EnforcedLimits, Engine, Extern, Instance, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder, TypedFunc,
    TypedResumableCall, Val, WasmParams, WasmResults,
};

use crate::context::CallContext;
use crate::manifest::{MAX_MANIFEST_BYTES, Manifest};
use crate::{Error, Result};

/// The plug-in ABI version this host implements (`lcc_abi_version` must return it).
pub const ABI_VERSION: i32 = 1;

/// The custom section a plug-in's manifest is stamped into (the SDK's `plugin-stamp`,
/// `docs/plugins.md`): the bytes `lcc_manifest` returns. Neutral on purpose: it is stored in
/// plug-in files, so it never follows the product's name.
pub const MANIFEST_SECTION: &str = "photo-plugin.manifest";

/// Fuel handed out per slice; between slices the host checks the deadline.
const FUEL_SLICE: u64 = 20_000_000;
/// Most log lines kept from one call, and the longest line.
const MAX_LOG_LINES: usize = 200;
const MAX_LOG_LINE: usize = 2048;
/// The imports a module may have.
const IMPORTS: &[&str] = &["call", "response", "log"];

/// Resource limits for running plug-ins.
#[derive(Clone, Debug, PartialEq)]
pub struct Limits {
    /// Largest module accepted.
    pub max_module_bytes: usize,
    /// Cap on a plug-in's linear memory.
    pub max_memory_bytes: usize,
    /// Instructions (fuel) one call may use.
    pub fuel: u64,
    /// Deepest call nesting.
    pub max_recursion_depth: usize,
    /// Wall-clock budget for one call, in milliseconds (native builds; the fuel bounds the web).
    /// Time spent in host requests (network) counts.
    pub wall_time_ms: u64,
    /// Largest request or reply passed either way.
    pub max_message_bytes: usize,
    /// Most `host.call`s in one call.
    pub max_host_calls: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_module_bytes: 32 << 20,
            max_memory_bytes: 256 << 20,
            fuel: 2_000_000_000,
            max_recursion_depth: 1024,
            wall_time_ms: 120_000,
            max_message_bytes: 16 << 20,
            max_host_calls: 10_000,
        }
    }
}

/// A loaded, validated plug-in. Every call gets its own store and instance.
pub struct Plugin {
    manifest: Manifest,
    engine: Engine,
    module: Module,
    limits: Limits,
    size: usize,
    /// SHA-256 of the module's bytes: whether an update changed the code.
    digest: [u8; 32],
}

impl fmt::Debug for Plugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Plugin").field("manifest", &self.manifest).field("size", &self.size).finish()
    }
}

/// Where [`Plugin::inspect`] read a module's manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManifestSource {
    /// The stamped [`MANIFEST_SECTION`], read without running the module: the file's claim, which
    /// [`Plugin::load`] checks against `lcc_manifest`.
    Section,
    /// `lcc_manifest`, run in the sandbox.
    Export,
}

impl ManifestSource {
    /// `"section"` or `"export"`, as `plugin.inspectFile` gives it.
    pub fn as_str(self) -> &'static str {
        match self {
            ManifestSource::Section => "section",
            ManifestSource::Export => "export",
        }
    }
}

/// A module's manifest, read without installing it ([`Plugin::inspect`]).
#[derive(Clone, Debug)]
pub struct Inspection {
    /// The manifest, from where [`Inspection::source`] says.
    pub manifest: Manifest,
    /// The stamped section (the file's claim) or `lcc_manifest`'s answer.
    pub source: ManifestSource,
    /// SHA-256 of the module's bytes, as [`Plugin::digest`].
    pub digest: [u8; 32],
}

impl Inspection {
    /// The manifest is `lcc_manifest`'s answer, not only the file's claim.
    pub fn verified(&self) -> bool {
        self.source == ManifestSource::Export
    }
}

/// What one call returned, and what it logged (also when it failed).
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub value: Result<Value>,
    pub logs: Vec<String>,
}

struct Deadline {
    #[cfg(not(target_arch = "wasm32"))]
    at: std::time::Instant,
}

impl Deadline {
    fn new(limits: &Limits) -> Self {
        #[cfg(target_arch = "wasm32")]
        let _ = limits;
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            at: std::time::Instant::now() + std::time::Duration::from_millis(limits.wall_time_ms),
        }
    }
    fn check(&self) -> Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        if std::time::Instant::now() >= self.at {
            return Err(Error::Limit("time budget".into()));
        }
        Ok(())
    }
}

/// Per-instance state: the memory limiter, the pending reply and the log.
struct Data {
    limits: StoreLimits,
    pending: Vec<u8>,
    logs: Vec<String>,
}

/// A `host.call` on its way out of the interpreter.
#[derive(Debug)]
struct HostCall {
    ptr: i32,
    len: i32,
}
impl fmt::Display for HostCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "host.call")
    }
}
impl wasmi::errors::HostError for HostCall {}

struct Live {
    store: Store<Data>,
    instance: Instance,
    memory: Memory,
}

impl Live {
    fn func<P: WasmParams, R: WasmResults>(&self, name: &str) -> Result<TypedFunc<P, R>> {
        self.instance.get_typed_func::<P, R>(&self.store, name).map_err(|e| Error::Abi(format!("export `{name}`: {e}")))
    }
    fn read(&self, ptr: i32, len: usize, what: &str) -> Result<Vec<u8>> {
        let ptr = ptr as u32 as usize;
        self.memory
            .data(&self.store)
            .get(ptr..ptr.saturating_add(len))
            .map(<[u8]>::to_vec)
            .ok_or_else(|| Error::Abi(format!("{what} points outside memory")))
    }
}

fn wasm_err(e: wasmi::Error) -> Error {
    if e.as_trap_code() == Some(wasmi::TrapCode::OutOfFuel) { Error::Limit("instruction budget".into()) } else { Error::Trap(e.to_string()) }
}

fn memory_of(caller: &Caller<'_, Data>) -> std::result::Result<Memory, wasmi::Error> {
    caller.get_export("memory").and_then(Extern::into_memory).ok_or_else(|| wasmi::Error::new("module exports no memory"))
}

fn unpack(packed: i64) -> (i32, usize) {
    let p = packed as u64;
    ((p & 0xffff_ffff) as u32 as i32, (p >> 32) as usize)
}

impl Plugin {
    /// Validates and loads a module: checks its imports, the ABI version and the manifest. A module
    /// with a stamped [`MANIFEST_SECTION`] is refused unless the section is exactly what
    /// `lcc_manifest` returns, so an installed plug-in's section is always true.
    pub fn load(bytes: &[u8], limits: Limits) -> Result<Plugin> {
        let (engine, module) = compile(bytes, &limits)?;
        Plugin::load_compiled(bytes, limits, engine, module)
    }

    /// [`Plugin::load`] for a module [`compile`] has already validated.
    fn load_compiled(bytes: &[u8], limits: Limits, engine: Engine, module: Module) -> Result<Plugin> {
        let section = stamped_section(&module)?.map(<[u8]>::to_vec);
        let digest = Sha256::digest(bytes).into();
        let mut p = Plugin { manifest: placeholder_manifest(), engine, module, limits, size: bytes.len(), digest };
        let deadline = Deadline::new(&p.limits);
        let mut live = p.instantiate()?;
        let version: TypedFunc<(), i32> = live.func("lcc_abi_version")?;
        let v = p.call(&mut live, &version, (), &deadline, None)?;
        if v != ABI_VERSION {
            return Err(Error::Abi(format!("lcc_abi_version returned {v}; this host implements version {ABI_VERSION}")));
        }
        let manifest: TypedFunc<(), i64> = live.func("lcc_manifest")?;
        let (ptr, len) = unpack(p.call(&mut live, &manifest, (), &deadline, None)?);
        if len > MAX_MANIFEST_BYTES {
            return Err(Error::Manifest(format!("manifest is {len} bytes (limit {MAX_MANIFEST_BYTES})")));
        }
        let answer = live.read(ptr, len, "lcc_manifest")?;
        if let Some(section) = section
            && section != answer
        {
            return Err(Error::Manifest(stale_section(&section, &answer)));
        }
        p.manifest = Manifest::parse(&answer)?;
        live.func::<i32, i32>("lcc_alloc")?;
        live.func::<(i32, i32), i64>("lcc_handle")?;
        Ok(p)
    }

    /// Reads a module's manifest without installing it. A module with a stamped
    /// [`MANIFEST_SECTION`] is validated but never instantiated: the manifest is the section's, a
    /// claim [`Plugin::load`] checks on install ([`ManifestSource::Section`]). One without a section,
    /// or any module when `verify` is set, is loaded as [`Plugin::load`] loads it and gives
    /// `lcc_manifest`'s answer ([`ManifestSource::Export`]).
    pub fn inspect(bytes: &[u8], limits: Limits, verify: bool) -> Result<Inspection> {
        let (engine, module) = compile(bytes, &limits)?;
        if !verify && let Some(section) = stamped_section(&module)? {
            check_exports(&module)?;
            let manifest = Manifest::parse(section)?;
            return Ok(Inspection { manifest, source: ManifestSource::Section, digest: Sha256::digest(bytes).into() });
        }
        let p = Plugin::load_compiled(bytes, limits, engine, module)?;
        Ok(Inspection { manifest: p.manifest, source: ManifestSource::Export, digest: p.digest })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn id(&self) -> &str {
        &self.manifest.id
    }
    pub fn limits(&self) -> &Limits {
        &self.limits
    }
    /// Module size in bytes.
    pub fn size(&self) -> usize {
        self.size
    }
    /// SHA-256 of the module's bytes.
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
    /// [`Plugin::digest`] as lowercase hex, as `plugin.list` gives it (`moduleHash`, like a pixel filter's).
    pub fn module_hash(&self) -> String {
        self.digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn instantiate(&self) -> Result<Live> {
        let limits =
            StoreLimitsBuilder::new().memory_size(self.limits.max_memory_bytes).memories(1).tables(4).table_elements(100_000).instances(1).build();
        let mut store = Store::new(&self.engine, Data { limits, pending: Vec::new(), logs: Vec::new() });
        store.limiter(|d| &mut d.limits);
        store.set_fuel(self.limits.fuel).map_err(wasm_err)?;
        let mut linker = Linker::<Data>::new(&self.engine);
        let def = |e: wasmi::errors::LinkerError| Error::Abi(e.to_string());
        linker
            .func_wrap("host", "call", |ptr: i32, len: i32| -> std::result::Result<i32, wasmi::Error> {
                Err(wasmi::Error::host(HostCall { ptr, len }))
            })
            .map_err(def)?;
        linker
            .func_wrap("host", "response", |mut caller: Caller<'_, Data>, ptr: i32| -> std::result::Result<(), wasmi::Error> {
                let memory = memory_of(&caller)?;
                let reply = std::mem::take(&mut caller.data_mut().pending);
                memory.write(&mut caller, ptr as u32 as usize, &reply).map_err(|_| wasmi::Error::new("host.response: buffer outside memory"))
            })
            .map_err(def)?;
        linker
            .func_wrap("host", "log", |mut caller: Caller<'_, Data>, ptr: i32, len: i32| -> std::result::Result<(), wasmi::Error> {
                let memory = memory_of(&caller)?;
                let (ptr, len) = (ptr as u32 as usize, (len as u32 as usize).min(MAX_LOG_LINE));
                let line = memory.data(&caller).get(ptr..ptr.saturating_add(len)).map(|b| String::from_utf8_lossy(b).into_owned());
                let line = line.ok_or_else(|| wasmi::Error::new("host.log: text outside memory"))?;
                let logs = &mut caller.data_mut().logs;
                if logs.len() < MAX_LOG_LINES {
                    logs.push(line);
                }
                Ok(())
            })
            .map_err(def)?;
        let instance = linker.instantiate_and_start(&mut store, &self.module).map_err(wasm_err)?;
        let memory =
            instance.get_memory(&store, "memory").ok_or_else(|| Error::Abi("the module must export its linear memory as `memory`".into()))?;
        Ok(Live { store, instance, memory })
    }

    /// Calls `f` under the call's fuel, in slices, serving `host.call`s through `ctx` (none during
    /// loading: a module that calls the host from its manifest fails).
    fn call<P: WasmParams, R: WasmResults>(
        &self,
        live: &mut Live,
        f: &TypedFunc<P, R>,
        params: P,
        deadline: &Deadline,
        mut ctx: Option<&mut CallContext<'_>>,
    ) -> Result<R> {
        let mut left = live.store.get_fuel().map_err(wasm_err)?;
        let give = left.min(FUEL_SLICE);
        left -= give;
        live.store.set_fuel(give).map_err(wasm_err)?;
        let mut calls = 0u32;
        let mut call = f.call_resumable(&mut live.store, params).map_err(wasm_err)?;
        loop {
            match call {
                TypedResumableCall::Finished(r) => {
                    let rest = live.store.get_fuel().unwrap_or(0);
                    live.store.set_fuel(left.saturating_add(rest)).map_err(wasm_err)?;
                    return Ok(r);
                }
                TypedResumableCall::HostTrap(inv) => {
                    let Some(&HostCall { ptr, len }) = inv.host_error().downcast_ref::<HostCall>() else {
                        return Err(Error::Trap(inv.host_error().to_string()));
                    };
                    let Some(ctx) = ctx.as_deref_mut() else {
                        return Err(Error::Abi("host.call is not available while loading".into()));
                    };
                    calls += 1;
                    if calls > self.limits.max_host_calls {
                        return Err(Error::Limit("host call budget".into()));
                    }
                    let len = len as u32 as usize;
                    if len > self.limits.max_message_bytes {
                        return Err(Error::Limit("message size".into()));
                    }
                    let request = live.read(ptr, len, "host.call")?;
                    let reply = match serde_json::from_slice::<Value>(&request) {
                        Ok(req) => match req.get("method").and_then(Value::as_str) {
                            Some(method) => match ctx.dispatch(method, req.get("params").unwrap_or(&Value::Null)) {
                                Ok(v) => json!({"ok": v}),
                                Err(e) => json!({"error": e}),
                            },
                            None => json!({"error": "request needs a `method`"}),
                        },
                        Err(e) => json!({"error": format!("request is not JSON: {e}")}),
                    };
                    let mut bytes = serde_json::to_vec(&reply).map_err(|e| Error::Failed(e.to_string()))?;
                    if bytes.len() > self.limits.max_message_bytes {
                        bytes = serde_json::to_vec(&json!({"error": "reply too large"})).map_err(|e| Error::Failed(e.to_string()))?;
                    }
                    deadline.check()?;
                    let n = i32::try_from(bytes.len()).map_err(|_| Error::Limit("message size".into()))?;
                    live.store.data_mut().pending = bytes;
                    call = inv.resume(&mut live.store, &[Val::I32(n)]).map_err(wasm_err)?;
                }
                TypedResumableCall::OutOfFuel(inv) => {
                    let need = inv.required_fuel();
                    if left == 0 || need > left {
                        return Err(Error::Limit("instruction budget".into()));
                    }
                    deadline.check()?;
                    let give = left.min(FUEL_SLICE.max(need));
                    left -= give;
                    live.store.set_fuel(give).map_err(wasm_err)?;
                    call = inv.resume(&mut live.store).map_err(wasm_err)?;
                }
            }
        }
    }

    /// Sends `request` to `lcc_handle` in a fresh instance; returns the reply and the log.
    /// A reply `{"error": "…"}` is an [`Error::Failed`].
    pub(crate) fn invoke(&self, request: &Value, ctx: &mut CallContext<'_>) -> Outcome {
        let mut logs = Vec::new();
        let value = self.invoke_inner(request, ctx, &mut logs);
        Outcome { value, logs }
    }

    fn invoke_inner(&self, request: &Value, ctx: &mut CallContext<'_>, logs: &mut Vec<String>) -> Result<Value> {
        let deadline = Deadline::new(&self.limits);
        let mut live = self.instantiate()?;
        let bytes = serde_json::to_vec(request).map_err(|e| Error::Params(e.to_string()))?;
        if bytes.len() > self.limits.max_message_bytes {
            return Err(Error::Limit("message size".into()));
        }
        let n = i32::try_from(bytes.len()).map_err(|_| Error::Limit("message size".into()))?;
        let alloc: TypedFunc<i32, i32> = live.func("lcc_alloc")?;
        let handle: TypedFunc<(i32, i32), i64> = live.func("lcc_handle")?;
        let ptr = self.call(&mut live, &alloc, n.max(1), &deadline, None)?;
        if ptr == 0 {
            return Err(Error::Failed("lcc_alloc returned 0 (out of memory)".into()));
        }
        let start = ptr as u32 as usize;
        live.memory
            .data_mut(&mut live.store)
            .get_mut(start..start.saturating_add(bytes.len()))
            .ok_or_else(|| Error::Abi("lcc_alloc returned a block outside memory".into()))?
            .copy_from_slice(&bytes);
        let result = self.call(&mut live, &handle, (ptr, n), &deadline, Some(ctx));
        *logs = std::mem::take(&mut live.store.data_mut().logs);
        let (rptr, rlen) = unpack(result?);
        if rlen > self.limits.max_message_bytes {
            return Err(Error::Limit("message size".into()));
        }
        let reply = live.read(rptr, rlen, "lcc_handle")?;
        let value: Value = serde_json::from_slice(&reply).map_err(|e| Error::Abi(format!("lcc_handle reply is not JSON: {e}")))?;
        if let Some(e) = value.get("error") {
            let msg = e.as_str().map_or_else(|| e.to_string(), str::to_string);
            return Err(Error::Failed(msg));
        }
        Ok(value.get("ok").cloned().unwrap_or(value))
    }
}

/// Validates `bytes` as a plug-in module (size, header, wasmi's validation, imports) without
/// instantiating it.
fn compile(bytes: &[u8], limits: &Limits) -> Result<(Engine, Module)> {
    if bytes.len() > limits.max_module_bytes {
        return Err(Error::Module(format!("module is {} bytes (limit {})", bytes.len(), limits.max_module_bytes)));
    }
    if !bytes.starts_with(b"\0asm") {
        return Err(Error::Module("not a WebAssembly binary (missing \\0asm header)".into()));
    }
    let mut config = Config::default();
    config.consume_fuel(true).enforced_limits(EnforcedLimits::strict()).set_max_recursion_depth(limits.max_recursion_depth.max(16));
    config.compilation_mode(wasmi::CompilationMode::Eager);
    let engine = Engine::new(&config);
    let module = Module::new(&engine, bytes).map_err(|e| Error::Module(e.to_string()))?;
    if let Some(imp) = module.imports().find(|i| i.module() != "host" || !IMPORTS.contains(&i.name())) {
        return Err(Error::Abi(format!(
            "the module imports `{}.{}`; plug-ins may import only host.call, host.response and host.log (no WASI)",
            imp.module(),
            imp.name()
        )));
    }
    Ok((engine, module))
}

/// The module's [`MANIFEST_SECTION`], `None` when it wasn't stamped. More than one, or one over
/// [`MAX_MANIFEST_BYTES`], is an error.
fn stamped_section(module: &Module) -> Result<Option<&[u8]>> {
    let mut found = module.custom_sections().filter(|s| s.name() == MANIFEST_SECTION).map(|s| s.data());
    let Some(section) = found.next() else { return Ok(None) };
    let more = found.count();
    if more > 0 {
        return Err(Error::Manifest(format!(
            "the module has {} {MANIFEST_SECTION} sections; a stamped plug-in has exactly one (stamp it again)",
            more.saturating_add(1)
        )));
    }
    if section.len() > MAX_MANIFEST_BYTES {
        return Err(Error::Manifest(format!("the {MANIFEST_SECTION} section is {} bytes (limit {MAX_MANIFEST_BYTES})", section.len())));
    }
    Ok(Some(section))
}

/// The ABI's exports, by type, read from the module without running it: what [`Plugin::load`]
/// would look up after instantiating.
fn check_exports(module: &Module) -> Result<()> {
    use wasmi::ValType::{I32, I64};
    let wanted: [(&str, &[wasmi::ValType], &[wasmi::ValType]); 4] =
        [("lcc_abi_version", &[], &[I32]), ("lcc_manifest", &[], &[I64]), ("lcc_alloc", &[I32], &[I32]), ("lcc_handle", &[I32, I32], &[I64])];
    for (name, params, results) in wanted {
        let ok = module.get_export(name).and_then(|t| t.func().cloned()).is_some_and(|f| f.params() == params && f.results() == results);
        if !ok {
            return Err(Error::Abi(format!("export `{name}`: missing, or not the ABI's function type")));
        }
    }
    if module.get_export("memory").and_then(|t| t.memory().cloned()).is_none() {
        return Err(Error::Abi("the module must export its linear memory as `memory`".into()));
    }
    Ok(())
}

/// Why a stamped section isn't `lcc_manifest`'s answer, naming the top-level fields that differ.
fn stale_section(section: &[u8], answer: &[u8]) -> String {
    let object = |b: &[u8]| match serde_json::from_slice::<Value>(b) {
        Ok(Value::Object(m)) => Some(m),
        _ => None,
    };
    let head = format!("the {MANIFEST_SECTION} section differs from what lcc_manifest returns");
    let (Some(a), Some(b)) = (object(section), object(answer)) else {
        return format!("{head} (it isn't the same JSON object); the plug-in must be stamped again");
    };
    let keys: std::collections::BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    let fields: Vec<String> = keys.into_iter().filter(|k| a.get(*k) != b.get(*k)).map(|k| format!("`{k}`")).collect();
    if fields.is_empty() {
        return format!("{head} byte for byte (the same fields, written differently); the plug-in must be stamped again");
    }
    format!("{head} in {}; the plug-in must be stamped again", fields.join(", "))
}

fn placeholder_manifest() -> Manifest {
    Manifest {
        id: String::new(),
        name: String::new(),
        version: String::new(),
        requires: Vec::new(),
        description: String::new(),
        author: String::new(),
        permissions: Default::default(),
        commands: Vec::new(),
        hooks: Default::default(),
        publish: None,
        export_destinations: Vec::new(),
        metadata_fields: Vec::new(),
        settings: Vec::new(),
        unknown_fields: Vec::new(),
        icon: None,
        header: None,
        about: Default::default(),
        dropped: Vec::new(),
    }
}
