//! The client: configuration, the request builder and the response.

use std::io::{BufReader, BufWriter, Read, Write};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::conn::{self, ConnectOptions};
use crate::http::{self, BodyReader, Framing};
use crate::{Multipart, NetError, Trust, Url};

/// HTTP method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
        }
    }
}

/// Header names that carry credentials: never sent to another origin after a redirect.
const SENSITIVE: &[&str] = &["authorization", "proxy-authorization", "cookie", "x-api-key"];

/// Settings shared by every request of a [`Client`].
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Per address tried.
    pub connect_timeout: Duration,
    /// Longest wait for any progress while sending or receiving.
    pub stall_timeout: Duration,
    /// Redirects followed per request (0: none, the 3xx response is returned).
    pub max_redirects: u8,
    /// `http://host:port` of an HTTP proxy (HTTPS goes through it with `CONNECT`).
    pub proxy: Option<String>,
    /// Which server certificates are trusted.
    pub trust: Trust,
    /// Cap for [`Response::bytes`], [`Response::text`] and [`Response::json`].
    pub max_body: u64,
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig {
            connect_timeout: Duration::from_secs(10),
            stall_timeout: Duration::from_secs(30),
            max_redirects: 5,
            proxy: None,
            trust: Trust::new(),
            max_body: 64 * 1024 * 1024,
        }
    }
}

/// An HTTP(S) client. Blocking: run it off the UI thread. Cheap to clone.
#[derive(Clone)]
pub struct Client {
    config: ClientConfig,
    proxy: Option<Url>,
    tls: Arc<rustls::ClientConfig>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("config", &self.config).finish()
    }
}

impl Client {
    pub fn new(config: ClientConfig) -> Result<Client, NetError> {
        let proxy = match &config.proxy {
            Some(p) => {
                let u = Url::parse(p)?;
                if u.tls {
                    return Err(NetError::BadUrl("only http:// proxies are supported".into()));
                }
                Some(u)
            }
            None => None,
        };
        let tls = config.trust.client_config()?;
        Ok(Client { config, proxy, tls })
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    pub fn request(&self, method: Method, url: &str) -> Request<'_> {
        Request {
            client: self,
            method,
            url: Url::parse(url),
            headers: Vec::new(),
            secret_headers: Vec::new(),
            body: Body::Empty,
            cancel: None,
            progress: None,
            error: None,
            accept_gzip: true,
        }
    }

    pub fn get(&self, url: &str) -> Request<'_> {
        self.request(Method::Get, url)
    }
    pub fn head(&self, url: &str) -> Request<'_> {
        self.request(Method::Head, url)
    }
    pub fn post(&self, url: &str) -> Request<'_> {
        self.request(Method::Post, url)
    }
    pub fn put(&self, url: &str) -> Request<'_> {
        self.request(Method::Put, url)
    }
    pub fn patch(&self, url: &str) -> Request<'_> {
        self.request(Method::Patch, url)
    }
    pub fn delete(&self, url: &str) -> Request<'_> {
        self.request(Method::Delete, url)
    }
}

enum Body {
    Empty,
    Bytes { data: Vec<u8>, content_type: String },
    Multipart(Multipart),
}

/// Progress callback: `(bytes sent, total)` while uploading.
pub type Progress<'a> = &'a mut dyn FnMut(u64, Option<u64>);

/// A request under construction; [`Request::send`] performs it.
pub struct Request<'a> {
    client: &'a Client,
    method: Method,
    url: Result<Url, NetError>,
    headers: Vec<(String, String)>,
    secret_headers: Vec<String>,
    body: Body,
    cancel: Option<Arc<AtomicBool>>,
    progress: Option<Progress<'a>>,
    error: Option<NetError>,
    accept_gzip: bool,
}

fn check_header(name: &str, value: &str) -> Result<(), NetError> {
    let bad_name = name.is_empty() || name.chars().any(|c| !c.is_ascii_graphic() || c == ':');
    if bad_name || value.chars().any(|c| c == '\r' || c == '\n' || c == '\0') {
        // the value is never put in the message: it may be a secret
        return Err(NetError::BadHeader(format!("{:?}", name.chars().take(64).collect::<String>())));
    }
    Ok(())
}

