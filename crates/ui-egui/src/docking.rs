//! Application panel identities and persistence around the framework's docking: the panel layout is
//! craft-behavior's dock model (`craft::behavior::dock`), drawn by the framework's `DockArea`
//! (`craft::ui::patterns::dock`). Document contents remain owned by their existing view implementations.

use craft::behavior::dock::{
    Action, Column, ColumnLocation, DockEdge, DockError, DockPlacement, DockSide, Group, GroupLocation, Layout, Location, Node, PanelState,
    Permissions, SplitSize,
};
use craft::behavior::{Axis, CancelReason};
use craft::host::target::WindowKind;
use craft::ui::contract::{Bind, BodyCx, PlainData, SizeLimits, Tracked};
use craft::ui::docking::{DockConfig, DockPanels, GroupAt, PanelSpec, PanelSpecs};
use craft::ui::patterns::dock::DockArea;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::sync::Arc;

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
    fn home_side(self) -> DockEdge {
        if self == Self::Library { DockEdge::Start } else { DockEdge::End }
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

/// The name of the dock area that draws the panels; its root id comes from it, so it is not translated.
const AREA: &str = "Panels";

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
    if *panel == Panel::Canvas { Permissions::PROTECTED.accepts_splits(true) } else { Permissions::default() }
}

fn tabs(panel: Panel) -> Node<Panel> {
    Node::tabs(vec![panel], 0)
}

fn split(axis: Axis, size: SplitSize, first: Node<Panel>, second: Node<Panel>) -> Node<Panel> {
    Node::split(axis, size, first, second)
}

/// `[x, y, width, height]`, as the control channel and saved settings write a rect.
fn rect4(rect: [f32; 4]) -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(rect[0], rect[1]), egui::vec2(rect[2], rect[3]))
}

/// `rect` with its width replaced by `width`, keeping its position and height.
fn with_width(rect: egui::Rect, width: f32) -> egui::Rect {
    egui::Rect::from_min_size(rect.min, egui::vec2(width, rect.height()))
}

/// The saved arrangement. Closed panels keep their last group, side, size or floating rect.
pub type Workspace = craft::behavior::dock::Workspace<Panel>;

/// The arrangement as [`crate::UiState`] keeps it: the [`Workspace`], saved as it is, whose `Debug` shows only what
/// the arrangement is. The framework's workspace also prints its revision and generation counters, which are
/// process-wide (a clone is a new revision), so two equal UI states would print differently.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Docked(pub Workspace);

impl Docked {
    /// The workspace.
    pub fn into_inner(self) -> Workspace {
        self.0
    }
}

impl From<Workspace> for Docked {
    fn from(workspace: Workspace) -> Self {
        Docked(workspace)
    }
}

impl std::ops::Deref for Docked {
    type Target = Workspace;
    fn deref(&self) -> &Workspace {
        &self.0
    }
}

impl std::ops::DerefMut for Docked {
    fn deref_mut(&mut self) -> &mut Workspace {
        &mut self.0
    }
}

impl std::fmt::Debug for Docked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workspace").field("layout", self.layout()).field("hidden", &self.hidden()).field("floated", &self.floated()).finish()
    }
}

/// The app's arrangement, or the one its panel settings make when it has none yet.
fn current(app: &crate::LightcraftApp) -> Workspace {
    app.ui.docking.clone().map(Docked::into_inner).unwrap_or_else(|| from_ui(app))
}

fn column(panel: Panel, width: f32) -> Column<Panel> {
    Column::new(vec![Group::new(vec![panel], 0)]).with_width(width)
}

/// Library in a start dock and Tools in an end dock beside the photo. Presets has a column of
/// its own on Tools' photo side rather than a group under Tools: both are tall lists that are
/// read side by side while trying presets, and stacking them would halve both.
fn docked(left: f32, right: f32) -> Workspace {
    let start = Node::dock(DockEdge::Start, vec![column(Panel::Library, left)]);
    let end = Node::dock(DockEdge::End, vec![column(Panel::Tools, right), column(Panel::Presets, PRESETS_WIDTH)]);
    Workspace::new(Layout::new(
        Some(split(
            Axis::X,
            SplitSize::FixedFirst(left),
            start,
            split(Axis::X, SplitSize::FixedSecond(right + PRESETS_WIDTH), tabs(Panel::Canvas), end),
        )),
        Vec::new(),
    ))
}

pub fn default_workspace() -> Workspace {
    docked(crate::state::LEFT_WIDTH.default, crate::state::RIGHT_WIDTH.default)
}

/// Where a panel without a usable recorded place opens: its own column at its dock's window
/// edge, or Presets on the photo side of Tools while Tools is in the end dock.
fn home(layout: &Layout<Panel>, panel: Panel) -> Location<Panel> {
    let column = ColumnLocation::new(Some(panel.home_side()), Some(panel.home_width()), false, None);
    let docked_at =
        |other: Panel| layout.location(&other).ok().and_then(|location| location.group).and_then(|group| group.column).and_then(|c| c.side);
    let beside = (panel == Panel::Presets && docked_at(Panel::Tools) == Some(DockEdge::End)).then_some(Panel::Tools);
    let placement = if beside.is_some() { DockPlacement::column(true) } else { DockPlacement::tab(None) };
    let mut location = Location::new(beside, placement);
    location.group = Some(GroupLocation::new(false, None, Some(column)));
    location
}

fn error(error: DockError) -> String {
    error.to_string()
}

/// The shared checks plus LightCraft's own: saved settings cannot float, hide, share or
/// collapse the photo view, nor send a hidden panel back onto it.
pub fn validate(workspace: &Workspace) -> Result<(), String> {
    workspace.validate().map_err(error)?;
    let mut nodes: Vec<_> = workspace.layout().root.iter().collect();
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
    if workspace.hidden().iter().any(|(_, location)| {
        location.anchor == Some(Panel::Canvas) && matches!(location.placement, DockPlacement::Tab { .. } | DockPlacement::Split(DockSide::Center))
    }) {
        return Err("A hidden panel cannot restore over the document view".into());
    }
    Ok(())
}

