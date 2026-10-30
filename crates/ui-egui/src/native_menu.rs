//! Linux desktop menu projection of the existing local/macOS command model.
//! No transport or window-system dependency: web and other hosts keep the same
//! local menus. Clicks are revalidated on the UI thread before dispatch.

use craft_desktop::{Action, Click, Item, Menu};

use crate::LightcraftApp;
use crate::menubar::{self, MenuNode};

fn blocked(app: &LightcraftApp) -> bool {
    app.ui.dialog.is_some()
        || app.quit_prompt.is_some()
        || app.library_problem.is_some()
        || !app.pending_picks.is_empty()
        || app.recording_shortcut.is_some()
        || app.ui.quit
}

fn items(nodes: Vec<MenuNode>, blocked: bool) -> Vec<Item> {
    nodes
        .into_iter()
        .enumerate()
        .map(|(index, node)| match node {
            MenuNode::Item { id, params, label, shortcut, enabled, checked } => Item {
                key: MenuNode::key(&id, &params),
                label: menubar::display_item_label(&id, &params, &label).to_owned(),
                enabled: enabled && !blocked,
                checked,
                // The registry spells the platform's primary modifier Cmd;
                // DBusMenu on Linux needs Control, not a Super/Meta shortcut.
                shortcut: shortcut.map(|s| menubar::shortcut_text(&s, false)).unwrap_or_default(),
                action: Some(Action { command: id, params }),
                ..Default::default()
            },
            MenuNode::Submenu { label, children } => Item {
                key: format!("submenu:{label}"),
                label: crate::i18n::tr(&label).to_owned(),
                enabled: !blocked,
                children: items(children, blocked),
                ..Default::default()
            },
            MenuNode::Separator => Item { key: format!("separator:{index}"), separator: true, ..Default::default() },
        })
        .collect()
}

pub fn snapshot(app: &LightcraftApp) -> Menu {
    crate::i18n::set_language(app.ui.language);
    let blocked = blocked(app);
    Menu {
        document: app.session.library_generation(),
        items: menubar::menu_bar(app)
            .into_iter()
            .map(|(title, nodes)| Item {
                key: title.clone(),
                label: crate::i18n::tr(&title).to_owned(),
                enabled: !blocked,
                children: items(nodes, blocked),
                ..Default::default()
            })
            .collect(),
    }
}

/// Reject queued actions from a replaced library, disabled/removed commands or
/// a newly opened dialog. Use the same dispatcher as the local menu, including
/// delete confirmation, Compare/Survey targets and automatic advance.
pub fn activate(app: &mut LightcraftApp, click: Click) -> Result<(), String> {
    if click.document != app.session.library_generation() || blocked(app) {
        return Ok(());
    }
    fn available(nodes: &[Item], action: &Action) -> bool {
        nodes.iter().any(|item| item.enabled && (item.action.as_ref() == Some(action) || available(&item.children, action)))
    }
    if available(&snapshot(app).items, &click.action) {
        menubar::run_item(app, &click.action.command, click.action.params)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn app() -> LightcraftApp {
        LightcraftApp::new(lightcraft_engine::Session::new(), Default::default())
    }

    fn find<'a>(nodes: &'a [Item], command: &str) -> Option<&'a Item> {
        nodes.iter().find_map(|item| {
            if item.action.as_ref().is_some_and(|action| action.command == command) { Some(item) } else { find(&item.children, command) }
        })
    }

    fn click(app: &LightcraftApp, command: &str) -> Click {
        let menu = snapshot(app);
        Click { document: menu.document, action: find(&menu.items, command).unwrap().action.clone().unwrap() }
    }

    #[test]
    fn global_menu_toggle_shares_local_state_without_hiding_local_menu() {
        let mut app = app();
        let before = app.ui.filmstrip;
        let action = click(&app, "view.filmstrip");
        activate(&mut app, action).unwrap();
        assert_eq!(app.ui.filmstrip, !before);
        assert_eq!(find(&snapshot(&app).items, "view.filmstrip").unwrap().checked, Some(!before));
        assert!(!app.native_menu);
        assert!(app.native_shortcuts.is_empty());
    }

    #[test]
    fn global_menu_rejects_stale_library_modal_and_injected_actions() {
        let mut app = app();
        let old = click(&app, "view.filmstrip");
        app.session = lightcraft_engine::Session::new();
        let before = app.ui.filmstrip;
        activate(&mut app, old).unwrap();
        assert_eq!(app.ui.filmstrip, before);

        let queued = click(&app, "view.filmstrip");
        app.ui.dialog = Some(crate::state::Dialog::About);
        assert!(!find(&snapshot(&app).items, "view.filmstrip").unwrap().enabled);
        activate(&mut app, queued).unwrap();
        assert_eq!(app.ui.filmstrip, before);
        app.ui.dialog = None;
        let forged = Click {
            document: app.session.library_generation(),
            action: Action { command: "view.filmstrip".into(), params: json!({"unadvertised": true}) },
        };
        activate(&mut app, forged).unwrap();
        assert_eq!(app.ui.filmstrip, before);
    }

    #[test]
    fn global_menu_follows_rating_undo_and_delete_confirmation() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
        let document = app.session.library_generation();
        assert!(!find(&snapshot(&app).items, "edit.undo").unwrap().enabled);
        let rate = Action { command: "photo.rate".into(), params: json!({"rating": 3}) };
        activate(&mut app, Click { document, action: rate }).unwrap();
        assert!(find(&snapshot(&app).items, "edit.undo").unwrap().enabled);
        let deletion = click(&app, "photo.delete");
        activate(&mut app, deletion).unwrap();
        assert!(app.ui.dialog.is_some(), "native menu must preserve the existing delete prompt");
    }

    #[test]
    fn global_menu_labels_preserve_user_data_and_keys_ignore_translation() {
        let nodes = vec![MenuNode::Item {
            id: "album.addPhotos".into(),
            params: json!({"id": 7}),
            label: "File".into(),
            shortcut: Some("Cmd+Shift+A".into()),
            enabled: true,
            checked: None,
        }];
        let converted = items(nodes, false);
        assert_eq!(converted[0].label, "File");
        assert_eq!(converted[0].shortcut, "Ctrl+Shift+A");
        assert_eq!(converted[0].key, "album.addPhotos|{\"id\":7}");
    }
}
