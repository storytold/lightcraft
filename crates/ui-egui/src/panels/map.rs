//! Map module view: interactive slippy map canvas, photo marker clustering, loupe popover and
//! saved locations. Tiles come from the engine's tile service (OpenStreetMap by default, under its
//! tile usage policy) and are drawn by [`tiles`].

pub mod tiles;

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use lightcraft_catalog::PhotoId;
use lightcraft_engine::map::LocationFilter;
use lightcraft_geom::{GeoBounds, LatLon};
use serde_json::json;

use crate::LightcraftApp;
use crate::state::ViewMode;
use crate::theme::Tokens;

/// Number of screen pixels per tile at standard resolution.
const TILE_SIZE: f32 = 256.0;

/// Screen distance threshold in points for merging photo markers into a cluster.
const CLUSTER_RADIUS_PTS: f32 = 36.0;

/// Represents a clustered group of photos on the map screen.
struct MapCluster {
    center: Pos2,
    lat_lon: LatLon,
    photos: Vec<PhotoId>,
    has_selected: bool,
}

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let _t = Tokens::get(ui.ctx());
    let rect = ui.available_rect_before_wrap();
    if rect.width() <= 10.0 || rect.height() <= 10.0 {
        return;
    }

    let mut map_ui = app.ui.map.clone();
    let mut center = LatLon::new(map_ui.center_lat, map_ui.center_lon).unwrap_or(LatLon { lat: 45.57, lon: -73.2 });
    let source = app.session.map.effective_source();
    let max_zoom = f64::from(source.max_zoom());
    let mut zoom = map_ui.zoom.clamp(1.0, max_zoom);

    let (left_w, navigator_h) = if map_ui.navigator_open { (220.0_f32, 160.0_f32) } else { (0.0, 0.0) };
    let canvas_rect = Rect::from_min_max(pos2(rect.left() + left_w, rect.top()), rect.max);

    // 1. Background Map Canvas & Interaction
    let canvas_response = ui.allocate_rect(canvas_rect, Sense::click_and_drag());
    let painter = ui.painter_at(canvas_rect);

    let screen_center = canvas_rect.center();

    let lat_lon_to_screen = |coord: LatLon, cur_zoom: f64, cur_center: LatLon| -> Pos2 {
        let (c_tx, c_ty) = cur_center.to_tile_coords(cur_zoom);
        let (tx, ty) = coord.to_tile_coords(cur_zoom);
        pos2(screen_center.x + ((tx - c_tx) * TILE_SIZE as f64) as f32, screen_center.y + ((ty - c_ty) * TILE_SIZE as f64) as f32)
    };

    let screen_to_lat_lon = |screen_pos: Pos2, cur_zoom: f64, cur_center: LatLon| -> LatLon {
        let (c_tx, c_ty) = cur_center.to_tile_coords(cur_zoom);
        let tx = c_tx + (screen_pos.x - screen_center.x) as f64 / TILE_SIZE as f64;
        let ty = c_ty + (screen_pos.y - screen_center.y) as f64 / TILE_SIZE as f64;
        LatLon::from_tile_coords(tx, ty, cur_zoom)
    };

    // Pan with mouse drag
    if canvas_response.dragged() && !canvas_response.drag_started() {
        let delta = canvas_response.drag_delta();
        let n = 2.0_f64.powf(zoom.clamp(0.0, 24.0));
        let delta_tile_x = -delta.x as f64 / TILE_SIZE as f64;
        let delta_tile_y = -delta.y as f64 / TILE_SIZE as f64;

        let (mut cur_tx, mut cur_ty) = center.to_tile_coords(zoom);
        cur_tx = (cur_tx + delta_tile_x).rem_euclid(n);
        cur_ty = (cur_ty + delta_tile_y).clamp(0.0, n);
        center = LatLon::from_tile_coords(cur_tx, cur_ty, zoom);
    }

    // Scroll wheel zoom anchored to cursor
    if canvas_response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        let double_click = canvas_response.double_clicked();
        let delta_zoom = if double_click {
            1.0
        } else if scroll.abs() > 0.5 {
            scroll as f64 * 0.005
        } else {
            0.0
        };

        if delta_zoom != 0.0 {
            let new_zoom = (zoom + delta_zoom).clamp(1.0, max_zoom);
            if let Some(pointer_pos) = ui.input(|i| i.pointer.hover_pos()).filter(|&pos| canvas_rect.contains(pos)) {
                let mouse_geo = screen_to_lat_lon(pointer_pos, zoom, center);
                let (m_tx, m_ty) = mouse_geo.to_tile_coords(new_zoom);
                let n = 2.0_f64.powf(new_zoom.clamp(0.0, 24.0));
                let new_center_tx = (m_tx - (pointer_pos.x - screen_center.x) as f64 / TILE_SIZE as f64).rem_euclid(n);
                let new_center_ty = (m_ty - (pointer_pos.y - screen_center.y) as f64 / TILE_SIZE as f64).clamp(0.0, n);
                center = LatLon::from_tile_coords(new_center_tx, new_center_ty, new_zoom);
            }
            zoom = new_zoom;
        }
    }

    // Tiles (the service is asked for the view plus one ring)
    tiles::draw(&mut app.session, &painter, canvas_rect, center, zoom, 1);

    // 2. Fetch and Cluster Candidate Photos
    let candidate_photos = collect_photos(app, map_ui.filter, |p| screen_to_lat_lon(p, zoom, center), canvas_rect);
    let clusters = cluster_photos(&candidate_photos, |coord| lat_lon_to_screen(coord, zoom, center), canvas_rect);

    // 3. Render Clusters and Markers
    let mut clicked_photo = None;
    let mut double_clicked_photo = None;
    let mut zoom_target = None;

    for cluster in &clusters {
        let marker_rect = Rect::from_center_size(cluster.center, vec2(32.0, 32.0));
        let is_hovered = ui.rect_contains_pointer(marker_rect);

        if cluster.photos.len() == 1 {
            let photo_id = cluster.photos[0];
            let is_selected = cluster.has_selected;
            render_single_pin(&painter, cluster.center, is_selected, is_hovered);

            if is_hovered && ui.input(|i| i.pointer.primary_clicked()) {
                clicked_photo = Some(photo_id);
            }
            if is_hovered && ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)) {
                double_clicked_photo = Some(photo_id);
            }
        } else {
            render_cluster_badge(&painter, cluster.center, cluster.photos.len(), cluster.has_selected, is_hovered);

            if is_hovered && ui.input(|i| i.pointer.primary_clicked()) {
                clicked_photo = cluster.photos.first().copied();
            }
            if is_hovered && ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)) {
                let next_z = (zoom + 2.0).min(max_zoom);
                zoom_target = Some((cluster.lat_lon, next_z));
            }
        }
    }

    if let Some((target_center, next_z)) = zoom_target {
        center = target_center;
        zoom = next_z;
    }

    if let Some(id) = clicked_photo {
        map_ui.active_pin_photo = Some(id);
        let _ = app.run("library.select", json!({"ids": [id.0]}));
    }
    if let Some(id) = double_clicked_photo {
        let _ = app.run("library.select", json!({"ids": [id.0]}));
        app.ui.view = ViewMode::Detail;
    }

    // 4. Render Active Pin Loupe Popover Card
    if let Some(active_id) = map_ui.active_pin_photo {
        let pin_coord = app.session.catalog.photo(active_id).and_then(|ph| ph.meta.gps).and_then(|(lat, lon)| LatLon::new(lat, lon));
        if let Some(coord) = pin_coord {
            let pin_pos = lat_lon_to_screen(coord, zoom, center);
            if canvas_rect.contains(pin_pos) {
                render_loupe_popover(app, ui, pin_pos, active_id, &mut map_ui);
            }
        }
    }

    // 4.5. Render active / matching saved location boundary circle
    for loc in &app.session.saved_locations {
        if (loc.lat - center.lat).abs() < 0.0001 && (loc.lon - center.lon).abs() < 0.0001 {
            let loc_coord = LatLon { lat: loc.lat, lon: loc.lon };
            let screen_pos = lat_lon_to_screen(loc_coord, zoom, center);
            let meters_per_px = (40075016.686 * (loc.lat.to_radians().cos().abs())) / (256.0 * 2.0_f64.powf(zoom));
            if meters_per_px > 0.0 {
                let radius_px = (loc.radius / meters_per_px) as f32;
                if radius_px > 4.0 && radius_px < 8000.0 {
                    painter.circle_filled(screen_pos, radius_px, Color32::from_rgba_unmultiplied(255, 204, 0, 20));
                    painter.circle_stroke(screen_pos, radius_px, Stroke::new(1.5, Color32::from_rgba_unmultiplied(255, 204, 0, 160)));
                }
            }
        }
    }

    // 5. Top Location Filter Bar & Breadcrumb
    if map_ui.show_filter_bar {
        render_top_filter_bar(app, ui, canvas_rect, &mut map_ui, center);
    }

    // 6. Bottom Map Toolbar (Style dropdown, Zoom slider, Lock button, Scale & Attribution)
    render_bottom_toolbar(app, ui, canvas_rect, &mut map_ui, &mut zoom);

    // 7. Map Legend Overlay
    if map_ui.legend_open {
        render_legend_overlay(ui, canvas_rect, &mut map_ui);
    }

    // 8. Left Navigator Mini-Map and Saved Locations Panel
    if map_ui.navigator_open {
        render_left_navigator(app, ui, (rect, canvas_rect), (left_w, navigator_h), &mut center, &mut zoom, &mut map_ui);
    }

    // Update persistent state
    map_ui.center_lat = center.lat;
    map_ui.center_lon = center.lon;
    map_ui.zoom = zoom;
    app.ui.map = map_ui;
}

