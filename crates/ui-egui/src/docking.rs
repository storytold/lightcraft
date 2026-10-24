//! Application panel identities and persistence around the shared docking renderer.
//! Document contents remain owned by their existing view implementations.

use craft_ui::docking::{
    Action, DockArea, DockCommand, DockError, DockOperation, DockStyle, FloatSpace, Layout, Node, PanelLimits, PanelState, Permissions, Placement,
    UiDock, Zone,
};
use craft_ui::layout::{SplitAxis, SplitSize};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Panel {
    Canvas,
    Library,
    Tools,
    Presets,
}

impl Panel {
    pub const ALL: &[Self] = &[Self::Canvas, Self::Library, Self::Tools, Self::Presets];
    pub fn id(self) -> &'static str {
        match self {
            Self::Canvas => "canvas",
            Self::Library => "library",
            Self::Tools => "tools",
            Self::Presets => "presets",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Canvas => "Photos",
            Self::Library => "Library",
            Self::Tools => "Tools",
            Self::Presets => "Presets",
        }
    }
    fn default_side(self) -> Zone {
        match self {
            Self::Canvas => Zone::Right,
            Self::Library => Zone::Left,
            Self::Tools => Zone::Right,
            Self::Presets => Zone::Right,
        }
    }
}

fn permissions(panel: &Panel) -> Permissions {
    // Panels open and split beside the photo view, but never join it as tabs.
    if *panel == Panel::Canvas { Permissions { accepts_splits: true, ..Permissions::PROTECTED } } else { Permissions::default() }
}

fn tabs(panel: Panel) -> Node<Panel> {
    Node::Tabs { panels: vec![panel], active: 0 }
}

fn split(axis: SplitAxis, size: SplitSize, first: Node<Panel>, second: Node<Panel>) -> Node<Panel> {
    Node::Split { axis, size, first: Box::new(first), second: Box::new(second) }
}

/// The saved arrangement. Closed panels keep their last group, side, size or floating rect.
pub type Workspace = craft_ui::docking::Workspace<Panel>;

pub fn default_workspace() -> Workspace {
    Workspace::new(Layout {
        root: Some(split(
            SplitAxis::Horizontal,
            SplitSize::FixedFirst(crate::state::LEFT_WIDTH.default),
            tabs(Panel::Library),
            split(
                SplitAxis::Horizontal,
                SplitSize::FixedSecond(crate::state::RIGHT_WIDTH.default),
                split(SplitAxis::Horizontal, SplitSize::FixedSecond(300.0), tabs(Panel::Canvas), tabs(Panel::Presets)),
                tabs(Panel::Tools),
            ),
        )),
        floating: Vec::new(),
    })
}

fn error(error: DockError) -> String {
    error.to_string()
}

/// The shared checks plus LightCraft's own: saved settings cannot float, hide, share or
/// collapse the photo view, nor send a hidden panel back onto it.
pub fn validate(workspace: &Workspace) -> Result<(), String> {
    workspace.validate().map_err(error)?;
    let mut nodes: Vec<_> = workspace.layout.root.iter().collect();
    let mut alone = false;
    while let Some(node) = nodes.pop() {
        match node {
            Node::Split { first, second, .. } => {
                nodes.push(first);
                nodes.push(second);
            }
            Node::Tabs { panels, .. } => alone |= panels.as_slice() == [Panel::Canvas],
            _ => {}
        }
    }
    if !alone {
        return Err("The document view must stay docked in a group of its own".into());
    }
    if workspace.hidden.iter().any(|(_, location)| {
        location.anchor == Some(Panel::Canvas) && matches!(location.placement, Placement::Tab { .. } | Placement::Split(Zone::Center))
    }) {
        return Err("A hidden panel cannot restore over the document view".into());
    }
    Ok(())
}

fn apply(workspace: &mut Workspace, action: Action<Panel>) -> Result<(), String> {
    let mut next = workspace.clone();
    next.apply(action, permissions).map_err(error)?;
    validate(&next)?;
    *workspace = next;
    Ok(())
}

