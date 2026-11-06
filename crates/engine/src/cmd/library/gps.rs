/// Coordinates typed by a person: "51.5072, -0.1276", "51.5072 N 0.1276 W",
/// "51°30'26\"N 0°7'39\"W" (degrees, minutes, seconds; N/S/E/W or signs).
pub(crate) fn parse_gps(t: &str) -> Option<(f64, f64)> {
    // typographic minus signs and dashes mean a negative value, like ASCII '-'
    let t = t.trim();
    if t.is_empty() || t.chars().take(129).count() > 128 {
        return None;
    }
    let normalized = t.replace(['\u{2212}', '\u{2012}', '\u{2013}', '\u{FE63}', '\u{FF0D}'], "-");
    let t = normalized.as_str();

    if let Some((a, b)) = t.split_once(',') {
        if b.contains(',') {
            return None;
        }
        return parse_gps_pair(a, b);
    }

    // A suffix N/S marks the end of latitude; a prefix N/S uses the next hemisphere prefix as
    // the boundary. If only longitude has a hemisphere, the whitespace pass below handles it.
    let mut parsed: Option<(f64, f64)> = None;
    let mut latitude_markers = t.char_indices().filter(|(_, c)| matches!(c, 'N' | 'S' | 'n' | 's'));
    if let Some((first, marker)) = latitude_markers.next() {
        if first == 0 {
            if let Some((second, _)) = t.char_indices().skip(1).find(|(_, c)| is_gps_hemisphere(*c))
                && let Some(pair) = parse_gps_pair(t.get(..second)?, t.get(second..)?)
            {
                parsed = Some(pair);
            }
        } else {
            let end = first.checked_add(marker.len_utf8())?;
            return parse_gps_pair(t.get(..end)?, t.get(end..)?);
        }
    }

    // A comma-free pair can be separated by whitespace. Trying each boundary also permits spaces
    // between DMS components without letting unconsumed text vanish. If multiple boundaries parse,
    // they must agree so ambiguous text cannot be reinterpreted.
    for (at, separator) in t.char_indices().filter(|(_, c)| c.is_whitespace()) {
        let after = at.checked_add(separator.len_utf8())?;
        if let Some(pair) = parse_gps_pair(t.get(..at)?, t.get(after..)?) {
            match parsed {
                Some(previous) if previous != pair => return None,
                None => parsed = Some(pair),
                Some(_) => {}
            }
        }
    }
    parsed
}

#[derive(Clone, Copy)]
enum GpsAxis {
    Latitude,
    Longitude,
}

fn is_gps_hemisphere(c: char) -> bool {
    matches!(c, 'N' | 'S' | 'E' | 'W' | 'n' | 's' | 'e' | 'w')
}

fn parse_gps_pair(latitude: &str, longitude: &str) -> Option<(f64, f64)> {
    Some((parse_gps_coordinate(latitude, GpsAxis::Latitude)?, parse_gps_coordinate(longitude, GpsAxis::Longitude)?))
}

