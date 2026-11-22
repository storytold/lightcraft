//! `<cli> calibrate-chart --shot RAW --corners "x,y x,y x,y x,y" [--light L] [--shot …]… [--chart 2014|2005]
//! [--dcp OUT.dcp] [--preview DIR] [--report FILE.json] [--contributor NAME]`: measure a camera's colour matrices
//! from raw shots of a ColorChecker Classic (`lcc_colourfit::chart`). Prints the camera database entry (TOML, for
//! `crates/camdb/data/cameras/<maker>.toml`) and the per-patch colour error before and after.

use std::path::PathBuf;

use lcc_colourfit::chart::{self, Edition, Light, Shot};

pub fn run(args: &[String]) -> Result<(), String> {
    let mut shots: Vec<Shot> = Vec::new();
    let (mut dcp, mut preview, mut report, mut contributor) = (None, None, None, None);
    let mut edition = Edition::default();
    let mut i = 0;
    let take = |i: &mut usize, flag: &str| -> Result<String, String> {
        *i += 1;
        args.get(*i).cloned().ok_or_else(|| format!("{flag} needs a value"))
    };
    while i < args.len() {
        match args.get(i).map_or("", String::as_str) {
            "--shot" => shots.push(Shot { path: PathBuf::from(take(&mut i, "--shot")?), light: default_light(), corners: [[f64::NAN; 2]; 4] }),
            "--corners" => {
                let v = take(&mut i, "--corners")?;
                let c = chart::parse_corners(&v)
                    .ok_or_else(|| format!("--corners: `{v}` is not four x,y pairs (fractions of the photo's width and height)"))?;
                shots.last_mut().ok_or("--corners goes after its --shot")?.corners = c;
            }
            "--light" => {
                let v = take(&mut i, "--light")?;
                let l = Light::parse(&v)
                    .ok_or_else(|| format!("--light: `{v}` is not daylight, d65, d55, d50, shade, tungsten or a temperature in kelvin"))?;
                shots.last_mut().ok_or("--light goes after its --shot")?.light = l;
            }
            "--chart" => {
                let v = take(&mut i, "--chart")?;
                edition = Edition::parse(&v).ok_or_else(|| format!("--chart: `{v}` is not 2014 (made after November 2014) or 2005 (before)"))?;
            }
            "--dcp" => dcp = Some(PathBuf::from(take(&mut i, "--dcp")?)),
            "--preview" => preview = Some(PathBuf::from(take(&mut i, "--preview")?)),
            "--report" => report = Some(PathBuf::from(take(&mut i, "--report")?)),
            "--contributor" => contributor = Some(take(&mut i, "--contributor")?),
            a => return Err(format!("unknown argument `{a}`")),
        }
        i += 1;
    }
    if shots.is_empty() {
        return Err("calibrate-chart needs at least one --shot RAW --corners \"x,y x,y x,y x,y\"".into());
    }
    if let Some(s) = shots.iter().find(|s| s.corners.iter().flatten().any(|v| v.is_nan())) {
        return Err(format!("{}: give its --corners (the centres of the chart's four corner patches)", s.path.display()));
    }
    let (entry, rep, sampled) = chart::calibrate(&shots, edition, contributor.as_deref()).map_err(|e| e.to_string())?;
    if let Some(dir) = &preview {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for (shot, d) in shots.iter().zip(&sampled) {
            let (w, h, px) = chart::preview(d);
            let (Ok(w), Ok(h)) = (u32::try_from(w), u32::try_from(h)) else { continue };
            let img = lcc_codecs::EncodeImage::new(w, h, 3, lcc_codecs::Samples::U8(&px));
            let png = lcc_codecs::encode_png(&img, &lcc_codecs::EncodeMeta::default()).map_err(|e| e.to_string())?;
            let path = chart::preview_path(dir, shot);
            std::fs::write(&path, png).map_err(|e| format!("{}: {e}", path.display()))?;
            eprintln!("calibrate-chart: preview {}", path.display());
        }
    }
    if let Some(path) = &dcp {
        let model = lcc_engine::colour_export::profile_model(Some(&entry.make), &entry.model);
        let profile = lcc_raw::dcp::Dcp {
            model: model.clone(),
            name: format!("{} chart {model}", lcc_brand::DISPLAY_NAME),
            copyright: entry.provenance.contributor.clone().unwrap_or_default(),
            color: entry.color_data(),
        };
        std::fs::write(path, lcc_raw::dcp::write(&profile)?).map_err(|e| format!("{}: {e}", path.display()))?;
        eprintln!("calibrate-chart: wrote {} (camera profile for {model})", path.display());
    }
    if let Some(path) = &report {
        let text = serde_json::to_string_pretty(&rep).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    eprint!("{}", chart::text(&rep));
    let toml = lcc_camdb::to_toml(std::slice::from_ref(&entry)).map_err(|e| e.to_string())?;
    print!("{toml}");
    eprintln!(
        "\ncalibrate-chart: the entry above goes into crates/camdb/data/cameras/<maker>.toml (or try it at once: the .dcp, via Camera ▸ Import DCP)"
    );
    Ok(())
}

fn default_light() -> Light {
    // daylight (D55) when the shot doesn't say
    Light { xy: [0.3324, 0.3474], kelvin: 5503.0, code: 20 }
}