/// Shows a panel at its recorded place, else beside the photo on its usual side. A panel that
/// already has the asked visibility keeps its tab selection.
fn set_visible(workspace: &mut Workspace, panel: Panel, visible: bool) -> Result<(), String> {
    if workspace.layout.contains(&panel) == visible {
        return Ok(());
    }
    if !visible {
        return apply(workspace, Action::Close { panel });
    }
    validate(workspace)?;
    let mut next = workspace.clone();
    if next.set_visible(panel, true, None, permissions).is_ok() && validate(&next).is_ok() {
        *workspace = next;
        return Ok(());
    }
    apply(workspace, Action::OpenAt { panel, anchor: Panel::Canvas, placement: Placement::Split(panel.default_side()) })
}

/// Membership is passive state; an explicit command also exposes the panel's body.
fn reveal(workspace: &mut Workspace, panel: Panel) -> Result<(), String> {
    set_visible(workspace, panel, true)?;
    apply(workspace, Action::Activate { panel })?;
    if workspace.layout.panel_state(&panel) != PanelState::Front {
        // A minimized window or group opens again; where nothing is minimized the action is
        // refused and the layout stays as it is.
        let _ = apply(workspace, Action::SetMinimized { panel, minimized: false });
    }
    Ok(())
}

pub fn exposed_in(workspace: &Workspace, panel: Panel) -> bool {
    workspace.layout.panel_state(&panel) == PanelState::Front
}

/// `ui.dock` params, parsed by the shared command vocabulary.
fn run(workspace: &mut Workspace, params: &Value) -> Result<(), String> {
    validate(workspace)?;
    let mut tagged = params.clone();
    let fields = tagged.as_object_mut().ok_or("ui.dock params must be an object")?;
    fields.insert("command".into(), json!("ui.dock"));
    let command: DockCommand<Panel> = serde_json::from_value(tagged).map_err(|error| error.to_string())?;
    if let DockCommand::UiDock(UiDock::Operation(DockOperation::Open { panel })) = command {
        return reveal(workspace, panel);
    }
    let mut next = workspace.clone();
    // `ui.dock` operations carry their own rects; only `window.panel.float` reads the space.
    command.apply(&mut next, false, &FloatSpace::new(egui::Rect::ZERO), permissions).map_err(error)?;
    validate(&next)?;
    *workspace = next;
    Ok(())
}

fn from_ui(app: &crate::LightcraftApp) -> Workspace {
    let mut workspace = default_workspace();
    if let Some(Node::Split { size, second, .. }) = workspace.layout.root.as_mut() {
        *size = SplitSize::FixedFirst(crate::state::LEFT_WIDTH.clamp(app.ui.left_width));
        if let Node::Split { size, .. } = second.as_mut() {
            *size = SplitSize::FixedSecond(crate::state::RIGHT_WIDTH.clamp(app.ui.right_width));
        }
    }
    workspace
}

fn sync(workspace: &mut Workspace, app: &crate::LightcraftApp) -> Result<(), String> {
    set_visible(workspace, Panel::Library, app.ui.left_panel)?;
    set_visible(workspace, Panel::Tools, app.ui.right != crate::state::RightPanel::None)?;
    set_visible(workspace, Panel::Presets, app.ui.presets)
}

fn publish_visibility(app: &mut crate::LightcraftApp, workspace: &Workspace) {
    app.ui.left_panel = workspace.layout.contains(&Panel::Library);
    app.ui.presets = workspace.layout.contains(&Panel::Presets);
    if !workspace.layout.contains(&Panel::Tools) {
        app.ui.right = crate::state::RightPanel::None;
    } else if app.ui.right == crate::state::RightPanel::None {
        app.ui.right = crate::state::RightPanel::Edit;
    }
    for (panel, width, limits) in
        [(Panel::Library, &mut app.ui.left_width, crate::state::LEFT_WIDTH), (Panel::Tools, &mut app.ui.right_width, crate::state::RIGHT_WIDTH)]
    {
        let Ok(location) = workspace.layout.location(&panel) else {
            continue;
        };
        let value = match (location.floating, location.placement, location.size) {
            (Some([_, _, width, _]), _, _)
            | (None, Placement::Split(Zone::Left), Some(SplitSize::FixedFirst(width)))
            | (None, Placement::Split(Zone::Right), Some(SplitSize::FixedSecond(width))) => Some(width),
            // A vertical split stores height, and the opposite fixed side sizes a sibling.
            _ => None,
        };
        if let Some(value) = value {
            *width = limits.clamp(value);
        }
    }
}