fn apply(workspace: &mut Workspace, action: Action<Panel>) -> Result<(), String> {
    apply_all(workspace, vec![action])
}

/// Every action in order, or none: the workspace changes only when all of them apply and the
/// result passes [`validate`].
fn apply_all(workspace: &mut Workspace, actions: Vec<Action<Panel>>) -> Result<(), String> {
    let mut next = workspace.clone();
    next.apply_all(actions, false, permissions).map_err(error)?;
    validate(&next)?;
    *workspace = next;
    Ok(())
}

/// `workspace` with the closed panels' places `hidden` in place of its own.
fn with_hidden(workspace: &Workspace, hidden: Vec<(Panel, Location<Panel>)>) -> Workspace {
    Workspace::from_parts(workspace.layout().clone(), hidden, workspace.floated().to_vec())
}

/// Shows a panel at its recorded place, else in its dock column ([`home`]). A panel that
/// already has the asked visibility keeps its tab selection.
fn set_visible(workspace: &mut Workspace, panel: Panel, visible: bool) -> Result<(), String> {
    if workspace.layout().contains(&panel) == visible {
        return Ok(());
    }
    if !visible {
        return apply(workspace, Action::close(panel));
    }
    validate(workspace)?;
    // A recorded place that can't be rebuilt falls back to joining the fallback's tabs. The
    // photo view takes no tabs, so that is refused and the panel goes to its home column below.
    if workspace.hidden().iter().any(|(hidden, _)| *hidden == panel) {
        let mut next = workspace.clone();
        if next.set_visible(panel, true, Some(Panel::Canvas), false, permissions).is_ok() && validate(&next).is_ok() {
            *workspace = next;
            return Ok(());
        }
    }
    let mut hidden: Vec<_> = workspace.hidden().iter().filter(|(hidden, _)| *hidden != panel).cloned().collect();
    hidden.push((panel, home(workspace.layout(), panel)));
    let mut next = with_hidden(workspace, hidden);
    next.set_visible(panel, true, Some(Panel::Canvas), false, permissions).map_err(error)?;
    validate(&next)?;
    *workspace = next;
    Ok(())
}

/// Membership is passive state; an explicit command also exposes the panel's body.
fn reveal(workspace: &mut Workspace, panel: Panel) -> Result<(), String> {
    set_visible(workspace, panel, true)?;
    apply(workspace, Action::activate(panel))?;
    if workspace.layout().panel_state(&panel) != PanelState::Front {
        // A minimized window or group opens again; where nothing is minimized the action is
        // refused and the layout stays as it is.
        let _ = apply(workspace, Action::set_minimized(panel, false));
    }
    Ok(())
}

pub fn exposed_in(workspace: &Workspace, panel: Panel) -> bool {
    workspace.layout().panel_state(&panel) == PanelState::Front
}

/// `ui.dock` params: a raw dock action, or an operation.
#[derive(Deserialize)]
#[serde(untagged)]
enum UiDock {
    Action { action: Action<Panel> },
    Operation(DockOperation),
}

/// A `ui.dock` operation, tagged by `operation`. `move` takes `target` (the anchor), `zone`
/// (default `center`) and `before`, which only applies to a center or tab move.
#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "camelCase")]
enum DockOperation {
    /// Show a panel at its last location and select it.
    Open {
        panel: Panel,
    },
    Close {
        panel: Panel,
    },
    Activate {
        panel: Panel,
    },
    Float {
        panel: Panel,
        rect: [f32; 4],
    },
    MoveFloating {
        panel: Panel,
        rect: [f32; 4],
    },
    Move {
        panel: Panel,
        target: Panel,
        #[serde(default)]
        zone: Option<ZoneName>,
        #[serde(default, deserialize_with = "present")]
        before: Option<Option<Panel>>,
    },
    SetStackOpen {
        panel: Panel,
        open: bool,
    },
    /// `height` is required; `null` clears it.
    ResizeStack {
        panel: Panel,
        #[serde(deserialize_with = "required")]
        height: Option<f32>,
    },
    ResizeSplit {
        path: Vec<bool>,
        size: SplitSize,
    },
}

/// A drop zone by name, as the control channel writes it. PascalCase names also parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ZoneName {
    Tab,
    #[serde(alias = "Center")]
    Center,
    #[serde(alias = "Left")]
    Left,
    #[serde(alias = "Right")]
    Right,
    #[serde(alias = "Top")]
    Top,
    #[serde(alias = "Bottom")]
    Bottom,
    Above,
    Below,
}

/// A present field, `null` included: `Some(None)` for `null`, `None` when absent.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

/// A required field that may be `null`.
fn required<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<T, D::Error> {
    T::deserialize(deserializer)
}

/// Where a `move` puts the panel relative to `anchor`: `zone` (default `center`) and `before`,
/// with the side zones mapped onto the anchor's container ([`DockPlacement::for_zone`]).
fn placement(layout: &Layout<Panel>, anchor: &Panel, zone: Option<ZoneName>, before: Option<Option<Panel>>) -> Result<DockPlacement<Panel>, String> {
    let side = |side: DockSide| DockPlacement::for_zone(layout, anchor, side).map_err(error);
    match (zone.unwrap_or(ZoneName::Center), before) {
        (ZoneName::Tab | ZoneName::Center, Some(before)) => Ok(DockPlacement::tab(before)),
        (ZoneName::Tab, None) => Ok(DockPlacement::tab(None)),
        (ZoneName::Center, None) => Ok(DockPlacement::Split(DockSide::Center)),
        (_, Some(_)) => Err(error(DockError::InvalidPlacement)),
        (ZoneName::Left, None) => side(DockSide::Start),
        (ZoneName::Right, None) => side(DockSide::End),
        (ZoneName::Top, None) => side(DockSide::Top),
        (ZoneName::Bottom, None) => side(DockSide::Bottom),
        (ZoneName::Above, None) => Ok(DockPlacement::group(false, None)),
        (ZoneName::Below, None) => Ok(DockPlacement::group(true, None)),
    }
}