fn parse_gps_coordinate(text: &str, axis: GpsAxis) -> Option<f64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    let prefix = ['N', 'S', 'E', 'W']
        .into_iter()
        .find_map(|h| text.strip_prefix(h).or_else(|| text.strip_prefix(h.to_ascii_lowercase())).map(|rest| (h, rest)));
    let suffix = ['N', 'S', 'E', 'W']
        .into_iter()
        .find_map(|h| text.strip_suffix(h).or_else(|| text.strip_suffix(h.to_ascii_lowercase())).map(|rest| (rest, h)));
    let (body, hemisphere) = if let Some((h, body)) = prefix {
        (body, Some(h))
    } else if let Some((body, h)) = suffix {
        (body, Some(h))
    } else {
        (text, None)
    };
    if body.chars().any(is_gps_hemisphere) {
        return None;
    }
    let body = body.trim();
    if body.is_empty() {
        return None;
    }

    let hemisphere_negative = match (axis, hemisphere) {
        (_, None) => None,
        (GpsAxis::Latitude, Some('N')) | (GpsAxis::Longitude, Some('E')) => Some(false),
        (GpsAxis::Latitude, Some('S')) | (GpsAxis::Longitude, Some('W')) => Some(true),
        _ => return None,
    };

    let mut components = Vec::with_capacity(3);
    let mut token = String::new();
    let mut explicit_sign = None;
    let mut degree_mark = false;
    let mut minute_mark = false;
    let mut second_mark = false;

    for c in body.chars() {
        match c {
            '0'..='9' | '.' => token.push(c),
            '+' | '-' if components.is_empty() && token.is_empty() => token.push(c),
            c if c.is_whitespace() => flush_gps_number(&mut token, &mut components, &mut explicit_sign)?,
            '°' => {
                flush_gps_number(&mut token, &mut components, &mut explicit_sign)?;
                if components.len() != 1 || degree_mark || minute_mark || second_mark {
                    return None;
                }
                degree_mark = true;
            }
            '\'' | '′' => {
                flush_gps_number(&mut token, &mut components, &mut explicit_sign)?;
                if components.len() != 2 || minute_mark || second_mark {
                    return None;
                }
                minute_mark = true;
            }
            '"' | '″' => {
                flush_gps_number(&mut token, &mut components, &mut explicit_sign)?;
                if components.len() != 3 || !minute_mark || second_mark {
                    return None;
                }
                second_mark = true;
            }
            _ => return None,
        }
    }
    flush_gps_number(&mut token, &mut components, &mut explicit_sign)?;

    let [degrees, rest @ ..] = components.as_slice() else { return None };
    if rest.iter().any(|v| *v < 0.0 || *v >= 60.0) {
        return None;
    }
    let magnitude = degrees.abs() + rest.first().copied().unwrap_or(0.0) / 60.0 + rest.get(1).copied().unwrap_or(0.0) / 3600.0;
    if !magnitude.is_finite() {
        return None;
    }

    let negative = match (hemisphere_negative, explicit_sign) {
        (Some(hemisphere_negative), Some(explicit_negative)) if hemisphere_negative != explicit_negative => return None,
        (Some(hemisphere_negative), _) => hemisphere_negative,
        (None, Some(explicit_negative)) => explicit_negative,
        (None, None) => false,
    };
    let value = if negative { -magnitude } else { magnitude };
    let limit = match axis {
        GpsAxis::Latitude => 90.0,
        GpsAxis::Longitude => 180.0,
    };
    (value.is_finite() && value.abs() <= limit).then_some(value)
}

fn flush_gps_number(token: &mut String, components: &mut Vec<f64>, explicit_sign: &mut Option<bool>) -> Option<()> {
    if token.is_empty() {
        return Some(());
    }
    if components.len() == 3 {
        return None;
    }
    let value = token.parse::<f64>().ok()?;
    if !value.is_finite() {
        return None;
    }
    if components.is_empty() && (token.starts_with('-') || token.starts_with('+')) {
        *explicit_sign = Some(token.starts_with('-'));
    }
    components.push(value);
    token.clear();
    Some(())
}

#[cfg(test)]
mod gps_tests {
    use serde_json::json;

    #[test]
    fn coordinates_people_type() {
        let p = super::parse_gps;
        assert_eq!(p("51.5072, -0.1276"), Some((51.5072, -0.1276)));
        assert_eq!(p("10 20 30"), None);
        assert_eq!(p("10  20"), Some((10.0, 20.0)));
        let (la, lo) = p("51°30'26\"N 0°7'39\"W").unwrap();
        assert!((la - 51.50722).abs() < 1e-4 && (lo + 0.1275).abs() < 1e-4, "{la} {lo}");
        let (la, lo) = p("33.8688 S, 151.2093 E").unwrap();
        assert!((la + 33.8688).abs() < 1e-9 && (lo - 151.2093).abs() < 1e-9);
        assert_eq!(p("51.5 N 0.1 W"), Some((51.5, -0.1)));
        assert_eq!(p("N 51.5072 W 0.1276"), Some((51.5072, -0.1276)));
        assert_eq!(p("S 33.8688 E 151.2093"), Some((-33.8688, 151.2093)));
        assert_eq!(p("\u{2212}33.8688, 151.2093"), Some((-33.8688, 151.2093)));
        assert_eq!(p("51.5072 -0.1276"), Some((51.5072, -0.1276)));
        assert_eq!(p("51.5072 0.1276 W"), Some((51.5072, -0.1276)));
        let (la, lo) = p("51° 30' 26\" 0° 7' 39\"").unwrap();
        assert!((la - 51.50722).abs() < 1e-4 && (lo - 0.1275).abs() < 1e-4, "{la} {lo}");
        assert_eq!(p("95, 10"), None);
        assert_eq!(p("hello"), None);
        assert_eq!(p("s 33.8688 e 151.2093"), Some((-33.8688, 151.2093)));
        assert_eq!(p("0N, 0E"), Some((0.0, 0.0)));
        assert_eq!(p("90N, 180E"), Some((90.0, 180.0)));
        assert_eq!(p("90S, 180W"), Some((-90.0, -180.0)));
    }

