//! The identity plate's graphic: an SVG (rasterised with resvg, pure Rust) or a PNG / JPEG file,
//! fitted to the module bar's height. Loaded once per path and kept as a texture; a file that
//! can't be read or decoded is an error the command reports, never a panic.

/// The largest plate file read (a logo is kilobytes; this bounds a hostile choice).
const MAX_FILE: u64 = 8 << 20;
/// The plate's pixel height before display scaling (the bar is 44 px; 2× for sharp HiDPI).
pub const PIXEL_HEIGHT: u32 = 64;
/// The widest plate, in pixels (a banner logo is clipped to this).
const MAX_WIDTH: u32 = 1024;

/// Decode `path` into premultiplied RGBA pixels `PIXEL_HEIGHT` tall (or smaller), keeping its aspect.
pub fn load(path: &str) -> Result<egui::ColorImage, String> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = path;
        Err("an identity plate image needs the desktop app".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let len = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?.len();
        if len > MAX_FILE {
            return Err(format!("{path} is too large for an identity plate ({len} bytes)"));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        let svg = std::path::Path::new(path).extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("svg"));
        if svg { rasterise_svg(&bytes) } else { decode_raster(&bytes) }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn rasterise_svg(bytes: &[u8]) -> Result<egui::ColorImage, String> {
    let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default()).map_err(|e| format!("not a usable SVG: {e}"))?;
    let size = tree.size();
    let (w, h) = (size.width(), size.height());
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return Err("the SVG has no size".into());
    }
    let scale = PIXEL_HEIGHT as f32 / h;
    let pw = ((w * scale).round() as u32).clamp(1, MAX_WIDTH);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pw, PIXEL_HEIGHT).ok_or("can't allocate the plate")?;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    Ok(egui::ColorImage::from_rgba_premultiplied([pw as usize, PIXEL_HEIGHT as usize], pixmap.data()))
}

#[cfg(not(target_arch = "wasm32"))]
fn decode_raster(bytes: &[u8]) -> Result<egui::ColorImage, String> {
    // fitted into a box twice the plate's height wide-ish: a logo is short and wide
    let t = lcc_codecs::decode_thumbnail(bytes, MAX_WIDTH).map_err(|e| format!("not a usable image: {e}"))?;
    let img = t.image;
    if img.width == 0 || img.height == 0 || img.data.len() != img.width.saturating_mul(img.height) {
        return Err("the image is empty".into());
    }
    let flat: Vec<u8> = img.data.iter().flatten().copied().collect();
    let full = egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &flat);
    // scale to the plate's height (nearest: cheap, once per choice)
    let h = (PIXEL_HEIGHT as usize).min(img.height);
    let w = (img.width.saturating_mul(h) / img.height).clamp(1, MAX_WIDTH as usize);
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        let sy = (y * img.height / h).min(img.height - 1);
        for x in 0..w {
            let sx = (x * img.width / w).min(img.width - 1);
            pixels.push(full.pixels.get(sy * img.width + sx).copied().unwrap_or_default());
        }
    }
    Ok(egui::ColorImage { size: [w, h], source_size: egui::vec2(w as f32, h as f32), pixels })
}

/// The plate's texture for `path`, loaded on first use and cached in egui's memory by path.
/// `None` when it can't be loaded (the command that chose it already said why).
pub fn texture(ctx: &egui::Context, path: &str) -> Option<egui::TextureHandle> {
    let key = egui::Id::new(("identity-plate-image", path.to_string()));
    if let Some(cached) = ctx.data(|d| d.get_temp::<Option<egui::TextureHandle>>(key)) {
        return cached;
    }
    let tex = load(path).ok().map(|img| ctx.load_texture("identity-plate", img, egui::TextureOptions::LINEAR));
    ctx.data_mut(|d| d.insert_temp(key, tex.clone()));
    tex
}

/// The open Identity Plate Setup dialog (Edit ▸ Identity Plate Setup…). Its edits apply to the
/// module bar as they are made; Cancel puts back the plate it opened with.
#[derive(Clone, Debug, PartialEq)]
pub struct Setup {
    /// The plate when the dialog opened.
    pub before: Box<crate::module::IdentityPlate>,
    /// "Personalized" is chosen (else the app's own plate).
    pub personalized: bool,
    /// "Use a graphical identity plate" is chosen (possibly before any file is).
    pub graphic: bool,
}