impl DockOperation {
    /// The dock action of an operation on a shown panel; `open` is handled before this.
    fn action(self, layout: &Layout<Panel>) -> Result<Action<Panel>, String> {
        Ok(match self {
            Self::Open { panel } | Self::Activate { panel } => {
                if !layout.contains(&panel) {
                    return Err(error(DockError::MissingPanel));
                }
                Action::activate(panel)
            }
            Self::Close { panel } => Action::close(panel),
            Self::Float { panel, rect } => Action::float(panel, rect4(rect)),
            Self::MoveFloating { panel, rect } => Action::move_floating(panel, rect4(rect)),
            Self::Move { panel, target, zone, before } => Action::move_beside(panel, target, placement(layout, &target, zone, before)?),
            Self::SetStackOpen { panel, open } => Action::set_stack_open(panel, open),
            Self::ResizeStack { panel, height } => Action::resize_stack(panel, height),
            Self::ResizeSplit { path, size } => Action::resize_split(path, size),
        })
    }
}

/// `ui.dock` params: a dock action, or an operation. All or nothing.
fn run(workspace: &mut Workspace, params: &Value) -> Result<(), String> {
    validate(workspace)?;
    if !params.is_object() {
        return Err("ui.dock params must be an object".into());
    }
    let command: UiDock = serde_json::from_value(params.clone()).map_err(|error| error.to_string())?;
    let action = match command {
        UiDock::Operation(DockOperation::Open { panel }) => return reveal(workspace, panel),
        // Activating a closed panel shows it at its last location first.
        UiDock::Operation(DockOperation::Activate { panel }) if !workspace.layout().contains(&panel) => {
            let mut next = workspace.clone();
            next.set_visible(panel, true, None, false, permissions).map_err(error)?;
            validate(&next)?;
            *workspace = next;
            return Ok(());
        }
        UiDock::Action { action } => action,
        UiDock::Operation(operation) => operation.action(workspace.layout())?,
    };
    apply(workspace, action)
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
    let layout = workspace.layout();
    app.ui.left_panel = layout.contains(&Panel::Library);
    app.ui.presets = layout.contains(&Panel::Presets);
    if !layout.contains(&Panel::Tools) {
        app.ui.right = crate::state::RightPanel::None;
    } else if app.ui.right == crate::state::RightPanel::None {
        app.ui.right = crate::state::RightPanel::Edit;
    }
    for (panel, width, limits) in
        [(Panel::Library, &mut app.ui.left_width, crate::state::LEFT_WIDTH), (Panel::Tools, &mut app.ui.right_width, crate::state::RIGHT_WIDTH)]
    {
        let Ok(location) = layout.location(&panel) else {
            continue;
        };
        let column = location.group.as_ref().and_then(|group| group.column);
        let value = match (location.floating, column, &location.placement, location.size) {
            (Some(rect), ..) => Some(rect.width()),
            (None, Some(column), ..) => column.width,
            (None, None, DockPlacement::Split(DockSide::Start), Some(SplitSize::FixedFirst(width)))
            | (None, None, DockPlacement::Split(DockSide::End), Some(SplitSize::FixedSecond(width))) => Some(width),
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
        let mut workspace = current(app);
        sync(&mut workspace, app)?;
        reveal(&mut workspace, panel)?;
        cancel_replaced_gesture(app, Some(&workspace));
        app.ui.docking = Some(Docked(workspace));
    }
    Ok(())
}

/// Check the target's actual egui layer even while a different widget owns the drag.
pub(crate) fn drop_target(ui: &egui::Ui, rect: egui::Rect, pointer: Option<egui::Pos2>) -> bool {
    pointer.is_some_and(|pos| {
        rect.intersect(ui.clip_rect()).contains(pos) && ui.ctx().layer_id_at(pos) == Some(ui.layer_id()) && !under_floating_window(ui.ctx(), rect.intersect(ui.clip_rect()), pos)
    })
}

/// Whether a floating dock window the last pass drew covers `pos` over a target at `rect` outside that window. The
/// framework's floating windows are not egui areas, so egui's layer test does not see them over the dock.
fn under_floating_window(ctx: &egui::Context, rect: egui::Rect, pos: egui::Pos2) -> bool {
    let Some(Some(rendered)) = ctx.data(|data| data.get_temp::<Option<Arc<Workspace>>>(rendered_key())) else { return false };
    rendered.layout().floating.iter().any(|window| window.rect.contains(pos) && !window.rect.contains_rect(rect))
}

/// Ends the dock's running gestures without a drop: a tab or group drag, and a held divider or
/// floating-window move, which would otherwise go on resizing a layout that was replaced.
fn cancel_gestures(ctx: &egui::Context) {
    craft::ui::runtime::drag::cancel(ctx, CancelReason::HostChanged);
    craft::ui::runtime::edit::cancel_all(ctx, CancelReason::HostChanged);
}

/// Explicit host commands replace the snapshot before rendering. Cancel only their old
/// gestures; actions returned by the renderer must keep continuous resizing alive.
pub(crate) fn cancel_replaced_gesture(app: &crate::LightcraftApp, workspace: Option<&Workspace>) {
    let replaced = match (app.ui.docking.as_deref(), workspace) {
        (Some(old), Some(next)) => old.layout() != next.layout() || old.hidden() != next.hidden(),
        (None, None) => false,
        _ => true,
    };
    if replaced && let Some(ctx) = &app.tasks.repaint {
        cancel_gestures(ctx);
    }
}

pub fn command(app: &mut crate::LightcraftApp, reset: bool, params: &Value) -> Result<Value, String> {
    command_impl(app, reset, params, false)
}

fn command_impl(app: &mut crate::LightcraftApp, reset: bool, params: &Value, renderer_action: bool) -> Result<Value, String> {
    let mut workspace = current(app);
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
    app.ui.docking = Some(Docked(workspace));
    Ok(json!({"docking": app.ui.docking}))
}

/// The panels' names, limits and rights in the interface language: the spec model the dock reads,
/// kept in a `Tracked` so the dock reads it again only when the language changes.
#[derive(Clone, Debug, PartialEq, PlainData)]
struct Specs {
    language: String,
}

impl PanelSpecs<Panel> for Specs {
    fn spec(&self, panel: &Panel) -> PanelSpec {
        let spec = PanelSpec::new(crate::i18n::tr(panel.label()).to_string()).limits(panel_limits(*panel));
        // LightCraft's panels frame and pad their own content, as they did before the dock: the dock shows their
        // bodies edge to edge, without its panel padding (only a document's group does that; LightCraft has no OS
        // windows for the kind to change). A framework gap: no body-padding option of its own.
        if *panel == Panel::Canvas { spec.canvas() } else { spec.window_kind(WindowKind::Document) }
    }

    fn permissions(&self, panel: &Panel) -> Permissions {
        permissions(panel)
    }
}

thread_local! {
    /// The workspace the dock shows, the same `Arc` while the layout and the closed panels' places stay
    /// the same, so the dock's derived values follow its revision; and the spec model in the language it
    /// was built for.
    static SHOWN: RefCell<Option<Arc<Workspace>>> = const { RefCell::new(None) };
    static SPECS: RefCell<Option<Arc<Tracked<Specs>>>> = const { RefCell::new(None) };
}

/// `workspace` with its dock columns narrowed to leave the photo [`crate::state::MIN_PHOTO_WIDTH`] of `room`: each
/// column gives way in proportion to its width above its first panel's minimum. Unchanged when everything fits.
fn fitted(workspace: &Workspace, room: f32) -> Workspace {
    let mut docked: Vec<(Panel, f32, f32)> = Vec::new();
    let mut nodes: Vec<_> = workspace.layout().root.iter().collect();
    while let Some(node) = nodes.pop() {
        match node {
            Node::Split { first, second, .. } => nodes.extend([&**first, &**second]),
            Node::Dock { columns, .. } => {
                for column in columns {
                    if let (Some(width), Some(panel)) = (column.width, column.groups.first().and_then(|group| group.panels.first())) {
                        docked.push((*panel, width, panel_limits(*panel).min.x));
                    }
                }
            }
            _ => {}
        }
    }
    let excess = docked.iter().map(|(_, width, _)| width).sum::<f32>() + crate::state::MIN_PHOTO_WIDTH - room;
    let give: f32 = docked.iter().map(|(_, width, min)| (width - min).max(0.0)).sum();
    if !room.is_finite() || !(excess > 0.0) || !(give > 0.0) {
        return workspace.clone();
    }
    let share = (excess / give).min(1.0);
    let mut narrowed = workspace.clone();
    for (panel, width, min) in docked {
        let _ = apply(&mut narrowed, Action::resize_column(panel, width - (width - min).max(0.0) * share));
    }
    narrowed
}

/// `workspace` as the dock shows it: the kept `Arc` while it is equal.
fn shown_workspace(workspace: &Workspace) -> Arc<Workspace> {
    SHOWN.with(|shown| {
        let mut shown = shown.borrow_mut();
        match shown.as_ref().filter(|kept| ***kept == *workspace) {
            Some(kept) => Arc::clone(kept),
            None => {
                let made = Arc::new(workspace.clone());
                *shown = Some(Arc::clone(&made));
                made
            }
        }
    })
}

/// The spec model in the current interface language, the same one while the language stays.
fn specs() -> Arc<Tracked<Specs>> {
    let language = crate::i18n::language().code();
    SPECS.with(|specs| {
        let mut specs = specs.borrow_mut();
        match specs.as_ref().filter(|kept| kept.language == language) {
            Some(kept) => Arc::clone(kept),
            None => {
                let made = Arc::new(Tracked::new(Specs { language: language.to_owned() }));
                *specs = Some(Arc::clone(&made));
                made
            }
        }
    })
}

/// How LightCraft's dock behaves: a panel dropped at an empty window edge makes a dock, the photo
/// takes no tabs, and floating panels stay in the app window (they had no OS windows before).
fn dock_config() -> DockConfig {
    DockConfig::default().side_edge_zones(false).new_dock_zones(true).close_others_item(true).native_windows(false)
}

/// The panel bodies, and the photo view's place: the photo is drawn after the dock, so tool and
/// preset panels publish hover previews before the photo consumes them.
struct Content<'a> {
    app: &'a mut crate::LightcraftApp,
    canvas: Option<egui::Ui>,
}