    #[test]
    fn gps_does_not_reinterpret_exponents() {
        assert_eq!(super::parse_gps("1e-3, 2e-3"), None);
    }

    #[test]
    fn gps_rejects_wrong_axis_or_conflicting_hemispheres() {
        let p = super::parse_gps;
        assert_eq!(p("45E, 7N"), None);
        assert_eq!(p("45E 7N"), None);
        assert_eq!(p("45NS, 7E"), None);
        assert_eq!(p("-45N, 7E"), None);
        assert_eq!(p("45N 7NE"), None);
    }

    #[test]
    fn gps_tries_whitespace_split_when_prefix_marker_split_fails() {
        assert_eq!(super::parse_gps("N51 0E"), Some((51.0, 0.0)));
    }

    #[test]
    fn gps_rejects_unconsumed_coordinate_text() {
        assert_eq!(super::parse_gps("garbage45, 7junk"), None);
    }

    #[test]
    fn gps_rejects_oversized_coordinate_text() {
        let input = format!("{}, 0", "0".repeat(129));
        assert_eq!(super::parse_gps(&input), None);
    }

    #[test]
    fn gps_rejects_invalid_dms_components() {
        let p = super::parse_gps;
        assert_eq!(p("45°60'0\"N, 7E"), None);
        assert_eq!(p("45°0'60\"N, 7E"), None);
        assert_eq!(p("45°-1'0\"N, 7E"), None);
        assert_eq!(p("45°0'-1\"N, 7E"), None);
    }

    #[test]
    fn gps_rejected_text_does_not_change_metadata() {
        let mut s = crate::Session::with_demo();
        let ids: Vec<_> = s.catalog.photos().take(2).map(|photo| photo.id).collect();
        assert_eq!(ids.len(), 2, "demo has two records for the batch check");
        let id_values: Vec<_> = ids.iter().map(|id| id.0).collect();
        s.execute("library.select", &json!({"ids": id_values})).unwrap();
        s.execute("photo.setMeta", &json!({"title": "Before"})).unwrap();
        s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.25})).unwrap();
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(!s.redo.is_empty(), "rejection must preserve a populated redo history");

        let snapshot = s.catalog.to_snapshot();
        let revision = s.catalog.revision;
        let selection = s.selection.clone();
        let commits = s.commits();
        let undo = format!("{:?}", s.undo);
        let redo = format!("{:?}", s.redo);
        let error = s.execute("photo.setMeta", &json!({"gps": "garbage45, 7junk", "title": "After"})).unwrap_err();

        assert!(matches!(error, crate::EngineError::BadParams { ref cmd, .. } if cmd == "photo.setMeta"), "{error}");
        assert_eq!(s.catalog.to_snapshot(), snapshot, "catalog data is unchanged");
        assert_eq!(s.catalog.revision, revision, "catalog revision is unchanged");
        assert_eq!(s.selection, selection, "selection is unchanged");
        assert_eq!(s.commits(), commits, "commit count is unchanged");
        assert_eq!(format!("{:?}", s.undo), undo, "undo history is unchanged");
        assert_eq!(format!("{:?}", s.redo), redo, "redo history is unchanged");
        for id in ids {
            assert_eq!(s.catalog.photo(id).map(|photo| photo.meta.title.as_str()), Some("Before"));
        }
    }

    #[test]
    fn gps_valid_edit_is_one_undo_step() {
        let mut s = crate::Session::with_demo();
        let id = s.active().or_else(|| s.catalog.photos().next().map(|photo| photo.id)).unwrap();
        let previous = s.catalog.photo(id).unwrap().meta.clone();
        let commits = s.commits();
        s.execute("photo.setMeta", &json!({"gps": "48°51'30\"N 2°17'40\"E", "title": "GPS edited"})).unwrap();
        let photo = s.catalog.photo(id).unwrap();
        assert_eq!(photo.meta.title, "GPS edited");
        assert!(photo.meta.gps.is_some());
        assert_eq!(s.commits(), commits + 1);

        s.execute("edit.undo", &json!({})).unwrap();
        let photo = s.catalog.photo(id).unwrap();
        assert_eq!(photo.meta.title, previous.title);
        assert_eq!(photo.meta.gps, previous.gps);
    }
}
