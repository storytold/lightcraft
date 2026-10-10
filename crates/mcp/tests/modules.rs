//! P3.7 audit: every module is scriptable over MCP. Headless (no window), each module's engine
//! commands are listed as `cmd_*` tools and one harmless command per module runs through
//! `tools/call`. (The UI-only commands of each module, reachable when MCP drives the running app,
//! are audited in `dac_ui_egui::creations_ui`'s tests.)

use dac_mcp::{Headless, Server, command_tool_name};
use serde_json::{Value, json};

fn rpc(s: &mut Server, id: u64, method: &str, params: Value) -> Value {
    let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
    let reply = s.handle_line(&line).expect("reply");
    serde_json::from_str(&reply).expect("json reply")
}

/// (module, command id prefixes that belong to it, a harmless call)
fn modules() -> Vec<(&'static str, &'static [&'static str], &'static str, Value)> {
    vec![
        ("Library", &["library.", "album.", "catalog."], "library.state", json!({})),
        ("Develop", &["develop."], "develop.controls", json!({})),
        ("Map", &["map."], "map.pins", json!({})),
        ("Book", &["book.", "creation.", "layout."], "layout.templates", json!({"kind": "book"})),
        ("Slideshow", &["creation."], "creation.list", json!({"kind": "slideshow"})),
        ("Print", &["print.", "creation.", "layout."], "layout.templates", json!({"kind": "print"})),
        ("Web", &["web."], "web.preview", json!({})),
    ]
}

#[test]
fn every_module_has_commands_and_runs_one_headless() {
    let mut s = Server::new(Box::new(Headless::demo()));
    let list = rpc(&mut s, 1, "tools/list", json!({}));
    let names: Vec<String> =
        list["result"]["tools"].as_array().expect("tools").iter().filter_map(|t| t["name"].as_str().map(str::to_string)).collect();
    let hl = Headless::demo();
    let ids: Vec<&str> = hl.session.commands().iter().map(|c| c.id).collect();
    let mut n = 2;
    for (module, prefixes, call, params) in modules() {
        let mine: Vec<&&str> = ids.iter().filter(|id| prefixes.iter().any(|p| id.starts_with(p))).collect();
        assert!(!mine.is_empty(), "{module}: no commands");
        for id in &mine {
            assert!(names.contains(&command_tool_name(id)), "{module}: {id} is not an MCP tool");
        }
        n += 1;
        let r = rpc(&mut s, n, "tools/call", json!({"name": command_tool_name(call), "arguments": params}));
        assert_ne!(r["result"]["isError"], true, "{module}: {call}: {r}");
    }
    // the saved-creation commands of all four output modules
    for id in ["creation.save", "creation.get", "creation.update", "creation.list", "print.render", "book.render", "web.export", "web.saveGallery"] {
        assert!(ids.contains(&id), "{id}");
    }
}
