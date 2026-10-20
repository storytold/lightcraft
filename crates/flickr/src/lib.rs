//! Uploading photos to Flickr: sign-in with OAuth 1.0a and signed multipart uploads, over the pure
//! Rust HTTPS client of `lightcraft-fetch`.
//!
//! **Sign-in** is Flickr's out-of-band flow, for a desktop app with no web server of its own:
//! 1. [`Client::request_token`] gets a temporary token;
//! 2. the user opens [`Client::authorize_url`] in a browser, approves LightCraft (write access:
//!    it uploads), and Flickr shows a code like `123-456-789`;
//! 3. [`Client::access_token`] trades the temporary token and that code for a lasting [`Token`],
//!    which the host keeps ([`account`]).
//!
//! **Uploads** ([`Client::upload`]) post the encoded file with its title, description, tags and
//! privacy to `up.flickr.com`; transient failures (a dropped connection, Flickr busy) are retried
//! a few times. Every call checks a cancel flag and gives up on a stalled server, so the calling
//! thread never hangs.
//!
//! Every app that talks to Flickr needs an API key and its secret (the "consumer"), from
//! <https://www.flickr.com/services/apps/create/noncommercial/>. An open-source app can't ship a secret, so the
//! host supplies it (see the engine's `flickr` module): the user's own, or one built in.
//!
//! Native only: on wasm32 this crate is empty.

#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod account;
pub mod oauth;
pub mod reply;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lightcraft_fetch::http::{self, HttpError, Limits, Url};
use serde::{Deserialize, Serialize};

/// Where Flickr can't be reached or refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The connection failed or stalled, or the request was cancelled.
    Http(HttpError),
    /// An HTTP error status, with what the server said.
    Status(u16, String),
    /// Flickr's own error (`<err code msg/>`).
    Flickr { code: u32, message: String },
    /// An answer that could not be understood.
    Protocol(String),
}

impl Error {
    /// Safe to send again: Flickr certainly didn't act on the request. Either it never reached
    /// Flickr (no connection), or Flickr answered that it didn't take it and to try later (HTTP
    /// 429 or 503, Flickr's 105 "service unavailable" and 106 "write failed"). Anything else
    /// that goes wrong once the request is on its way may have happened after Flickr accepted
    /// it ([`Error::outcome_unknown`]), and sending it again could upload a photo twice.
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Http(HttpError::Connect(_)) => true,
            Error::Status(s, _) => *s == 429 || *s == 503,
            Error::Flickr { code, .. } => matches!(code, 105 | 106),
            _ => false,
        }
    }

    /// The request went out but its answer was lost (the connection dropped or stalled, or a
    /// gateway gave up waiting): Flickr may or may not have acted on it.
    pub fn outcome_unknown(&self) -> bool {
        match self {
            Error::Http(HttpError::Stalled | HttpError::Io(_) | HttpError::Protocol(_)) => true,
            Error::Status(s, _) => *s >= 500 && *s != 503,
            _ => false,
        }
    }

    /// The account's sign-in no longer works (revoked, expired, or the API key changed).
    pub fn needs_sign_in(&self) -> bool {
        matches!(self, Error::Flickr { code: 96..=99, .. }) || matches!(self, Error::Status(401, _))
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self, Error::Http(HttpError::Cancelled))
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Http(HttpError::Cancelled) => write!(f, "cancelled"),
            Error::Http(e) => write!(f, "Flickr could not be reached: {e}"),
            Error::Status(401, body) => {
                let problem = reply::field(&reply::form(body), "oauth_problem").map(str::to_string);
                match problem.as_deref() {
                    Some("token_rejected" | "token_expired" | "token_revoked") => write!(f, "Flickr no longer accepts this sign-in: sign in again"),
                    Some("verifier_invalid") => write!(f, "Flickr did not accept the code: check it, or start the sign-in again"),
                    Some("consumer_key_unknown" | "consumer_key_rejected" | "signature_invalid") => {
                        write!(f, "Flickr did not accept the API key or its secret: check both")
                    }
                    Some(p) => write!(f, "Flickr refused the sign-in ({p})"),
                    None => write!(f, "Flickr refused the request (HTTP 401)"),
                }
            }
            Error::Status(s, body) => write!(f, "{}", reply::snippet(&format!("Flickr answered HTTP {s}"), body)),
            Error::Flickr { code, message } => {
                let hint = match code {
                    _ if message.contains("maximum number of photosets") => " (the account can't have more albums)",
                    6 => " (the account's upload limit is reached)",
                    8 => " (the file is too large for Flickr)",
                    96..=99 => " (sign in to Flickr again)",
                    100 => " (check the API key)",
                    _ => "",
                };
                write!(f, "Flickr error {code}: {message}{hint}")
            }
            Error::Protocol(e) => write!(f, "unexpected answer from Flickr: {e}"),
        }
    }
}

