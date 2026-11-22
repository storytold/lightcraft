//! Bring-your-own-key generative fill providers (`docs/generative-fill.md`, provider research in
//! the PR notes): what each one takes and returns, how much it costs, what it does with the
//! upload, and a client for each over [`lcc_fetch::http`] (pure-Rust TLS).
//!
//! Nothing here runs by itself: the engine calls [`generate`] only after the user clicked
//! Generate and accepted the provider's terms in the consent dialog. Requests carry only the
//! context crop and its mask. The API key goes in the provider's own header and nowhere else:
//! it is never logged, and every error message passes through [`redact`].
//!
//! For tests (and to try the flow without an account) `<PREFIX>_GENFILL_MOCK` names a local mock
//! server ([`mock`]); only loopback addresses are accepted there, so a key can't be sent elsewhere
//! by mistake.

pub mod keys;
pub mod mock;
pub mod util;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use lcc_fetch::http::{self, HttpError, Limits, Url};
use serde_json::{Value, json};

use util::{Multipart, b64_decode, b64_encode, data_uri_bytes, png_decode_rgb, png_encode};

/// A provider the app can send a crop to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provider {
    OpenAi,
    Stability,
    Bfl,
    Fal,
}

/// What a provider does with an upload, in plain words for the consent dialog.
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    /// One or two sentences: retention, training, opt-outs.
    pub summary: &'static str,
    /// The provider's own page to read.
    pub url: &'static str,
    /// Whether uploads may be used to train models unless the user opts out in their account.
    pub trains_by_default: bool,
    /// When this summary was checked against the provider's pages.
    pub checked: &'static str,
    /// What the provider puts in its results to mark them as AI-made (provenance metadata,
    /// invisible watermarks), as far as it says. The app keeps only the pixels it fills, so
    /// metadata such as C2PA does not reach the photo or its exports; a watermark in the pixels may.
    pub provenance: &'static str,
}

/// Static facts about a provider.
#[derive(Clone, Copy, Debug)]
pub struct Info {
    pub id: &'static str,
    pub name: &'static str,
    /// The model used for Fill (with a prompt) and for Remove (no prompt).
    pub fill_model: &'static str,
    pub remove_model: &'static str,
    /// Largest crop side sent at native resolution.
    pub max_side: u32,
    /// Sizes the provider accepts, if only some (the crop is cut to one of these aspects).
    pub sizes: &'static [(u32, u32)],
    /// Most results one request returns (more variations are more requests).
    pub per_request: u32,
    /// Approximate price of one result, US dollars (the dialog says "about").
    pub usd_per_image: f64,
    /// Where the user creates a key.
    pub key_url: &'static str,
    pub policy: Policy,
    /// Something the user should know before using it (shown in the dialog).
    pub caveat: &'static str,
}

pub const ALL: [Provider; 4] = [Provider::OpenAi, Provider::Stability, Provider::Bfl, Provider::Fal];

impl Provider {
    pub fn from_id(id: &str) -> Option<Provider> {
        ALL.into_iter().find(|p| p.info().id == id)
    }

    pub fn info(self) -> &'static Info {
        match self {
            Provider::OpenAi => &OPENAI,
            Provider::Stability => &STABILITY,
            Provider::Bfl => &BFL,
            Provider::Fal => &FAL,
        }
    }

    fn base(self) -> &'static str {
        match self {
            Provider::OpenAi => "https://api.openai.com",
            Provider::Stability => "https://api.stability.ai",
            Provider::Bfl => "https://api.bfl.ai",
            Provider::Fal => "https://queue.fal.run",
        }
    }

    /// Host names a provider's follow-up URLs (polling, results) may point at.
    fn trusted_host(self, host: &str) -> bool {
        let under = |d: &str| host == d || host.ends_with(&format!(".{d}"));
        match self {
            Provider::OpenAi => under("openai.com"),
            Provider::Stability => under("stability.ai"),
            Provider::Bfl => under("bfl.ai"),
            Provider::Fal => under("fal.run") || under("fal.media") || under("fal.ai"),
        }
    }
}

