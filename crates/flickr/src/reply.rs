//! What Flickr answers: `application/x-www-form-urlencoded` pairs from the OAuth endpoints, and
//! the REST XML envelope (`<rsp stat="ok">…</rsp>` / `<rsp stat="fail"><err code msg/></rsp>`)
//! from uploads. Answers are untrusted text: parsed without indexing, entities decoded, nothing
//! assumed about their length beyond the caller's cap.

use crate::Error;

/// `%XX` and `+` decoded (invalid escapes are kept as written).
pub fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let hex = |j: usize| bytes.get(j).and_then(|c| char::from(*c).to_digit(16));
        match b {
            b'+' => out.push(b' '),
            b'%' => match (hex(i + 1), hex(i + 2)) {
                (Some(h), Some(l)) => {
                    out.push((h * 16 + l) as u8);
                    i += 2;
                }
                _ => out.push(b),
            },
            _ => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `a=1&b=two` → `[("a", "1"), ("b", "two")]`, decoded.
pub fn form(body: &str) -> Vec<(String, String)> {
    body.trim().split('&').filter(|p| !p.is_empty()).map(|p| p.split_once('=').unwrap_or((p, ""))).map(|(k, v)| (decode(k), decode(v))).collect()
}

/// The value of `name` in a form answer.
pub fn field<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// The five XML entities and numeric references decoded.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(rest.get(..i).unwrap_or_default());
        let after = rest.get(i..).unwrap_or_default();
        let Some(end) = after.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = after.get(1..).unwrap_or_default();
            continue;
        };
        let name = after.get(1..end).unwrap_or_default();
        let c = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => name
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = after.get(end + 1..).unwrap_or_default();
            }
            None => {
                out.push('&');
                rest = after.get(1..).unwrap_or_default();
            }
        }
    }
    out.push_str(rest);
    out
}

/// The value of attribute `name` in the start tag `tag` (`<err code="98" …`).
fn attr(tag: &str, name: &str) -> Option<String> {
    let at = tag.find(&format!("{name}=\""))?;
    let start = at + name.len() + 2;
    let rest = tag.get(start..)?;
    Some(unescape(rest.get(..rest.find('"')?)?))
}

/// The text inside the first `<name>…</name>` of `xml`.
fn element(xml: &str, name: &str) -> Option<String> {
    let open = xml.find(&format!("<{name}"))?;
    let after = xml.get(open..)?;
    let body_start = after.find('>')? + 1;
    let body = after.get(body_start..)?;
    Some(unescape(body.get(..body.find(&format!("</{name}>"))?)?.trim()))
}

/// The id of the photo an upload made, or Flickr's error.
pub fn upload_answer(xml: &str) -> Result<String, Error> {
    let rsp_at = xml.find("<rsp").ok_or_else(|| Error::Protocol(snippet("not a Flickr answer", xml)))?;
    let rsp = xml.get(rsp_at..).unwrap_or_default();
    let tag = rsp.get(..rsp.find('>').unwrap_or(rsp.len())).unwrap_or_default();
    match attr(tag, "stat").as_deref() {
        Some("ok") => element(rsp, "photoid").filter(|id| !id.is_empty()).ok_or_else(|| Error::Protocol(snippet("no photo id in the answer", xml))),
        Some(_) => {
            let err = rsp.find("<err").and_then(|i| rsp.get(i..)).unwrap_or_default();
            let err = err.get(..err.find('>').unwrap_or(err.len())).unwrap_or_default();
            let code = attr(err, "code").and_then(|c| c.parse().ok()).unwrap_or(0);
            Err(Error::Flickr { code, message: attr(err, "msg").unwrap_or_else(|| "unknown error".into()) })
        }
        None => Err(Error::Protocol(snippet("an answer without a status", xml))),
    }
}