impl From<HttpError> for Error {
    fn from(e: HttpError) -> Self {
        Error::Http(e)
    }
}

/// The application's Flickr API key and its secret (OAuth "consumer").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApiKey {
    pub key: String,
    pub secret: String,
}

impl ApiKey {
    pub fn is_set(&self) -> bool {
        !self.key.trim().is_empty() && !self.secret.trim().is_empty()
    }
}

/// The temporary token of a sign-in in progress (step 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestToken {
    pub token: String,
    pub secret: String,
}

/// A signed-in Flickr account: the lasting token and who it belongs to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Token {
    pub token: String,
    pub secret: String,
    /// Flickr's id for the account (`12345678@N01`), used in photo page URLs.
    pub user_nsid: String,
    pub username: String,
    pub fullname: String,
}

/// Where Flickr lives (tests point it at a local server).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoints {
    /// OAuth endpoints and the website: `https://www.flickr.com`.
    pub www: String,
    /// Uploads: `https://up.flickr.com`.
    pub upload: String,
    /// The REST API (albums): `https://api.flickr.com`.
    pub api: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints { www: "https://www.flickr.com".into(), upload: "https://up.flickr.com".into(), api: "https://api.flickr.com".into() }
    }
}

impl Endpoints {
    /// Everything at one address (a local stand-in for Flickr in tests).
    pub fn all(base: &str) -> Endpoints {
        Endpoints { www: base.into(), upload: base.into(), api: base.into() }
    }
}

/// One of the account's albums (Flickr calls them photosets).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Album {
    pub id: String,
    pub title: String,
    /// Photos in it (as Flickr last counted).
    pub photos: u64,
}

/// Who may see an uploaded photo.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Privacy {
    #[default]
    Public,
    Friends,
    Family,
    FriendsAndFamily,
    Private,
}

impl Privacy {
    pub fn parse(s: &str) -> Option<Privacy> {
        Some(match s {
            "public" => Privacy::Public,
            "friends" => Privacy::Friends,
            "family" => Privacy::Family,
            "friendsAndFamily" => Privacy::FriendsAndFamily,
            "private" => Privacy::Private,
            _ => return None,
        })
    }

    /// `(is_public, is_friend, is_family)`.
    fn flags(self) -> (bool, bool, bool) {
        match self {
            Privacy::Public => (true, false, false),
            Privacy::Friends => (false, true, false),
            Privacy::Family => (false, false, true),
            Privacy::FriendsAndFamily => (false, true, true),
            Privacy::Private => (false, false, false),
        }
    }
}

/// What Flickr is told about an uploaded photo.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PhotoInfo {
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    pub privacy: Privacy,
    /// Kept out of public searches.
    pub hidden: bool,
}

