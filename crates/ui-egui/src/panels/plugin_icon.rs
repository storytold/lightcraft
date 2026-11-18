//! The icon the Plug-in Manager draws for a plug-in: a 5×5 pixel grid, mirrored left to right, in
//! a colour, all from a hash of the plug-in's id (like GitHub's identicons). The same id gives the
//! same icon on every machine and every start (the hash has no random seed), and an update keeps
//! its icon (the id is hashed, not the module). Drawn in code: original work.
//!
//! The manager draws its marks (enabled, didn't load) beside the icon, never inside it, so no icon
//! can pass for one.

use egui::{Color32, Rect, Response, Sense, Ui, vec2};

/// Cells per side.
pub const CELLS: usize = 5;

/// An icon: which cells are filled (`cells[row][column]`) and their hue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identicon {
    pub cells: [[bool; CELLS]; CELLS],
    /// Degrees, 0–359.
    pub hue: u16,
}

/// FNV-1a (64-bit): stable across builds and platforms, unlike std's hasher.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

/// Spreads the hash's bits (FNV's low bits follow the last bytes closely).
fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// The icon for plug-in `id` (any string: empty, huge or odd ids give an icon too).
pub fn identicon(id: &str) -> Identicon {
    let h = mix(fnv1a(id.as_bytes()));
    let mut cells = [[false; CELLS]; CELLS];
    let half = CELLS.div_ceil(2);
    for (r, row) in cells.iter_mut().enumerate() {
        for c in 0..half {
            let on = (h >> (r * half + c)) & 1 == 1;
            if let Some(x) = row.get_mut(c) {
                *x = on;
            }
            if let Some(x) = row.get_mut(CELLS - 1 - c) {
                *x = on;
            }
        }
    }
    // never a blank tile: the centre cell at least
    if !cells.iter().flatten().any(|c| *c)
        && let Some(x) = cells.get_mut(CELLS / 2).and_then(|r| r.get_mut(CELLS / 2))
    {
        *x = true;
    }
    Identicon { cells, hue: ((h >> 32) % 360) as u16 }
}

/// HSL (hue in degrees, saturation and lightness 0–1) to a colour.
fn hsl(hue: f32, s: f32, l: f32) -> Color32 {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h = (hue.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let byte = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgb(byte(r), byte(g), byte(b))
}

/// Paints `icon` into `rect`: a tinted tile with the filled cells in the icon's colour.
pub fn paint(ui: &Ui, rect: Rect, icon: &Identicon) {
    let hue = f32::from(icon.hue);
    let (fg, bg) = if ui.visuals().dark_mode { (hsl(hue, 0.55, 0.62), hsl(hue, 0.25, 0.22)) } else { (hsl(hue, 0.6, 0.45), hsl(hue, 0.35, 0.9)) };
    let p = ui.painter();
    p.rect_filled(rect, rect.width() * 0.18, bg);
    let inner = rect.shrink(rect.width() * 0.15);
    let cell = inner.width() / CELLS as f32;
    for (r, row) in icon.cells.iter().enumerate() {
        for (c, on) in row.iter().enumerate() {
            if *on {
                let min = inner.min + vec2(c as f32 * cell, r as f32 * cell);
                p.rect_filled(Rect::from_min_size(min, vec2(cell, cell)), 0.0, fg);
            }
        }
    }
}

/// What the manager keeps of a plug-in's own icon: the module it came from and its texture
/// (`None`: that module has none, or it can't be drawn).
type Own = (String, Option<egui::TextureHandle>);

fn own_key(id: &str) -> egui::Id {
    egui::Id::new(("plugin-own-icon", id.to_string()))
}

/// The texture of plug-in `id`'s own icon (a pixel filter's manifest `icon`), once [`keep_custom`]
/// stored it; `None` for the identicon.
pub fn custom(ctx: &egui::Context, id: &str) -> Option<egui::TextureHandle> {
    ctx.data(|d| d.get_temp::<Own>(own_key(id))).and_then(|(_, tex)| tex)
}

/// Whether the icon stored for plug-in `id` came from module `module_hash` (an update brings a
/// new module, and maybe a new icon).
pub fn custom_is_current(ctx: &egui::Context, id: &str, module_hash: &str) -> bool {
    ctx.data(|d| d.get_temp::<Own>(own_key(id))).is_some_and(|(h, _)| h == module_hash)
}

/// Stores plug-in `id`'s own icon from module `module_hash` (`None`: it has none, or it can't be
/// drawn).
pub fn keep_custom(ctx: &egui::Context, id: &str, module_hash: &str, texture: Option<egui::TextureHandle>) {
    ctx.data_mut(|d| d.insert_temp::<Own>(own_key(id), (module_hash.to_string(), texture)));
}

/// Paints plug-in `id`'s icon into `rect`: its own, else its identicon.
pub fn paint_for(ui: &Ui, rect: Rect, id: &str) {
    match custom(ui.ctx(), id) {
        Some(tex) => {
            // fitted into the square with its aspect kept (a wide picture sits centred)
            let size = tex.size_vec2();
            let scale = (rect.width() / size.x.max(1.0)).min(rect.height() / size.y.max(1.0));
            let fitted = Rect::from_center_size(rect.center(), size * scale);
            let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            ui.painter().image(tex.id(), fitted, uv, Color32::WHITE);
        }
        None => paint(ui, rect, &identicon(id)),
    }
}

/// Allocates a `size`-point square and paints plug-in `id`'s icon in it.
pub fn show(ui: &mut Ui, id: &str, size: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    paint_for(ui, rect, id);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hash has no seed: this id's icon is the same on every run and every machine.
    #[test]
    fn an_id_gives_the_icon_it_always_had() {
        let on = |row: &str| -> [bool; CELLS] { std::array::from_fn(|c| row.as_bytes()[c] == b'#') };
        let want = Identicon { cells: ["#####", "##.##", ".....", ".###.", "..#.."].map(on), hue: 116 };
        assert_eq!(identicon("io.github.nwh212.chromaneg"), want);
    }

    #[test]
    fn different_ids_give_different_icons() {
        let ids: Vec<String> = (0..40)
            .map(|i| format!("io.github.someone.plugin{i}"))
            .chain(["io.github.nwh212.chromaneg", "io.github.clydecbrown.pureraw", "film-lab.convert", "film-lab.advanced"].map(String::from))
            .collect();
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert_ne!(identicon(a), identicon(b), "{a} and {b} look the same");
            }
        }
    }

    #[test]
    fn icons_are_mirrored_and_never_blank_whatever_the_id() {
        // (hostile ids too: very long, control characters, bidi overrides, astral characters)
        let long = "x".repeat(10_000);
        for id in ["a", "b", "", "io.github.nwh212.chromaneg", long.as_str(), "\u{0}\u{7}\n\t\u{202e}☃️漢字\u{10ffff}", "\u{0}\u{0}\u{0}"] {
            let icon = identicon(id);
            for row in icon.cells {
                for c in 0..CELLS {
                    assert_eq!(row[c], row[CELLS - 1 - c], "{id:?}: mirrored left to right");
                }
            }
            assert!(icon.cells.iter().flatten().any(|c| *c), "{id:?}: something to see");
            assert!(icon.hue < 360);
        }
    }
}