const OPENAI: Info = Info {
    id: "openai",
    name: "OpenAI (GPT Image)",
    fill_model: "gpt-image-1.5",
    remove_model: "gpt-image-1.5",
    max_side: 1536,
    sizes: &[(1024, 1024), (1536, 1024), (1024, 1536)],
    per_request: 4,
    usd_per_image: 0.2,
    key_url: "https://platform.openai.com/api-keys",
    policy: Policy {
        summary: "OpenAI does not use API data to train its models unless you opt in. It keeps request logs for up to 30 days to check for abuse, and may keep an image longer if its safety systems flag it (zero retention needs OpenAI's approval).",
        url: "https://developers.openai.com/api/docs/guides/your-data",
        trains_by_default: false,
        checked: "2026-10-14",
        provenance: "OpenAI says images from its API carry C2PA provenance metadata and an invisible SynthID watermark in the pixels.",
    },
    caveat: "The model redraws the whole crop; the app keeps only the area you painted. Your organisation may need to be verified to use GPT Image models.",
};

const STABILITY: Info = Info {
    id: "stability",
    name: "Stability AI (Inpaint / Erase)",
    fill_model: "stable-image/edit/inpaint",
    remove_model: "stable-image/edit/erase",
    max_side: 3072,
    sizes: &[],
    per_request: 1,
    usd_per_image: 0.05,
    key_url: "https://platform.stability.ai/account/keys",
    policy: Policy {
        summary: "By default Stability AI may use what you send, and what it returns, to train its models. Turn this off in your Stability account (Account: \"Training: Improve the Model for Everyone\": No) before sending client photos. Stability does not publish how long it keeps uploads.",
        url: "https://stability.ai/privacy-center",
        trains_by_default: true,
        checked: "2026-10-14",
        provenance: "Stability's API documentation does not say; others report C2PA Content Credentials on images from its hosted service.",
    },
    caveat: "Turn off model training in your Stability account before sending client photos.",
};

const BFL: Info = Info {
    id: "bfl",
    name: "Black Forest Labs (FLUX Fill)",
    fill_model: "flux-pro-1.0-fill",
    remove_model: "flux-tools/erase-v1",
    max_side: 2048,
    sizes: &[],
    per_request: 1,
    usd_per_image: 0.05,
    key_url: "https://dashboard.bfl.ai/",
    policy: Policy {
        summary: "Black Forest Labs' API terms allow it to use what you send and receive to train its models, with no opt-out offered, and they do not allow photos of anyone under 18 or of identifiable people without their consent. Results are a link that expires after 10 minutes.",
        url: "https://bfl.ai/legal/flux-api-service-terms",
        trains_by_default: true,
        checked: "2026-10-14",
        provenance: "Black Forest Labs may embed C2PA Content Credentials in results, and its terms forbid removing them.",
    },
    caveat: "Not for photos of children, and only with consent from the people shown.",
};

const FAL: Info = Info {
    id: "fal",
    name: "fal.ai (FLUX Fill, object removal)",
    fill_model: "fal-ai/flux-pro/v1/fill",
    remove_model: "fal-ai/object-removal/mask",
    max_side: 2048,
    sizes: &[],
    per_request: 4,
    usd_per_image: 0.06,
    key_url: "https://fal.ai/dashboard/keys",
    policy: Policy {
        summary: "The app asks fal not to store the request (X-Fal-Store-IO: 0) and to return the result inline; fal does not say whether it trains on uploads. The model maker's terms also apply: for FLUX, Black Forest Labs' (no photos of anyone under 18).",
        url: "https://fal.ai/docs/model-apis/data-retention",
        trains_by_default: false,
        checked: "2026-10-14",
        provenance: "Depends on the model; fal does not document it (FLUX: Black Forest Labs may embed C2PA Content Credentials).",
    },
    caveat: "FLUX models are under Black Forest Labs' terms: not for photos of children.",
};

/// The default provider (privacy terms, a real mask, one synchronous call).
pub const DEFAULT: Provider = Provider::OpenAi;

/// The prompt used to remove (when the user gives none) with providers that always want one.
pub const REMOVE_PROMPT: &str = "Remove whatever is in the masked area and fill it with the background that would be behind it, matching the surrounding scene, light, grain and perspective exactly. Do not add any new object or person.";

