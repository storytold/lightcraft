//! Call-time policy for the MCP server: which tools and command ids an agent may run, plus
//! argument checks for the operations that delete, move, or write outside the library.
//!
//! Everything is configured from the environment so no CLI change is required. The natural
//! place for these variables is the `env` block of the MCP client config (`.mcp.json`,
//! `claude_desktop_config.json`, Codex `config.toml`):
//!
//! - `LIGHTCRAFT_MCP_SAFE=1`           enable [`SAFE_DENY`], [`SAFE_DENY_TOOLS`] and the argument checks
//! - `LIGHTCRAFT_MCP_DENY=a.b,c.*`     extra command ids (exact, or `prefix.*`) and/or tool names to refuse
//! - `LIGHTCRAFT_MCP_ALLOW_TOOLS=x,y`  when set, only these tool names are listed and callable
//! - `LIGHTCRAFT_MCP_EXPORT_ROOT=DIR`  every export `path` / `dir` must resolve inside DIR
//!
//! The policy is enforced in `tools/call` (not only in `tools/list`), so `run_command`,
//! `command_run`, `command_batch` and the generated `cmd_*` tools cannot bypass it.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::tools::COMMAND_TOOL_PREFIX;

/// Command ids refused under `LIGHTCRAFT_MCP_SAFE=1`.
pub const SAFE_DENY: &[&str] = &[
    // hands files to Apple Photos (and so iCloud); any MCP client can add any readable file
    "export.addToPhotos",
    // reads / writes the system clipboard of the desktop session
    "ui.clipboard",
    // drops arbitrary paths into the running app
    "ui.dropFiles",
    "app.quit",
    "folder.move",
];

/// Helper tool names refused under `LIGHTCRAFT_MCP_SAFE=1`.
pub const SAFE_DENY_TOOLS: &[&str] = &["clipboard"];

#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// Argument-level checks on (`import`, `export`) are on.
    pub safe: bool,
    /// Command ids (exact or `prefix.*`) that are never executed.
    pub deny: BTreeSet<String>,
    /// Tool names that are never listed or executed.
    pub deny_tools: BTreeSet<String>,
    /// When set, the only tool names that are listed or executed.
    pub allow_tools: Option<BTreeSet<String>>,
    /// When set, every export destination must resolve inside this directory.
    pub export_root: Option<PathBuf>,
}

fn csv(var: &str) -> impl Iterator<Item = String> {
    std::env::var(var).unwrap_or_default().split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>().into_iter()
}

impl Policy {
    /// A policy that refuses nothing (the upstream behaviour).
    pub fn permissive() -> Self {
        Self::default()
    }

    /// Read `LIGHTCRAFT_MCP_*` from the environment.
    pub fn from_env() -> Self {
        let safe = matches!(std::env::var("LIGHTCRAFT_MCP_SAFE").as_deref(), Ok("1") | Ok("true") | Ok("yes"));
        let mut p = Self { safe, ..Self::default() };
        if safe {
            p.deny.extend(SAFE_DENY.iter().map(|s| s.to_string()));
            p.deny_tools.extend(SAFE_DENY_TOOLS.iter().map(|s| s.to_string()));
        }
        for item in csv("LIGHTCRAFT_MCP_DENY") {
            // A command id contains a dot (`library.import`, `photo.*`); anything else is a tool name.
            if item.contains('.') {
                p.deny.insert(item);
            } else {
                p.deny_tools.insert(item);
            }
        }
        let allow: BTreeSet<String> = csv("LIGHTCRAFT_MCP_ALLOW_TOOLS").collect();
        if !allow.is_empty() {
            p.allow_tools = Some(allow);
        }
        if let Ok(root) = std::env::var("LIGHTCRAFT_MCP_EXPORT_ROOT") {
            let root = root.trim();
            if !root.is_empty() {
                p.export_root = Some(PathBuf::from(root));
            }
        }
        p
    }

