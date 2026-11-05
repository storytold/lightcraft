use super::json;

/// Apply a dialog's action (also used by `ui.dialog.confirm`).
/// What [`fmt_gap`] writes, read back: `90`, `90 s`, `1 min 30 s`, `2 h 5 min`, `1 day`
/// (units `s`, `min`, `h`, `day`, each optional after a plain number of seconds).
pub fn parse_gap(text: &str) -> Option<f64> {
    // the one translated text `fmt_gap` writes ("1 Tag")
    if text.trim() == crate::i18n::tr("1 day") {
        return Some(86400.0);
    }
    let mut total = 0.0;
    let mut seen = false;
    let mut words = text.split_whitespace().peekable();
    while let Some(word) = words.next() {
        // "90s" as well as "90 s"
        let split = word.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(word.len());
        let (num, unit) = word.split_at(split);
        let n: f64 = num.parse().ok().filter(|n: &f64| n.is_finite())?;
        let unit = if unit.is_empty() {
            match words.peek() {
                Some(u) if u.parse::<f64>().is_err() => words.next().unwrap_or_default(),
                _ => "s",
            }
        } else {
            unit
        };
        let scale = match unit.trim_end_matches('.').to_lowercase().as_str() {
            "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
            "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3600.0,
            "d" | "day" | "days" => 86400.0,
            _ => return None,
        };
        total += n * scale;
        seen = true;
    }
    seen.then_some(total)
}

/// The longest burst gap `photo.analyze` accepts: a day.
pub const MAX_BURST_GAP: f32 = 86_400.0;

/// Keep what was typed in the Assisted Culling value fields within what `photo.analyze` accepts
/// (a gap of 0 s to a day, a similarity of 0 to 1), not within the sliders' drag range.
pub fn clamp_cull_burst(gap: &mut f32, similarity: &mut f32) {
    *gap = if gap.is_finite() { gap.clamp(0.0, MAX_BURST_GAP) } else { 10.0 };
    *similarity = if similarity.is_finite() { similarity.clamp(0.0, 1.0) } else { 0.93 };
}

/// The `photo.analyze` parameters of the Assisted Culling dialog, its values as they are.
pub fn cull_params(reject_below: f32, pick_best: bool, burst_gap: f32, similarity: f32) -> serde_json::Value {
    let mut p = json!({"pickBest": pick_best, "burstGap": burst_gap, "similarity": similarity});
    if reject_below > 0.0 {
        p["rejectBelow"] = json!(reject_below);
    }
    p
}

/// `90` → `1 min 30 s`.
pub fn fmt_gap(v: f64) -> String {
    let v = v.round() as u64;
    match v {
        0..60 => format!("{v} s"),
        60..3600 if v.is_multiple_of(60) => format!("{} min", v / 60),
        60..3600 => format!("{} min {} s", v / 60, v % 60),
        3600..86400 if v.is_multiple_of(3600) => format!("{} h", v / 3600),
        3600..86400 if v.is_multiple_of(60) => format!("{} h {} min", v / 3600, v % 3600 / 60),
        // the seconds too, so editing the text reads back the same gap
        3600..86400 => format!("{} h {} min {} s", v / 3600, v % 3600 / 60, v % 60),
        _ => crate::i18n::tr("1 day").into(),
    }
}

#[cfg(test)]
mod gap_tests {
    use super::{fmt_gap, parse_gap};

    #[test]
    fn gap_text_reads_back_what_it_shows() {
        for v in [0.0, 1.0, 12.0, 59.0, 60.0, 90.0, 600.0, 3599.0, 3600.0, 3601.0, 3661.0, 3900.0, 7325.0, 86340.0, 86399.0] {
            assert_eq!(parse_gap(&fmt_gap(v)), Some(v), "{}", fmt_gap(v));
        }
        assert_eq!(parse_gap("1 day"), Some(86400.0));
        assert_eq!(parse_gap("45"), Some(45.0));
        assert_eq!(parse_gap(" 45s "), Some(45.0));
        assert_eq!(parse_gap("2min 5s"), Some(125.0));
        for bad in ["", "abc", "5 parsecs", "NaN", "1 min x"] {
            assert_eq!(parse_gap(bad), None, "{bad:?}");
        }
        // a day reads back in the UI language too ("1 Tag")
        crate::i18n::set_language(crate::i18n::Locale::De);
        let shown = fmt_gap(86400.0);
        let back = parse_gap(&shown);
        crate::i18n::set_language(crate::i18n::Locale::En);
        assert_eq!((shown.as_str(), back), ("1 Tag", Some(86400.0)));
    }