/// Flickr's `tags` argument: space-separated, a tag with spaces in double quotes (which a tag
/// can't itself contain), empty and repeated tags left out.
pub fn tags_argument(tags: &[String]) -> String {
    let mut seen = std::collections::HashSet::new();
    tags.iter()
        .map(|t| t.replace('"', "").split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty() && seen.insert(t.to_lowercase()))
        .map(|t| if t.contains(' ') { format!("\"{t}\"") } else { t })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The page of an uploaded photo.
pub fn photo_url(user_nsid: &str, photo_id: &str) -> String {
    format!("https://www.flickr.com/photos/{}/{}", oauth::encode(user_nsid), oauth::encode(photo_id))
}

/// Largest answer read from Flickr's OAuth and upload endpoints (they are a few hundred bytes).
const MAX_ANSWER: usize = 64 * 1024;
/// Largest REST answer read (a page of 500 albums is a few hundred KB).
const MAX_REST_ANSWER: usize = 8 << 20;
/// Album pages read at most (500 albums each).
const MAX_ALBUM_PAGES: u64 = 20;
/// Attempts per upload when the failure is transient.
const ATTEMPTS: usize = 3;

/// Talks to Flickr for one API key.
#[derive(Clone, Debug)]
pub struct Client {
    pub key: ApiKey,
    pub endpoints: Endpoints,
    pub connect_timeout: Duration,
    /// Longest wait for any progress (sending a file or waiting for Flickr to answer).
    pub stall_timeout: Duration,
    /// Pause before the retry of a transient failure (doubled each time).
    pub retry_pause: Duration,
}

impl Client {
    pub fn new(key: ApiKey) -> Client {
        Client {
            key,
            endpoints: Endpoints::default(),
            connect_timeout: Duration::from_secs(20),
            // Flickr may take a while to answer a large upload once it has the bytes
            stall_timeout: Duration::from_secs(120),
            retry_pause: Duration::from_secs(3),
        }
    }

    fn limits<'a>(&self, cancel: &'a AtomicBool) -> Limits<'a> {
        Limits { connect: self.connect_timeout, stall: self.stall_timeout, cancel }
    }

    /// The `oauth_*` parameters of a request (without the signature), plus `extra`.
    fn params(&self, token: Option<&str>, extra: &[(&str, String)]) -> Vec<(String, String)> {
        let mut p = vec![
            ("oauth_consumer_key".to_string(), self.key.key.trim().to_string()),
            ("oauth_nonce".to_string(), oauth::unique()),
            ("oauth_signature_method".to_string(), "HMAC-SHA1".to_string()),
            ("oauth_timestamp".to_string(), oauth::timestamp()),
            ("oauth_version".to_string(), "1.0".to_string()),
        ];
        if let Some(t) = token {
            p.push(("oauth_token".to_string(), t.to_string()));
        }
        p.extend(extra.iter().map(|(k, v)| (k.to_string(), v.clone())));
        p
    }

    /// A signed `GET` to an OAuth endpoint; its form answer.
    fn oauth_get(
        &self,
        path: &str,
        token: Option<(&str, &str)>,
        extra: &[(&str, String)],
        cancel: &AtomicBool,
    ) -> Result<Vec<(String, String)>, Error> {
        if !self.key.is_set() {
            return Err(Error::Protocol("no Flickr API key".into()));
        }
        let url = format!("{}{path}", self.endpoints.www.trim_end_matches('/'));
        let mut p = self.params(token.map(|t| t.0), extra);
        let sig = oauth::sign("GET", &url, &p, self.key.secret.trim(), token.map_or("", |t| t.1)).map_err(Error::Protocol)?;
        p.push(("oauth_signature".to_string(), sig));
        let query = p.iter().map(|(k, v)| format!("{}={}", oauth::encode(k), oauth::encode(v))).collect::<Vec<_>>().join("&");
        let limits = self.limits(cancel);
        let mut r = http::get(&Url::parse(&format!("{url}?{query}"))?, &[], &limits)?;
        let body = String::from_utf8_lossy(&r.read_to_end(MAX_ANSWER, &limits)?).into_owned();
        if r.status != 200 {
            return Err(Error::Status(r.status, body));
        }
        Ok(reply::form(&body))
    }

    /// Sign-in step 1: a temporary token, to be approved at [`Client::authorize_url`].
    pub fn request_token(&self, cancel: &AtomicBool) -> Result<RequestToken, Error> {
        let a = self.oauth_get("/services/oauth/request_token", None, &[("oauth_callback", "oob".into())], cancel)?;
        let (Some(token), Some(secret)) = (reply::field(&a, "oauth_token"), reply::field(&a, "oauth_token_secret")) else {
            return Err(Error::Protocol("no request token in Flickr's answer".into()));
        };
        Ok(RequestToken { token: token.to_string(), secret: secret.to_string() })
    }

    /// Sign-in step 2: the page where the user approves LightCraft uploading to their account.
    pub fn authorize_url(&self, request: &RequestToken) -> String {
        format!("{}/services/oauth/authorize?oauth_token={}&perms=write", self.endpoints.www.trim_end_matches('/'), oauth::encode(&request.token))
    }

    /// Sign-in step 3: trade the approved temporary token and the code Flickr showed for the
    /// account's lasting token.
    pub fn access_token(&self, request: &RequestToken, code: &str, cancel: &AtomicBool) -> Result<Token, Error> {
        let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        if code.is_empty() {
            return Err(Error::Protocol("enter the code Flickr showed after you approved LightCraft".into()));
        }
        let a = self.oauth_get("/services/oauth/access_token", Some((&request.token, &request.secret)), &[("oauth_verifier", code)], cancel)?;
        let get = |k: &str| reply::field(&a, k).unwrap_or_default().to_string();
        let t = Token {
            token: get("oauth_token"),
            secret: get("oauth_token_secret"),
            user_nsid: get("user_nsid"),
            username: get("username"),
            fullname: get("fullname"),
        };
        if t.token.is_empty() || t.secret.is_empty() {
            return Err(Error::Protocol("no access token in Flickr's answer".into()));
        }
        Ok(t)
    }

    /// Upload `bytes` (an encoded image named `file_name`) to the account of `token`; the new
    /// photo's id. Only failures where Flickr certainly didn't take the photo are retried
    /// ([`Error::is_transient`]): a lost answer is reported, not retried, so a photo is never
    /// uploaded twice by a retry.
    pub fn upload(&self, token: &Token, info: &PhotoInfo, file_name: &str, bytes: &[u8], cancel: &AtomicBool) -> Result<String, Error> {
        let mut pause = self.retry_pause;
        let mut attempt = 1;
        loop {
            match self.upload_once(token, info, file_name, bytes, cancel) {
                Err(e) if e.is_transient() && attempt < ATTEMPTS => {
                    log::warn!("flickr: upload of {file_name} failed ({e}); retrying");
                    wait(pause, cancel)?;
                    pause = pause.saturating_mul(2);
                    attempt += 1;
                }
                r => return r,
            }
        }
    }

    fn upload_once(&self, token: &Token, info: &PhotoInfo, file_name: &str, bytes: &[u8], cancel: &AtomicBool) -> Result<String, Error> {
        if !self.key.is_set() {
            return Err(Error::Protocol("no Flickr API key".into()));
        }
        let url = format!("{}/services/upload/", self.endpoints.upload.trim_end_matches('/'));
        let (public, friend, family) = info.privacy.flags();
        let flag = |b: bool| if b { "1" } else { "0" }.to_string();
        let mut fields: Vec<(&str, String)> = vec![("is_public", flag(public)), ("is_friend", flag(friend)), ("is_family", flag(family))];
        fields.push(("hidden", if info.hidden { "2" } else { "1" }.to_string()));
        for (k, v) in [("title", info.title.trim()), ("description", info.description.trim())] {
            if !v.is_empty() {
                fields.push((k, v.to_string()));
            }
        }
        let tags = tags_argument(&info.tags);
        if !tags.is_empty() {
            fields.push(("tags", tags));
        }
        // every field but the photo is signed
        let mut p = self.params(Some(&token.token), &fields);
        let sig = oauth::sign("POST", &url, &p, self.key.secret.trim(), &token.secret).map_err(Error::Protocol)?;
        p.push(("oauth_signature".to_string(), sig));
        let boundary = format!("lightcraft-{}", oauth::unique());
        let body = multipart(&boundary, &p, file_name, bytes);
        let limits = self.limits(cancel);
        let mut r = http::post(&Url::parse(&url)?, &[("Content-Type", format!("multipart/form-data; boundary={boundary}"))], &body, &limits)?;
        drop(body);
        let answer = String::from_utf8_lossy(&r.read_to_end(MAX_ANSWER, &limits)?).into_owned();
        if r.status != 200 {
            return Err(Error::Status(r.status, answer));
        }
        reply::upload_answer(&answer)
    }
}