/// One generation request: an RGB crop, its mask (255 = fill), the prompt, how many results.
#[derive(Clone, Debug)]
pub struct Request {
    pub width: usize,
    pub height: usize,
    /// Interleaved 8-bit RGB (`width × height × 3`).
    pub rgb: Vec<u8>,
    /// `width × height`, 255 = generate, 0 = keep.
    pub mask: Vec<u8>,
    /// Empty = remove (the provider's erase model where it has one).
    pub prompt: String,
    pub variations: u32,
    pub seed: u64,
}

/// One result: interleaved 8-bit RGB of any size (the caller fits it to the crop).
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

/// Why a generation failed, for the user.
#[derive(Clone, Debug, PartialEq)]
pub enum GenError {
    NoKey,
    Cancelled,
    /// The key was refused (401/403).
    Unauthorized(String),
    /// Out of credits / payment needed (402).
    Payment(String),
    /// Too many requests (429).
    RateLimited(String),
    /// The provider refused the content (moderation).
    Refused(String),
    /// Network, timeouts, TLS.
    Network(String),
    /// Anything else the provider said.
    Provider(String),
}

impl std::fmt::Display for GenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GenError::NoKey => write!(f, "no API key for this provider: add yours in Settings → Generative Fill"),
            GenError::Cancelled => write!(f, "cancelled"),
            GenError::Unauthorized(e) => write!(f, "the provider refused the API key: {e}"),
            GenError::Payment(e) => write!(f, "the provider asks for payment or credits: {e}"),
            GenError::RateLimited(e) => write!(f, "too many requests, try again in a minute: {e}"),
            GenError::Refused(e) => write!(f, "the provider declined to generate this: {e}"),
            GenError::Network(e) => write!(f, "could not reach the provider: {e}"),
            GenError::Provider(e) => write!(f, "the provider returned an error: {e}"),
        }
    }
}

impl std::error::Error for GenError {}

/// `text` with every occurrence of `secret` (and of its first 12 characters, for truncated echoes)
/// replaced by `•••`. Keys shorter than 6 characters are not searched for (they'd match words).
pub fn redact(text: &str, secret: &str) -> String {
    let secret = secret.trim();
    if secret.chars().count() < 6 {
        return text.to_string();
    }
    let mut out = text.replace(secret, "•••");
    let head: String = secret.chars().take(12).collect();
    if head.chars().count() >= 8 {
        out = out.replace(&head, "•••");
    }
    out
}

/// Where requests go: the provider, or a loopback mock (`<PREFIX>_GENFILL_MOCK`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    base: String,
    mock: bool,
}

impl Endpoint {
    /// The provider's own address.
    pub fn real(p: Provider) -> Endpoint {
        Endpoint { base: p.base().to_string(), mock: false }
    }

    /// A mock server on this machine; anything but `http://127.0.0.1`, `[::1]` or `localhost` is refused.
    pub fn mock(base: &str) -> Result<Endpoint, String> {
        let u = Url::parse(base).map_err(|e| e.to_string())?;
        if !matches!(u.host.as_str(), "127.0.0.1" | "::1" | "localhost") {
            return Err("the generative-fill mock server must be on this machine (127.0.0.1)".into());
        }
        Ok(Endpoint { base: base.trim_end_matches('/').to_string(), mock: true })
    }

    /// `<PREFIX>_GENFILL_MOCK` if set (and local), else the provider.
    pub fn for_provider(p: Provider) -> Endpoint {
        match lcc_brand::env("GENFILL_MOCK") {
            Some(m) if !m.trim().is_empty() => Endpoint::mock(m.trim()).unwrap_or_else(|_| Endpoint::real(p)),
            _ => Endpoint::real(p),
        }
    }

    pub fn is_mock(&self) -> bool {
        self.mock
    }

    fn url(&self, path: &str) -> Result<Url, GenError> {
        Url::parse(&format!("{}{path}", self.base)).map_err(|e| GenError::Network(e.to_string()))
    }

    /// A follow-up URL the provider handed back, if it points somewhere it should.
    fn follow(&self, p: Provider, url: &str) -> Result<Url, GenError> {
        let u = Url::parse(url).map_err(|e| GenError::Provider(format!("unusable result address: {e}")))?;
        let ok = if self.mock { self.url("/").is_ok_and(|b| b.host == u.host && b.port == u.port) } else { u.tls && p.trusted_host(&u.host) };
        if !ok {
            return Err(GenError::Provider(format!("the provider pointed at an unexpected address ({})", u.display())));
        }
        Ok(u)
    }
}