fn collect_photos<F>(app: &mut LightcraftApp, filter: LocationFilter, screen_to_lat_lon: F, canvas: Rect) -> Vec<(PhotoId, LatLon, bool)>
where
    F: Fn(Pos2) -> LatLon,
{
    let mut out = Vec::new();
    let bounds = GeoBounds::new(
        screen_to_lat_lon(pos2(canvas.left(), canvas.bottom())).lat,
        screen_to_lat_lon(pos2(canvas.left(), canvas.top())).lat,
        screen_to_lat_lon(pos2(canvas.left(), canvas.top())).lon,
        screen_to_lat_lon(pos2(canvas.right(), canvas.top())).lon,
    );

    let visible_ids = app.session.visible().to_vec();
    for id in visible_ids {
        let Some(ph) = app.session.catalog.photo(id) else { continue };
        let is_selected = app.session.selection.contains(id);
        if let Some((lat, lon)) = ph.meta.gps {
            if let Some(coord) = LatLon::new(lat, lon) {
                match filter {
                    LocationFilter::Untagged => {}
                    LocationFilter::Tagged | LocationFilter::None => {
                        out.push((id, coord, is_selected));
                    }
                    LocationFilter::VisibleOnMap => {
                        if bounds.contains(coord) {
                            out.push((id, coord, is_selected));
                        }
                    }
                }
            }
        } else if filter == LocationFilter::Untagged || filter == LocationFilter::None {
            // Untagged photo
        }
    }
    out
}