/// `dialog.identityPlateSetup`: `{}` opens the dialog; `{"personalized": bool}` picks the app's
/// plate (`false` resets it) or a personalised one; `{"close": "ok" | "cancel"}` closes it, Cancel
/// restoring the plate it opened with.
pub fn setup_command(app: &mut crate::LccApp, p: &serde_json::Value) -> Result<serde_json::Value, String> {
    use serde_json::{Value, json};
    let close = p.get("close").map(|v| v.as_str().ok_or("dialog.identityPlateSetup: close is \"ok\" or \"cancel\"")).transpose()?;
    let personalized = p.get("personalized").map(|v| v.as_bool().ok_or("dialog.identityPlateSetup: personalized is true or false")).transpose()?;
    if close.is_none() && personalized.is_none() {
        let plate = &mut app.ui.identity_plate;
        if plate.setup.is_none() {
            let before = crate::module::IdentityPlate { setup: None, ..plate.clone() };
            let personalized = !before.is_default();
            let graphic = before.graphic().is_some();
            plate.setup = Some(Setup { before: Box::new(before), personalized, graphic });
        }
    }
    if let Some(on) = personalized {
        let setup = app.ui.identity_plate.setup.as_mut().ok_or("dialog.identityPlateSetup: the dialog isn't open")?;
        setup.personalized = on;
        if !on {
            app.run("view.identityPlate", json!({"reset": true}))?;
        }
    }
    match close {
        None => {}
        Some("ok") => app.ui.identity_plate.setup = None,
        Some("cancel") => {
            if let Some(setup) = app.ui.identity_plate.setup.take() {
                app.ui.identity_plate = *setup.before;
            }
        }
        Some(c) => return Err(format!("dialog.identityPlateSetup: close `{c}` is \"ok\" or \"cancel\"")),
    }
    let plate = &app.ui.identity_plate;
    Ok(json!({"open": plate.setup.is_some(), "personalized": plate.setup.as_ref().map(|s| Value::Bool(s.personalized)), "plate": plate}))
}