impl DockPanels<Panel> for Content<'_> {
    fn body(&mut self, panel: &Panel, ui: &mut egui::Ui, _: &mut BodyCx<'_>) {
        let app = &mut *self.app;
        match panel {
            // The photo view's group is the host's (`group_view`).
            Panel::Canvas => {}
            Panel::Library => {
                crate::widgets::announce_region(ui, egui::accesskit::Role::Navigation, crate::i18n::tr(Panel::Library.label()));
                crate::widgets::register(ui, "panel:left_panel", ui.max_rect());
                crate::panels::left::show(app, ui);
            }
            Panel::Tools => {
                crate::widgets::announce_region(ui, egui::accesskit::Role::Complementary, crate::i18n::tr(Panel::Tools.label()));
                crate::widgets::register(ui, "panel:right_panel", ui.max_rect());
                crate::panels::right::show(app, ui);
            }
            Panel::Presets => {
                crate::widgets::announce_region(ui, egui::accesskit::Role::Complementary, crate::i18n::tr(Panel::Presets.label()));
                crate::panels::presets::show(app, ui);
            }
        }
    }

    fn group_view(&mut self, panels: &[Panel], _: usize, ui: &mut egui::Ui, at: GroupAt, _: &mut BodyCx<'_>) -> Option<f32> {
        if !panels.contains(&Panel::Canvas) {
            return None;
        }
        // The photo view fills its frame; protected, it never sits in a flyout, which is what measures.
        let GroupAt::Show(rect) = at else { return Some(0.0) };
        let child = ui.new_child(egui::UiBuilder::new().id_salt("photo-view").max_rect(rect));
        crate::widgets::announce_region(&child, egui::accesskit::Role::Main, crate::i18n::tr(Panel::Canvas.label()));
        self.canvas = Some(child);
        Some(rect.height())
    }
}