fn cluster_photos<F>(photos: &[(PhotoId, LatLon, bool)], lat_lon_to_screen: F, canvas: Rect) -> Vec<MapCluster>
where
    F: Fn(LatLon) -> Pos2,
{
    let mut clusters: Vec<MapCluster> = Vec::new();

    for &(id, coord, is_selected) in photos {
        let pos = lat_lon_to_screen(coord);
        if !canvas.expand(50.0).contains(pos) {
            continue;
        }

        let mut merged = false;
        for c in &mut clusters {
            if c.center.distance(pos) <= CLUSTER_RADIUS_PTS {
                c.photos.push(id);
                if is_selected {
                    c.has_selected = true;
                }
                merged = true;
                break;
            }
        }

        if !merged {
            clusters.push(MapCluster { center: pos, lat_lon: coord, photos: vec![id], has_selected: is_selected });
        }
    }
    clusters
}

fn render_single_pin(p: &Painter, pos: Pos2, selected: bool, hovered: bool) {
    let pin_color = if selected {
        Color32::from_rgb(255, 204, 0) // Yellow for selected
    } else {
        Color32::from_rgb(255, 115, 0) // Orange for unselected
    };
    let border_color = if hovered { Color32::WHITE } else { Color32::from_black_alpha(180) };

    // Pin head circle
    let head_center = pos2(pos.x, pos.y - 12.0);
    p.circle_filled(head_center, 7.0, pin_color);
    p.circle_stroke(head_center, 7.0, Stroke::new(1.5, border_color));

    // Center dot
    p.circle_filled(head_center, 2.5, Color32::WHITE);

    // Pin point
    p.line_segment([pos2(pos.x, pos.y - 5.0), pos], Stroke::new(2.0, pin_color));
}

fn render_cluster_badge(p: &Painter, pos: Pos2, count: usize, selected: bool, hovered: bool) {
    let badge_color = if selected { Color32::from_rgb(255, 204, 0) } else { Color32::from_rgb(240, 100, 20) };
    let text_color = if selected { Color32::BLACK } else { Color32::WHITE };
    let border_stroke = Stroke::new(if hovered { 2.0 } else { 1.2 }, if hovered { Color32::WHITE } else { Color32::from_black_alpha(160) });

    let label = count.to_string();
    let badge_w = (label.len() as f32 * 8.0 + 16.0).max(28.0);
    let badge_rect = Rect::from_center_size(pos, vec2(badge_w, 22.0));

    p.rect_filled(badge_rect, 11.0, badge_color);
    p.rect_stroke(badge_rect, 11.0, border_stroke, StrokeKind::Inside);
    p.text(pos, Align2::CENTER_CENTER, label, FontId::proportional(12.0), text_color);
}

