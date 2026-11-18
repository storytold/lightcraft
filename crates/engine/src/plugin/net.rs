//! HTTPS for plug-in catalogs and GitHub releases, over the client LightCraft downloads its models
//! with (`lightcraft-fetch`: pure-Rust TLS, connect and stall timeouts, a cancel flag).
//! craft-plugin's own `http` feature implements this for another copy of craft-update, so
//! LightCraft wraps its own client instead.
//!
//! Every hop of a redirect is checked before it is followed (`https://` on GitHub's hosts,
//! [`craft_plugin::catalog::check_redirect`]), and a body is refused as soon as it passes the
//! size asked for.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use craft_plugin::catalog::check_redirect;
use craft_plugin::fetch::{Fetch, MAX_REDIRECTS};
use lightcraft_fetch::http::{self, HttpError, Limits, Url};

/// How long connecting, and then a silence in the middle of a body, may take.
const CONNECT: Duration = Duration::from_secs(20);
const STALL: Duration = Duration::from_secs(30);

/// `lightcraft-fetch` as craft-plugin's [`Fetch`]; stops once `cancel` is set (the activity
/// stack's ✕).
pub(super) struct NetFetch {
    cancel: Arc<AtomicBool>,
}

impl NetFetch {
    pub(super) fn new(cancel: Arc<AtomicBool>) -> NetFetch {
        NetFetch { cancel }
    }
}

/// `url` written out again (scheme, host, port when it isn't the scheme's, path and query).
fn url_text(u: &Url) -> String {
    let scheme = if u.tls { "https" } else { "http" };
    let default = if u.tls { 443 } else { 80 };
    if u.port == default { format!("{scheme}://{}{}", u.host, u.path) } else { format!("{scheme}://{}:{}{}", u.host, u.port, u.path) }
}

fn text(e: HttpError) -> String {
    e.to_string()
}

impl Fetch for NetFetch {
    fn get(&mut self, url: &str, max: usize) -> Result<Option<Vec<u8>>, String> {
        let limits = Limits { connect: CONNECT, stall: STALL, cancel: &self.cancel };
        // (`url` itself was checked by craft-plugin: GitHub's hosts, or its API)
        let mut at = Url::parse(url).map_err(text)?;
        if !at.tls {
            return Err(format!("{url} isn't https://"));
        }
        for _ in 0..=MAX_REDIRECTS {
            let mut headers = Vec::new();
            if at.host.eq_ignore_ascii_case("api.github.com") {
                headers.push(("Accept", "application/vnd.github+json".to_string()));
            }
            let mut response = http::get(&at, &headers, &limits).map_err(text)?;
            match response.status {
                200 => {
                    if response.content_length().is_some_and(|n| n > max as u64) {
                        return Err(format!("{} is larger than {} KB", url_text(&at), max / 1024));
                    }
                    let mut body = Vec::new();
                    let mut buf = vec![0u8; 64 * 1024];
                    loop {
                        let n = response.read(&mut buf, &limits).map_err(text)?;
                        if n == 0 {
                            return Ok(Some(body));
                        }
                        if body.len().saturating_add(n) > max {
                            return Err(format!("{} is larger than {} KB", url_text(&at), max / 1024));
                        }
                        body.extend_from_slice(buf.get(..n).unwrap_or_default());
                    }
                }
                404 => return Ok(None),
                301 | 302 | 303 | 307 | 308 => {
                    let location = response.header("location").ok_or_else(|| format!("{} redirected nowhere", url_text(&at)))?.to_string();
                    let next = at.join(&location).map_err(text)?;
                    // the API's own redirects (a renamed repository) stay on the API
                    let api = |u: &Url| u.tls && u.port == 443 && u.host.eq_ignore_ascii_case("api.github.com");
                    if !(api(&at) && api(&next)) {
                        check_redirect(&url_text(&next))?;
                    }
                    at = next;
                }
                403 | 429 if at.host.eq_ignore_ascii_case("api.github.com") => {
                    return Err("GitHub's API refused the request (its limit for this hour may be used up): try again later".into());
                }
                status => return Err(format!("{} answered HTTP {status}", url_text(&at))),
            }
        }
        Err(format!("{url} redirected more than {MAX_REDIRECTS} times"))
    }
}