pub(crate) fn exposed(app: &crate::LightcraftApp, panel: Panel) -> bool {
    app.ui.docking.as_ref().is_none_or(|workspace| exposed_in(workspace, panel))
}

pub(crate) fn reveal_command(app: &mut crate::LightcraftApp, id: &str) -> Result<(), String> {
    let panel = if id == "panel.presets" && app.ui.presets {
        Some(Panel::Presets)
    } else if id == "app.showInFolders" {
        // the sidebar it scrolls, also when it is a tab behind another panel
        Some(Panel::Library)
    } else if (id.starts_with("panel.") && id != "panel.close" && id != "panel.presets"
        || id.starts_with("section.")
        || id.starts_with("tool.") && id != "tool.none")
        && app.ui.right != crate::state::RightPanel::None
    {
        Some(Panel::Tools)
    } else {
        None
    };
    if let Some(panel) = panel {
        let mut workspace = app.ui.docking.clone().unwrap_or_else(|| from_ui(app));
        sync(&mut workspace, app)?;
        reveal(&mut workspace, panel)?;
        cancel_replaced_gesture(app, Some(&workspace));
        app.ui.docking = Some(workspace);
    }
    Ok(())
}

/// Check the target's actual egui layer even while a different widget owns the drag.
pub(crate) fn drop_target(ui: &egui::Ui, rect: egui::Rect, pointer: Option<egui::Pos2>) -> bool {
    pointer.is_some_and(|pos| rect.intersect(ui.clip_rect()).contains(pos) && ui.ctx().layer_id_at(pos) == Some(ui.layer_id()))
}

/// Explicit host commands replace the snapshot before rendering. Cancel only their old
/// gestures; actions returned by the renderer must keep continuous resizing alive.
pub(crate) fn cancel_replaced_gesture(app: &crate::LightcraftApp, workspace: Option<&Workspace>) {
    let replaced = match (app.ui.docking.as_ref(), workspace) {
        (Some(old), Some(next)) => old.layout != next.layout || old.hidden != next.hidden,
        (None, None) => false,
        _ => true,
    };
    if replaced && let Some(ctx) = &app.tasks.repaint {
        craft_ui::docking::cancel_drag::<Panel>(ctx, egui::Id::new("lightcraft-docking"));
    }
}

pub fn command(app: &mut crate::LightcraftApp, reset: bool, params: &Value) -> Result<Value, String> {
    command_impl(app, reset, params, false)
}

fn command_impl(app: &mut crate::LightcraftApp, reset: bool, params: &Value, renderer_action: bool) -> Result<Value, String> {
    let mut workspace = app.ui.docking.clone().unwrap_or_else(|| from_ui(app));
    if reset {
        workspace = default_workspace();
        // The usual LightCraft starting workspace shows its current tool beside the photo.
        set_visible(&mut workspace, Panel::Library, false)?;
        set_visible(&mut workspace, Panel::Presets, false)?;
    } else {
        sync(&mut workspace, app)?;
        run(&mut workspace, params)?;
    }
    if !renderer_action {
        cancel_replaced_gesture(app, Some(&workspace));
    }
    publish_visibility(app, &workspace);
    app.ui.docking = Some(workspace);
    Ok(json!({"docking": app.ui.docking}))
}