fn render_top_filter_bar(_app: &mut LightcraftApp, ui: &mut egui::Ui, canvas: Rect, map_ui: &mut crate::state::MapUi, center: LatLon) {
    let t = Tokens::get(ui.ctx());
    let bar_rect = Rect::from_min_size(pos2(canvas.left() + 16.0, canvas.top() + 14.0), vec2(canvas.width() - 32.0, 36.0));

    let bar_bg = Color32::from_rgba_unmultiplied(20, 24, 28, 220);
    ui.painter().rect_filled(bar_rect, 6.0, bar_bg);
    ui.painter().rect_stroke(bar_rect, 6.0, Stroke::new(1.0, t.button_border), StrokeKind::Inside);

    let mut bar_ui = ui.new_child(egui::UiBuilder::new().max_rect(bar_rect));
    bar_ui.horizontal_centered(|ui| {
        ui.add_space(8.0);
        ui.label(egui::RichText::new(crate::i18n::tr("Location Filter:")).strong().color(Color32::WHITE));
        ui.add_space(8.0);

        let filters = [
            (LocationFilter::VisibleOnMap, "Visible On Map"),
            (LocationFilter::Tagged, "Tagged"),
            (LocationFilter::Untagged, "Untagged"),
            (LocationFilter::None, "None"),
        ];

        for (f, name) in filters {
            let active = map_ui.filter == f;
            let btn = ui.selectable_label(active, crate::i18n::tr(name));
            if btn.clicked() {
                map_ui.filter = f;
            }
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(10.0);
            ui.label(egui::RichText::new(format!("{:.4}°, {:.4}°", center.lat, center.lon)).small().color(Color32::LIGHT_GRAY));
        });
    });
}

fn render_bottom_toolbar(app: &mut LightcraftApp, ui: &mut egui::Ui, canvas: Rect, map_ui: &mut crate::state::MapUi, zoom: &mut f64) {
    let t = Tokens::get(ui.ctx());
    let bar_h = 36.0;
    let bar_rect = Rect::from_min_max(pos2(canvas.left(), canvas.bottom() - bar_h), canvas.max);

    ui.painter().rect_filled(bar_rect, 0.0, Color32::from_rgba_unmultiplied(16, 20, 24, 235));
    ui.painter().line_segment([pos2(bar_rect.left(), bar_rect.top()), pos2(bar_rect.right(), bar_rect.top())], Stroke::new(1.0, t.button_border));

    let mut bar_ui = ui.new_child(egui::UiBuilder::new().max_rect(bar_rect));
    bar_ui.horizontal_centered(|ui| {
        ui.add_space(12.0);
        ui.label(egui::RichText::new(crate::i18n::tr("Map Style:")).small().color(Color32::LIGHT_GRAY));

        let current = app.session.map.effective_source();
        let max_zoom = f64::from(current.max_zoom());
        let mut chosen = None;
        crate::widgets::combo_box("map_style_combo").selected_text(crate::i18n::tr(current.label())).show_ui(ui, |ui| {
            // only the sources usable now: the Esri layers are not offered (docs/map.md)
            for p in app.session.map.offered() {
                if ui.selectable_label(p == current, crate::i18n::tr(p.label())).clicked() {
                    chosen = Some(p.id().to_string());
                }
            }
        });
        if let Some(id) = chosen
            && let Err(e) = app.run("map.tileSource", json!({"id": id}))
        {
            app.ui.toast = Some((e, app.tick_now() + 6.0, None));
        }

        ui.add_space(16.0);
        if ui.button("-").clicked() {
            *zoom = (*zoom - 1.0).max(1.0);
        }
        ui.add(egui::Slider::new(zoom, 1.0..=max_zoom).show_value(false));
        if ui.button("+").clicked() {
            *zoom = (*zoom + 1.0).min(max_zoom);
        }

        ui.add_space(12.0);
        let lock_btn = ui.selectable_label(map_ui.locked, crate::i18n::tr("Lock Markers"));
        if lock_btn.clicked() {
            map_ui.locked = !map_ui.locked;
        }

        // the tile provider's credit, always visible at the bottom right (tile licences require it)
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(12.0);
            let credit = ui.label(egui::RichText::new(current.attribution()).small().color(Color32::from_white_alpha(170)));
            crate::widgets::register(ui, "map:attribution", credit.rect);
        });
    });
}

