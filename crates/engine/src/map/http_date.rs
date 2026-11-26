//! HTTP dates (RFC 9110 §5.6.7) for tile freshness and `Retry-After`: `Sun, 06 Nov 1994 08:49:37 GMT`
//! (IMF-fixdate, what servers send), and the obsolete RFC 850 (`Sunday, 06-Nov-94 08:49:37 GMT`)
//! and asctime (`Sun Nov  6 08:49:37 1994`) forms recipients must accept. Times are Unix seconds
//! (UTC). Calendar maths goes through `chrono` (AGENTS.md: time goes through chrono).

use chrono::NaiveDateTime;

/// Parse an HTTP date to Unix seconds; `None` when it isn't one.
pub fn parse(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() || s.len() > 64 {
        return None;
    }
    if let Some(rest) = s.strip_suffix(" GMT") {
        // IMF-fixdate: Sun, 06 Nov 1994 08:49:37 GMT
        if let Ok(t) = NaiveDateTime::parse_from_str(rest, "%a, %d %b %Y %H:%M:%S") {
            return Some(t.and_utc().timestamp());
        }
        // RFC 850: Sunday, 06-Nov-94 08:49:37 GMT (two-digit years: 70–99 are 19xx, the rest 20xx)
        if let Ok(t) = NaiveDateTime::parse_from_str(rest, "%A, %d-%b-%y %H:%M:%S") {
            return Some(t.and_utc().timestamp());
        }
        return None;
    }
    // asctime: Sun Nov  6 08:49:37 1994 (the day may be space-padded)
    let parts: Vec<&str> = s.split_whitespace().collect();
    if let [wd, mon, d, t, y] = parts.as_slice() {
        let normal = format!("{wd} {mon} {d:0>2} {t} {y}");
        if let Ok(t) = NaiveDateTime::parse_from_str(&normal, "%a %b %d %H:%M:%S %Y") {
            return Some(t.and_utc().timestamp());
        }
    }
    None
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
            "a b c d e",
            &"x".repeat(1000),
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }
}
