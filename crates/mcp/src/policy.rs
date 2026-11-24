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

/// The comma-separated, trimmed, non-empty items of variable `var`, as `get` finds it.
fn csv(get: &impl Fn(&str) -> Option<String>, var: &str) -> Vec<String> {
    get(var).unwrap_or_default().split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

impl Policy {
    /// A policy that refuses nothing (the upstream behaviour).
    pub fn permissive() -> Self {
        Self::default()
    }

    /// Read `LIGHTCRAFT_MCP_*` from the environment.
    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Read `LIGHTCRAFT_MCP_*` through `get` (the environment, or a map in tests).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let safe = matches!(get("LIGHTCRAFT_MCP_SAFE").as_deref(), Some("1") | Some("true") | Some("yes"));
        let mut p = Self { safe, ..Self::default() };
        if safe {
            p.deny.extend(SAFE_DENY.iter().map(|s| s.to_string()));
            p.deny_tools.extend(SAFE_DENY_TOOLS.iter().map(|s| s.to_string()));
        }
        for item in csv(&get, "LIGHTCRAFT_MCP_DENY") {
            // A command id contains a dot (`library.import`, `photo.*`); anything else is a tool name.
            if item.contains('.') {
                p.deny.insert(item);
            } else {
                p.deny_tools.insert(item);
            }
        }
        let allow: BTreeSet<String> = csv(&get, "LIGHTCRAFT_MCP_ALLOW_TOOLS").into_iter().collect();
        if !allow.is_empty() {
            p.allow_tools = Some(allow);
        }
        if let Some(root) = get("LIGHTCRAFT_MCP_EXPORT_ROOT") {
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
        for id in helper_commands(name).unwrap_or(&[]) {
            if self.command_denied(id) {
                return Err(format!("tool `{name}` runs command `{id}`, which is denied by policy"));
            }
        }
        // `tools.rs` maps `cmd_a_b` back to the command `a.b` (ids never contain `_`), so the match is exact.
        if let Some(rest) = name.strip_prefix(COMMAND_TOOL_PREFIX) {
            let id = rest.replace('_', ".");
            if self.command_denied(&id) {
                return Err(format!("tool `{name}` runs command `{id}`, which is denied by policy"));
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

    /// Refuse `id` when the deny list names it.
    fn deny_check(&self, id: &str) -> Result<(), String> {
        if self.command_denied(id) { Err(format!("command `{id}` is denied by policy")) } else { Ok(()) }
    }

    /// The single enforcement point: called from `tools/call` before anything executes. It may also
    /// rewrite `args` (safe mode pins `addToPhotos` to `false` on every export route, so a preset
    /// cannot switch it on); the caller must run the tool with the rewritten arguments.
    pub fn check_call(&self, name: &str, args: &mut Value) -> Result<(), String> {
        if !self.is_active() {
            return Ok(());
        }
        self.tool_allowed(name)?;
        match name {
            "command_run" => self.check_slot(args, "id", "params"),
            "run_command" => self.check_slot(args, "command", "params"),
            "command_batch" => {
                for step in args.get_mut("steps").and_then(Value::as_array_mut).into_iter().flatten() {
                    self.check_slot(step, "id", "params")?;
                }
                Ok(())
            }
            "import" => self.check_route("library.import", args),
            "export" => self.check_route("app.export", args),
            n => match n.strip_prefix(COMMAND_TOOL_PREFIX) {
                Some(rest) => self.check_route(&rest.replace('_', "."), args),
                None => Ok(()),
            },
        }
    }

    /// A call that names its command in `holder[id_key]` and carries its params in `holder[params_key]`
    /// (`command_run`, `run_command`, one `command_batch` step).
    fn check_slot(&self, holder: &mut Value, id_key: &str, params_key: &str) -> Result<(), String> {
        let Some(id) = holder.get(id_key).and_then(Value::as_str).map(str::to_string) else { return Ok(()) };
        self.deny_check(&id)?;
        if !matches!(id.as_str(), "library.import" | "app.export") {
            return Ok(());
        }
        let Some(obj) = holder.as_object_mut() else { return Ok(()) };
        self.check_route(&id, obj.entry(params_key).or_insert(Value::Null))
    }

    /// Deny-list and argument checks for running `id` with `params` (rewritten in place where needed).
    fn check_route(&self, id: &str, params: &mut Value) -> Result<(), String> {
        self.deny_check(id)?;
        match id {
            "library.import" if self.safe => self.check_import(params),
            "app.export" if self.safe || self.export_root.is_some() => self.check_export(params),
            _ => Ok(()),
        }
    }

    /// Safe mode: an import must not move (and so delete) the source files.
    fn check_import(&self, params: &Value) -> Result<(), String> {
        if params.get("mode").and_then(Value::as_str).map(|m| m.trim().to_ascii_lowercase()).as_deref() == Some("move") {
            return Err("import mode `move` deletes the source files and is refused under LIGHTCRAFT_MCP_SAFE; use `copy` or `add`".into());
        }
        Ok(())
    }

    /// Export params: no Apple Photos hand-off in safe mode (pinned to `false`, which also overrides a
    /// preset), `subfolder` stays inside `dir`, and with an export root every destination is inside it.
    fn check_export(&self, params: &mut Value) -> Result<(), String> {
        if params.is_null() {
            *params = Value::Object(Default::default());
        }
        let Some(obj) = params.as_object_mut() else { return Err("export params must be an object".into()) };
        if self.safe {
            if obj.get("addToPhotos").and_then(Value::as_bool) == Some(true) {
                return Err("`addToPhotos` is refused under LIGHTCRAFT_MCP_SAFE".into());
            }
            // The call's own params override a preset's, so this beats a preset that sets it.
            obj.insert("addToPhotos".into(), Value::Bool(false));
        }
        // What is checked below is what the exporter must receive: write the trimmed `path` and `dir` back,
        // or `" C:\x"` is absolute for the check and relative for the writer.
        for key in ["path", "dir"] {
            if let Some(Value::String(v)) = obj.get_mut(key) {
                let trimmed = v.trim();
                if trimmed.len() != v.len() {
                    *v = trimmed.to_string();
                }
            }
        }
        if self.export_root.is_some() && obj.contains_key("preset") {
            // A preset may carry `destinationMode` / `subfolder`; pin them unless the call gives its own (checked below).
            obj.entry("destinationMode").or_insert_with(|| Value::String("specificFolder".into()));
            obj.entry("subfolder").or_insert_with(|| Value::String(String::new()));
        }
        // The engine trims `subfolder` and replaces `\`, `:` and NUL with `_` (export/params.rs
        // `from_json`); judge the value it will actually use. `..` and absolute parts would climb out of `dir`.
        let raw_subfolder = obj.get("subfolder").and_then(Value::as_str).map(str::to_string);
        let subfolder = raw_subfolder.as_deref().map(normalise_subfolder);
        if let (Some(raw), Some(sub)) = (&raw_subfolder, &subfolder) {
            let escapes = Path::new(sub).components().any(|c| matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_)));
            if escapes || raw.contains('\\') {
                return Err(format!("export `subfolder` {raw:?} must be a relative path without `..`"));
            }
        }
        let Some(root) = &self.export_root else { return Ok(()) };
        let same_as_original = obj.get("destinationMode").and_then(Value::as_str).is_some_and(|m| m.trim().eq_ignore_ascii_case("sameAsOriginal"));
        if same_as_original {
            return Err("`destinationMode: sameAsOriginal` writes next to the originals, outside LIGHTCRAFT_MCP_EXPORT_ROOT".into());
        }
        let path = obj.get("path").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
        let dir = obj.get("dir").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
        if path.is_none() && dir.is_none() {
            return Err(format!(
                "export needs an explicit `dir` or `path` inside {} (the app's remembered export folder is not allowed)",
                root.display()
            ));
        }
        if let Some(p) = path {
            // `path` is a file: its parent must be inside the root.
            let parent = Path::new(p).parent().unwrap_or_else(|| Path::new(p));
            within(root, parent).map_err(|e| format!("export `path` {p}: {e}"))?;
        }
        if let Some(d) = dir {
            // Files land in `dir` joined with the subfolder, so that is the place to check.
            let target = match subfolder.as_deref().filter(|s| !s.is_empty()) {
                Some(sub) => Path::new(d).join(sub),
                None => PathBuf::from(d),
            };
            within(root, &target).map_err(|e| format!("export `dir` {d}: {e}"))?;
        }
        Ok(())
    }
}

/// `subfolder` as the engine reads it: trimmed, with `\`, `:` and NUL replaced by `_`.
fn normalise_subfolder(sub: &str) -> String {
    sub.trim().replace(['\\', ':', '\0'], "_")
}

/// The command ids a helper tool dispatches in `tools.rs` `call_tool`, incidental ones included (the
/// `library.select` behind an `id`), so denying an id also denies the helper that runs it. `None`
/// for tools that name their command in the arguments (`run_command`, `command_run`, ...) and for `cmd_*`.
fn helper_commands(tool: &str) -> Option<&'static [&'static str]> {
    Some(match tool {
        "list_commands" | "command_list" => &["engine.commands"],
        "doc_inspect" => &["library.state", "catalog.stats"],
        "render_photo" | "render_preview" => &["export.checkTarget", "library.state", "ui.render"],
        "import" => &["library.import"],
        "query_photos" => &["catalog.query"],
        "select_photos" => &["library.select"],
        "list_controls" => &["develop.controls"],
        "get_develop" => &["develop.get"],
        "set_develop" => &["library.select", "develop.set", "develop.merge", "develop.controls"],
        "apply_preset" => &["preset.apply"],
        "crop" => &["library.select", "crop.reset", "crop.set"],
        "export" => &["library.select", "app.export"],
        "screenshot" | "ui_screenshot" => &["export.checkTarget", "ui.screenshot"],
        "inspect_ui" | "ui_inspect" => &["ui.inspect"],
        "set_ui" => &["ui.set"],
        "list_widgets" => &["ui.widgets"],
        "click" => &["ui.clickWidget", "ui.click"],
        "press_key" => &["ui.key"],
        "type_text" => &["ui.text"],
        "clipboard" => &["ui.clipboard"],
        "pointer_gesture" => &["ui.pointer"],
        _ => return None,
    })
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

/// `p` with its longest existing ancestor canonicalized and the not-yet-existing rest appended; `..` in
/// that rest is an error.
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

    /// A policy as `LIGHTCRAFT_MCP_SAFE=1` builds it.
    fn safe() -> Policy {
        let mut p = Policy { safe: true, ..Policy::default() };
        p.deny.extend(SAFE_DENY.iter().map(|s| s.to_string()));
        p.deny_tools.extend(SAFE_DENY_TOOLS.iter().map(|s| s.to_string()));
        p
    }

    /// `check_call` on a copy of `args`: the verdict, with the arguments as the tool would receive them.
    fn call(p: &Policy, name: &str, args: Value) -> Result<Value, String> {
        let mut args = args;
        p.check_call(name, &mut args).map(|()| args)
    }

    /// A fresh export root under the temp folder; `tag` keeps tests apart.
    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("lc-policy-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// A policy read from `vars` instead of the process environment.
    fn from_vars(vars: &[(&str, &str)]) -> Policy {
        Policy::from_lookup(|k| vars.iter().find(|(name, _)| *name == k).map(|(_, v)| v.to_string()))
    }

    #[test]
    fn an_empty_allow_list_means_no_allow_list() {
        for value in ["", "  ", " , ,"] {
            assert_eq!(from_vars(&[("LIGHTCRAFT_MCP_ALLOW_TOOLS", value)]).allow_tools, None, "{value:?}");
        }
        assert!(!from_vars(&[]).is_active(), "nothing set: the policy is inert");
    }

    #[test]
    fn a_non_empty_allow_list_lets_only_those_tools_through() {
        let p = from_vars(&[("LIGHTCRAFT_MCP_ALLOW_TOOLS", " render_photo , query_photos ")]);
        assert!(p.tool_allowed("render_photo").is_ok());
        assert!(p.tool_allowed("query_photos").is_ok());
        assert!(p.tool_allowed("export").is_err());
        assert!(p.tool_allowed("cmd_photo_rate").is_err());
    }

    #[test]
    fn safe_from_the_environment_denies_the_safe_set() {
        for on in ["1", "true", "yes"] {
            let p = from_vars(&[("LIGHTCRAFT_MCP_SAFE", on)]);
            assert!(p.safe, "{on}");
            assert!(SAFE_DENY.iter().all(|id| p.deny.contains(*id)), "{on}: {:?}", p.deny);
            assert!(SAFE_DENY_TOOLS.iter().all(|t| p.deny_tools.contains(*t)), "{on}");
        }
        for off in ["0", "", "no"] {
            assert!(!from_vars(&[("LIGHTCRAFT_MCP_SAFE", off)]).safe, "{off:?}");
        }
    }

    #[test]
    fn deny_is_split_and_sorted_into_command_ids_and_tool_names() {
        let p = from_vars(&[("LIGHTCRAFT_MCP_DENY", " folder.*, photo.rate ,export,, clipboard ")]);
        assert_eq!(p.deny.iter().map(String::as_str).collect::<Vec<_>>(), ["folder.*", "photo.rate"]);
        assert_eq!(p.deny_tools.iter().map(String::as_str).collect::<Vec<_>>(), ["clipboard", "export"]);
        // safe and deny add up
        let p = from_vars(&[("LIGHTCRAFT_MCP_SAFE", "1"), ("LIGHTCRAFT_MCP_DENY", "photo.rate")]);
        assert!(p.deny.contains("photo.rate") && p.deny.contains("app.quit"));
    }

    /// Runs in a child process (see [`from_env_reads_the_process_environment`]): the real
    /// `Policy::from_env`, with `LIGHTCRAFT_MCP_SAFE=1` set by the parent.
    #[test]
    #[ignore = "subprocess helper for from_env_reads_the_process_environment"]
    fn from_env_child() {
        if std::env::var_os("LC_POLICY_FROM_ENV_CHILD").is_none() {
            return; // started by hand with --ignored: nothing to check without the parent's environment
        }
        let p = Policy::from_env();
        assert!(p.safe, "LIGHTCRAFT_MCP_SAFE=1 turns safe mode on: {p:?}");
        assert!(SAFE_DENY.iter().all(|id| p.deny.contains(*id)), "{:?}", p.deny);
        assert!(p.allow_tools.is_none() && p.export_root.is_none(), "{p:?}");
        println!("FROM-ENV-CHILD-CHECKED");
    }

    /// `from_env` itself reads the real environment: run it in a child, so the test never changes this process's.
    #[test]
    fn from_env_reads_the_process_environment() {
        let _serial = crate::serial_sockets_and_children();
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "policy::tests::from_env_child", "--ignored", "--nocapture"])
            .env("LC_POLICY_FROM_ENV_CHILD", "1")
            .env("LIGHTCRAFT_MCP_SAFE", "1")
            .env_remove("LIGHTCRAFT_MCP_DENY")
            .env_remove("LIGHTCRAFT_MCP_ALLOW_TOOLS")
            .env_remove("LIGHTCRAFT_MCP_EXPORT_ROOT")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
        assert!(stdout.contains("FROM-ENV-CHILD-CHECKED"), "the child did not run its checks: {stdout}");
    }

    #[test]
    fn the_export_root_is_trimmed_and_empty_means_none() {
        assert_eq!(from_vars(&[("LIGHTCRAFT_MCP_EXPORT_ROOT", " /srv/out ")]).export_root, Some(PathBuf::from("/srv/out")));
        assert_eq!(from_vars(&[("LIGHTCRAFT_MCP_EXPORT_ROOT", "  ")]).export_root, None);
    }

    #[test]
    fn permissive_policy_refuses_nothing() {
        let p = Policy::permissive();
        assert!(call(&p, "clipboard", json!({"action": "copy"})).is_ok());
        assert!(call(&p, "import", json!({"paths": ["/x"], "mode": "move"})).is_ok());
    }

    #[test]
    fn safe_policy_blocks_every_route_to_a_denied_command() {
        let p = safe();
        assert!(call(&p, "clipboard", json!({"action": "copy"})).is_err());
        assert!(call(&p, "cmd_ui_clipboard", json!({})).is_err());
        assert!(call(&p, "run_command", json!({"command": "ui.clipboard"})).is_err());
        assert!(call(&p, "command_run", json!({"id": "export.addToPhotos"})).is_err());
        assert!(call(&p, "command_batch", json!({"steps": [{"id": "photo.rate"}, {"id": "app.quit"}]})).is_err());
        assert!(call(&p, "command_batch", json!({"steps": [{"id": "photo.rate"}]})).is_ok());
    }

    #[test]
    fn safe_policy_refuses_move_imports_and_apple_photos() {
        let p = safe();
        assert!(call(&p, "import", json!({"paths": ["/x"], "mode": "move"})).is_err());
        assert!(call(&p, "import", json!({"paths": ["/x"], "mode": " Move "})).is_err());
        assert!(call(&p, "import", json!({"paths": ["/x"], "mode": "copy"})).is_ok());
        assert!(call(&p, "run_command", json!({"command": "library.import", "params": {"mode": "move"}})).is_err());
        assert!(call(&p, "export", json!({"dir": "/tmp", "addToPhotos": true})).is_err());
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
        assert!(call(&p, "export", json!({})).is_err());
        assert!(call(&p, "render_photo", json!({})).is_ok());
    }

    #[test]
    fn denying_a_command_id_also_denies_the_helper_tool_that_runs_it() {
        let mut p = Policy::default();
        p.deny.insert("catalog.query".into());
        assert!(call(&p, "query_photos", json!({})).unwrap_err().contains("catalog.query"));
        assert!(call(&p, "cmd_catalog_query", json!({})).is_err());
        assert!(call(&p, "run_command", json!({"command": "catalog.query"})).is_err());
        assert!(call(&p, "get_develop", json!({})).is_ok(), "other helpers stay usable");
        // and it is not offered either
        let listed = p.filter_tools(vec![json!({"name": "query_photos"}), json!({"name": "get_develop"})]);
        assert_eq!(listed, vec![json!({"name": "get_develop"})]);
        // every helper maps to the ids it dispatches
        for (tool, id) in [
            ("select_photos", "library.select"),
            ("set_develop", "library.select"),
            ("list_controls", "develop.controls"),
            ("get_develop", "develop.get"),
            ("set_develop", "develop.set"),
            ("apply_preset", "preset.apply"),
            ("crop", "crop.set"),
            ("render_photo", "ui.render"),
            ("render_preview", "ui.render"),
            ("doc_inspect", "catalog.stats"),
            ("command_list", "engine.commands"),
            ("screenshot", "ui.screenshot"),
            ("inspect_ui", "ui.inspect"),
            ("set_ui", "ui.set"),
            ("list_widgets", "ui.widgets"),
            ("click", "ui.click"),
            ("press_key", "ui.key"),
            ("type_text", "ui.text"),
            ("pointer_gesture", "ui.pointer"),
            ("export", "app.export"),
            ("import", "library.import"),
        ] {
            let mut p = Policy::default();
            p.deny.insert(id.into());
            assert!(call(&p, tool, json!({})).is_err(), "{tool} should be refused when {id} is denied");
        }
    }

    #[test]
    fn safe_mode_pins_add_to_photos_false_on_every_export_route() {
        let p = safe();
        // a preset may carry addToPhotos: true; the call's own value overrides the preset's
        let out = call(&p, "export", json!({"preset": "x", "dir": "/tmp"})).unwrap();
        assert_eq!(out["addToPhotos"], false);
        assert_eq!(out["preset"], "x");
        assert_eq!(call(&p, "cmd_app_export", json!({"preset": "x"})).unwrap()["addToPhotos"], false);
        assert_eq!(call(&p, "command_run", json!({"id": "app.export", "params": {"preset": "x"}})).unwrap()["params"]["addToPhotos"], false);
        // no params at all
        assert_eq!(call(&p, "run_command", json!({"command": "app.export"})).unwrap()["params"]["addToPhotos"], false);
        let batch =
            call(&p, "command_batch", json!({"steps": [{"id": "photo.rate", "params": {}}, {"id": "app.export", "params": {"preset": "x"}}]}))
                .unwrap();
        assert_eq!(batch["steps"][1]["params"]["addToPhotos"], false);
        assert!(batch["steps"][0]["params"].get("addToPhotos").is_none(), "other steps are left alone");
        // an explicit request is refused rather than quietly turned off
        assert!(call(&p, "export", json!({"addToPhotos": true})).is_err());
        // not in safe mode: arguments are untouched
        let open = Policy { deny: ["folder.move".to_string()].into_iter().collect(), ..Policy::default() };
        assert!(call(&open, "export", json!({"preset": "x"})).unwrap().get("addToPhotos").is_none());
    }

    #[test]
    fn export_root_confines_dir_and_path() {
        let root = temp_root("confine");
        let p = Policy { export_root: Some(root.clone()), ..Policy::default() };
        let inside = root.join("out").to_string_lossy().to_string();
        let file_inside = root.join("out").join("a.jpg").to_string_lossy().to_string();
        let escape = root.join("..").join("x").to_string_lossy().to_string();
        assert!(call(&p, "export", json!({"dir": inside})).is_ok());
        assert!(call(&p, "export", json!({"path": file_inside})).is_ok());
        assert!(call(&p, "export", json!({"dir": escape})).is_err());
        assert!(call(&p, "export", json!({"dir": "relative/dir"})).is_err());
        assert!(call(&p, "export", json!({})).is_err(), "no implicit remembered folder");
        assert!(call(&p, "export", json!({"dir": inside, "destinationMode": "sameAsOriginal"})).is_err());
        assert!(call(&p, "export", json!({"dir": inside, "destinationMode": " SameAsOriginal "})).is_err(), "the engine reads it case-insensitively");
        assert!(call(&p, "export", json!({"dir": inside, "subfolder": "../../escape"})).is_err());
        assert!(call(&p, "export", json!({"dir": inside, "subfolder": "2026/trip"})).is_ok());
        // the engine trims the subfolder first, so these are `..`
        assert!(call(&p, "export", json!({"dir": inside, "subfolder": " .."})).is_err());
        assert!(call(&p, "export", json!({"dir": inside, "subfolder": "..\t"})).is_err());
        assert!(call(&p, "export", json!({"dir": inside, "subfolder": "..\\x"})).is_err());
        // `:` becomes `_`, which is an ordinary name
        assert!(call(&p, "export", json!({"dir": inside, "subfolder": "a:b"})).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_exporter_receives_the_trimmed_path_and_dir_that_were_checked() {
        let root = temp_root("trim");
        let p = Policy { export_root: Some(root.clone()), ..Policy::default() };
        let out = root.join("out").to_string_lossy().to_string();
        let file = root.join("out").join("a.jpg").to_string_lossy().to_string();
        let args = call(&p, "export", json!({"dir": format!(" {out}\t")})).unwrap();
        assert_eq!(args["dir"], out.as_str(), "the checked value is the one passed on");
        let args = call(&p, "run_command", json!({"command": "app.export", "params": {"path": format!("  {file} ")}})).unwrap();
        assert_eq!(args["params"]["path"], file.as_str());
        // trimmed, it is relative: refused (not absolute for the check and relative for the writer)
        assert!(call(&p, "export", json!({"dir": " relative"})).is_err());
        assert!(call(&p, "export", json!({"path": " relative/a.jpg"})).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_preset_cannot_pick_another_destination_under_an_export_root() {
        let root = temp_root("preset");
        let p = Policy { export_root: Some(root.clone()), ..Policy::default() };
        let dir = root.to_string_lossy().to_string();
        let out = call(&p, "export", json!({"dir": dir, "preset": "web"})).unwrap();
        assert_eq!(out["destinationMode"], "specificFolder");
        assert_eq!(out["subfolder"], "");
        // what the call sets itself is kept (and checked)
        let out = call(&p, "export", json!({"dir": dir, "preset": "web", "subfolder": "trip"})).unwrap();
        assert_eq!(out["subfolder"], "trip");
        assert!(call(&p, "export", json!({"dir": dir, "preset": "web", "destinationMode": "sameAsOriginal"})).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_lead_out_of_the_export_root() {
        use std::os::unix::fs::symlink;
        let root = temp_root("symlink");
        let outside = temp_root("symlink-outside");
        let p = Policy { export_root: Some(root.clone()), ..Policy::default() };
        // an in-root link that points outside
        symlink(&outside, root.join("link")).unwrap();
        let through_link = root.join("link").join("x").to_string_lossy().to_string();
        assert!(call(&p, "export", json!({"dir": through_link})).is_err());
        // `a:b` is written as `a_b`; if that is a link pointing outside, the joined path is outside
        std::fs::create_dir_all(root.join("out")).unwrap();
        symlink(&outside, root.join("out").join("a_b")).unwrap();
        let dir = root.join("out").to_string_lossy().to_string();
        assert!(call(&p, "export", json!({"dir": dir, "subfolder": "a:b"})).is_err());
        assert!(call(&p, "export", json!({"dir": dir, "subfolder": "fine"})).is_ok());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