/// A REST answer (`format=json&nojsoncallback=1`): the JSON when `stat` is `ok`, else Flickr's
/// error (`{"stat":"fail","code":1,"message":"…"}`).
pub fn rest_answer(body: &[u8]) -> Result<serde_json::Value, Error> {
    let v: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| Error::Protocol(snippet("not a Flickr answer", &String::from_utf8_lossy(body))))?;
    match v["stat"].as_str() {
        Some("ok") => Ok(v),
        Some(_) => Err(Error::Flickr {
            code: number(&v["code"]).and_then(|c| u32::try_from(c).ok()).unwrap_or(0),
            message: text(&v["message"]).unwrap_or_else(|| "unknown error".into()),
        }),
        None => Err(Error::Protocol(snippet("an answer without a status", &String::from_utf8_lossy(body)))),
    }
}

/// A JSON string, or a number written as text (Flickr writes some ids either way).
pub fn text(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.trim().to_string()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A JSON count, or one written as text.
pub fn number(v: &serde_json::Value) -> Option<u64> {
    match v {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// `what: <the first 120 characters>` for an error message.
pub fn snippet(what: &str, body: &str) -> String {
    let text: String = body.chars().filter(|c| !c.is_control()).take(120).collect();
    if text.is_empty() { what.to_string() } else { format!("{what}: {text}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_form_answers() {
        let p = form(
            "fullname=Jamal%20Fanaian&oauth_token=72157626318069415-087bfc7b5816092c&oauth_token_secret=a202d1f853ec69de&user_nsid=21207597%40N07&username=jamalfanaian\n",
        );
        assert_eq!(field(&p, "fullname"), Some("Jamal Fanaian"));
        assert_eq!(field(&p, "user_nsid"), Some("21207597@N07"));
        assert_eq!(field(&p, "username"), Some("jamalfanaian"));
        assert_eq!(field(&p, "missing"), None);
        assert_eq!(decode("a+b%2"), "a b%2");
        assert_eq!(decode("%E2%9C%93%zz"), "✓%zz");
    }

    #[test]
    fn reads_an_upload_id_or_its_error() {
        let ok = "<?xml version=\"1.0\" encoding=\"utf-8\" ?>\n<rsp stat=\"ok\">\n<photoid>52345678901</photoid>\n</rsp>";
        assert_eq!(upload_answer(ok).unwrap(), "52345678901");
        let fail = "<rsp stat=\"fail\">\n\t<err code=\"98\" msg=\"Invalid auth token &amp; more\" />\n</rsp>";
        match upload_answer(fail) {
            Err(Error::Flickr { code, message }) => assert_eq!((code, message.as_str()), (98, "Invalid auth token & more")),
            other => panic!("{other:?}"),
        }
        assert!(matches!(upload_answer("<html>502 Bad Gateway</html>"), Err(Error::Protocol(_))));
        assert!(matches!(upload_answer("<rsp stat=\"ok\"></rsp>"), Err(Error::Protocol(_))));
    }

    #[test]
    fn reads_rest_answers() {
        let ok = rest_answer(br#"{"photosets":{"page":1},"stat":"ok"}"#).unwrap();
        assert_eq!(number(&ok["photosets"]["page"]), Some(1));
        match rest_answer(br#"{"stat":"fail","code":1,"message":"Photoset not found"}"#) {
            Err(Error::Flickr { code, message }) => assert_eq!((code, message.as_str()), (1, "Photoset not found")),
            other => panic!("{other:?}"),
        }
        assert!(matches!(rest_answer(b"<html>"), Err(Error::Protocol(_))));
        assert!(matches!(rest_answer(b"{}"), Err(Error::Protocol(_))));
        assert_eq!((text(&serde_json::json!(721)), number(&serde_json::json!("12"))), (Some("721".into()), Some(12)));
        assert_eq!((text(&serde_json::json!(null)), number(&serde_json::json!(-1))), (None, None));
    }

    #[test]
    fn hostile_answers_never_panic() {
        for s in [
            "",
            "<rsp",
            "<rsp stat=\"",
            "<rsp stat=\"fail\"><err code=\"x",
            "&",
            "&#xFFFFFFFF;",
            "&#;",
            "<rsp stat=\"ok\"><photoid>",
            "%",
            "%%%",
            "=&=&",
        ] {
            let _ = upload_answer(s);
            let _ = form(s);
            let _ = unescape(s);
        }
        assert_eq!(unescape("a &lt;b&gt; &#233; &#x2713; &bogus; &"), "a <b> é ✓ &bogus; &");
    }
}