    pub fn is_active(&self) -> bool {
        self.safe || !self.deny.is_empty() || !self.deny_tools.is_empty() || self.allow_tools.is_some() || self.export_root.is_some()
    }

    /// True when `id` matches an entry of the deny list (`a.b` exactly, or `a.*` by prefix).
    pub fn command_denied(&self, id: &str) -> bool {
        self.deny.iter().any(|d| match d.strip_suffix(".*") {
            Some(prefix) => id == prefix || id.starts_with(&format!("{prefix}.")),
            None => d == id,
        })
    }

    /// The `cmd_*` tool name of a command id (`a.b` → `cmd_a_b`), as `tools.rs` generates it.
    fn tool_name_of(id: &str) -> String {
        format!("{COMMAND_TOOL_PREFIX}{}", id.replace('.', "_"))
    }

    /// Whether a tool may appear in `tools/list` and be called.
    pub fn tool_allowed(&self, name: &str) -> Result<(), String> {
        if let Some(allow) = &self.allow_tools
            && !allow.contains(name)
        {
            return Err(format!("tool `{name}` is not in LIGHTCRAFT_MCP_ALLOW_TOOLS"));
        }
        if self.deny_tools.contains(name) {
            return Err(format!("tool `{name}` is denied by policy"));
        }
        if name.starts_with(COMMAND_TOOL_PREFIX) {
            for d in &self.deny {
                let hit = match d.strip_suffix(".*") {
                    Some(prefix) => {
                        let p = Self::tool_name_of(prefix);
                        name == p || name.starts_with(&format!("{p}_"))
                    }
                    None => name == Self::tool_name_of(d),
                };
                if hit {
                    return Err(format!("tool `{name}` runs command `{d}`, which is denied by policy"));
                }
            }
        }
        Ok(())
    }

    /// Keep only the tools the policy allows.
    pub fn filter_tools(&self, tools: Vec<Value>) -> Vec<Value> {
        if !self.is_active() {
            return tools;
        }
        tools.into_iter().filter(|t| t.get("name").and_then(Value::as_str).is_some_and(|n| self.tool_allowed(n).is_ok())).collect()
    }

    /// The single enforcement point: called from `tools/call` before anything executes.
    pub fn check_call(&self, name: &str, args: &Value) -> Result<(), String> {
        if !self.is_active() {
            return Ok(());
        }
        self.tool_allowed(name)?;

        // (command id, its params) pairs this call would execute.
        let mut invoked: Vec<(String, Value)> = Vec::new();
        match name {
            "command_run" => push_id(&mut invoked, args.get("id"), args.get("params")),
            "run_command" => push_id(&mut invoked, args.get("command"), args.get("params")),
            "command_batch" => {
                for step in args.get("steps").and_then(Value::as_array).into_iter().flatten() {
                    push_id(&mut invoked, step.get("id"), step.get("params"));
                }
            }
            "import" => invoked.push(("library.import".into(), args.clone())),
            "export" => invoked.push(("app.export".into(), args.clone())),
            "clipboard" => invoked.push(("ui.clipboard".into(), args.clone())),
            n if n.starts_with(COMMAND_TOOL_PREFIX) => {
                // Exact id is not recoverable from the tool name (underscores); `tool_allowed`
                // already matched it against the deny list. Keep the args for the checks below.
                if n == "cmd_app_export" {
                    invoked.push(("app.export".into(), args.clone()));
                } else if n == "cmd_library_import" {
                    invoked.push(("library.import".into(), args.clone()));
                }
            }
            _ => {}
        }

        for (id, params) in &invoked {
            if self.command_denied(id) {
                return Err(format!("command `{id}` is denied by policy"));
            }
            if self.safe {
                match id.as_str() {
                    "library.import" => self.check_import(params)?,
                    "app.export" => self.check_export(params)?,
                    _ => {}
                }
            } else if id == "app.export" && self.export_root.is_some() {
                self.check_export(params)?;
            }
        }
        Ok(())
    }

