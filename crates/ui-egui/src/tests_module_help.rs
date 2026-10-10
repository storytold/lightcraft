//! P6.3: the Classic-style module help (⌘/) lists each module's own keys and closes on a key.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::module::ModuleId;
use crate::{DacApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = DacApp::new(dac_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.settle(Duration::from_secs(120));
    h
}

fn key(h: &mut Headless, key: &str, cmd: bool) {
    let r = h.request("ui.key", json!({"key": key, "cmd": cmd}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

#[test]
fn every_module_binds_the_help_key_and_lists_its_own_keys() {
    for m in ModuleId::ALL {
        let keys = crate::module::get(m).keymap();
        assert!(keys.contains(&crate::help_overlay::KEY), "{m:?} binds ⌘/ to its help");
    }
    let mut h = demo();
    for m in ["library", "develop", "map", "book", "slideshow", "print", "web"] {
        let r = h.request("engine.execute", json!({"command": "module.switch", "params": {"module": m}}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        key(&mut h, "/", true);
        assert!(h.app.help.open, "{m}: ⌘/ opens the module help");
        assert!(h.app.ui.dialog.is_none(), "{m}: not the keymap editor");
        let text = h.painted_text().join("\n");
        let module = h.app.ui.module;
        assert!(text.contains(module.label()), "{m}: the sheet names the module");
        let (own, global) = crate::help_overlay::rows(&h.app, module, false);
        for (_, what) in &own {
            assert!(text.contains(what.as_str()), "{m}: lists {what}");
        }
        assert!(!global.is_empty(), "{m}: and the global keys that work there");
        // any key closes it
        key(&mut h, "Escape", false);
        assert!(!h.app.help.open, "{m}: a key closes the help");
    }
    // Book's own keys are its page and history keys
    let (own, _) = crate::help_overlay::rows(&h.app, ModuleId::Book, false);
    assert!(own.len() >= 3, "{own:?}");
    // the module's command families reach the keymap editor's "Only <module> keys" filter
    assert!(crate::panels::keymap::in_module(ModuleId::Book, "book.addPage"));
    assert!(crate::panels::keymap::in_module(ModuleId::Map, "map.fit"));
    assert!(!crate::panels::keymap::in_module(ModuleId::Map, "book.addPage"));
    // the command toggles and reports, and All Shortcuts opens the editor
    let r = h.request("engine.execute", json!({"command": "module.help", "params": {"show": true}}), T);
    assert_eq!(r["result"]["open"], true, "{r}");
    h.step();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": "button:moduleHelp.all"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(!h.app.help.open);
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::Shortcuts)));
}