/// Timeouts and cancellation for one generation.
pub struct Control<'a> {
    pub cancel: &'a AtomicBool,
    /// Longest wait for the next bytes (image models can think for a minute or two).
    pub stall: Duration,
    /// Longest whole generation (polling included).
    pub deadline: Duration,
    /// Requests the provider already accepted (and bills), kept across a failure so that trying again fetches their
    /// results instead of paying for new ones. `None`: not kept.
    pub accepted: Option<&'a Accepted>,
}

/// Generations a provider accepted whose results are still to be fetched: `(provider, slot, addresses)`, the slot
/// being the request's place in the generation (a variation), the addresses where its result is asked for
/// (`{"polling"}` for Black Forest Labs, `{"status", "response"}` for fal). Providers that answer with the image
/// itself (OpenAI, Stability) have none.
#[derive(Debug, Default)]
pub struct Accepted(Mutex<Vec<(Provider, u32, Value)>>);

impl Accepted {
    fn get(&self, p: Provider, slot: u32) -> Option<Value> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).iter().find(|(q, s, _)| *q == p && *s == slot).map(|(_, _, v)| v.clone())
    }

    fn put(&self, p: Provider, slot: u32, v: Value) {
        let mut l = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        l.retain(|(q, s, _)| !(*q == p && *s == slot));
        l.push((p, slot, v));
    }

    /// How many accepted generations wait for their results.
    pub fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Control<'_> {
    fn remember(&self, p: Provider, slot: u32, v: Value) {
        if let Some(a) = self.accepted {
            a.put(p, slot, v);
        }
    }

    fn accepted(&self, p: Provider, slot: u32) -> Option<Value> {
        self.accepted.and_then(|a| a.get(p, slot))
    }
}

impl Control<'_> {
    fn limits(&self) -> Limits<'_> {
        Limits { connect: Duration::from_secs(20), stall: self.stall, cancel: self.cancel }
    }
}

/// Largest response read (base64 images of a few MP).
const MAX_RESPONSE: usize = 96 * 1024 * 1024;

fn net(e: HttpError, tries: u32) -> GenError {
    match e {
        HttpError::Cancelled => GenError::Cancelled,
        e if tries > 1 => GenError::Network(format!(
            "{e} (tried {tries} times, each on a new connection; if this keeps happening, a VPN, proxy or antivirus that inspects secure connections may be in the way)"
        )),
        e => GenError::Network(e.to_string()),
    }
}

/// Tries of a request that reads (`GET`): a broken connection is followed by a new one.
const READ_TRIES: u32 = 3;

