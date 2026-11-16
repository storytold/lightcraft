use serde::{Deserialize, Serialize};

/// State for the Map module (viewport center, zoom, locked, filters). The tile source is a
/// library preference in the engine (`map.tileSource`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MapUi {
    pub center_lat: f64,
    pub center_lon: f64,
    pub zoom: f64,
    pub navigator_zoom: f64,
    pub filter: lightcraft_engine::map::LocationFilter,
    pub show_filter_bar: bool,
    pub locked: bool,
    pub active_pin_photo: Option<lightcraft_catalog::PhotoId>,
    pub tracklog_path: Option<String>,
    pub tracklog_visible: bool,
    pub legend_open: bool,
    pub navigator_open: bool,
    /// The view was set (fitted, moved, zoomed) this session: the first showing fits the photos
    /// only while this is false. Not saved.
    #[serde(skip)]
    pub fitted: bool,
    /// The map canvas last drawn (Fit to Photos sizes the view to it). Not saved.
    #[serde(skip)]
    pub last_canvas: Option<egui::Rect>,
    /// A single photo's pin being dragged to a new place. Not saved.
    #[serde(skip)]
    pub pin_drag: Option<crate::panels::map::PinDrag>,
    /// The photos of the pin whose right-click menu is open. Not saved.
    #[serde(skip)]
    pub menu_for: Vec<u64>,
}

impl Default for MapUi {
    fn default() -> Self {
        Self {
            center_lat: 45.57,
            center_lon: -73.2,
            zoom: 12.0,
            navigator_zoom: 3.0,
            filter: lightcraft_engine::map::LocationFilter::None,
            show_filter_bar: true,
            locked: false,
            active_pin_photo: None,
            tracklog_path: None,
            tracklog_visible: true,
            legend_open: true,
            navigator_open: true,
            fitted: false,
            last_canvas: None,
            pin_drag: None,
            menu_for: Vec::new(),
        }
    }
}
