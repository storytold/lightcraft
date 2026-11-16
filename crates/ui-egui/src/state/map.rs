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
    /// The area Visible on Map narrows the photos to (the view's, once it rested). Not saved.
    #[serde(skip)]
    pub applied_area: Option<lightcraft_catalog::rules::GpsArea>,
    /// The view's area last seen, and when it was first seen (to wait for the view to rest).
    #[serde(skip)]
    pub seen_area: Option<(lightcraft_catalog::rules::GpsArea, f64)>,
    /// The saved location picked in the list (its handles are shown). Not saved.
    #[serde(skip)]
    pub active_location: Option<u64>,
    /// The active location's handle being dragged. Not saved.
    #[serde(skip)]
    pub location_drag: Option<crate::panels::map::locations::LocationDrag>,
    /// The location being renamed in the list, and the name typed so far. Not saved.
    #[serde(skip)]
    pub renaming: Option<(u64, String)>,
    /// The location whose Delete… asks for confirmation. Not saved.
    #[serde(skip)]
    pub confirm_delete: Option<u64>,
    /// Photos in a private location whose Info panel map was asked for (Show Map). Not saved.
    #[serde(skip)]
    pub private_shown: Vec<u64>,
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
            applied_area: None,
            seen_area: None,
            active_location: None,
            location_drag: None,
            renaming: None,
            confirm_delete: None,
            private_shown: Vec::new(),
        }
    }
}