/// Send one request and read the whole reply: `(status, body)`. Each try is a new connection (none is ever
/// reused). A failed read (`GET`: polling, a result) is tried again up to [`READ_TRIES`] times; a request that
/// starts a paid generation (`POST`) once more only when it certainly never reached the provider (the connection
/// broke while it was being sent, or the provider's TLS layer refused it), so it is never paid for twice.
fn call(method: &str, url: &Url, headers: &[(&str, String)], body: Option<&[u8]>, ctl: &Control) -> Result<(u16, Vec<u8>), GenError> {
    let limits = ctl.limits();
    let mut tries = 0u32;
    loop {
        tries += 1;
        let once = || -> Result<(u16, Vec<u8>), (HttpError, http::Stage)> {
            let mut r = http::request_staged(method, url, headers, body, &limits)?;
            let status = r.status;
            let bytes = r.read_to_end(MAX_RESPONSE, &limits).map_err(|e| (e, http::Stage::Receive))?;
            Ok((status, bytes))
        };
        let (e, stage) = match once() {
            Ok(r) => return Ok(r),
            Err(f) => f,
        };
        let again = http::transient(&e) && if method == "GET" { tries < READ_TRIES } else { tries < 2 && http::not_delivered(&e, stage) };
        if !again {
            return Err(net(e, tries));
        }
        log::warn!("generative fill: {method} {} failed ({e}); trying again on a new connection", url.display());
        // a moment before the next try, cancellable
        let until = Instant::now() + Duration::from_millis(400 * u64::from(tries));
        while Instant::now() < until {
            if ctl.cancel.load(Ordering::Relaxed) {
                return Err(GenError::Cancelled);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

/// A provider's error body in a sentence (its `message`/`error`/`detail`, else the start of the body).
fn error_text(body: &[u8]) -> String {
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let pick = |v: &Value| -> Option<String> {
        for k in ["message", "error", "detail", "errors", "name"] {
            match v.get(k) {
                Some(Value::String(s)) => return Some(s.clone()),
                Some(Value::Object(o)) => {
                    if let Some(Value::String(s)) = o.get("message") {
                        return Some(s.clone());
                    }
                }
                Some(Value::Array(a)) => {
                    let s: Vec<String> = a.iter().filter_map(|e| e.get("msg").or(Some(e)).and_then(Value::as_str).map(str::to_string)).collect();
                    if !s.is_empty() {
                        return Some(s.join("; "));
                    }
                }
                _ => {}
            }
        }
        None
    };
    pick(&v).unwrap_or_else(|| String::from_utf8_lossy(body.get(..body.len().min(300)).unwrap_or_default()).into_owned()).chars().take(400).collect()
}

fn check_status(status: u16, body: &[u8]) -> Result<(), GenError> {
    if (200..300).contains(&status) {
        return Ok(());
    }
    let text = error_text(body);
    let lower = text.to_ascii_lowercase();
    Err(match status {
        401 | 403 => GenError::Unauthorized(text),
        402 => GenError::Payment(text),
        429 => GenError::RateLimited(text),
        _ if lower.contains("moderat") || lower.contains("safety") || lower.contains("content_filtered") || lower.contains("nsfw") => {
            GenError::Refused(text)
        }
        _ => GenError::Provider(format!("{status}: {text}")),
    })
}

fn json_body(body: &[u8]) -> Result<Value, GenError> {
    serde_json::from_slice(body).map_err(|e| GenError::Provider(format!("unreadable reply: {e}")))
}

fn decode_png(bytes: &[u8]) -> Result<Image, GenError> {
    let (w, h, rgb) = png_decode_rgb(bytes).map_err(GenError::Provider)?;
    Ok(Image { width: w, height: h, rgb })
}

fn decode_b64_png(s: &str) -> Result<Image, GenError> {
    let bytes = b64_decode(s).ok_or_else(|| GenError::Provider("unreadable image data".into()))?;
    decode_png(&bytes)
}

/// Images of a request, as the provider wants them: (image PNG, mask PNG).
fn pngs(p: Provider, r: &Request) -> Result<(Vec<u8>, Vec<u8>), GenError> {
    let bad = |e: String| GenError::Provider(format!("could not encode the crop: {e}"));
    let image = png_encode(r.width, r.height, 3, &r.rgb).map_err(bad)?;
    let mask = match p {
        // OpenAI: an RGBA PNG whose transparent pixels are the ones to edit
        Provider::OpenAi => {
            let rgba: Vec<u8> = r.mask.iter().flat_map(|m| if *m >= 128 { [0, 0, 0, 0] } else { [0, 0, 0, 255] }).collect();
            png_encode(r.width, r.height, 4, &rgba).map_err(bad)?
        }
        // the others: white = edit
        _ => {
            let grey: Vec<u8> = r.mask.iter().map(|m| if *m >= 128 { 255 } else { 0 }).collect();
            png_encode(r.width, r.height, 1, &grey).map_err(bad)?
        }
    };
    Ok((image, mask))
}

fn openai_size(w: usize, h: usize) -> &'static str {
    let a = w as f64 / h.max(1) as f64;
    if a > 1.2 {
        "1536x1024"
    } else if a < 1.0 / 1.2 {
        "1024x1536"
    } else {
        "1024x1024"
    }
}

/// Run one generation (see the module docs); results come back in the provider's sizes.
pub fn generate(p: Provider, key: &str, at: &Endpoint, r: &Request, ctl: &Control) -> Result<Vec<Image>, GenError> {
    let key = key.trim();
    if key.is_empty() {
        return Err(GenError::NoKey);
    }
    if r.width == 0 || r.height == 0 || r.rgb.len() != r.width * r.height * 3 || r.mask.len() != r.width * r.height {
        return Err(GenError::Provider("the crop and its mask differ in size".into()));
    }
    let n = r.variations.clamp(1, 8);
    let started = Instant::now();
    let out = match p {
        Provider::OpenAi => openai(key, at, r, n, ctl),
        Provider::Stability => (0..n).map(|i| stability(key, at, r, i, ctl)).collect(),
        Provider::Bfl => (0..n).map(|i| bfl(key, at, r, i, ctl, started)).collect(),
        Provider::Fal => fal(key, at, r, n, ctl, started),
    };
    out.map_err(|e| match e {
        GenError::Unauthorized(t) => GenError::Unauthorized(redact(&t, key)),
        GenError::Payment(t) => GenError::Payment(redact(&t, key)),
        GenError::RateLimited(t) => GenError::RateLimited(redact(&t, key)),
        GenError::Refused(t) => GenError::Refused(redact(&t, key)),
        GenError::Network(t) => GenError::Network(redact(&t, key)),
        GenError::Provider(t) => GenError::Provider(redact(&t, key)),
        e => e,
    })
}

fn openai(key: &str, at: &Endpoint, r: &Request, n: u32, ctl: &Control) -> Result<Vec<Image>, GenError> {
    let (image, mask) = pngs(Provider::OpenAi, r)?;
    let prompt = if r.prompt.trim().is_empty() { REMOVE_PROMPT.to_string() } else { r.prompt.trim().to_string() };
    let mut m = Multipart::new(r.seed);
    m.text("model", OPENAI.fill_model)
        .text("prompt", &prompt)
        .text("n", &n.min(OPENAI.per_request).to_string())
        .text("size", openai_size(r.width, r.height))
        .text("quality", "high")
        .text("input_fidelity", "high")
        .text("output_format", "png")
        .file("image", "image.png", "image/png", &image)
        .file("mask", "mask.png", "image/png", &mask);
    let (ct, body) = m.finish();
    let mut out = Vec::new();
    // more than one request when asked for more than one request gives
    while out.len() < n as usize {
        if ctl.cancel.load(Ordering::Relaxed) {
            return Err(GenError::Cancelled);
        }
        let (status, reply) = call(
            "POST",
            &at.url("/v1/images/edits")?,
            &[("Authorization", format!("Bearer {key}")), ("Content-Type", ct.clone())],
            Some(&body),
            ctl,
        )?;
        check_status(status, &reply)?;
        let v = json_body(&reply)?;
        let data = v.get("data").and_then(Value::as_array).ok_or_else(|| GenError::Provider("no images in the reply".into()))?;
        if data.is_empty() {
            return Err(GenError::Provider("no images in the reply".into()));
        }
        for d in data {
            let b = d.get("b64_json").and_then(Value::as_str).ok_or_else(|| GenError::Provider("an image without data".into()))?;
            out.push(decode_b64_png(b)?);
        }
    }
    out.truncate(n as usize);
    Ok(out)
}

fn stability(key: &str, at: &Endpoint, r: &Request, i: u32, ctl: &Control) -> Result<Image, GenError> {
    let (image, mask) = pngs(Provider::Stability, r)?;
    let erase = r.prompt.trim().is_empty();
    let mut m = Multipart::new(r.seed ^ u64::from(i));
    m.file("image", "image.png", "image/png", &image)
        .file("mask", "mask.png", "image/png", &mask)
        .text("output_format", "png")
        .text("grow_mask", "3");
    if !erase {
        m.text("prompt", r.prompt.trim());
    }
    m.text("seed", &((r.seed.wrapping_add(u64::from(i))) % 4_294_967_294).to_string());
    let (ct, body) = m.finish();
    let path = if erase { "/v2beta/stable-image/edit/erase" } else { "/v2beta/stable-image/edit/inpaint" };
    let (status, reply) = call(
        "POST",
        &at.url(path)?,
        &[("Authorization", format!("Bearer {key}")), ("Content-Type", ct), ("Accept", "application/json".into())],
        Some(&body),
        ctl,
    )?;
    check_status(status, &reply)?;
    let v = json_body(&reply)?;
    if let Some(f) = v.get("finish_reason").and_then(Value::as_str)
        && f.eq_ignore_ascii_case("CONTENT_FILTERED")
    {
        return Err(GenError::Refused("the result was filtered".into()));
    }
    decode_b64_png(v.get("image").and_then(Value::as_str).ok_or_else(|| GenError::Provider("no image in the reply".into()))?)
}

/// Poll `url` (with `headers`) until `done` says the reply holds the result, at most until the deadline.
fn poll(
    url: &Url,
    headers: &[(&str, String)],
    ctl: &Control,
    started: Instant,
    done: impl Fn(&Value) -> Result<bool, GenError>,
) -> Result<Value, GenError> {
    let mut wait = Duration::from_millis(500);
    loop {
        if ctl.cancel.load(Ordering::Relaxed) {
            return Err(GenError::Cancelled);
        }
        if started.elapsed() > ctl.deadline {
            return Err(GenError::Network("the provider took too long".into()));
        }
        let (status, reply) = call("GET", url, headers, None, ctl)?;
        check_status(status, &reply)?;
        let v = json_body(&reply)?;
        if done(&v)? {
            return Ok(v);
        }
        // sleep in short steps so Cancel is quick
        let until = Instant::now() + wait;
        while Instant::now() < until {
            if ctl.cancel.load(Ordering::Relaxed) {
                return Err(GenError::Cancelled);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        wait = (wait * 3 / 2).min(Duration::from_secs(3));
    }
}

/// Download a result image (no key: these are pre-signed links), or read a data URI.
fn fetch_image(p: Provider, at: &Endpoint, url: &str, ctl: &Control) -> Result<Image, GenError> {
    if let Some(bytes) = data_uri_bytes(url) {
        return decode_png(&bytes);
    }
    let u = at.follow(p, url)?;
    let (status, bytes) = call("GET", &u, &[], None, ctl)?;
    check_status(status, &bytes)?;
    decode_png(&bytes)
}

fn bfl(key: &str, at: &Endpoint, r: &Request, i: u32, ctl: &Control, started: Instant) -> Result<Image, GenError> {
    let (image, mask) = pngs(Provider::Bfl, r)?;
    let erase = r.prompt.trim().is_empty();
    let seed = r.seed.wrapping_add(u64::from(i)) % 4_294_967_294;
    let (path, body) = if erase {
        (
            "/v1/flux-tools/erase-v1",
            json!({"image": b64_encode(&image), "mask": b64_encode(&mask), "dilate_pixels": 4, "seed": seed, "output_format": "png", "safety_tolerance": 2}),
        )
    } else {
        (
            "/v1/flux-pro-1.0-fill",
            json!({"image": b64_encode(&image), "mask": b64_encode(&mask), "prompt": r.prompt.trim(), "steps": 50, "guidance": 60, "seed": seed, "output_format": "png", "safety_tolerance": 2, "prompt_upsampling": false}),
        )
    };
    let headers = [("x-key", key.to_string()), ("Content-Type", "application/json".to_string()), ("Accept", "application/json".to_string())];
    // a generation the provider already accepted is not sent (and paid for) again: its result is fetched
    let polling = match ctl.accepted(Provider::Bfl, i).and_then(|v| v.get("polling").and_then(Value::as_str).map(str::to_string)) {
        Some(p) => p,
        None => {
            let body = serde_json::to_vec(&body).map_err(|e| GenError::Provider(e.to_string()))?;
            let (status, reply) = call("POST", &at.url(path)?, &headers, Some(&body), ctl)?;
            check_status(status, &reply)?;
            let v = json_body(&reply)?;
            let polling =
                v.get("polling_url").and_then(Value::as_str).ok_or_else(|| GenError::Provider("no polling address in the reply".into()))?.to_string();
            ctl.remember(Provider::Bfl, i, json!({"polling": polling}));
            polling
        }
    };
    let polling = at.follow(Provider::Bfl, &polling)?;
    let res = poll(&polling, &headers[..1], ctl, started, |v| match v.get("status").and_then(Value::as_str).unwrap_or_default() {
        "Ready" => Ok(true),
        "Pending" | "Reasoning" | "Generating" | "Queued" | "Task not found" => Ok(false),
        s if s.contains("Moderated") => Err(GenError::Refused(s.to_string())),
        s => Err(GenError::Provider(format!("status {s}"))),
    })?;
    let sample = res.pointer("/result/sample").and_then(Value::as_str).ok_or_else(|| GenError::Provider("no result image".into()))?;
    fetch_image(Provider::Bfl, at, sample, ctl)
}

fn fal(key: &str, at: &Endpoint, r: &Request, n: u32, ctl: &Control, started: Instant) -> Result<Vec<Image>, GenError> {
    let (image, mask) = pngs(Provider::Fal, r)?;
    let erase = r.prompt.trim().is_empty();
    let (img_uri, mask_uri) = (format!("data:image/png;base64,{}", b64_encode(&image)), format!("data:image/png;base64,{}", b64_encode(&mask)));
    let headers = [
        ("Authorization", format!("Key {key}")),
        ("Content-Type", "application/json".to_string()),
        ("Accept", "application/json".to_string()),
        ("X-Fal-Store-IO", "0".to_string()),
    ];
    let one = |slot: u32, model: &str, body: Value| -> Result<Vec<Image>, GenError> {
        let addresses =
            |v: &Value| -> Option<(String, String)> { Some((v.get("status")?.as_str()?.to_string(), v.get("response")?.as_str()?.to_string())) };
        // a generation the provider already accepted is not sent (and paid for) again: its result is fetched
        let (status_url, response_url) = match ctl.accepted(Provider::Fal, slot).as_ref().and_then(addresses) {
            Some(a) => a,
            None => {
                let body = serde_json::to_vec(&body).map_err(|e| GenError::Provider(e.to_string()))?;
                let (status, reply) = call("POST", &at.url(&format!("/{model}"))?, &headers, Some(&body), ctl)?;
                check_status(status, &reply)?;
                let v = json_body(&reply)?;
                let status_url = v
                    .get("status_url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| GenError::Provider("no status address in the reply".into()))?
                    .to_string();
                let response_url = v
                    .get("response_url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| GenError::Provider("no result address in the reply".into()))?
                    .to_string();
                ctl.remember(Provider::Fal, slot, json!({"status": status_url, "response": response_url}));
                (status_url, response_url)
            }
        };
        let (su, ru) = (at.follow(Provider::Fal, &status_url)?, at.follow(Provider::Fal, &response_url)?);
        poll(&su, &headers[..1], ctl, started, |v| match v.get("status").and_then(Value::as_str).unwrap_or_default() {
            "COMPLETED" => Ok(true),
            "IN_QUEUE" | "IN_PROGRESS" => Ok(false),
            s => Err(GenError::Provider(format!("status {s}"))),
        })?;
        let (status, reply) = call("GET", &ru, &headers[..1], None, ctl)?;
        check_status(status, &reply)?;
        let v = json_body(&reply)?;
        if v.get("has_nsfw_concepts").and_then(Value::as_array).is_some_and(|a| a.iter().any(|b| b.as_bool() == Some(true))) {
            return Err(GenError::Refused("the result was filtered".into()));
        }
        let imgs: Vec<&str> = match v.get("images").and_then(Value::as_array) {
            Some(a) => a.iter().filter_map(|i| i.get("url").and_then(Value::as_str)).collect(),
            None => v.pointer("/image/url").and_then(Value::as_str).into_iter().collect(),
        };
        if imgs.is_empty() {
            return Err(GenError::Provider("no images in the reply".into()));
        }
        imgs.into_iter().map(|u| fetch_image(Provider::Fal, at, u, ctl)).collect()
    };
    if erase {
        let mut out = Vec::new();
        for slot in 0..n {
            out.extend(one(
                slot,
                FAL.remove_model,
                json!({"image_url": img_uri, "mask_url": mask_uri, "model": "best_quality", "mask_expansion": 4}),
            )?);
        }
        Ok(out)
    } else {
        one(
            0,
            FAL.fill_model,
            json!({"prompt": r.prompt.trim(), "image_url": img_uri, "mask_url": mask_uri, "num_images": n.min(FAL.per_request), "output_format": "png", "safety_tolerance": "2", "sync_mode": true, "seed": r.seed % 4_294_967_294}),
        )
    }
}

/// Requests a generation of `variations` results takes with `p` (for the cost estimate).
pub fn requests_for(p: Provider, variations: u32, remove: bool) -> u32 {
    let n = variations.clamp(1, 8);
    match p {
        Provider::OpenAi => n.div_ceil(OPENAI.per_request),
        Provider::Fal if !remove => n.div_ceil(FAL.per_request),
        _ => n,
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_hostile;