    fn check_import(&self, params: &Value) -> Result<(), String> {
        if params.get("mode").and_then(Value::as_str) == Some("move") {
            return Err("import mode `move` deletes the source files and is refused under LIGHTCRAFT_MCP_SAFE; use `copy` or `add`".into());
        }
        Ok(())
    }

    fn check_export(&self, params: &Value) -> Result<(), String> {
        if self.safe && params.get("addToPhotos").and_then(Value::as_bool) == Some(true) {
            return Err("`addToPhotos` is refused under LIGHTCRAFT_MCP_SAFE".into());
        }
        // Upstream only replaces `\`, `:` and NUL in `subfolder`; `/` and `..` pass through
        // (export/params.rs `from_json`), so `subfolder: "../../x"` climbs out of `dir`.
        if let Some(sub) = params.get("subfolder").and_then(Value::as_str) {
            let escapes = Path::new(sub).components().any(|c| matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_)));
            if escapes || sub.contains('\\') {
                return Err(format!("export `subfolder` {sub:?} must be a relative path without `..`"));
            }
        }
        let Some(root) = &self.export_root else { return Ok(()) };
        if params.get("destinationMode").and_then(Value::as_str) == Some("sameAsOriginal") {
            return Err("`destinationMode: sameAsOriginal` writes next to the originals, outside LIGHTCRAFT_MCP_EXPORT_ROOT".into());
        }
        let path = params.get("path").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
        let dir = params.get("dir").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
        match (path, dir) {
            (None, None) => {
                Err(format!("export needs an explicit `dir` or `path` inside {} (the app's remembered export folder is not allowed)", root.display()))
            }
            (p, d) => {
                if let Some(p) = p {
                    // `path` is a file: its parent must be inside the root.
                    let parent = Path::new(p).parent().unwrap_or_else(|| Path::new(p));
                    within(root, parent).map_err(|e| format!("export `path` {p}: {e}"))?;
                }
                if let Some(d) = d {
                    within(root, Path::new(d)).map_err(|e| format!("export `dir` {d}: {e}"))?;
                }
                Ok(())
            }
        }
    }
}

fn push_id(out: &mut Vec<(String, Value)>, id: Option<&Value>, params: Option<&Value>) {
    if let Some(id) = id.and_then(Value::as_str) {
        out.push((id.to_string(), params.cloned().unwrap_or(Value::Null)));
    }
}

/// Resolve `p` without following it blindly: the longest existing prefix is canonicalized
/// (so symlinks and `..` there are real), the rest must not contain `..`, and the result must
/// start with the canonicalized `root`.
fn within(root: &Path, p: &Path) -> Result<(), String> {
    if !p.is_absolute() {
        return Err("must be an absolute path".into());
    }
    let root = root.canonicalize().map_err(|e| format!("export root {}: {e}", root.display()))?;
    let resolved = resolve_lexically(p)?;
    if resolved.starts_with(&root) { Ok(()) } else { Err(format!("is outside the export root {}", root.display())) }
}