/// The side panels' automation rects (`panel:left_panel`, `panel:right_panel`) span their dock column, as the old
/// side panels did: the framework group pads its body, so the body's own rect is narrower than the width the panel
/// was given. Registered while the body draws, widened here once the area has placed the columns; a panel that is
/// not first in a docked column (floating, a flyout, a split) keeps its body's rect.
fn register_column_widths(ctx: &egui::Context, columns: &[(Panel, egui::Rect)]) {
    ctx.data_mut(|data| {
        let registry = data.get_temp_mut_or_default::<crate::widgets::Registry>(egui::Id::unique("lc-registry"));
        for (name, panel) in [("panel:left_panel", Panel::Library), ("panel:right_panel", Panel::Tools)] {
            let Some(column) = columns.iter().find(|(first, _)| *first == panel).map(|(_, rect)| *rect) else { continue };
            for (_, rect) in registry.rects.iter_mut().filter(|(id, _)| id == name) {
                *rect = egui::Rect::from_x_y_ranges(column.x_range(), rect.y_range());
            }
        }
    });
}

/// Where the last pass placed each dock column, named by a panel of its first group (tests find
/// the column edges there).
#[cfg(test)]
fn columns_key() -> egui::Id {
    egui::Id::unique("lightcraft-dock-columns")
}

/// The rect the last pass gave the dock column whose first group holds `panel`.
#[cfg(test)]
pub(crate) fn column_rect(ctx: &egui::Context, panel: Panel) -> Option<egui::Rect> {
    ctx.data(|data| data.get_temp::<Vec<(Panel, egui::Rect)>>(columns_key()))?.into_iter().find(|(first, _)| *first == panel).map(|(_, rect)| rect)
}

/// Draw the docked workspace: restore or fall back from an invalid layout, sync panel
/// visibility, render the panels and the current view in the canvas, then apply the docking
/// actions. If the layout changed during the pass, any in-flight dock gesture is cancelled and
/// the docking actions it produced are not applied.
pub fn show(app: &mut crate::LightcraftApp, ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let mut workspace = current(app);
    if let Err(error) = validate(&workspace).and_then(|()| sync(&mut workspace, app)) {
        app.ui.toast = Some((crate::i18n::tr_format!("Panel layout could not be restored: {error}", error = error), app.tick_now() + 8.0, None));
        workspace = from_ui(app);
        let _ = sync(&mut workspace, app);
    }
    // Legacy visibility commands may replace the layout before this render starts.
    cancel_replaced_gesture(app, Some(&workspace));
    // A command between passes (ui.set, panel.close, ui.dock, a reset) may have replaced the layout the area last
    // drew: a divider or window move still held from then must not go on editing the replacement. Cancelled here, in
    // the pass, where the area's gestures see it before they run.
    // A press held from before the replacement edits nothing until it is let go: its actions were aimed at the old
    // layout (the area may go on reporting a divider drag the cancel has not reached yet).
    let held = ui.input(|i| i.pointer.any_down());
    let mut stale_press = held && ui.data(|d| d.get_temp::<bool>(stale_press_key())).unwrap_or(false);
    if rendered_differs(ui.ctx(), &workspace) {
        cancel_gestures(ui.ctx());
        stale_press |= held;
    }
    ui.data_mut(|d| d.insert_temp(stale_press_key(), stale_press));
    // Tool/Canvas callbacks dispatch app commands and must see the real workspace.
    app.ui.docking = Some(Docked(workspace.clone()));
    // A window too narrow for the docks' widths and the photo's minimum shows the docks narrower, each giving way in
    // proportion to what it has above its minimum, as the side panels did; the saved widths stay.
    let shown = shown_workspace(&fitted(&workspace, ui.available_width()));
    let specs = specs();
    let (output, canvas) = {
        let mut content = Content { app: &mut *app, canvas: None };
        let output = DockArea::new(AREA, Bind::given(Arc::clone(&shown)), &*specs, &mut content, dock_config()).show(ui);
        (output, content.canvas.take())
    };
    if let Some(invalid) = &output.error {
        log::warn!("the panel layout was not drawn: {}", invalid.error);
    }
    #[cfg(test)]
    ui.ctx().data_mut(|data| data.insert_temp(columns_key(), output.columns.clone()));
    register_column_widths(ui.ctx(), &output.columns);
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
    let mut workspace = current(app);
    let _ = sync(&mut workspace, app);
    let stale = workspace.layout() != rendered.layout() || workspace.hidden() != rendered.hidden();
    if stale {
        cancel_gestures(ui.ctx());
    }
    publish_visibility(app, &workspace);
    app.ui.docking = Some(Docked(workspace));
    if stale {
        remember_rendered(ui.ctx(), app);
        return;
    }
    // The area writes nothing itself: the user's changes come back as actions, applied through
    // `ui.dock` as a command's are, then the panel bodies' requests.
    let actions = if stale_press { Vec::new() } else { output.actions };
    for action in actions.into_iter().chain(output.requested) {
        if let Err(error) = serde_json::to_value(action)
            .map_err(|error| error.to_string())
            .and_then(|action| command_impl(app, false, &json!({"action": action}), true))
        {
            app.ui.toast = Some((crate::i18n::tr_format!("Panel move was not applied: {error}", error = error), app.tick_now() + 8.0, None));
        }
    }
    remember_rendered(ui.ctx(), app);
}