pub fn show(app: &mut crate::LightcraftApp, ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let mut workspace = app.ui.docking.clone().unwrap_or_else(|| from_ui(app));
    if let Err(error) = validate(&workspace).and_then(|()| sync(&mut workspace, app)) {
        app.ui.toast = Some((format!("Panel layout could not be restored: {error}"), ui.input(|input| input.time) + 8.0, None));
        workspace = from_ui(app);
        let _ = sync(&mut workspace, app);
    }
    // Legacy visibility commands may replace the layout before this render starts.
    cancel_replaced_gesture(app, Some(&workspace));
    // Tool/Canvas callbacks dispatch app commands and must see the real workspace.
    app.ui.docking = Some(workspace.clone());
    let mut style = DockStyle::from_ui(ui);
    style.min_pane = 96.0;
    style.float_label = crate::i18n::tr("Float panel").into();
    style.close_label = crate::i18n::tr("Close panel").into();
    style.move_panel_to_label = crate::i18n::tr("Move to group").into();
    style.panels_label = crate::i18n::tr("Panels").into();
    style.resize_label = crate::i18n::tr("Resize panels").into();
    style.resize_window_label = crate::i18n::tr("Resize panel window").into();
    style.background = t.chrome;
    style.tab_background = t.chrome;
    style.active_background = t.canvas;
    style.text = t.text;
    style.inactive_text = t.text_dim;
    style.border = egui::Stroke::new(1.0, t.divider);
    style.accent = t.accent;
    style.font = t.font(12.0);
    // Tool and preset panels publish hover previews before the photo consumes them.
    let mut canvas = None;
    let output = DockArea::new(egui::Id::new("lightcraft-docking")).locked(workspace.locked).show_with_limits(
        ui,
        &workspace.layout,
        &style,
        |panel| crate::i18n::tr(panel.label()).into(),
        permissions,
        panel_limits,
        |ui, panel| match panel {
            Panel::Canvas => {
                canvas = Some(ui.new_child(egui::UiBuilder::new().id_salt("photo-view").max_rect(ui.max_rect())));
            }
            Panel::Library => {
                crate::widgets::register(ui.ctx(), "panel:left_panel", ui.max_rect());
                crate::panels::left::show(app, ui);
            }
            Panel::Tools => {
                crate::widgets::register(ui.ctx(), "panel:right_panel", ui.max_rect());
                crate::panels::right::show(app, ui);
            }
            Panel::Presets => crate::panels::presets::show(app, ui),
        },
    );
    if let Some(mut canvas) = canvas {
        let ui = &mut canvas;
        crate::panels::bottombar::show(app, ui);
        let bg = if matches!(
            app.ui.view,
            crate::state::ViewMode::Detail | crate::state::ViewMode::Compare | crate::state::ViewMode::Survey | crate::state::ViewMode::Reference
        ) {
            t.canvas
        } else {
            t.grid_bg
        };
        crate::panels::compare::prepare(app);
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(bg)).show(ui, |ui| match app.ui.view {
            crate::state::ViewMode::PhotoGrid | crate::state::ViewMode::SquareGrid => crate::panels::grid::show(app, ui),
            crate::state::ViewMode::Detail => crate::panels::detail::show(app, ui),
            crate::state::ViewMode::Compare => crate::panels::compare::show_compare(app, ui),
            crate::state::ViewMode::Survey => crate::panels::compare::show_survey(app, ui),
            crate::state::ViewMode::Reference => crate::panels::compare::show_reference(app, ui),
            crate::state::ViewMode::People => crate::panels::people::show(app, ui),
        });
    }

    let rendered = workspace;
    let mut workspace = app.ui.docking.clone().unwrap_or_else(|| from_ui(app));
    let _ = sync(&mut workspace, app);
    let stale = workspace.layout != rendered.layout || workspace.hidden != rendered.hidden;
    if stale {
        craft_ui::docking::cancel_drag::<Panel>(ui.ctx(), egui::Id::new("lightcraft-docking"));
    }
    publish_visibility(app, &workspace);
    app.ui.docking = Some(workspace);
    if stale {
        return;
    }
    for action in output.actions {
        if let Err(error) = serde_json::to_value(action)
            .map_err(|error| error.to_string())
            .and_then(|action| command_impl(app, false, &json!({"action": action}), true))
        {
            app.ui.toast = Some((format!("Panel move was not applied: {error}"), ui.input(|input| input.time) + 8.0, None));
        }
    }
}

