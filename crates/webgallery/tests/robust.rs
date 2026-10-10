//! Never-crash (P6.2): gallery settings (agents, saved galleries in `web.json`) and photo
//! metadata are untrusted; a damaged one is an error or a sanitized value, never a panic, and
//! metadata never escapes into the generated HTML as markup.

use std::collections::BTreeMap;

use dac_webgallery::settings::{GallerySettings, Server, color_rgb, normalize_color};
use dac_webgallery::site::{GalleryPhoto, generate};
use dac_webgallery::tokens::expand;

const EVIL: &str = "<script>alert(1)</script>";

fn photos(fields: &[(&str, &str)], n: usize, aspect: f32) -> Vec<GalleryPhoto> {
    let f: BTreeMap<String, String> = fields.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    (0..n).map(|_| GalleryPhoto { fields: f.clone(), aspect }).collect()
}

#[test]
fn hostile_gallery_settings_never_panic() {
    let seed = serde_json::to_string(&GallerySettings::default()).unwrap();
    // a web.json as the engine keeps it: saved galleries and SFTP servers
    let store = format!(
        r#"{{"galleries":[{{"name":"g","settings":{seed},"ids":[1,2]}}],"servers":[{{"name":"s","host":"h","port":22,"user":"u","path":"/www","keyFile":"","knownFingerprint":"SHA256:x"}}]}}"#
    );
    let ph = photos(&[("title", "T"), ("caption", EVIL), ("filename", "a.jpg")], 3, 1.5);
    dac_fuzzkit::run_json("webgallery.settings", &[&seed, &store], 1500, |s| {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(s) else { return };
        if let Some(srv) = v.pointer("/servers/0") {
            let _ = serde_json::from_value::<Server>(srv.clone());
        }
        let v = v.pointer("/galleries/0/settings").cloned().unwrap_or(v);
        let st = GallerySettings::from_json(&v).or_else(|_| GallerySettings::default().merged(&v));
        if let Ok(st) = st {
            let _ = generate(&st.sanitized(), &ph);
        }
    });
}

#[test]
fn hostile_metadata_never_panics_or_injects_markup() {
    let st = GallerySettings::default();
    let seeds = [EVIL, "{title} – {caption}", "\"onload=\"x", "&#x3C;b&#x3E;", "\u{0}\u{202e}"];
    let mut rng = dac_fuzzkit::Rng::new(9);
    dac_fuzzkit::run_str("webgallery.metadata", &seeds, 1500, |s| {
        let aspect = [1.5, 0.0, -1.0, f32::NAN, f32::INFINITY, 1e-30][rng.below(6)];
        let ph = photos(&[("title", s), ("caption", s), ("filename", s), (s, s)], 1 + rng.below(3), aspect);
        let _ = expand(s, &ph[0].fields);
        let _ = (normalize_color(s), color_rgb(s));
        if let Ok(site) = generate(&st, &ph) {
            for f in &site.files {
                if f.path.ends_with(".html") {
                    assert!(!String::from_utf8_lossy(&f.bytes).contains(EVIL), "{} carries raw markup", f.path);
                }
            }
        }
    });
}
