//! What each running copy of the app advertises over multicast DNS service discovery (RFC 6762 /
//! RFC 6763) while it looks for devices (Settings ▸ Sync open) or has paired devices: who it is
//! and where to connect.
//!
//! The service type is `_<svc>._tcp.local` ([`service_type`], from the Android package name); the instance name is the device
//! id; the SRV record names the TCP port; the TXT record carries `v`, `app`, `id`, `name`, `key`
//! and `pairing`. This works with Bonjour (iOS allows nothing else) and Android's NSD.

/// The UDP port of multicast DNS (tests pass their own to [`crate::Service::start`]).
pub const PORT: u16 = 5353;
/// The mDNS multicast group (IPv4).
pub const GROUP: std::net::Ipv4Addr = std::net::Ipv4Addr::new(224, 0, 0, 251);
/// The largest datagram read.
pub const MAX_LEN: usize = 9000;
/// The TXT format version; others are ignored.
pub const VERSION: u32 = 1;
/// The connection protocol this build speaks (`lcc-peer-v2`, [`crate::secure::PROTOCOL`]), as the
/// advert's `proto` entry. The TXT format version stays, so builds on different protocols still
/// see each other and can say that both need the same version of the app.
pub const PROTO: u32 = 2;

/// The DNS-SD service name (RFC 6335 §5.1: 1–15 of `a-z`, `0-9`, `-`, at least one letter, no
/// leading, trailing or doubled hyphen) made from `env_prefix`.
pub fn service_name(prefix: &str) -> String {
    let mut s = String::new();
    for c in prefix.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else if !s.is_empty() && !s.ends_with('-') {
            s.push('-');
        }
        if s.len() >= 15 {
            break;
        }
    }
    let s = s.trim_matches('-').to_string();
    if s.chars().any(|c| c.is_ascii_lowercase()) { s } else { "app-peer".to_string() }
}

/// `_<svc>._tcp.local` for this app: `<svc>` follows the Android package's last segment
/// (`android_package` in brand.toml, e.g. a `com.example.SomeApp` package → `someapp`), so the
/// app has one network identity on every platform. Devices only find each other when they
/// advertise the same type, so a renamed package needs every device on the new version.
pub fn service_type() -> String {
    format!("_{}._tcp.local", service_name(package_segment(lcc_brand::ANDROID_PACKAGE)))
}