/// Keep the established ui.set width controls connected to the panel's current parent split.
pub(crate) fn set_widths(state: &mut crate::UiState, params: &Value) -> Result<(), String> {
    state.left_width = crate::state::LEFT_WIDTH.clamp(state.left_width);
    state.right_width = crate::state::RIGHT_WIDTH.clamp(state.right_width);
    let Some(workspace) = state.docking.as_mut() else {
        return Ok(());
    };
    validate(workspace)?;
    for (key, panel, width) in [("leftWidth", Panel::Library, state.left_width), ("rightWidth", Panel::Tools, state.right_width)] {
        if params.get(key).is_none() {
            continue;
        }
        if let Some((_, location)) = workspace.hidden.iter_mut().find(|(id, _)| *id == panel) {
            if let Some(rect) = &mut location.floating {
                rect[2] = width;
            }
            match location.placement {
                Placement::Split(Zone::Left) => location.size = Some(SplitSize::FixedFirst(width)),
                Placement::Split(Zone::Right) => location.size = Some(SplitSize::FixedSecond(width)),
                _ => {} // Width controls do not rewrite a saved vertical split's height.
            }
            continue;
        }
        if let Some(group) = workspace.layout.floating.iter().find(|group| group.contains(&panel)) {
            let mut rect = group.rect;
            rect[2] = width;
            apply(workspace, Action::MoveFloating { panel, rect })?;
            continue;
        }
        let mut nodes = workspace.layout.root.as_ref().map(|node| vec![(node, Vec::new())]).unwrap_or_default();
        let mut change = None;
        while let Some((node, path)) = nodes.pop() {
            if let Node::Split { axis, first, second, .. } = node {
                let contains = |node: &Node<Panel>| match node {
                    Node::Tabs { panels, .. } => panels.contains(&panel),
                    Node::Stack { entries } => entries.iter().any(|entry| entry.panel == panel),
                    _ => false,
                };
                if *axis == SplitAxis::Horizontal {
                    if contains(first) {
                        change = Some(Action::ResizeSplit { path: path.clone(), size: SplitSize::FixedFirst(width) });
                    }
                    if contains(second) {
                        change = Some(Action::ResizeSplit { path: path.clone(), size: SplitSize::FixedSecond(width) });
                    }
                }
                let mut second_path = path.clone();
                second_path.push(true);
                let mut first_path = path;
                first_path.push(false);
                nodes.push((second, second_path));
                nodes.push((first, first_path));
            }
        }
        if let Some(action) = change {
            apply(workspace, action)?;
        }
    }
    Ok(())
}

