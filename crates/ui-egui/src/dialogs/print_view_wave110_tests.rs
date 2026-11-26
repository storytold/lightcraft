//! Production settings controls exercised with real pointer and keyboard input, without previews or file I/O.
use super::*;
use crate::headless::HeadlessView;

struct Settings {
    app: LightcraftApp,
    view: HeadlessView,
    time: f64,
}

impl Settings {
    fn new(params: Value) -> Self {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
        app.session.execute("print.set", &params).unwrap();
        let view = HeadlessView::new();
        app.install_framework(&view.ctx);
        let mut this = Self { app, view, time: 0.0 };
        this.frame(vec![]);
        this.frame(vec![]);
        this
    }

    fn frame(&mut self, events: Vec<egui::Event>) {
        self.time += 1.0 / 60.0;
        crate::widgets::take_registry(&self.view.ctx);
        let raw = HeadlessView::raw_input(vec2(1000.0, 2400.0), 1.0, self.time, events);
        self.view.run(raw, |ui| {
            ui.set_width(SETTINGS_W - 14.0);
            let st = self.app.session.print.clone();
            let t = Tokens::get(ui.ctx());
            let mut pu = std::mem::take(&mut self.app.ui.print);
            settings(&mut self.app, ui, &t, &mut pu, &st, 0);
            self.app.ui.print = pu;
        });
    }

    fn rect(&self, id: &str) -> Rect {
        self.view
            .ctx
            .data(|d| d.get_temp::<crate::widgets::Registry>(egui::Id::unique("lc-registry")))
            .unwrap()
            .rects
            .into_iter()
            .find(|(name, _)| name == id)
            .unwrap_or_else(|| panic!("missing {id}"))
            .1
    }

    fn click(&mut self, id: &str) {
        let pos = self.rect(id).center();
        self.frame(vec![egui::Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            self.frame(vec![egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
        }
        self.frame(vec![]);
        self.frame(vec![]);
    }

    fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
        self.frame(vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }]);
        self.frame(vec![egui::Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers }]);
    }

    fn edit(&mut self, id: &str, text: &str) {
        self.click(id);
        self.key(egui::Key::A, egui::Modifiers { ctrl: true, command: true, ..Default::default() });
        self.frame(vec![egui::Event::Text(text.into())]);
        self.key(egui::Key::Enter, Default::default());
        self.frame(vec![]);
    }
}

#[test]
fn sdk_print_wave110_checks_and_integer_grid_reach_engine() {
    let mut h = Settings::new(json!({"layout": {"style": "contactSheet"}}));
    let rows = h.app.session.print.layout.grid.rows;
    let zoom = h.app.session.print.image.zoom_to_fill;
    h.click("check:printZoomFill");
    assert_eq!(h.app.session.print.image.zoom_to_fill, !zoom);
    h.edit("drag:printColumns", "7");
    assert_eq!(h.app.session.print.layout.grid.columns, 7);
    assert_eq!(h.app.session.print.layout.grid.rows, rows, "editing columns preserves rows");
    assert_eq!(h.app.session.catalog.len(), 0);
    assert!(h.app.export.is_none(), "settings cannot start a print");
}

#[test]
fn sdk_print_wave110_typed_metric_length_and_disabled_color_are_preserved() {
    let mut h = Settings::new(json!({"guides": {"units": "mm"}, "layout": {"page": {"margins": {"left": 0.25}}}}));
    h.edit("drag:printMarginLeft", "25.4");
    assert!((h.app.session.print.layout.page.margins.left - 1.0).abs() < 1e-8, "millimeters convert to stored inches");
    let bpc = h.app.session.print.job.color.black_point_compensation;
    h.click("check:printBpc");
    assert_eq!(h.app.session.print.job.color.black_point_compensation, bpc, "perceptual intent disables BPC");
    h.click("button:printIntent-1");
    h.click("check:printBpc");
    assert_eq!(h.app.session.print.job.color.black_point_compensation, !bpc);
    h.edit("drag:printPpi", "144");
    assert_eq!(h.app.session.print.job.ppi, 144.0);
    assert_eq!(h.app.session.print.layout.page.margins.left, 1.0, "editing resolution preserves layout");
    assert!(h.app.export.is_none());
}
