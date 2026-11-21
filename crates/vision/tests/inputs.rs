//! The images and texts the golden test and `tools/siglip_reference.py` share. The images are
//! procedural (`lightcraft-scenes` plus simple patterns), regenerated identically on both sides
//! (the Python script reads them as PNGs written by `examples/siglip_inputs.rs`).

#![allow(dead_code)]

use lightcraft_scenes::{Kind, demo_library};
use lightcraft_vision::Rgb8;

fn scene(kind: Kind, w: usize, h: usize) -> Rgb8 {
    let s = demo_library().into_iter().find(|s| s.kind == kind).unwrap();
    let rgba = s.render(w, h).to_srgb8();
    Rgb8 { width: rgba.width, height: rgba.height, data: rgba.data.iter().map(|p| [p[0], p[1], p[2]]).collect() }
}

/// `(name, image)`: different sizes and shapes exercise the resize (down, up, unchanged).
pub fn images() -> Vec<(&'static str, Rgb8)> {
    vec![
        ("beach", scene(Kind::Beach, 384, 256)),
        ("forest", scene(Kind::MistyForest, 300, 200)),
        ("flower", scene(Kind::Macro, 200, 300)),
        (
            "red_square",
            Rgb8::from_fn(256, 256, |x, y| if (64..192).contains(&x) && (64..192).contains(&y) { [220, 20, 30] } else { [128, 128, 128] }),
        ),
        ("gradient", Rgb8::from_fn(64, 48, |x, y| [(x * 4) as u8, (y * 5) as u8, 128])),
    ]
}

/// Texts embedded and scored against every image (already lower-case, as the model expects).
pub const TEXTS: &[&str] = &["a beach", "a forest", "a red car", "a scorpion", "a flower", "une plage au coucher du soleil", "海滩上的日落"];

/// Tokenizer-only cases (raw text, no lower-casing): spaces, accents, byte fallback, added
/// tokens, emoji, scripts, truncation.
pub fn token_cases() -> Vec<String> {
    let mut v: Vec<String> = [
        "a photo of a cat",
        "A Red  Car",
        " leading and trailing ",
        "café crème brûlée",
        "tab\tand\nnewline\n\nparagraph",
        "<eos> inside <unused3> text <table>",
        "🦂 scorpion 🦂🦂",
        "Скорпион в пустыне",
        "サソリ と 砂漠",
        "\u{10FFFD}\u{E000} private use",
        "e\u{301} combining",
        "x",
        "",
        "    ",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    v.push("kids on a beach at sunset ".repeat(20));
    v
}