fn panel_limits(panel: &Panel) -> PanelLimits {
    let (min, max_width) = match panel {
        Panel::Canvas => (egui::vec2(crate::state::MIN_PHOTO_WIDTH, 160.0), 1_000_000.0),
        Panel::Library => (egui::vec2(crate::state::LEFT_WIDTH.min, 120.0), crate::state::LEFT_WIDTH.max),
        Panel::Tools => (egui::vec2(crate::state::RIGHT_WIDTH.min, 120.0), crate::state::RIGHT_WIDTH.max),
        Panel::Presets => (egui::vec2(250.0, 120.0), 520.0),
    };
    PanelLimits { min, max: egui::vec2(max_width, 1_000_000.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_widths() -> crate::LightcraftApp {
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
        app.ui.left_width = 333.0;
        app.ui.right_width = 410.0;
        app
    }

    fn tools_split(axis: SplitAxis, size: SplitSize, first: bool) -> Workspace {
        let rest = split(
            SplitAxis::Horizontal,
            SplitSize::FixedFirst(333.0),
            tabs(Panel::Library),
            split(SplitAxis::Horizontal, SplitSize::FixedSecond(300.0), tabs(Panel::Canvas), tabs(Panel::Presets)),
        );
        let root = if first { split(axis, size, tabs(Panel::Tools), rest) } else { split(axis, size, rest, tabs(Panel::Tools)) };
        Workspace::new(Layout { root: Some(root), floating: Vec::new() })
    }

    #[test]
    fn explicit_reveal_opens_collapsed_tools_and_survives_reload() {
        let mut app = app_with_widths();
        app.ui.right = crate::state::RightPanel::Crop;
        let entries = [Panel::Tools, Panel::Library, Panel::Presets]
            .into_iter()
            .map(|panel| craft_ui::docking::StackEntry { panel, open: panel != Panel::Tools, height: Some(180.0) })
            .collect();
        app.ui.docking = Some(Workspace::new(Layout {
            root: Some(split(SplitAxis::Horizontal, SplitSize::FixedSecond(350.0), tabs(Panel::Canvas), Node::Stack { entries })),
            floating: Vec::new(),
        }));
        let undo = app.session.undo.len();
        app.run("panel.crop", json!({})).unwrap();
        assert_eq!(app.ui.right, crate::state::RightPanel::Crop);
        let workspace = app.ui.docking.as_ref().unwrap();
        assert!(exposed_in(workspace, Panel::Tools));
        assert_eq!(app.session.undo.len(), undo);
        let saved = serde_json::to_string(workspace).unwrap();
        let reloaded: Workspace = serde_json::from_str(&saved).unwrap();
        validate(&reloaded).unwrap();
        assert!(exposed_in(&reloaded, Panel::Tools));
    }

    #[test]
    fn legacy_width_publication_ignores_heights_and_sibling_widths() {
        for (axis, size, first) in [
            (SplitAxis::Vertical, SplitSize::FixedFirst(180.0), true),
            (SplitAxis::Vertical, SplitSize::FixedSecond(180.0), false),
            (SplitAxis::Horizontal, SplitSize::FixedSecond(180.0), true),
            (SplitAxis::Horizontal, SplitSize::FixedFirst(180.0), false),
        ] {
            let workspace = tools_split(axis, size, first);
            validate(&workspace).unwrap();
            let mut app = app_with_widths();
            publish_visibility(&mut app, &workspace);
            assert_eq!((app.ui.left_width, app.ui.right_width), (333.0, 410.0), "{axis:?} {size:?}, first={first}");
        }
    }

    #[test]
    fn legacy_width_publication_reads_own_horizontal_and_floating_widths() {
        for (size, first) in [(SplitSize::FixedFirst(310.0), true), (SplitSize::FixedSecond(310.0), false)] {
            let workspace = tools_split(SplitAxis::Horizontal, size, first);
            let mut app = app_with_widths();
            publish_visibility(&mut app, &workspace);
            assert_eq!((app.ui.left_width, app.ui.right_width), (333.0, 310.0));
        }
        let mut workspace = default_workspace();
        apply(&mut workspace, Action::Float { panel: Panel::Tools, rect: [12.0, 34.0, 390.0, 450.0] }).unwrap();
        let mut app = app_with_widths();
        publish_visibility(&mut app, &workspace);
        assert_eq!(app.ui.right_width, 390.0);
    }

    #[test]
    fn legacy_width_update_preserves_hidden_top_panel_height() {
        let mut workspace = default_workspace();
        apply(&mut workspace, Action::Move { panel: Panel::Tools, anchor: Panel::Canvas, placement: Placement::Split(Zone::Top) }).unwrap();
        set_visible(&mut workspace, Panel::Tools, false).unwrap();
        let height = workspace.hidden.iter().find(|(panel, _)| *panel == Panel::Tools).unwrap().1.size;
        let mut state = crate::UiState { docking: Some(workspace), right_width: 410.0, ..Default::default() };
        set_widths(&mut state, &json!({"rightWidth":410.0})).unwrap();
        assert_eq!(state.docking.as_ref().unwrap().hidden.iter().find(|(panel, _)| *panel == Panel::Tools).unwrap().1.size, height);
    }

    /// Settings written before workspaces had a version: `{layout, hidden}`, a floating
    /// window as one group, hidden locations without dock state.
    #[test]
    fn version_one_workspace_file_loads_and_reopens_its_hidden_panel() {
        let v1 = json!({
            "layout": {
                "root": {"Split": {
                    "axis": "Horizontal",
                    "size": {"FixedFirst": 300.0},
                    "first": {"Tabs": {"panels": ["library"], "active": 0}},
                    "second": {"Tabs": {"panels": ["canvas"], "active": 0}}
                }},
                "floating": [{"panels": ["tools"], "active": 0, "rect": [120.0, 90.0, 320.0, 600.0]}]
            },
            "hidden": [["presets", {
                "floating": null,
                "anchor": "canvas",
                "placement": {"Split": "Right"},
                "size": {"FixedSecond": 280.0},
                "stack": null,
                "sibling": ["canvas"]
            }]]
        });
        let mut workspace: Workspace = serde_json::from_value(v1).unwrap();
        validate(&workspace).unwrap();
        assert_eq!(workspace.version, 1);
        let window = workspace.layout.floating.first().unwrap();
        assert!(window.contains(&Panel::Tools));
        assert_eq!(window.rect, [120.0, 90.0, 320.0, 600.0]);
        assert!(exposed_in(&workspace, Panel::Library));
        set_visible(&mut workspace, Panel::Presets, true).unwrap();
        let location = workspace.layout.location(&Panel::Presets).unwrap();
        assert_eq!((location.anchor, location.size), (Some(Panel::Canvas), Some(SplitSize::FixedSecond(280.0))));
        let saved = serde_json::to_value(&workspace).unwrap();
        assert_eq!(saved["version"], 2);
        assert_eq!(saved["layout"]["floating"][0]["panels"], json!(["tools"]), "a one-group window keeps the v1 shape");
    }
}