/// Draws the Identity Plate Setup dialog while it is open.
pub fn show_setup(app: &mut crate::LccApp, ctx: &egui::Context) {
    use serde_json::{Value, json};
    let plate = app.ui.identity_plate.clone();
    let Some(setup) = plate.setup.clone() else { return };
    let t = crate::theme::Tokens::get(ctx);
    let mut actions: Vec<(&str, Value)> = Vec::new();
    let window_id = egui::Id::new("identity-plate-setup");
    let mut open = true;
    let shown = egui::Window::new(crate::i18n::tr("Identity Plate Editor"))
        .id(window_id)
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(560.0)
        .max_height(crate::dialog_frame::max_window_height(ctx))
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            let mut layout = crate::dialog_frame::ModalLayout::begin(ui, window_id);
            ui.horizontal(|ui| {
                ui.label(crate::i18n::tr("Identity Plate:"));
                let current = if setup.personalized { crate::i18n::tr("Personalized").to_string() } else { lcc_brand::DISPLAY_NAME.to_string() };
                let r = egui::ComboBox::from_id_salt("identityPlateKind").selected_text(current).width(180.0).show_ui(ui, |ui| {
                    if ui.selectable_label(!setup.personalized, lcc_brand::DISPLAY_NAME).clicked() {
                        actions.push(("dialog.identityPlateSetup", json!({"personalized": false})));
                    }
                    if ui.selectable_label(setup.personalized, crate::i18n::tr("Personalized")).clicked() {
                        actions.push(("dialog.identityPlateSetup", json!({"personalized": true})));
                    }
                });
                crate::widgets::register(ui.ctx(), "combo:identityPlateKind", r.response.rect);
            });
            let mut status = plate.show_status;
            let r = ui.checkbox(&mut status, crate::i18n::tr("Show Status and Activity"));
            crate::widgets::register(ui.ctx(), "check:identityPlateStatus", r.rect);
            if r.changed() {
                actions.push(("view.identityPlate", json!({"showStatus": status})));
            }
            ui.add_space(6.0);
            layout.body(ui, |ui| {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if !setup.personalized {
                        ui.add_space(30.0);
                        ui.vertical_centered(|ui| {
                            ui.label(egui::RichText::new(crate::i18n::tr("No options for the {app} identity plate.")).italics())
                        });
                        ui.add_space(30.0);
                        return;
                    }
                    ui.label(crate::i18n::tr("Personalize {app} with your own branding elements."));
                    let graphic = setup.graphic;
                    ui.horizontal(|ui| {
                        let r = ui.radio(!graphic, crate::i18n::tr("Use a styled text identity plate"));
                        crate::widgets::register(ui.ctx(), "radio:identityPlateText", r.rect);
                        if r.clicked() {
                            actions.push(("view.identityPlate", json!({"kind": "text"})));
                        }
                        let r = ui.radio(graphic, crate::i18n::tr("Use a graphical identity plate"));
                        crate::widgets::register(ui.ctx(), "radio:identityPlateGraphic", r.rect);
                        if r.clicked() {
                            actions.push(("view.identityPlate", json!({"kind": "graphic"})));
                        }
                    });
                    // the preview: the plate on the module bar's colour
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 64.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 2.0, t.chrome);
                    let shown = if graphic && plate.graphic().is_none() {
                        None
                    } else {
                        Some(crate::module::paint_plate(ui, &plate, egui::pos2(rect.left() + 14.0, rect.center().y), rect.width() - 28.0))
                    };
                    if shown.is_none() {
                        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, crate::i18n::tr("No image chosen"), t.font(13.0), t.text_dim);
                    }
                    crate::widgets::register(ui.ctx(), "region:identityPlatePreview", rect);
                    if graphic {
                        ui.horizontal(|ui| {
                            let r = ui.button(crate::i18n::tr("Choose File…"));
                            crate::widgets::register(ui.ctx(), "button:identityPlateLocate", r.rect);
                            if r.clicked() {
                                actions.push(("dialog.identityPlateImage", json!({})));
                            }
                            let r = ui.add_enabled(!plate.image.is_empty(), egui::Button::new(crate::i18n::tr("Clear Image")));
                            crate::widgets::register(ui.ctx(), "button:identityPlateClear", r.rect);
                            if r.clicked() {
                                actions.push(("view.identityPlate", json!({"image": ""})));
                            }
                        });
                        ui.label(egui::RichText::new(crate::i18n::tr("An SVG, PNG or JPEG file; it is shown 32 points tall.")).weak());
                        return;
                    }
                    ui.horizontal(|ui| {
                        ui.label(crate::i18n::tr("Text"));
                        let mut text = plate.text.clone();
                        let r = crate::text_field::TextField::singleline("field:identityPlateText", &mut text)
                            .hint(lcc_brand::DISPLAY_NAME)
                            .width(260.0)
                            .show(ui);
                        if r.response.changed() {
                            actions.push(("view.identityPlate", json!({"text": text})));
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(crate::i18n::tr("Size"));
                        let mut size = plate.size;
                        let r = ui.add(egui::Slider::new(&mut size, 9.0..=32.0).step_by(1.0).suffix(" pt"));
                        crate::widgets::register(ui.ctx(), "slider:identityPlateSize", r.rect);
                        if r.changed() {
                            actions.push(("view.identityPlate", json!({"size": size})));
                        }
                    });
                    ui.horizontal(|ui| {
                        let mut bold = plate.bold;
                        let r = ui.checkbox(&mut bold, crate::i18n::tr("Bold"));
                        crate::widgets::register(ui.ctx(), "check:identityPlateBold", r.rect);
                        if r.changed() {
                            actions.push(("view.identityPlate", json!({"bold": bold})));
                        }
                        let mut mark = plate.mark;
                        let r = ui.checkbox(&mut mark, crate::i18n::tr("Show Brand Mark"));
                        crate::widgets::register(ui.ctx(), "check:identityPlateMark", r.rect);
                        if r.changed() {
                            actions.push(("view.identityPlate", json!({"mark": mark})));
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(crate::i18n::tr("Color"));
                        let mut col = plate.color.unwrap_or([t.text.r(), t.text.g(), t.text.b()]);
                        let r = ui.color_edit_button_srgb(&mut col);
                        crate::widgets::register(ui.ctx(), "color:identityPlateColor", r.rect);
                        if r.changed() {
                            actions.push(("view.identityPlate", json!({"color": format!("#{:02x}{:02x}{:02x}", col[0], col[1], col[2])})));
                        }
                        let r = ui.add_enabled(plate.color.is_some(), egui::Button::new(crate::i18n::tr("Theme Color")));
                        crate::widgets::register(ui.ctx(), "button:identityPlateThemeColor", r.rect);
                        if r.clicked() {
                            actions.push(("view.identityPlate", json!({"color": null})));
                        }
                    });
                });
            });
            layout.footer(ui, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let r = ui.button(crate::i18n::tr("Cancel"));
                    crate::widgets::register(ui.ctx(), "button:identityPlateCancel", r.rect);
                    if r.clicked() {
                        actions.push(("dialog.identityPlateSetup", json!({"close": "cancel"})));
                    }
                    let r = ui.button(egui::RichText::new(crate::i18n::tr("OK")).strong());
                    crate::widgets::register(ui.ctx(), "button:identityPlateOk", r.rect);
                    if r.clicked() {
                        actions.push(("dialog.identityPlateSetup", json!({"close": "ok"})));
                    }
                });
            });
        });
    crate::dialog_frame::ModalLayout::finish(ctx, window_id, shown.as_ref().map(|w| w.response.rect));
    if !open {
        actions.push(("dialog.identityPlateSetup", json!({"close": "cancel"})));
    }
    for (cmd, params) in actions {
        if let Err(e) = app.run(cmd, params) {
            app.ui.status = e;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_svg_rasterises_to_the_plate_height_keeping_its_aspect() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="50"><rect width="200" height="50" fill="red"/></svg>"#;
        let img = rasterise_svg(svg).unwrap();
        assert_eq!(img.size, [256, PIXEL_HEIGHT as usize]);
        assert!(img.pixels.iter().any(|p| p.r() > 200));
    }

    #[test]
    fn bad_files_are_errors_not_panics() {
        assert!(rasterise_svg(b"<svg").is_err());
        assert!(rasterise_svg(br#"<svg xmlns="http://www.w3.org/2000/svg" width="0" height="0"/>"#).is_err());
        assert!(decode_raster(b"\x89PNG garbage").is_err());
        assert!(load("/no/such/plate.svg").is_err());
    }
}