impl<'a> Request<'a> {
    /// Add a header.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        match check_header(name, value) {
            Ok(()) => self.headers.push((name.to_string(), value.to_string())),
            Err(e) => self.error = self.error.or(Some(e)),
        }
        self
    }

    /// Add a header holding a credential: it is dropped when a redirect leaves the origin.
    pub fn secret_header(mut self, name: &str, value: &str) -> Self {
        self.secret_headers.push(name.to_ascii_lowercase());
        self.header(name, value)
    }

    /// `Authorization: Bearer …`.
    pub fn bearer(self, token: &str) -> Self {
        self.secret_header("Authorization", &format!("Bearer {token}"))
    }

    /// A raw body.
    pub fn body(mut self, content_type: &str, data: Vec<u8>) -> Self {
        self.body = Body::Bytes { data, content_type: content_type.to_string() };
        self
    }

    /// A JSON body.
    pub fn json<T: Serialize + ?Sized>(mut self, value: &T) -> Self {
        match serde_json::to_vec(value) {
            Ok(data) => self.body = Body::Bytes { data, content_type: "application/json".into() },
            Err(e) => self.error = self.error.or(Some(NetError::Json(e.to_string()))),
        }
        self
    }

    /// A streaming `multipart/form-data` body.
    pub fn multipart(mut self, form: Multipart) -> Self {
        self.body = Body::Multipart(form);
        self
    }

    /// Abort (with [`NetError::Cancelled`]) once this flag is raised, also while reading the
    /// response body.
    pub fn cancel(mut self, flag: Arc<AtomicBool>) -> Self {
        self.cancel = Some(flag);
        self
    }

    /// Report upload progress.
    pub fn progress(mut self, f: Progress<'a>) -> Self {
        self.progress = Some(f);
        self
    }

    /// Don't ask for a compressed response (downloads with `Range`).
    pub fn identity_encoding(mut self) -> Self {
        self.accept_gzip = false;
        self
    }

    /// Perform the request, following redirects. Any status is a successful `send`; see
    /// [`Response::error_for_status`].
    pub fn send(self) -> Result<Response, NetError> {
        let Request { client, mut method, url, mut headers, secret_headers, mut body, cancel, mut progress, error, accept_gzip } = self;
        if let Some(e) = error {
            return Err(e);
        }
        let mut url = url?;
        let origin = url.origin();
        let mut redirects = 0u8;
        loop {
            let path_only = url.path.split('?').next().unwrap_or("/");
            log::debug!("net: {} {}{}", method.as_str(), url.origin(), path_only);
            let opts = ConnectOptions {
                connect_timeout: client.config.connect_timeout,
                stall: client.config.stall_timeout,
                cancel: cancel.clone(),
                proxy: client.proxy.as_ref(),
                tls: Some(client.tls.clone()),
            };
            let conn = conn::connect(&url, opts)?;
            let mut w = BufWriter::with_capacity(64 * 1024, conn);
            let target = if client.proxy.is_some() && !url.tls { url.to_string() } else { url.path.clone() };
            let mut head = format!(
                "{} {target} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {}\r\nConnection: close\r\n",
                method.as_str(),
                url.host_header(),
                crate::user_agent()
            );
            let has = |n: &str| headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(n));
            if !has("accept") {
                head.push_str("Accept: */*\r\n");
            }
            if !has("accept-encoding") {
                head.push_str(if accept_gzip { "Accept-Encoding: gzip\r\n" } else { "Accept-Encoding: identity\r\n" });
            }
            let total = match &body {
                Body::Empty => (method != Method::Get && method != Method::Head && method != Method::Delete).then_some(0),
                Body::Bytes { data, .. } => Some(data.len() as u64),
                Body::Multipart(m) => Some(m.len()),
            };
            if let Some(n) = total {
                head.push_str(&format!("Content-Length: {n}\r\n"));
            }
            match &body {
                Body::Bytes { content_type, .. } if !has("content-type") => head.push_str(&format!("Content-Type: {content_type}\r\n")),
                Body::Multipart(m) => head.push_str(&format!("Content-Type: {}\r\n", m.content_type())),
                _ => {}
            }
            for (k, v) in &headers {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            head.push_str("\r\n");
            w.write_all(head.as_bytes())?;
            let replay = match std::mem::replace(&mut body, Body::Empty) {
                Body::Empty => Some(Body::Empty),
                Body::Bytes { data, content_type } => {
                    for chunk in data.chunks(64 * 1024) {
                        w.write_all(chunk)?;
                    }
                    if let Some(p) = progress.as_mut() {
                        p(data.len() as u64, total);
                    }
                    Some(Body::Bytes { data, content_type })
                }
                Body::Multipart(m) => {
                    let mut report = |sent: u64| {
                        if let Some(p) = progress.as_mut() {
                            p(sent, total);
                        }
                    };
                    m.write_to(&mut w, &mut report)?;
                    None
                }
            };
            w.flush()?;
            let conn = w.into_inner().map_err(|e| NetError::Io(e.to_string()))?;
            let mut r = BufReader::with_capacity(64 * 1024, conn);
            let head = http::read_head(&mut r)?;

            let location = head.header("location").filter(|_| matches!(head.status, 301 | 302 | 303 | 307 | 308));
            if let Some(location) = location {
                if redirects >= client.config.max_redirects {
                    if client.config.max_redirects == 0 {
                        return Response::new(head, r, url, method, client.config.max_body);
                    }
                    return Err(NetError::Redirect(format!("more than {} redirects", client.config.max_redirects)));
                }
                redirects += 1;
                let next = url.join(location)?;
                if url.tls && !next.tls {
                    return Err(NetError::Redirect("refusing to follow a redirect from https to http".into()));
                }
                if head.status == 303 || ((head.status == 301 || head.status == 302) && method == Method::Post) {
                    method = if method == Method::Head { Method::Head } else { Method::Get };
                    headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-type"));
                    body = Body::Empty;
                } else {
                    body = replay.ok_or_else(|| NetError::Redirect("a streamed upload can't be resent to the redirect target".into()))?;
                }
                if next.origin() != origin {
                    headers.retain(|(k, _)| {
                        let k = k.to_ascii_lowercase();
                        !SENSITIVE.contains(&k.as_str()) && !secret_headers.contains(&k)
                    });
                }
                url = next;
                continue;
            }
            return Response::new(head, r, url, method, client.config.max_body);
        }
    }
}