/// The last dot-separated segment of a package name.
fn package_segment(package: &str) -> &str {
    package.rsplit('.').next().unwrap_or(package)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Beacon {
    /// Which app sent it (`env_prefix` from brand.toml): other apps' adverts are ignored.
    pub app: String,
    pub v: u32,
    pub device_id: String,
    pub name: String,
    /// Ed25519 public key, base64.
    pub key: String,
    /// The TCP port it accepts connections on.
    pub port: u16,
    /// It is looking for devices to pair with (its Settings ▸ Sync is open).
    pub pairing: bool,
    /// The connection protocol it speaks ([`PROTO`]); 1 when the advert doesn't say (an older
    /// build). Devices on another protocol are listed, but can't be paired with or connected to.
    pub proto: u32,
}

impl Beacon {
    /// The TXT record strings.
    pub fn txt(&self) -> Vec<Vec<u8>> {
        [
            format!("v={}", self.v),
            format!("app={}", self.app),
            format!("id={}", self.device_id),
            format!("name={}", self.name),
            format!("key={}", self.key),
            format!("pairing={}", u8::from(self.pairing)),
            format!("proto={}", self.proto),
        ]
        .into_iter()
        .map(String::into_bytes)
        .filter(|s| s.len() <= 255)
        .collect()
    }

    /// An advert of this app and version from a TXT record and the SRV port; anything else is `None`.
    pub fn from_txt(txt: &[Vec<u8>], port: u16) -> Option<Beacon> {
        let get = |key: &str| {
            txt.iter().find_map(|s| {
                let s = std::str::from_utf8(s).ok()?;
                let (k, v) = s.split_once('=')?;
                k.eq_ignore_ascii_case(key).then(|| v.to_string())
            })
        };
        let b = Beacon {
            app: get("app")?,
            v: get("v")?.parse().ok()?,
            device_id: get("id")?.to_ascii_lowercase(),
            name: get("name").unwrap_or_default(),
            key: get("key")?,
            port,
            pairing: get("pairing").as_deref() == Some("1"),
            proto: get("proto").and_then(|p| p.parse().ok()).unwrap_or(1),
        };
        b.sane().then_some(b)
    }

    fn sane(&self) -> bool {
        self.app == lcc_brand::ENV_PREFIX
            && self.v == VERSION
            && self.port != 0
            && (1..=64).contains(&self.device_id.len())
            && self.device_id.chars().all(|c| c.is_ascii_hexdigit())
            && self.name.chars().count() <= crate::identity::MAX_NAME
            && !self.name.chars().any(char::is_control)
            && (1..=64).contains(&self.key.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Beacon {
        Beacon {
            app: lcc_brand::ENV_PREFIX.into(),
            v: VERSION,
            device_id: "ab12".into(),
            name: "Tablet = mine".into(),
            key: "k".into(),
            port: 4000,
            pairing: true,
            proto: PROTO,
        }
    }

    #[test]
    fn an_advert_without_a_protocol_is_an_older_build() {
        let txt: Vec<Vec<u8>> = sample().txt().into_iter().filter(|s| !s.starts_with(b"proto=")).collect();
        assert_eq!(Beacon::from_txt(&txt, 4000).map(|b| b.proto), Some(1));
        for junk in ["proto=", "proto=x", "proto=-1", "proto=99999999999999999999"] {
            let mut t = txt.clone();
            t.push(junk.as_bytes().to_vec());
            assert_eq!(Beacon::from_txt(&t, 4000).map(|b| b.proto), Some(1), "{junk}");
        }
    }

    #[test]
    fn adverts_round_trip_and_service_names_follow_rfc_6335() {
        // round trips
        {
            assert_eq!(Beacon::from_txt(&sample().txt(), 4000), Some(sample()));
        }
        // service names follow rfc 6335
        {
            for (prefix, want) in
                [("ALPHA_BETA_GAMMA_DELTA", "alpha-beta-gamm"), ("APP", "app"), ("__A__B__", "a-b"), ("123", "app-peer"), ("", "app-peer")]
            {
                assert_eq!(service_name(prefix), want, "{prefix}");
            }
            for prefix in [lcc_brand::ENV_PREFIX, "X_Y_Z_LONG_LONG_LONG_NAME", "A-_-B", "ÄÖÜ_é", "ABCDEFGHIJKLMN_O"] {
                let s = service_name(prefix);
                assert!((1..=15).contains(&s.len()), "{s}");
                assert!(s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'), "{s}");
                assert!(!s.starts_with('-') && !s.ends_with('-') && !s.contains("--"), "{s}");
            }
        }
        // the service follows the android package
        {
            assert_eq!(service_name(package_segment("com.example.SomeApp")), "someapp");
            assert_eq!(service_name(package_segment("Plain")), "plain");
            let t = service_type();
            assert!(t.starts_with('_') && t.ends_with("._tcp.local"), "{t}");
        }
    }

    #[test]
    fn hostile_adverts_are_ignored() {
        let mut other = sample();
        other.app = "SOMEONE_ELSE".into();
        let mut bad_id = sample();
        bad_id.device_id = "../../etc".into();
        let mut no_key = sample();
        no_key.key = String::new();
        let mut ctrl = sample();
        ctrl.name = "a\u{7}".into();
        for b in [other, bad_id, no_key, ctrl] {
            assert_eq!(Beacon::from_txt(&b.txt(), 4000), None);
        }
        assert_eq!(Beacon::from_txt(&sample().txt(), 0), None);
        for txt in [vec![], vec![b"\xff\xfe".to_vec()], vec![b"v=x".to_vec()], vec![b"=".to_vec(); 300]] {
            assert_eq!(Beacon::from_txt(&txt, 4000), None);
        }
    }
}