/// Where [`show`] keeps the workspace as its pass left it, its own actions applied.
fn rendered_key() -> egui::Id {
    egui::Id::unique("lightcraft-dock-rendered")
}

/// Where [`show`] notes that the held press began before the layout was replaced.
fn stale_press_key() -> egui::Id {
    egui::Id::unique("lightcraft-dock-stale-press")
}

/// Keeps the workspace the pass leaves, so the next pass can tell a replacement made between passes.
fn remember_rendered(ctx: &egui::Context, app: &crate::LightcraftApp) {
    let rendered = app.ui.docking.clone().map(|docked| Arc::new(docked.into_inner()));
    ctx.data_mut(|data| data.insert_temp(rendered_key(), rendered));
}

/// Whether `workspace` lays the panels out otherwise than the last pass left them: something outside the dock
/// replaced the layout in between. `false` before any pass drew the dock.
fn rendered_differs(ctx: &egui::Context, workspace: &Workspace) -> bool {
    let Some(last) = ctx.data(|data| data.get_temp::<Option<Arc<Workspace>>>(rendered_key())) else { return false };
    last.is_none_or(|last| last.layout() != workspace.layout() || last.hidden() != workspace.hidden())
}

/// Keep the established ui.set width controls connected to the panel's dock column, floating
/// window or parent split.
pub(crate) fn set_widths(state: &mut crate::UiState, params: &Value) -> Result<(), String> {
    state.left_width = crate::state::LEFT_WIDTH.clamp(state.left_width);
    state.right_width = crate::state::RIGHT_WIDTH.clamp(state.right_width);
    let Some(workspace) = state.docking.as_deref_mut() else {
        return Ok(());
    };
    validate(workspace)?;
    for (key, panel, width) in [("leftWidth", Panel::Library, state.left_width), ("rightWidth", Panel::Tools, state.right_width)] {
        if params.get(key).is_none() {
            continue;
        }
        if workspace.hidden().iter().any(|(id, _)| *id == panel) {
            let mut hidden = workspace.hidden().to_vec();
            if let Some((_, location)) = hidden.iter_mut().find(|(id, _)| *id == panel) {
                if let Some(rect) = &mut location.floating {
                    *rect = with_width(*rect, width);
                }
                if let Some(column) = location.group.as_mut().and_then(|group| group.column.as_mut()) {
                    column.width = Some(width);
                } else {
                    match location.placement {
                        DockPlacement::Split(DockSide::Start) => location.size = Some(SplitSize::FixedFirst(width)),
                        DockPlacement::Split(DockSide::End) => location.size = Some(SplitSize::FixedSecond(width)),
                        _ => {} // Width controls do not rewrite a saved vertical split's height.
                    }
                }
            }
            *workspace = with_hidden(workspace, hidden);
            continue;
        }
        if let Some(window) = workspace.layout().floating.iter().find(|window| window.contains(&panel)) {
            let rect = with_width(window.rect, width);
            apply(workspace, Action::move_floating(panel, rect))?;
            continue;
        }
        if workspace.layout().location(&panel).is_ok_and(|location| location.group.is_some_and(|group| group.column.is_some())) {
            apply(workspace, Action::resize_column(panel, width))?;
            continue;
        }
        let mut change = None;
        {
            let mut nodes = workspace.layout().root.as_ref().map(|node| vec![(node, Vec::new())]).unwrap_or_default();
            while let Some((node, path)) = nodes.pop() {
                if let Node::Split { axis, first, second, .. } = node {
                    let contains = |node: &Node<Panel>| match node {
                        Node::Tabs { panels, .. } => panels.contains(&panel),
                        Node::Stack { entries, .. } => entries.iter().any(|entry| entry.panel == panel),
                        _ => false,
                    };
                    if *axis == Axis::X {
                        if contains(first) {
                            change = Some(Action::resize_split(path.clone(), SplitSize::FixedFirst(width)));
                        }
                        if contains(second) {
                            change = Some(Action::resize_split(path.clone(), SplitSize::FixedSecond(width)));
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
        }
        if let Some(action) = change {
            apply(workspace, action)?;
        }
    }
    Ok(())
}

fn panel_limits(panel: Panel) -> SizeLimits {
    let (min, max_width) = match panel {
        Panel::Canvas => (egui::vec2(crate::state::MIN_PHOTO_WIDTH, 160.0), SizeLimits::BOUND),
        Panel::Library => (egui::vec2(crate::state::LEFT_WIDTH.min, 120.0), crate::state::LEFT_WIDTH.max),
        Panel::Tools => (egui::vec2(crate::state::RIGHT_WIDTH.min, 120.0), crate::state::RIGHT_WIDTH.max),
        Panel::Presets => (egui::vec2(250.0, 120.0), 520.0),
    };
    SizeLimits::new(min, egui::vec2(max_width, SizeLimits::BOUND))
}

/// The columns of the dock on `side`, from the window edge inwards: width and the panels of
/// each group. A column without a width of its own reads as NaN.
#[cfg(test)]
pub(crate) fn columns(workspace: &Workspace, side: DockEdge) -> Vec<(f32, Vec<Vec<Panel>>)> {
    let mut nodes: Vec<_> = workspace.layout().root.iter().collect();
    while let Some(node) = nodes.pop() {
        match node {
            Node::Split { first, second, .. } => nodes.extend([&**first, &**second]),
            Node::Dock { side: dock, columns, .. } if *dock == side => {
                return columns
                    .iter()
                    .map(|column| (column.width.unwrap_or(f32::NAN), column.groups.iter().map(|group| group.panels.clone()).collect()))
                    .collect();
            }
            _ => {}
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft::behavior::dock::StackEntry;

    fn app_with_widths() -> crate::LightcraftApp {
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
        app.ui.left_width = 333.0;
        app.ui.right_width = 410.0;
        app
    }

    fn tools_split(axis: Axis, size: SplitSize, first: bool) -> Workspace {
        let rest = split(
            Axis::X,
            SplitSize::FixedFirst(333.0),
            tabs(Panel::Library),
            split(Axis::X, SplitSize::FixedSecond(300.0), tabs(Panel::Canvas), tabs(Panel::Presets)),
        );
        let root = if first { split(axis, size, tabs(Panel::Tools), rest) } else { split(axis, size, rest, tabs(Panel::Tools)) };
        Workspace::new(Layout::new(Some(root), Vec::new()))
    }

    #[test]
    fn explicit_reveal_opens_collapsed_tools_and_survives_reload() {
        let mut app = app_with_widths();
        app.ui.right = crate::state::RightPanel::Crop;
        let entries = [Panel::Tools, Panel::Library, Panel::Presets]
            .into_iter()
            .map(|panel| {
                let mut entry = StackEntry::new(panel);
                entry.open = panel != Panel::Tools;
                entry.height = Some(180.0);
                entry
            })
            .collect();
        app.ui.docking = Some(Docked(Workspace::new(Layout::new(
            Some(split(Axis::X, SplitSize::FixedSecond(350.0), tabs(Panel::Canvas), Node::stack(entries))),
            Vec::new(),
        ))));
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
            (Axis::Y, SplitSize::FixedFirst(180.0), true),
            (Axis::Y, SplitSize::FixedSecond(180.0), false),
            (Axis::X, SplitSize::FixedSecond(180.0), true),
            (Axis::X, SplitSize::FixedFirst(180.0), false),
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
            let workspace = tools_split(Axis::X, size, first);
            let mut app = app_with_widths();
            publish_visibility(&mut app, &workspace);
            assert_eq!((app.ui.left_width, app.ui.right_width), (333.0, 310.0));
        }
        let mut workspace = default_workspace();
        apply(&mut workspace, Action::float(Panel::Tools, rect4([12.0, 34.0, 390.0, 450.0]))).unwrap();
        let mut app = app_with_widths();
        publish_visibility(&mut app, &workspace);
        assert_eq!(app.ui.right_width, 390.0);
    }

    #[test]
    fn legacy_width_update_preserves_hidden_top_panel_height() {
        let mut workspace = default_workspace();
        apply(&mut workspace, Action::move_beside(Panel::Tools, Panel::Canvas, DockPlacement::Split(DockSide::Top))).unwrap();
        set_visible(&mut workspace, Panel::Tools, false).unwrap();
        let height = workspace.hidden().iter().find(|(panel, _)| *panel == Panel::Tools).unwrap().1.size;
        let mut state = crate::UiState { docking: Some(Docked(workspace)), right_width: 410.0, ..Default::default() };
        set_widths(&mut state, &json!({"rightWidth":410.0})).unwrap();
        assert_eq!(state.docking.as_ref().unwrap().hidden().iter().find(|(panel, _)| *panel == Panel::Tools).unwrap().1.size, height);
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
        assert_eq!(workspace.version(), 1);
        let window = workspace.layout().floating.first().unwrap();
        assert!(window.contains(&Panel::Tools));
        assert_eq!(window.rect, rect4([120.0, 90.0, 320.0, 600.0]));
        assert!(exposed_in(&workspace, Panel::Library));
        set_visible(&mut workspace, Panel::Presets, true).unwrap();
        let location = workspace.layout().location(&Panel::Presets).unwrap();
        assert_eq!((location.anchor, location.size), (Some(Panel::Canvas), Some(SplitSize::FixedSecond(280.0))));
        let saved = serde_json::to_value(&workspace).unwrap();
        // The dock model writes its own format version (4 since floating windows are columns).
        assert_eq!(saved["version"], Workspace::VERSION);
        assert_eq!(saved["layout"]["floating"][0]["panels"], json!(["tools"]), "a one-group window keeps the v1 shape");
    }

    #[test]
    fn default_workspace_docks_library_at_the_start_and_tools_and_presets_at_the_end() {
        let workspace = default_workspace();
        validate(&workspace).unwrap();
        assert_eq!(columns(&workspace, DockEdge::Start), [(crate::state::LEFT_WIDTH.default, vec![vec![Panel::Library]])]);
        assert_eq!(
            columns(&workspace, DockEdge::End),
            [(crate::state::RIGHT_WIDTH.default, vec![vec![Panel::Tools]]), (PRESETS_WIDTH, vec![vec![Panel::Presets]])],
            "Presets has its own column on Tools' photo side"
        );
        let Some(Node::Split { second, .. }) = &workspace.layout().root else { panic!("{:?}", workspace.layout().root) };
        let Node::Split { first, .. } = &**second else { panic!("{second:?}") };
        assert_eq!(**first, tabs(Panel::Canvas), "the photo sits between the docks");
        assert!(workspace.layout().floating.is_empty());
        let app = app_with_widths();
        let workspace = from_ui(&app);
        assert_eq!(columns(&workspace, DockEdge::Start).first().map(|column| column.0), Some(333.0));
        assert_eq!(columns(&workspace, DockEdge::End).first().map(|column| column.0), Some(410.0));
    }

    #[test]
    fn dock_column_widths_publish_and_follow_width_controls() {
        let mut app = app_with_widths();
        let workspace = docked(300.0, 310.0);
        publish_visibility(&mut app, &workspace);
        assert_eq!((app.ui.left_width, app.ui.right_width), (300.0, 310.0));
        let mut state = crate::UiState { docking: Some(Docked(workspace)), left_width: 350.0, right_width: 400.0, ..Default::default() };
        set_visible(state.docking.as_mut().unwrap(), Panel::Library, false).unwrap();
        set_widths(&mut state, &json!({"leftWidth":350.0, "rightWidth":400.0})).unwrap();
        let workspace = state.docking.as_mut().unwrap();
        assert_eq!(columns(workspace, DockEdge::End).first().map(|column| column.0), Some(400.0), "Tools' column");
        set_visible(workspace, Panel::Library, true).unwrap();
        assert_eq!(columns(workspace, DockEdge::Start), [(350.0, vec![vec![Panel::Library]])], "the closed Library's column");
    }

    #[test]
    fn a_panel_without_a_recorded_place_opens_in_its_dock() {
        let root = split(Axis::X, SplitSize::FixedSecond(300.0), tabs(Panel::Canvas), Node::dock(DockEdge::End, vec![column(Panel::Tools, 300.0)]));
        let mut workspace = Workspace::new(Layout::new(Some(root), Vec::new()));
        set_visible(&mut workspace, Panel::Presets, true).unwrap();
        set_visible(&mut workspace, Panel::Library, true).unwrap();
        validate(&workspace).unwrap();
        assert_eq!(columns(&workspace, DockEdge::Start), [(crate::state::LEFT_WIDTH.default, vec![vec![Panel::Library]])]);
        assert_eq!(columns(&workspace, DockEdge::End), [(300.0, vec![vec![Panel::Tools]]), (PRESETS_WIDTH, vec![vec![Panel::Presets]])]);
        // Without Tools in the end dock, Presets takes the dock's window edge.
        let mut workspace = default_workspace();
        set_visible(&mut workspace, Panel::Presets, false).unwrap();
        workspace.forget_hidden(|_| true);
        apply(&mut workspace, Action::float(Panel::Tools, rect4([12.0, 34.0, 390.0, 450.0]))).unwrap();
        set_visible(&mut workspace, Panel::Presets, true).unwrap();
        assert_eq!(columns(&workspace, DockEdge::End), [(PRESETS_WIDTH, vec![vec![Panel::Presets]])]);
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
            Axis::X,
            SplitSize::FixedFirst(333.0),
            tabs(Panel::Library),
            split(Axis::X, SplitSize::FixedSecond(410.0), tabs(Panel::Canvas), tabs(Panel::Tools)),
        );
        let frames = Workspace::new(Layout::new(Some(frames), Vec::new()));
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
            assert!(!workspace.layout().contains(&Panel::Library), "the Library panel stays closed");
            assert_eq!(
                columns(workspace, DockEdge::End),
                [(crate::state::RIGHT_WIDTH.default, vec![vec![Panel::Tools]]), (PRESETS_WIDTH, vec![vec![Panel::Presets]])]
            );
        }
    }

    #[test]
    fn a_saved_dock_layout_round_trips() {
        let mut workspace = default_workspace();
        apply(&mut workspace, Action::resize_column(Panel::Tools, 333.0)).unwrap();
        apply(&mut workspace, Action::move_beside(Panel::Presets, Panel::Library, DockPlacement::group(true, None))).unwrap();
        set_visible(&mut workspace, Panel::Library, false).unwrap();
        let mut fields = saved_settings(serde_json::to_value(&workspace).unwrap(), Some(DOCKING_VERSION));
        assert!(!drop_outdated(&mut fields));
        let ui: crate::UiState = serde_json::from_value(Value::Object(fields)).unwrap();
        let loaded = ui.docking.unwrap().into_inner();
        validate(&loaded).unwrap();
        assert_eq!(loaded, workspace);
        assert_eq!(serde_json::to_value(crate::UiState::default()).unwrap()["dockingVersion"], DOCKING_VERSION, "saves write the version");
    }

    #[test]
    fn a_panel_whose_saved_place_is_gone_opens_in_its_dock() {
        let root = split(Axis::X, SplitSize::FixedSecond(300.0), tabs(Panel::Canvas), tabs(Panel::Tools));
        // Presets, the panel Library went back beside, is neither shown nor hidden.
        let location: Location<Panel> = serde_json::from_value(json!({
            "floating": null,
            "anchor": "presets",
            "placement": {"Split": "Left"},
            "size": {"FixedFirst": 280.0},
            "stack": null,
            "sibling": ["presets"]
        }))
        .unwrap();
        let mut workspace = Workspace::from_parts(Layout::new(Some(root), Vec::new()), vec![(Panel::Library, location)], Vec::new());
        validate(&workspace).unwrap();
        set_visible(&mut workspace, Panel::Library, true).unwrap();
        validate(&workspace).unwrap();
        assert_eq!(columns(&workspace, DockEdge::Start), [(crate::state::LEFT_WIDTH.default, vec![vec![Panel::Library]])]);
    }

    /// `ui.dock` operations parse as before: zones by name, `before` only with a tab or center
    /// move, and a missing panel or malformed params refused without a change.
    #[test]
    fn ui_dock_operations_parse_and_refuse_bad_params() {
        let mut workspace = default_workspace();
        run(&mut workspace, &json!({"operation":"move","panel":"presets","target":"tools","zone":"center"})).unwrap();
        assert_eq!(columns(&workspace, DockEdge::End).first().map(|column| column.1.clone()), Some(vec![vec![Panel::Tools, Panel::Presets]]));
        let before = workspace.clone();
        for params in [
            json!([]),
            json!({"operation":"move","panel":"library","target":"tools","zone":"left","before":"tools"}),
            json!({"operation":"resizeStack","panel":"tools"}),
            json!({"operation":"nothing","panel":"tools"}),
            json!({"operation":"float","panel":"tools","rect":[0,0,0,100]}),
        ] {
            assert!(run(&mut workspace, &params).is_err(), "{params}");
            assert_eq!(workspace, before, "{params}");
        }
        run(&mut workspace, &json!({"operation":"float","panel":"tools","rect":[20,30,300,400]})).unwrap();
        let window = workspace.layout().floating.iter().find(|window| window.contains(&Panel::Tools)).unwrap();
        assert_eq!(window.rect, rect4([20.0, 30.0, 300.0, 400.0]));
    }
}
