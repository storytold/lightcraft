//! Application panel identities and persistence around the shared docking renderer.
//! Document contents remain owned by their existing view implementations.

use craft_ui::docking::{
    Action, Column, ColumnLocation, DockArea, DockCommand, DockError, DockOperation, DockStyle, FloatSpace, Group, GroupLocation, Layout, Location,
    Node, PanelLimits, PanelState, Permissions, Placement, Side, UiDock, Zone,
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
    /// The dock the panel opens in when it has no recorded place.
    fn home_side(self) -> Side {
        if self == Self::Library { Side::Start } else { Side::End }
    }
    /// The width of the panel's own column in the default workspace.
    fn home_width(self) -> f32 {
        match self {
            Self::Canvas => crate::state::MIN_PHOTO_WIDTH,
            Self::Library => crate::state::LEFT_WIDTH.default,
            Self::Tools => crate::state::RIGHT_WIDTH.default,
            Self::Presets => PRESETS_WIDTH,
        }
    }
}

const PRESETS_WIDTH: f32 = 300.0;

/// The arrangement generation saved as `UiState::docking_version`. Settings without it hold the
/// side panels in split frames; they start again from [`default_workspace`].
pub const DOCKING_VERSION: u32 = 1;

/// Drops saved settings' arrangement from before [`DOCKING_VERSION`], with the places of its
/// closed panels, so the app starts from the default workspace; every other setting stays.
/// Returns true when an arrangement was dropped.
pub fn drop_outdated(settings: &mut serde_json::Map<String, Value>) -> bool {
    if settings.get("dockingVersion").and_then(Value::as_u64).is_some_and(|version| version >= u64::from(DOCKING_VERSION)) {
        return false;
    }
    settings.insert("dockingVersion".into(), json!(DOCKING_VERSION));
    settings.remove("docking").is_some_and(|docking| !docking.is_null())
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

fn column(panel: Panel, width: f32) -> Column<Panel> {
    Column::new(vec![Group::new(vec![panel], 0)], width)
}

/// Library in a start dock and Tools in an end dock beside the photo. Presets has a column of
/// its own on Tools' photo side rather than a group under Tools: both are tall lists that are
/// read side by side while trying presets, and stacking them would halve both.
fn docked(left: f32, right: f32) -> Workspace {
    let start = Node::Dock { side: Side::Start, columns: vec![column(Panel::Library, left)] };
    let end = Node::Dock { side: Side::End, columns: vec![column(Panel::Tools, right), column(Panel::Presets, PRESETS_WIDTH)] };
    Workspace::new(Layout {
        root: Some(split(
            SplitAxis::Horizontal,
            SplitSize::FixedFirst(left),
            start,
            split(SplitAxis::Horizontal, SplitSize::FixedSecond(right + PRESETS_WIDTH), tabs(Panel::Canvas), end),
        )),
        floating: Vec::new(),
    })
}

pub fn default_workspace() -> Workspace {
    docked(crate::state::LEFT_WIDTH.default, crate::state::RIGHT_WIDTH.default)
}

/// Where a panel without a usable recorded place opens: its own column at its dock's window
/// edge, or Presets on the photo side of Tools while Tools is in the end dock.
fn home(workspace: &Workspace, panel: Panel) -> Location<Panel> {
    let column = ColumnLocation { side: panel.home_side(), width: panel.home_width(), iconic: false, iconic_width: None };
    let docked_at =
        |other: Panel| workspace.layout.location(&other).ok().and_then(|location| location.group).and_then(|group| group.column).map(|c| c.side);
    let beside = (panel == Panel::Presets && docked_at(Panel::Tools) == Some(Side::End)).then_some(Panel::Tools);
    Location {
        floating: None,
        anchor: beside,
        placement: if beside.is_some() { Placement::Column { after: true } } else { Placement::Tab { before: None } },
        size: None,
        stack: None,
        sibling: Vec::new(),
        group: Some(GroupLocation { minimized: false, height: None, column: Some(column) }),
    }
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

/// Shows a panel at its recorded place, else in its dock column ([`home`]). A panel that
/// already has the asked visibility keeps its tab selection.
fn set_visible(workspace: &mut Workspace, panel: Panel, visible: bool) -> Result<(), String> {
    if workspace.layout.contains(&panel) == visible {
        return Ok(());
    }
    if !visible {
        return apply(workspace, Action::Close { panel });
    }
    validate(workspace)?;
    // A recorded place that can't be rebuilt falls back to joining the fallback's tabs. The
    // photo view takes no tabs, so that is refused and the panel goes to its home column below.
    if workspace.hidden.iter().any(|(hidden, _)| *hidden == panel) {
        let mut next = workspace.clone();
        if next.set_visible(panel, true, Some(Panel::Canvas), permissions).is_ok() && validate(&next).is_ok() {
            *workspace = next;
            return Ok(());
        }
    }
    let mut next = workspace.clone();
    next.hidden.retain(|(hidden, _)| *hidden != panel);
    let location = home(&next, panel);
    next.hidden.push((panel, location));
    next.set_visible(panel, true, Some(Panel::Canvas), permissions).map_err(error)?;
    validate(&next)?;
    *workspace = next;
    Ok(())
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
    docked(crate::state::LEFT_WIDTH.clamp(app.ui.left_width), crate::state::RIGHT_WIDTH.clamp(app.ui.right_width))
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
        let column = location.group.and_then(|group| group.column);
        let value = match (location.floating, column, location.placement, location.size) {
            (Some([_, _, width, _]), ..) => Some(width),
            (None, Some(column), ..) => Some(column.width),
            (None, None, Placement::Split(Zone::Left), Some(SplitSize::FixedFirst(width)))
            | (None, None, Placement::Split(Zone::Right), Some(SplitSize::FixedSecond(width))) => Some(width),
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
        || id == "view.rightPanel"
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
        craft_ui::docking::cancel_drag::<Panel>(ctx, egui::Id::unique("lightcraft-docking"));
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

/// Draw the docked workspace: restore or fall back from an invalid layout, sync panel
/// visibility, render the panels and the current view in the canvas, then apply the docking
/// actions. If the layout changed during the pass, any in-flight dock drag is cancelled and the
/// docking actions it produced are not applied.
pub fn show(app: &mut crate::LightcraftApp, ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let mut workspace = app.ui.docking.clone().unwrap_or_else(|| from_ui(app));
    if let Err(error) = validate(&workspace).and_then(|()| sync(&mut workspace, app)) {
        app.ui.toast = Some((format!("Panel layout could not be restored: {error}"), app.tick_now() + 8.0, None));
        workspace = from_ui(app);
        let _ = sync(&mut workspace, app);
    }
    // Legacy visibility commands may replace the layout before this render starts.
    cancel_replaced_gesture(app, Some(&workspace));
    // Tool/Canvas callbacks dispatch app commands and must see the real workspace.
    app.ui.docking = Some(workspace.clone());
    let mut style = DockStyle::from_ui(ui);
    style.min_pane = 96.0;
    // Free docking: a panel dropped at an empty window edge makes a dock, and the photo takes
    // drops on all four sides.
    style.new_dock_zones = true;
    style.side_edge_zones = false;
    style.float_label = crate::i18n::tr("Float panel").into();
    style.close_label = crate::i18n::tr("Close panel").into();
    style.move_panel_to_label = crate::i18n::tr("Move to group").into();
    style.panels_label = crate::i18n::tr("Panels").into();
    style.resize_label = crate::i18n::tr("Resize panels").into();
    style.resize_window_label = crate::i18n::tr("Resize panel window").into();
    style.move_group_label = crate::i18n::tr("Move tab group").into();
    style.move_group_to_label = crate::i18n::tr("Move group to").into();
    style.float_group_label = crate::i18n::tr("Float tab group").into();
    style.close_group_label = crate::i18n::tr("Close tab group").into();
    style.close_others_label = crate::i18n::tr("Close other panels in group").into();
    style.minimize_label = crate::i18n::tr("Minimize").into();
    style.expand_label = crate::i18n::tr("Expand").into();
    style.move_column_label = crate::i18n::tr("Move panel column").into();
    style.collapse_to_icons_label = crate::i18n::tr("Collapse to Icons").into();
    style.expand_panels_label = crate::i18n::tr("Expand Panels").into();
    style.collapse_column_label = crate::i18n::tr("Collapse column").into();
    style.expand_column_label = crate::i18n::tr("Expand column").into();
    style.auto_collapse_label = crate::i18n::tr("Auto-Collapse Iconic Panels").into();
    style.move_window_label = crate::i18n::tr("Move panel window").into();
    style.close_window_label = crate::i18n::tr("Close panel window").into();
    style.float_hint_label = crate::i18n::tr("Hold {key} to undock").into();
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
    let output = DockArea::new(egui::Id::unique("lightcraft-docking")).locked(workspace.locked).show_with_limits(
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
                crate::widgets::register(ui, "panel:left_panel", ui.max_rect());
                crate::panels::left::show(app, ui);
            }
            Panel::Tools => {
                crate::widgets::register(ui, "panel:right_panel", ui.max_rect());
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
        craft_ui::docking::cancel_drag::<Panel>(ui.ctx(), egui::Id::unique("lightcraft-docking"));
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
            app.ui.toast = Some((format!("Panel move was not applied: {error}"), app.tick_now() + 8.0, None));
        }
    }
}

/// Keep the established ui.set width controls connected to the panel's dock column, floating
/// window or parent split.
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
            if let Some(column) = location.group.as_mut().and_then(|group| group.column.as_mut()) {
                column.width = width;
                continue;
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
        if workspace.layout.location(&panel).is_ok_and(|location| location.group.is_some_and(|group| group.column.is_some())) {
            apply(workspace, Action::ResizeColumn { panel, width })?;
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

/// The columns of the dock on `side`, from the window edge inwards: width and the panels of
/// each group.
#[cfg(test)]
pub(crate) fn columns(workspace: &Workspace, side: Side) -> Vec<(f32, Vec<Vec<Panel>>)> {
    let mut nodes: Vec<_> = workspace.layout.root.iter().collect();
    while let Some(node) = nodes.pop() {
        match node {
            Node::Split { first, second, .. } => nodes.extend([&**first, &**second]),
            Node::Dock { side: dock, columns } if *dock == side => {
                return columns.iter().map(|column| (column.width, column.groups.iter().map(|group| group.panels.clone()).collect())).collect();
            }
            _ => {}
        }
    }
    Vec::new()
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
    fn legacy_width_publication_reads_own_split_and_floating_widths() {
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

    #[test]
    fn default_workspace_docks_library_at_the_start_and_tools_and_presets_at_the_end() {
        let workspace = default_workspace();
        validate(&workspace).unwrap();
        assert_eq!(columns(&workspace, Side::Start), [(crate::state::LEFT_WIDTH.default, vec![vec![Panel::Library]])]);
        assert_eq!(
            columns(&workspace, Side::End),
            [(crate::state::RIGHT_WIDTH.default, vec![vec![Panel::Tools]]), (PRESETS_WIDTH, vec![vec![Panel::Presets]])],
            "Presets has its own column on Tools' photo side"
        );
        let Some(Node::Split { second, .. }) = &workspace.layout.root else { panic!("{:?}", workspace.layout.root) };
        let Node::Split { first, .. } = &**second else { panic!("{second:?}") };
        assert_eq!(**first, tabs(Panel::Canvas), "the photo sits between the docks");
        assert!(workspace.layout.floating.is_empty());
        let app = app_with_widths();
        let workspace = from_ui(&app);
        assert_eq!(columns(&workspace, Side::Start).first().map(|column| column.0), Some(333.0));
        assert_eq!(columns(&workspace, Side::End).first().map(|column| column.0), Some(410.0));
    }

    #[test]
    fn dock_column_widths_publish_and_follow_width_controls() {
        let mut app = app_with_widths();
        let workspace = docked(300.0, 310.0);
        publish_visibility(&mut app, &workspace);
        assert_eq!((app.ui.left_width, app.ui.right_width), (300.0, 310.0));
        let mut state = crate::UiState { docking: Some(workspace), left_width: 350.0, right_width: 400.0, ..Default::default() };
        set_visible(state.docking.as_mut().unwrap(), Panel::Library, false).unwrap();
        set_widths(&mut state, &json!({"leftWidth":350.0, "rightWidth":400.0})).unwrap();
        let workspace = state.docking.as_mut().unwrap();
        assert_eq!(columns(workspace, Side::End).first().map(|column| column.0), Some(400.0), "Tools' column");
        set_visible(workspace, Panel::Library, true).unwrap();
        assert_eq!(columns(workspace, Side::Start), [(350.0, vec![vec![Panel::Library]])], "the closed Library's column");
    }

    #[test]
    fn a_panel_without_a_recorded_place_opens_in_its_dock() {
        let root = split(
            SplitAxis::Horizontal,
            SplitSize::FixedSecond(300.0),
            tabs(Panel::Canvas),
            Node::Dock { side: Side::End, columns: vec![column(Panel::Tools, 300.0)] },
        );
        let mut workspace = Workspace::new(Layout { root: Some(root), floating: Vec::new() });
        set_visible(&mut workspace, Panel::Presets, true).unwrap();
        set_visible(&mut workspace, Panel::Library, true).unwrap();
        validate(&workspace).unwrap();
        assert_eq!(columns(&workspace, Side::Start), [(crate::state::LEFT_WIDTH.default, vec![vec![Panel::Library]])]);
        assert_eq!(columns(&workspace, Side::End), [(300.0, vec![vec![Panel::Tools]]), (PRESETS_WIDTH, vec![vec![Panel::Presets]])]);
        // Without Tools in the end dock, Presets takes the dock's window edge.
        let mut workspace = default_workspace();
        set_visible(&mut workspace, Panel::Presets, false).unwrap();
        workspace.hidden.clear();
        apply(&mut workspace, Action::Float { panel: Panel::Tools, rect: [12.0, 34.0, 390.0, 450.0] }).unwrap();
        set_visible(&mut workspace, Panel::Presets, true).unwrap();
        assert_eq!(columns(&workspace, Side::End), [(PRESETS_WIDTH, vec![vec![Panel::Presets]])]);
    }

    fn saved_settings(docking: Value, version: Option<u32>) -> serde_json::Map<String, Value> {
        let ui = crate::UiState { thumb_size: 210.0, filmstrip: false, presets: true, ..Default::default() };
        let mut saved = serde_json::to_value(&ui).unwrap();
        let fields = saved.as_object_mut().unwrap();
        fields.insert("docking".into(), docking);
        match version {
            Some(version) => fields.insert("dockingVersion".into(), json!(version)),
            None => fields.remove("dockingVersion"),
        };
        fields.clone()
    }

    #[test]
    fn a_saved_split_frame_layout_without_a_docking_version_loads_as_the_default_docks() {
        let frames = split(
            SplitAxis::Horizontal,
            SplitSize::FixedFirst(333.0),
            tabs(Panel::Library),
            split(SplitAxis::Horizontal, SplitSize::FixedSecond(410.0), tabs(Panel::Canvas), tabs(Panel::Tools)),
        );
        let frames = Workspace::new(Layout { root: Some(frames), floating: Vec::new() });
        validate(&frames).unwrap();
        for version in [None, Some(0)] {
            let mut fields = saved_settings(serde_json::to_value(&frames).unwrap(), version);
            assert!(drop_outdated(&mut fields));
            let ui: crate::UiState = serde_json::from_value(Value::Object(fields)).unwrap();
            assert!(ui.docking.is_none());
            assert_eq!(ui.docking_version, DOCKING_VERSION);
            assert_eq!((ui.thumb_size, ui.filmstrip, ui.presets), (210.0, false, true), "other settings stay");
            let mut app = app_with_widths();
            app.ui = ui;
            command(&mut app, false, &json!({"operation":"activate","panel":"tools"})).unwrap();
            let workspace = app.ui.docking.as_ref().unwrap();
            assert!(!workspace.layout.contains(&Panel::Library), "the Library panel stays closed");
            assert_eq!(
                columns(workspace, Side::End),
                [(crate::state::RIGHT_WIDTH.default, vec![vec![Panel::Tools]]), (PRESETS_WIDTH, vec![vec![Panel::Presets]])]
            );
        }
    }

    #[test]
    fn a_saved_dock_layout_round_trips() {
        let mut workspace = default_workspace();
        apply(&mut workspace, Action::ResizeColumn { panel: Panel::Tools, width: 333.0 }).unwrap();
        apply(
            &mut workspace,
            Action::Move { panel: Panel::Presets, anchor: Panel::Library, placement: Placement::Group { below: true, height: None } },
        )
        .unwrap();
        set_visible(&mut workspace, Panel::Library, false).unwrap();
        let mut fields = saved_settings(serde_json::to_value(&workspace).unwrap(), Some(DOCKING_VERSION));
        assert!(!drop_outdated(&mut fields));
        let ui: crate::UiState = serde_json::from_value(Value::Object(fields)).unwrap();
        let loaded = ui.docking.unwrap();
        validate(&loaded).unwrap();
        assert_eq!(loaded, workspace);
        assert_eq!(serde_json::to_value(crate::UiState::default()).unwrap()["dockingVersion"], DOCKING_VERSION, "saves write the version");
    }

    #[test]
    fn a_panel_whose_saved_place_is_gone_opens_in_its_dock() {
        let root = split(SplitAxis::Horizontal, SplitSize::FixedSecond(300.0), tabs(Panel::Canvas), tabs(Panel::Tools));
        let mut workspace = Workspace::new(Layout { root: Some(root), floating: Vec::new() });
        // Presets, the panel Library went back beside, is neither shown nor hidden.
        let location: craft_ui::docking::Location<Panel> = serde_json::from_value(json!({
            "floating": null,
            "anchor": "presets",
            "placement": {"Split": "Left"},
            "size": {"FixedFirst": 280.0},
            "stack": null,
            "sibling": ["presets"]
        }))
        .unwrap();
        workspace.hidden.push((Panel::Library, location));
        validate(&workspace).unwrap();
        set_visible(&mut workspace, Panel::Library, true).unwrap();
        validate(&workspace).unwrap();
        assert_eq!(columns(&workspace, Side::Start), [(crate::state::LEFT_WIDTH.default, vec![vec![Panel::Library]])]);
    }
}
