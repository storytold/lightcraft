/// Coordinates typed by a person: "51.5072, -0.1276", "51.5072 N 0.1276 W",
/// "51°30'26\"N 0°7'39\"W" (degrees, minutes, seconds; N/S/E/W or signs).
pub(crate) fn parse_gps(t: &str) -> Option<(f64, f64)> {
    // typographic minus signs and dashes mean a negative value, like ASCII '-'
    let t = t.replace(['\u{2212}', '\u{2012}', '\u{2013}', '\u{FE63}', '\u{FF0D}'], "-");
    let t = t.trim();
    // split into the two coordinates: at a comma, else after the first N/S hemisphere letter
    let (a, b) = match t.split_once(',') {
        Some((a, b)) => (a.trim().to_string(), b.trim().to_string()),
        None => {
            let lead = t.starts_with(['N', 'S', 'n', 's']);
            let i = if lead {
                // hemisphere letter first: "N 51.5 W 0.1" splits at the E/W letter
                t.char_indices().skip(1).find(|(_, c)| matches!(c, 'E' | 'W' | 'e' | 'w')).map(|(i, _)| i)
            } else {
                t.find(['N', 'S', 'n', 's']).map(|i| i + 1)
            }
            .or_else(|| t.find(char::is_whitespace))?;
            (t[..i].trim().to_string(), t[i..].trim().to_string())
        }
    };
    let one = |s: &str, pos: char, neg: char| -> Option<f64> {
        let up = s.to_ascii_uppercase();
        let sign = if up.contains(neg) || up.trim_start().starts_with('-') { -1.0 } else { 1.0 };
        let nums: Vec<f64> =
            up.split(|c: char| !(c.is_ascii_digit() || c == '.')).filter(|x| !x.is_empty()).map(|x| x.parse::<f64>().ok()).collect::<Option<_>>()?;
        let v = match nums.as_slice() {
            [d] => *d,
            [d, m] => d + m / 60.0,
            [d, m, s] => d + m / 60.0 + s / 3600.0,
            _ => return None,
        };
        let _ = pos;
        Some(sign * v)
    };
    let (la, lo) = (one(&a, 'N', 'S')?, one(&b, 'E', 'W')?);
    (la.abs() <= 90.0 && lo.abs() <= 180.0).then_some((la, lo))
}

#[cfg(test)]
mod gps_tests {
    #[test]
    fn coordinates_people_type() {
        let p = super::parse_gps;
        assert_eq!(p("51.5072, -0.1276"), Some((51.5072, -0.1276)));
        let (la, lo) = p("51°30'26\"N 0°7'39\"W").unwrap();
        assert!((la - 51.50722).abs() < 1e-4 && (lo + 0.1275).abs() < 1e-4, "{la} {lo}");
        let (la, lo) = p("33.8688 S, 151.2093 E").unwrap();
        assert!((la + 33.8688).abs() < 1e-9 && (lo - 151.2093).abs() < 1e-9);
        assert_eq!(p("51.5 N 0.1 W"), Some((51.5, -0.1)));
        assert_eq!(p("N 51.5072 W 0.1276"), Some((51.5072, -0.1276)));
        assert_eq!(p("S 33.8688 E 151.2093"), Some((-33.8688, 151.2093)));
        assert_eq!(p("\u{2212}33.8688, 151.2093"), Some((-33.8688, 151.2093)));
        assert_eq!(p("95, 10"), None);
        assert_eq!(p("hello"), None);
    }
}