    #[test]
    fn gaps_past_the_slider_read_back_too() {
        // the Assisted Culling slider drags to 10 min; its value field takes up to a day
        for (v, text) in [(1800.0, "30 min"), (5400.0, "1 h 30 min"), (3601.0, "1 h 0 min 1 s"), (86400.0, "1 day")] {
            assert_eq!(fmt_gap(v), text);
            assert_eq!(parse_gap(text), Some(v), "{text}");
        }
        assert_eq!(parse_gap("1800"), Some(1800.0));
        assert_eq!(parse_gap("0"), Some(0.0));
    }
}

#[cfg(test)]
mod cull_dialog_tests {
    use std::time::Duration;

    use serde_json::json;

    use super::{clamp_cull_burst, cull_params};
    use crate::headless::Headless;
    use crate::state::Dialog;
    use crate::{LightcraftApp, Services};

    const T: Duration = Duration::from_secs(20);

    #[test]
    fn typed_values_are_kept_within_what_photo_analyze_accepts() {
        for (gap, sim) in [(0.0, 0.0), (1800.0, 1.0), (86400.0, 0.5), (0.5, 0.995)] {
            let (mut g, mut s) = (gap, sim);
            clamp_cull_burst(&mut g, &mut s);
            assert_eq!((g, s), (gap, sim), "in range: kept as typed");
        }
        for ((gap, sim), want) in [((-5.0, -0.1), (0.0, 0.0)), ((90000.0, 1.5), (86400.0, 1.0)), ((f32::NAN, f32::INFINITY), (10.0, 0.93))] {
            let (mut g, mut s) = (gap, sim);
            clamp_cull_burst(&mut g, &mut s);
            assert_eq!((g, s), want);
        }
        assert_eq!(cull_params(0.0, true, 1800.0, 1.0), json!({"pickBest": true, "burstGap": 1800.0, "similarity": 1.0}));
        assert_eq!(cull_params(20.0, false, 0.0, 0.7)["rejectBelow"], 20.0);
    }

    /// Click the value field right of a slider registered as `id`, type `text`, Enter.
    fn type_value(h: &mut Headless, id: &str, text: &str) {
        let r = h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("{id} not on screen"));
        let spacing = h.view.ctx.global_style().spacing.clone();
        let x = r.left() + spacing.slider_width + spacing.item_spacing.x + 6.0;
        assert_eq!(h.request("ui.click", json!({"x": x, "y": r.center().y}), T)["ok"], true);
        h.step();
        for key in ["a", "Backspace"] {
            assert_eq!(h.request("ui.key", json!({"key": key, "cmd": key == "a"}), T)["ok"], true);
        }
        assert_eq!(h.request("ui.text", json!({"text": text}), T)["ok"], true);
        assert_eq!(h.request("ui.key", json!({"key": "Enter"}), T)["ok"], true);
        h.step();
        h.step();
    }

    fn burst(h: &Headless) -> (f32, f32) {
        match &h.app.ui.dialog {
            Some(Dialog::Cull { burst_gap, similarity, .. }) => (*burst_gap, *similarity),
            d => panic!("{d:?}"),
        }
    }

    #[test]
    fn the_culling_value_fields_take_values_past_the_sliders() {
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
        let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
        h.step();
        h.app.run("dialog.cull", json!({})).unwrap();
        for _ in 0..4 {
            h.step();
        }
        assert_eq!(burst(&h), (10.0, 0.93));
        // half an hour and "only identical shots": past the sliders' 10 min and 0.99
        type_value(&mut h, "field:cullGap", "30 min");
        type_value(&mut h, "field:cullSimilar", "1.0");
        assert_eq!(burst(&h), (1800.0, 1.0));
        // a day is as far as photo.analyze goes; more is held there
        type_value(&mut h, "field:cullGap", "2 days");
        assert_eq!(burst(&h).0, 86400.0);
        type_value(&mut h, "field:cullGap", "0");
        assert_eq!(burst(&h), (0.0, 1.0));
        type_value(&mut h, "field:cullGap", "1800");
        assert_eq!(burst(&h).0, 1800.0);
        // a similarity between the slider's hundredths is kept to the thousandth
        type_value(&mut h, "field:cullSimilar", "0.995");
        assert_eq!(burst(&h).1, 0.995);
        type_value(&mut h, "field:cullSimilar", "0.9951");
        assert_eq!(burst(&h).1, 0.995);
        type_value(&mut h, "field:cullSimilar", "1.0");
        assert_eq!(burst(&h).1, 1.0);
        // OK hands them to photo.analyze as typed (it refuses values out of its range)
        let r = h.request("ui.dialog.confirm", json!({}), T);
        assert_eq!(r["ok"], true, "{r}");
        assert!(h.app.ui.dialog.is_none(), "{:?}", h.app.ui.dialog);
        assert_eq!(h.app.ui.cull_burst, Some((1800.0, 1.0)));
        // and the next opening starts from them, not from the sliders' range
        h.app.run("dialog.cull", json!({})).unwrap();
        h.step();
        h.step();
        assert_eq!(burst(&h), (1800.0, 1.0));
    }
}
