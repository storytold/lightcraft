use serde::{Deserialize, Serialize};

/// State for the Map module (viewport center, zoom, provider, locked, filters).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MapUi {
    pub center_lat: f64,
    pub center_lon: f64,
    pub zoom: f64,
    pub navigator_zoom: f64,
    pub provider: lightcraft_engine::map::MapProvider,
    pub filter: lightcraft_engine::map::LocationFilter,
    pub show_filter_bar: bool,
    pub locked: bool,
    pub active_pin_photo: Option<lightcraft_catalog::PhotoId>,
    pub tracklog_path: Option<String>,
    pub tracklog_visible: bool,
    pub legend_open: bool,
    pub navigator_open: bool,
}

impl Default for MapUi {
    fn default() -> Self {
        Self {
            center_lat: 45.57,
            center_lon: -73.2,
            zoom: 12.0,
            navigator_zoom: 3.0,
            provider: lightcraft_engine::map::MapProvider::OpenStreetMap,
            filter: lightcraft_engine::map::LocationFilter::None,
            show_filter_bar: true,
            locked: false,
            active_pin_photo: None,
            tracklog_path: None,
            tracklog_visible: true,
            legend_open: true,
            navigator_open: true,
        }
    }
}