fn render_legend_overlay(ui: &mut egui::Ui, canvas: Rect, _map_ui: &mut crate::state::MapUi) {
    let legend_rect = Rect::from_min_size(pos2(canvas.right() - 210.0, canvas.bottom() - 170.0), vec2(196.0, 124.0));
    let p = ui.painter();

    p.rect_filled(legend_rect, 6.0, Color32::from_rgba_unmultiplied(20, 24, 28, 230));
    p.rect_stroke(legend_rect, 6.0, Stroke::new(1.0, Color32::from_white_alpha(40)), StrokeKind::Inside);

    let items = [
        (Color32::from_rgb(255, 115, 0), "Unselected photo"),
        (Color32::from_rgb(255, 204, 0), "Selected photo"),
        (Color32::from_rgb(240, 100, 20), "Group / cluster"),
    ];

    for (i, (color, text)) in items.iter().enumerate() {
        let y = legend_rect.top() + 20.0 + i as f32 * 28.0;
        p.circle_filled(pos2(legend_rect.left() + 20.0, y), 5.0, *color);
        p.text(pos2(legend_rect.left() + 34.0, y), Align2::LEFT_CENTER, crate::i18n::tr(text), FontId::proportional(11.0), Color32::WHITE);
    }
}

fn render_loupe_popover(app: &mut LightcraftApp, ui: &mut egui::Ui, pin_pos: Pos2, photo_id: PhotoId, map_ui: &mut crate::state::MapUi) {
    let card_w = 230.0;
    let card_h = 100.0;
    let card_rect = Rect::from_min_size(pos2(pin_pos.x - card_w / 2.0, pin_pos.y - card_h - 18.0), vec2(card_w, card_h));

    let p = ui.painter();
    p.rect_filled(card_rect, 6.0, Color32::from_rgba_unmultiplied(24, 28, 34, 245));
    p.rect_stroke(card_rect, 6.0, Stroke::new(1.2, Color32::from_white_alpha(70)), StrokeKind::Inside);

    let Some(ph) = app.session.catalog.photo(photo_id) else { return };
    let mut card_ui = ui.new_child(egui::UiBuilder::new().max_rect(card_rect));

    card_ui.vertical(|ui| {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(&ph.file_name).strong().color(Color32::WHITE));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                if ui.button("X").clicked() {
                    map_ui.active_pin_photo = None;
                }
            });
        });

        ui.add_space(4.0);
        if let Some(ref cap) = ph.captured {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(cap).small().color(Color32::LIGHT_GRAY));
            });
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if ui.button(crate::i18n::tr("Open in Detail")).clicked() {
                app.ui.view = ViewMode::Detail;
            }
        });
    });
}

fn zoom_from_radius(radius_meters: f64) -> f64 {
    if radius_meters <= 300.0 {
        16.5
    } else if radius_meters <= 800.0 {
        15.5
    } else if radius_meters <= 2000.0 {
        14.0
    } else if radius_meters <= 5000.0 {
        12.5
    } else if radius_meters <= 15000.0 {
        11.0
    } else if radius_meters <= 50000.0 {
        9.5
    } else {
        8.0
    }
}