impl Client {
    /// A signed call of REST method `method` (`flickr.photosets.getList`…) for `token`'s account,
    /// as a `GET` with the arguments in the query, or a form `POST` for a change. Its JSON answer,
    /// or Flickr's error.
    fn rest(&self, post: bool, token: &Token, method: &str, args: &[(&str, String)], cancel: &AtomicBool) -> Result<serde_json::Value, Error> {
        if !self.key.is_set() {
            return Err(Error::Protocol("no Flickr API key".into()));
        }
        let url = format!("{}/services/rest/", self.endpoints.api.trim_end_matches('/'));
        let mut extra: Vec<(&str, String)> = vec![("method", method.to_string()), ("format", "json".into()), ("nojsoncallback", "1".into())];
        extra.extend(args.iter().cloned());
        let mut p = self.params(Some(&token.token), &extra);
        let verb = if post { "POST" } else { "GET" };
        let sig = oauth::sign(verb, &url, &p, self.key.secret.trim(), &token.secret).map_err(Error::Protocol)?;
        p.push(("oauth_signature".to_string(), sig));
        let form = p.iter().map(|(k, v)| format!("{}={}", oauth::encode(k), oauth::encode(v))).collect::<Vec<_>>().join("&");
        let limits = self.limits(cancel);
        let mut r = if post {
            http::post(&Url::parse(&url)?, &[("Content-Type", "application/x-www-form-urlencoded".into())], form.as_bytes(), &limits)?
        } else {
            http::get(&Url::parse(&format!("{url}?{form}"))?, &[], &limits)?
        };
        let body = r.read_to_end(MAX_REST_ANSWER, &limits)?;
        if r.status != 200 {
            return Err(Error::Status(r.status, String::from_utf8_lossy(&body).into_owned()));
        }
        reply::rest_answer(&body)
    }