fn resolve_lexically(p: &Path) -> Result<PathBuf, String> {
    // Longest existing ancestor → canonicalize; append the remainder, refusing `..`.
    let mut existing = p.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name().map(|n| n.to_os_string()) else {
            return Err("cannot resolve path".into());
        };
        rest.push(name);
        if !existing.pop() {
            return Err("cannot resolve path".into());
        }
    }
    let mut out = existing.canonicalize().map_err(|e| e.to_string())?;
    for part in rest.iter().rev() {
        match Path::new(part).components().next() {
            Some(Component::ParentDir) => return Err("`..` is not allowed".into()),
            Some(Component::CurDir) | None => {}
            _ => out.push(part),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn safe() -> Policy {
        let mut p = Policy { safe: true, ..Policy::default() };
        p.deny.extend(SAFE_DENY.iter().map(|s| s.to_string()));
        p.deny_tools.extend(SAFE_DENY_TOOLS.iter().map(|s| s.to_string()));
        p
    }

    #[test]
    fn permissive_policy_refuses_nothing() {
        let p = Policy::permissive();
        assert!(p.check_call("clipboard", &json!({"action": "copy"})).is_ok());
        assert!(p.check_call("import", &json!({"paths": ["/x"], "mode": "move"})).is_ok());
    }

    #[test]
    fn safe_policy_blocks_every_route_to_a_denied_command() {
        let p = safe();
        assert!(p.check_call("clipboard", &json!({"action": "copy"})).is_err());
        assert!(p.check_call("cmd_ui_clipboard", &json!({})).is_err());
        assert!(p.check_call("run_command", &json!({"command": "ui.clipboard"})).is_err());
        assert!(p.check_call("command_run", &json!({"id": "export.addToPhotos"})).is_err());
        assert!(p.check_call("command_batch", &json!({"steps": [{"id": "photo.rate"}, {"id": "app.quit"}]})).is_err());
        assert!(p.check_call("command_batch", &json!({"steps": [{"id": "photo.rate"}]})).is_ok());
    }

    #[test]
    fn safe_policy_refuses_move_imports_and_apple_photos() {
        let p = safe();
        assert!(p.check_call("import", &json!({"paths": ["/x"], "mode": "move"})).is_err());
        assert!(p.check_call("import", &json!({"paths": ["/x"], "mode": "copy"})).is_ok());
        assert!(p.check_call("run_command", &json!({"command": "library.import", "params": {"mode": "move"}})).is_err());
        assert!(p.check_call("export", &json!({"dir": "/tmp", "addToPhotos": true})).is_err());
    }

    #[test]
    fn prefix_globs_match_commands_and_their_tools() {
        let mut p = Policy::default();
        p.deny.insert("folder.*".into());
        assert!(p.command_denied("folder.move"));
        assert!(!p.command_denied("photo.rate"));
        assert!(p.tool_allowed("cmd_folder_move").is_err());
        assert!(p.tool_allowed("cmd_photo_rate").is_ok());
    }

    #[test]
    fn allow_list_hides_and_blocks_everything_else() {
        let p = Policy { allow_tools: Some(["render_photo".to_string()].into_iter().collect()), ..Policy::default() };
        let tools = vec![json!({"name": "render_photo"}), json!({"name": "export"})];
        let kept = p.filter_tools(tools);
        assert_eq!(kept.len(), 1);
        assert!(p.check_call("export", &json!({})).is_err());
        assert!(p.check_call("render_photo", &json!({})).is_ok());
    }

    #[test]
    fn export_root_confines_dir_and_path() {
        let root = std::env::temp_dir().join(format!("lc-policy-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let p = Policy { export_root: Some(root.clone()), ..Policy::default() };
        let inside = root.join("out").to_string_lossy().to_string();
        let file_inside = root.join("out").join("a.jpg").to_string_lossy().to_string();
        let escape = root.join("..").join("x").to_string_lossy().to_string();
        assert!(p.check_call("export", &json!({"dir": inside})).is_ok());
        assert!(p.check_call("export", &json!({"path": file_inside})).is_ok());
        assert!(p.check_call("export", &json!({"dir": escape})).is_err());
        assert!(p.check_call("export", &json!({"dir": "relative/dir"})).is_err());
        assert!(p.check_call("export", &json!({})).is_err(), "no implicit remembered folder");
        assert!(p.check_call("export", &json!({"dir": inside, "destinationMode": "sameAsOriginal"})).is_err());
        assert!(p.check_call("export", &json!({"dir": inside, "subfolder": "../../escape"})).is_err());
        assert!(p.check_call("export", &json!({"dir": inside, "subfolder": "2026/trip"})).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }
}