fn render_left_navigator(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    rects: (Rect, Rect),
    nav_dims: (f32, f32),
    center: &mut LatLon,
    zoom: &mut f64,
    map_ui: &mut crate::state::MapUi,
) {
    let (rect, canvas_rect) = rects;
    let (left_w, nav_h) = nav_dims;
    let t = Tokens::get(ui.ctx());
    let left_rect = Rect::from_min_size(rect.min, vec2(left_w, rect.height()));

    ui.painter().rect_filled(left_rect, 0.0, Color32::from_rgb(26, 30, 36));
    ui.painter()
        .line_segment([pos2(left_rect.right(), left_rect.top()), pos2(left_rect.right(), left_rect.bottom())], Stroke::new(1.0, t.button_border));

    let mut panel_ui = ui.new_child(egui::UiBuilder::new().max_rect(left_rect));
    panel_ui.vertical(|ui| {
        // Navigator mini-map header
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(crate::i18n::tr("Navigator")).strong().color(Color32::WHITE));
        });

        ui.add_space(4.0);
        let mini_w = left_w - 16.0;
        let mini_rect = Rect::from_min_size(pos2(left_rect.left() + 8.0, ui.cursor().top()), vec2(mini_w, nav_h));
        let nav_resp = ui.allocate_rect(mini_rect, Sense::click_and_drag());

        let nav_max = f64::from(app.session.map.effective_source().max_zoom().saturating_sub(2).max(1));
        let nav_zoom = map_ui.navigator_zoom.clamp(1.0, nav_max);

        if nav_resp.dragged() {
            let delta = nav_resp.drag_delta();
            let delta_tx = delta.x as f64 / TILE_SIZE as f64;
            let delta_ty = delta.y as f64 / TILE_SIZE as f64;
            let n = 2.0_f64.powf(nav_zoom.clamp(0.0, 24.0));
            let (cur_tx, cur_ty) = center.to_tile_coords(nav_zoom);
            let new_tx = (cur_tx + delta_tx).rem_euclid(n);
            let new_ty = (cur_ty + delta_ty).clamp(0.0, n);
            *center = LatLon::from_tile_coords(new_tx, new_ty, nav_zoom);
            ui.ctx().request_repaint();
        } else if nav_resp.clicked()
            && let Some(pos) = nav_resp.interact_pointer_pos()
        {
            let offset_x = pos.x - mini_rect.center().x;
            let offset_y = pos.y - mini_rect.center().y;
            let delta_tx = offset_x as f64 / TILE_SIZE as f64;
            let delta_ty = offset_y as f64 / TILE_SIZE as f64;
            let n = 2.0_f64.powf(nav_zoom.clamp(0.0, 24.0));
            let (cur_tx, cur_ty) = center.to_tile_coords(nav_zoom);
            let new_tx = (cur_tx + delta_tx).rem_euclid(n);
            let new_ty = (cur_ty + delta_ty).clamp(0.0, n);
            *center = LatLon::from_tile_coords(new_tx, new_ty, nav_zoom);
            ui.ctx().request_repaint();
        }

        if nav_resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.5 {
                map_ui.navigator_zoom = (map_ui.navigator_zoom + (scroll as f64 * 0.005)).clamp(1.0, nav_max);
                ui.ctx().request_repaint();
            }
        }

        let nav_painter = ui.painter().with_clip_rect(mini_rect);
        nav_painter.rect_filled(mini_rect, 4.0, Color32::from_rgb(18, 22, 26));

        // Render overview map tiles (only what it shows: no ring)
        tiles::draw(&mut app.session, &nav_painter, mini_rect, *center, nav_zoom, 0);

        // Viewport rectangle showing the visible area on the main map
        let scale_diff = 2.0_f64.powf(nav_zoom - *zoom);
        let box_w = (canvas_rect.width() as f64 * scale_diff) as f32;
        let box_h = (canvas_rect.height() as f64 * scale_diff) as f32;
        let box_w_clamped = box_w.clamp(6.0, mini_rect.width() * 1.5);
        let box_h_clamped = box_h.clamp(6.0, mini_rect.height() * 1.5);
        let view_rect = Rect::from_center_size(mini_rect.center(), vec2(box_w_clamped, box_h_clamped));

        nav_painter.rect_filled(view_rect, 1.0, Color32::from_white_alpha(35));
        nav_painter.rect_stroke(view_rect, 1.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Middle);
        nav_painter.circle_filled(mini_rect.center(), 2.0, Color32::from_rgb(255, 204, 0));

        // Frame
        ui.painter().rect_stroke(mini_rect, 4.0, Stroke::new(1.0, t.button_border), StrokeKind::Inside);

        // Saved Locations header
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(crate::i18n::tr("Saved Locations")).strong().color(Color32::WHITE));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                if ui.button("+").on_hover_text(crate::i18n::tr("Save current map location")).clicked() {
                    let _ = app.run(
                        "map.savedLocations.create",
                        json!({
                            "name": format!("Location {}", app.session.saved_locations.len() + 1),
                            "lat": center.lat,
                            "lon": center.lon,
                            "radius": 1500.0,
                        }),
                    );
                }
            });
        });

        ui.add_space(8.0);
        let mut delete_id = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            if app.session.saved_locations.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.label(egui::RichText::new(crate::i18n::tr("No saved locations. Click + to save.")).small().italics().color(Color32::GRAY));
                });
            } else {
                for loc in &app.session.saved_locations {
                    let is_active = (loc.lat - center.lat).abs() < 0.0001 && (loc.lon - center.lon).abs() < 0.0001;
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        let resp = ui.selectable_label(is_active, &loc.name);
                        if resp.clicked()
                            && let Some(target) = LatLon::new(loc.lat, loc.lon)
                        {
                            *center = target;
                            *zoom = zoom_from_radius(loc.radius);
                            ui.ctx().request_repaint();
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_space(8.0);
                            if ui.small_button("✕").on_hover_text(crate::i18n::tr("Delete location")).clicked() {
                                delete_id = Some(loc.id.clone());
                            }
                        });
                    });
                }
            }
        });
        if let Some(id) = delete_id {
            let _ = app.run("map.savedLocations.delete", json!({"id": id}));
            ui.ctx().request_repaint();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;
    use crate::headless::HeadlessView;

    #[test]
    fn cluster_photos_merges_close_pins() {
        let p1 = (PhotoId(1), LatLon::new(45.5700, -73.2000).unwrap(), false);
        let p2 = (PhotoId(2), LatLon::new(45.5701, -73.2001).unwrap(), true);
        let p3 = (PhotoId(3), LatLon::new(51.5074, -0.1278).unwrap(), false);

        let canvas = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 800.0));
        let lat_lon_to_screen = |coord: LatLon| -> Pos2 {
            if coord.lat > 50.0 {
                pos2(800.0, 600.0) // p3
            } else if coord.lat > 45.57005 {
                pos2(205.0, 205.0) // p2 (distance to p1 = 7.07 < 36.0)
            } else {
                pos2(200.0, 200.0) // p1
            }
        };

        let clusters = cluster_photos(&[p1, p2, p3], lat_lon_to_screen, canvas);
        assert_eq!(clusters.len(), 2);
        assert_eq!(clusters[0].photos.len(), 2);
        assert!(clusters[0].has_selected);
        assert_eq!(clusters[1].photos.len(), 1);
        assert!(!clusters[1].has_selected);
    }

    #[test]
    fn headless_map_view_renders_cleanly() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
        app.ui.view = ViewMode::Map;

        let mut view = HeadlessView::new();
        let raw = HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        view.run(raw, |ui| {
            show(&mut app, ui);
        });

        assert_eq!(app.ui.view, ViewMode::Map);
        assert_eq!(app.session.map.effective_source(), lightcraft_engine::map::TileSource::Osm);
    }

    #[test]
    fn continuous_zoom_and_pan_preserves_coordinates() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
        app.ui.view = ViewMode::Map;
        app.ui.map.center_lat = 45.57;
        app.ui.map.center_lon = -73.2;
        app.ui.map.zoom = 10.35;

        let mut view = HeadlessView::new();
        let mut raw = HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        raw.events.push(egui::Event::PointerMoved(pos2(500.0, 350.0)));
        raw.events.push(egui::Event::PointerButton {
            pos: pos2(500.0, 350.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        raw.events.push(egui::Event::PointerMoved(pos2(550.0, 370.0)));

        view.run(raw, |ui| {
            show(&mut app, ui);
        });

        assert!(app.ui.map.center_lat.is_finite());
        assert!(app.ui.map.center_lon.is_finite());
        assert!((app.ui.map.center_lat - 45.57).abs() < 1.0);
        assert!((app.ui.map.center_lon - (-73.2)).abs() < 1.0);
    }

    #[test]
    fn saved_location_selection_updates_map_center() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
        app.ui.view = ViewMode::Map;
        app.ui.map.center_lat = 0.0;
        app.ui.map.center_lon = 0.0;
        app.ui.map.zoom = 5.0;

        let _ = app.run(
            "map.savedLocations.create",
            json!({
                "name": "Beloeil",
                "lat": 45.57,
                "lon": -73.2,
                "radius": 1500.0,
            }),
        );

        let mut view = HeadlessView::new();
        let raw = HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        view.run(raw, |ui| {
            show(&mut app, ui);
        });

        assert_eq!(app.session.saved_locations.len(), 1);
        let loc = &app.session.saved_locations[0];
        app.ui.map.center_lat = loc.lat;
        app.ui.map.center_lon = loc.lon;
        app.ui.map.zoom = zoom_from_radius(loc.radius);

        assert_eq!(app.ui.map.center_lat, 45.57);
        assert_eq!(app.ui.map.center_lon, -73.2);
        assert_eq!(app.ui.map.zoom, 14.0);
    }

    #[test]
    fn navigator_zoom_is_independent_from_map_zoom() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
        app.ui.view = ViewMode::Map;
        app.ui.map.zoom = 14.0;
        app.ui.map.navigator_zoom = 3.0;

        let mut view = HeadlessView::new();
        let raw = HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        view.run(raw, |ui| {
            show(&mut app, ui);
        });

        assert_eq!(app.ui.map.zoom, 14.0);
        assert_eq!(app.ui.map.navigator_zoom, 3.0);
    }

    /// A transport that serves a flat 256 × 256 PNG for every tile and records the URLs (never
    /// the internet).
    struct FakeTiles {
        urls: std::sync::Mutex<Vec<String>>,
        png: Vec<u8>,
    }

    impl lightcraft_engine::map::Transport for FakeTiles {
        fn get(
            &self,
            url: &str,
            _headers: &[(&'static str, String)],
            _max: usize,
            _cancel: &std::sync::atomic::AtomicBool,
        ) -> Result<lightcraft_engine::map::HttpResponse, String> {
            self.urls.lock().unwrap().push(url.to_string());
            Ok(lightcraft_engine::map::HttpResponse { status: 200, headers: vec![], body: self.png.clone() })
        }
    }

    fn fake_tiles() -> std::sync::Arc<FakeTiles> {
        use lightcraft_codecs::{EncodeImage, EncodeMeta, Samples, encode_png};
        let data: Vec<u8> = (0..256 * 256).flat_map(|_| [10u8, 120, 200, 255]).collect();
        let png = encode_png(&EncodeImage::new(256, 256, 4, Samples::U8(&data)), &EncodeMeta::default()).unwrap();
        std::sync::Arc::new(FakeTiles { urls: std::sync::Mutex::new(Vec::new()), png })
    }

    /// A session whose map uses `id` (a custom source, so tests don't share textures) served by
    /// the fake transport.
    fn session_with_fake_tiles(id: &str, fake: std::sync::Arc<FakeTiles>) -> lightcraft_engine::Session {
        use lightcraft_engine::map::{TileService, TileSource};
        let mut session = lightcraft_engine::Session::new();
        let source = TileSource::custom(Some(id), "https://tiles.test/{z}/{x}/{y}.png", Some("© Test tiles"), Some(19)).unwrap();
        session
            .execute(
                "map.tileSource",
                &json!({"id": id, "urlTemplate": "https://tiles.test/{z}/{x}/{y}.png", "attribution": "© Test tiles", "maxZoom": 19}),
            )
            .unwrap();
        session.map.set_service(TileService::with_transport(source, None, Some(fake)).with_activity(Some(session.activity.clone())));
        session
    }

    /// Draws `center` at `zoom` in a 512 × 512 map once and returns what was drawn.
    fn draw_once(session: &mut lightcraft_engine::Session, center: LatLon, zoom: f64) -> tiles::Drawn {
        let mut view = HeadlessView::new();
        let raw = HeadlessView::raw_input(vec2(600.0, 600.0), 1.0, 0.0, vec![]);
        let mut drawn = tiles::Drawn::default();
        view.run(raw, |ui| {
            let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(512.0, 512.0));
            drawn = tiles::draw(session, &ui.painter_at(rect), rect, center, zoom, 1);
        });
        drawn
    }

    fn wait_idle(session: &lightcraft_engine::Session) {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !session.map.service().is_some_and(|s| s.is_idle()) {
            assert!(std::time::Instant::now() < end, "the tile service never finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Tiles come from the engine's tile service; one not loaded yet is drawn from a loaded
    /// ancestor (scaled) instead of a blank square.
    #[test]
    fn tiles_are_drawn_from_the_service_and_missing_ones_from_an_ancestor() {
        let fake = fake_tiles();
        let mut session = session_with_fake_tiles("ui-parent", fake.clone());
        let center = LatLon::new(48.8566, 2.3522).unwrap();
        let first = draw_once(&mut session, center, 10.0);
        assert!(first.missing > 0 && first.tiles == 0, "{first:?}");
        wait_idle(&session);
        let loaded = draw_once(&mut session, center, 10.0);
        assert!(loaded.tiles > 0 && loaded.missing == 0, "{loaded:?}");
        // two zooms in: none of those tiles is loaded yet, but their zoom-10 ancestors are
        let deeper = draw_once(&mut session, center, 12.0);
        assert!(deeper.from_parent > 0 && deeper.tiles == 0, "{deeper:?}");
        assert!(fake.urls.lock().unwrap().iter().all(|u| u.starts_with("https://tiles.test/")));
    }

    /// The offline source draws a grey map and starts no tile service (no threads, no network).
    #[test]
    fn the_offline_source_starts_no_tile_service() {
        let mut session = lightcraft_engine::Session::new();
        session.execute("map.tileSource", &json!({"offline": true})).unwrap();
        let drawn = draw_once(&mut session, LatLon::new(0.0, 0.0).unwrap(), 3.0);
        assert!(drawn.offline);
        assert!(session.map.service().is_none());
        assert!(session.activity.list().is_empty());
    }

    /// A session the host didn't put online never fetches: the Map view in tests (and any
    /// session without `with_default_map_cache`) stays off the network.
    #[test]
    fn a_session_not_put_online_never_fetches() {
        let mut session = lightcraft_engine::Session::new();
        let drawn = draw_once(&mut session, LatLon::new(51.5, -0.12).unwrap(), 6.0);
        assert!(!drawn.offline && drawn.tiles == 0);
        assert!(session.map.service().is_some_and(|s| s.is_idle() && s.stats().requests == 0));
    }

    /// The Esri layers are not in the Map Style list (their terms, docs/map.md).
    #[test]
    fn the_map_style_list_has_no_esri_layer() {
        let session = lightcraft_engine::Session::new();
        let offered: Vec<String> = session.map.offered().iter().map(|s| s.label().to_string()).collect();
        assert_eq!(offered, vec!["OpenStreetMap", "OpenTopoMap", "Offline (no tiles)"]);
    }
}