/// A response: status and headers, and the body as a stream ([`Read`]).
pub struct Response {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    /// Where the response came from (after redirects).
    pub url: Url,
    body: Box<dyn Read + Send>,
    max_body: u64,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // headers can carry cookies: only their names
        let names: Vec<&str> = self.headers.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("Response").field("status", &self.status).field("url", &self.url.origin()).field("headers", &names).finish()
    }
}

impl Response {
    fn new(head: http::Head, r: BufReader<conn::Conn>, url: Url, method: Method, max_body: u64) -> Result<Response, NetError> {
        let framing = Framing::of(&head, method == Method::Head)?;
        let raw = BodyReader::new(r, framing);
        let gzip = framing != Framing::Empty && head.header("content-encoding").is_some_and(|e| e.trim().eq_ignore_ascii_case("gzip"));
        let body: Box<dyn Read + Send> = if gzip { Box::new(flate2::read::GzDecoder::new(raw)) } else { Box::new(raw) };
        Ok(Response { status: head.status, reason: head.reason, headers: head.headers, url, body, max_body })
    }

    /// First value of a header (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// `Err(Status)` for a non-2xx response.
    pub fn error_for_status(self) -> Result<Response, NetError> {
        if self.is_success() { Ok(self) } else { Err(NetError::Status { code: self.status, reason: self.reason.clone() }) }
    }

    /// The whole body (at most the client's `max_body`).
    pub fn bytes(mut self) -> Result<Vec<u8>, NetError> {
        let cap = self.max_body;
        let mut out = Vec::new();
        (&mut self.body).take(cap.saturating_add(1)).read_to_end(&mut out)?;
        if out.len() as u64 > cap {
            return Err(NetError::TooLarge(cap));
        }
        Ok(out)
    }

    pub fn text(self) -> Result<String, NetError> {
        Ok(String::from_utf8_lossy(&self.bytes()?).into_owned())
    }

    pub fn json<T: DeserializeOwned>(self) -> Result<T, NetError> {
        serde_json::from_slice(&self.bytes()?).map_err(|e| NetError::Json(e.to_string()))
    }
}

impl Read for Response {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.body.read(buf)
    }
}