    /// The albums of `token`'s account, in Flickr's order (as arranged on the account).
    pub fn list_albums(&self, token: &Token, cancel: &AtomicBool) -> Result<Vec<Album>, Error> {
        let mut out = Vec::new();
        let mut page = 1u64;
        loop {
            let v = self.rest(false, token, "flickr.photosets.getList", &[("per_page", "500".into()), ("page", page.to_string())], cancel)?;
            let sets = &v["photosets"];
            for s in sets["photoset"].as_array().into_iter().flatten() {
                let Some(id) = reply::text(&s["id"]).filter(|id| !id.is_empty()) else { continue };
                let title = reply::text(&s["title"]["_content"]).or_else(|| reply::text(&s["title"])).unwrap_or_default();
                let photos = reply::number(&s["photos"]).or_else(|| reply::number(&s["count_photos"])).unwrap_or(0);
                out.push(Album { id, title, photos });
            }
            let pages = reply::number(&sets["pages"]).unwrap_or(1);
            if page >= pages || page >= MAX_ALBUM_PAGES {
                return Ok(out);
            }
            page += 1;
        }
    }

    /// Make a new album titled `title` on `token`'s account. Flickr needs one of the account's
    /// photos for it (its cover, and its first photo): `primary_photo_id`. → the new album's id.
    pub fn create_album(&self, token: &Token, title: &str, primary_photo_id: &str, cancel: &AtomicBool) -> Result<String, Error> {
        let title = title.trim();
        if title.is_empty() {
            return Err(Error::Protocol("an album needs a name".into()));
        }
        let args = [("title", title.to_string()), ("primary_photo_id", primary_photo_id.to_string())];
        let v = self.rest(true, token, "flickr.photosets.create", &args, cancel)?;
        reply::text(&v["photoset"]["id"]).filter(|id| !id.is_empty()).ok_or_else(|| Error::Protocol("no album id in Flickr's answer".into()))
    }

    /// Put photo `photo_id` in album `album_id`. A photo already there is fine.
    pub fn add_to_album(&self, token: &Token, album_id: &str, photo_id: &str, cancel: &AtomicBool) -> Result<(), Error> {
        let args = [("photoset_id", album_id.to_string()), ("photo_id", photo_id.to_string())];
        match self.rest(true, token, "flickr.photosets.addPhoto", &args, cancel) {
            // 3: "Photo already in set"
            Err(Error::Flickr { code: 3, .. }) | Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Sleep `d`, waking to check `cancel`.
fn wait(d: Duration, cancel: &AtomicBool) -> Result<(), Error> {
    let end = std::time::Instant::now() + d;
    while std::time::Instant::now() < end {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Http(HttpError::Cancelled));
        }
        std::thread::sleep(Duration::from_millis(50).min(end.saturating_duration_since(std::time::Instant::now())));
    }
    Ok(())
}

/// A `multipart/form-data` body: the text `fields`, then the file as `photo`.
fn multipart(boundary: &str, fields: &[(String, String)], file_name: &str, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len().saturating_add(4096));
    for (k, v) in fields {
        out.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n").as_bytes());
        out.extend_from_slice(v.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    // the name as a quoted header value: no quotes, line breaks or backslashes in it
    let name: String = file_name.chars().filter(|c| !matches!(c, '"' | '\\' | '\r' | '\n')).collect();
    let name = if name.trim().is_empty() { "photo.jpg".to_string() } else { name };
    out.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"photo\"; filename=\"{name}\"\r\n").as_bytes());
    out.extend_from_slice(format!("Content-Type: {}\r\n\r\n", content_type(&name)).as_bytes());
    out.extend_from_slice(bytes);
    out.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    out
}

fn content_type(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "tif" | "tiff" => "image/tiff",
        "gif" => "image/gif",
        _ => "image/jpeg",
    }
}

#[cfg(test)]
mod tests;
