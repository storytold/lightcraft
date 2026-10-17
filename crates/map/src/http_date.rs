//! HTTP dates (RFC 9110 §5.6.7): `Sun, 06 Nov 1994 08:49:37 GMT` (IMF-fixdate, what servers send),
//! and the obsolete RFC 850 (`Sunday, 06-Nov-94 08:49:37 GMT`) and asctime
//! (`Sun Nov  6 08:49:37 1994`) forms recipients must accept. Times are Unix seconds (UTC).

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `(year, month 1..=12, day)` of days since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn month(s: &str) -> Option<i64> {
    let s = s.to_ascii_lowercase();
    MONTHS.iter().position(|m| *m == s).map(|i| i as i64 + 1)
}

fn time(s: &str) -> Option<i64> {
    let mut it = s.split(':').map(|p| p.parse::<i64>().ok().filter(|_| p.len() == 2));
    let (h, m, sec) = (it.next()??, it.next()??, it.next()??);
    if it.next().is_some() || h > 23 || m > 59 || sec > 60 {
        return None;
    }
    Some(h * 3600 + m * 60 + sec)
}

fn at(y: i64, m: i64, d: i64, t: i64) -> Option<i64> {
    if !(1..=31).contains(&d) || !(1..=9999).contains(&y) {
        return None;
    }
    let days = days_from_civil(y, m, d);
    // reject 31 February and the like: the day must survive the round trip
    (civil_from_days(days) == (y, m, d)).then_some(days * 86_400 + t)
}

/// Parse an HTTP date to Unix seconds; `None` when it isn't one.
pub fn parse(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() > 64 {
        return None;
    }
    let parts: Vec<&str> = s.split_whitespace().collect();
    match parts.as_slice() {
        // IMF-fixdate: Sun, 06 Nov 1994 08:49:37 GMT
        [_, d, mon, y, t, "GMT"] => at(y.parse().ok()?, month(mon)?, d.parse().ok()?, time(t)?),
        // RFC 850: Sunday, 06-Nov-94 08:49:37 GMT
        [_, dmy, t, "GMT"] => {
            let mut it = dmy.split('-');
            let (d, mon, yy) = (it.next()?, it.next()?, it.next()?);
            let yy: i64 = yy.parse().ok().filter(|_| yy.len() == 2)?;
            // two-digit years: the RFC says to read ones more than 50 years ahead as the past
            let y = if yy < 70 { 2000 + yy } else { 1900 + yy };
            at(y, month(mon)?, d.parse().ok()?, time(t)?)
        }
        // asctime: Sun Nov  6 08:49:37 1994
        [_, mon, d, t, y] => at(y.parse().ok()?, month(mon)?, d.parse().ok()?, time(t)?),
        _ => None,
    }
}

/// Format Unix seconds as an IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`).
pub fn format(secs: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    let (days, t) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    let wd = DAYS.get(days.rem_euclid(7) as usize).copied().unwrap_or("Thu");
    let mon = MONTHS.get((m - 1).clamp(0, 11) as usize).copied().unwrap_or("jan");
    let mut mon = mon.to_string();
    if let Some(c) = mon.get_mut(..1) {
        c.make_ascii_uppercase();
    }
    format!("{wd}, {d:02} {mon} {y:04} {:02}:{:02}:{:02} GMT", t / 3600, t / 60 % 60, t % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_three_forms() {
        let want = 784_111_777; // 1994-11-06T08:49:37Z
        assert_eq!(parse("Sun, 06 Nov 1994 08:49:37 GMT"), Some(want));
        assert_eq!(parse("Sunday, 06-Nov-94 08:49:37 GMT"), Some(want));
        assert_eq!(parse("Sun Nov  6 08:49:37 1994"), Some(want));
        assert_eq!(parse("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(parse("Wed, 21 Oct 2026 07:28:00 GMT"), Some(1_792_567_680));
    }

    #[test]
    fn rejects_junk_without_panicking() {
        for bad in [
            "",
            "0",
            "-1",
            "tomorrow",
            "Sun, 31 Feb 2026 00:00:00 GMT",
            "Sun, 06 Nov 1994 25:00:00 GMT",
            "Sun, 06 Foo 1994 08:49:37 GMT",
            "Sun, 06 Nov 1994 08:49:37 PST",
            "Sun, 06 Nov 99999999999999999999 08:49:37 GMT",
            "Sun, 06 Nov 1994 8:9:7 GMT",
            &"x".repeat(1000),
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn formats_and_round_trips() {
        assert_eq!(format(784_111_777), "Sun, 06 Nov 1994 08:49:37 GMT");
        for t in [0, 951_782_400, 1_792_567_680, 4_102_444_799] {
            assert_eq!(parse(&format(t)), Some(t), "{t}");
        }
    }
}
