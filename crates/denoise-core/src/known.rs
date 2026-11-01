//! The denoise models LightCraft knows about: what each one is, who may use it, and where to fetch it.
//!
//! None is part of LightCraft. A model with an address here can be downloaded when the user asks and has accepted
//! its terms; anything else is brought by the user as an `.onnx` file with a `denoise-model.json` beside it.

#[cfg(any(feature = "rawnind-model", feature = "lightcraft-model", test))]
use crate::licence::{Commercial, Licence};

use crate::manifest::DenoiserManifest;
#[cfg(any(feature = "rawnind-model", feature = "lightcraft-model", test))]
use crate::manifest::{Domain, Gain};

/// A known model, and how to get it.
pub struct Known {
    pub manifest: DenoiserManifest,
    /// What to fetch (an archive or the `.onnx` itself); `None` when the user must bring the file.
    pub download: Option<Download>,
    /// The `.onnx` inside the downloaded archive (`None`: the download is the `.onnx`).
    pub entry: Option<&'static str>,
}

/// RawNIND's UtNet2 for Bayer sensors, as published for darktable (`rawdenoise-nind.dtmodel`, a zip with a Bayer and a
/// linear model). Its weights are GPL-3.0; the training pictures are CC BY 4.0 and CC0.
#[cfg(any(feature = "rawnind-model", test))]
fn rawnind_bayer() -> Known {
    let manifest = DenoiserManifest {
        id: "rawnind-bayer".into(),
        name: "RawNIND UtNet2 (Bayer)".into(),
        version: "1.0".into(),
        licence: Licence {
            name: "GPL-3.0".into(),
            commercial: Commercial::Yes,
            url: Some("https://www.gnu.org/licenses/gpl-3.0.html".into()),
            notice: "The weights are published under the GNU General Public License v3. LightCraft does not include them: they \
                     are downloaded to your computer at your request and used by it only. If you pass the model file on, the \
                     GPL applies to that copy. Commercial use is subject to the GPL terms, including applicable source, licence and notice obligations on redistribution."
                .into(),
        },
        source: Some("https://github.com/darktable-org/darktable-ai/tree/master/models/rawdenoise-nind".into()),
        sha256: Some("da27509dab6a2915da67e988acd86cf71f9d5bbc8d1aa0ed32933578a887b901".into()),
        size_bytes: Some(31_056_425),
        provenance: "Trained on RawNIND, real photographs of the same scenes taken noisy and clean (images CC BY 4.0 and CC0, \
                     published on Wikimedia Commons and by the University of Louvain). Paper: arXiv 2501.08924."
            .into(),
        domain: Domain::BayerToRgb,
        tile: 512,
        overlap: 64,
        gain: Gain::MatchMean { nominal: 1.0e6, max_deviation: 0.05 },
    };
    let download = Download {
        id: manifest.id.clone(),
        url: "https://github.com/darktable-org/darktable-ai/releases/download/release-5.6.0/rawdenoise-nind.dtmodel".into(),
        fallbacks: Vec::new(),
        file_name: "rawdenoise-nind.dtmodel".into(),
        size_bytes: 57_700_134,
        sha256: "d71b5f1e727c85a359e6f74dca9e2016c9d8fc3e2f7ac3e9b347d80ceca969af".into(),
    };
    Known { manifest, download: Some(download), entry: Some("model_bayer.onnx") }
}

/// Where LightCraft's own model is downloaded from first: a release asset of the project (the maintainers upload the
/// file there; until they do, this address fails and the next one is used).
pub const LIGHTCRAFT_BAYER_URL: &str = "https://github.com/storytold/lightcraft/releases/download/denoise-model-0.1.0/lightcraft-bayer-0.1.0.onnx";

/// The second source, used when the first fails: radfilms.com.au, the trainer's site, serving the same file. Its size
/// and SHA-256 (below) are checked whichever source served it.
pub const LIGHTCRAFT_BAYER_FALLBACK_URL: &str = "https://radfilms.com.au/lightcraft/models/lightcraft-bayer-0.1.0.onnx";

/// The environment variable that replaces both sources with one (an `https://` address), so a build or a maintainer
/// can host the same file elsewhere. There is no other source.
pub const LIGHTCRAFT_BAYER_URL_ENV: &str = "LIGHTCRAFT_DENOISE_MODEL_URL";

/// The addresses LightCraft's own model is fetched from, in order: [`LIGHTCRAFT_BAYER_URL_ENV`] alone when it holds
/// an `https://` address, else [`LIGHTCRAFT_BAYER_URL`] then [`LIGHTCRAFT_BAYER_FALLBACK_URL`].
pub fn lightcraft_bayer_urls() -> Vec<String> {
    urls_or_default(std::env::var(LIGHTCRAFT_BAYER_URL_ENV).ok().as_deref())
}

/// (An address with a `user:password@` is not taken: the download refuses one, and the address replaces both sources.)
fn urls_or_default(set: Option<&str>) -> Vec<String> {
    let usable = |u: &str| {
        let authority = u.strip_prefix("https://").and_then(|r| r.split(['/', '?', '#']).next()).unwrap_or("");
        !authority.is_empty() && !authority.contains('@') && !u.contains(char::is_whitespace)
    };
    match set.map(str::trim) {
        Some(u) if usable(u) => vec![u.to_string()],
        _ => vec![LIGHTCRAFT_BAYER_URL.to_string(), LIGHTCRAFT_BAYER_FALLBACK_URL.to_string()],
    }
}
/// SHA-256 and size of that `.onnx` (`tools/denoise-train/export.py` prints both).
#[cfg(any(feature = "lightcraft-model", test))]
const LIGHTCRAFT_BAYER_SHA256: &str = "abd3c4cdea163b27439cefae8972a21755510bce18ee036e991d20cf3247dea9";
#[cfg(any(feature = "lightcraft-model", test))]
const LIGHTCRAFT_BAYER_BYTES: u64 = 31_091_398;

/// LightCraft's own Bayer denoiser: a U-Net trained by LightCraft contributors with `tools/denoise-train` on CC0 raw
/// samples from raw.pixls.us with synthetic, calibrated sensor noise. The weights are MIT OR Apache-2.0, like
/// LightCraft; the download is the `.onnx` itself.
#[cfg(any(feature = "lightcraft-model", test))]
fn lightcraft_bayer() -> Known {
    lightcraft_bayer_from(lightcraft_bayer_urls())
}

/// [`lightcraft_bayer`] fetched from `urls`, in order (the first is the download's address, the rest its fallbacks).
#[cfg(any(feature = "lightcraft-model", test))]
fn lightcraft_bayer_from(urls: Vec<String>) -> Known {
    let manifest = DenoiserManifest {
        id: "lightcraft-bayer".into(),
        name: "LightCraft Denoise (Bayer)".into(),
        version: "0.1.0".into(),
        licence: Licence {
            name: "MIT OR Apache-2.0".into(),
            commercial: Commercial::Yes,
            url: Some("https://github.com/storytold/lightcraft/blob/main/LICENSE-MIT".into()),
            notice: "LightCraft's own denoise model, trained by the LightCraft contributors. The weights are licensed MIT OR \
                     Apache-2.0, like LightCraft: you may use them for any purpose, including commercial work, and pass them on \
                     with this notice. Training pictures: low-ISO raw samples from raw.pixls.us, each dedicated to the public \
                     domain (CC0) by its contributor; thank you."
                .into(),
        },
        source: Some("https://github.com/storytold/lightcraft/tree/main/tools/denoise-train".into()),
        sha256: Some(LIGHTCRAFT_BAYER_SHA256.into()),
        size_bytes: Some(LIGHTCRAFT_BAYER_BYTES),
        provenance: "Trained by LightCraft contributors with LightCraft's open training tools (tools/denoise-train) on clean \
                     low-ISO raw photos published on raw.pixls.us under CC0, with synthetic sensor noise calibrated on real \
                     cameras (numbers only). No photos with other licences were used."
            .into(),
        domain: Domain::BayerToRgb,
        tile: 512,
        overlap: 64,
        gain: Gain::None,
    };
    let mut urls = urls.into_iter();
    let download = Download {
        id: manifest.id.clone(),
        url: urls.next().unwrap_or_else(|| LIGHTCRAFT_BAYER_URL.to_string()),
        fallbacks: urls.collect(),
        file_name: "lightcraft-bayer-0.1.0.onnx".into(),
        size_bytes: LIGHTCRAFT_BAYER_BYTES,
        sha256: LIGHTCRAFT_BAYER_SHA256.into(),
    };
    Known { manifest, download: Some(download), entry: None }
}

/// Every model LightCraft knows (none in a default build: each is offered by its own feature).
pub fn all() -> Vec<Known> {
    Vec::from([
        #[cfg(feature = "lightcraft-model")]
        lightcraft_bayer(),
        #[cfg(feature = "rawnind-model")]
        rawnind_bayer(),
    ])
}

/// The known model with this id.
pub fn find(id: &str) -> Option<Known> {
    all().into_iter().find(|k| k.manifest.id == id)
}

/// The known model whose `.onnx` has this SHA-256.
pub fn by_sha256(sha256: &str) -> Option<DenoiserManifest> {
    all().into_iter().map(|k| k.manifest).find(|m| m.sha256.as_deref() == Some(sha256))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::validate;

    #[test]
    fn known_models_are_valid_unique_and_downloaded_over_https() {
        let all = all();
        assert_eq!(all.is_empty(), !cfg!(any(feature = "rawnind-model", feature = "lightcraft-model")));
        validate(&rawnind_bayer().manifest).unwrap();
        validate(&lightcraft_bayer().manifest).unwrap();
        for (i, k) in all.iter().enumerate() {
            validate(&k.manifest).unwrap();
            assert!(all.iter().skip(i + 1).all(|o| o.manifest.id != k.manifest.id), "unique ids");
            assert!(!k.manifest.licence.name.is_empty() && !k.manifest.licence.notice.is_empty(), "terms are shown before use");
            if let Some(d) = &k.download {
                assert_eq!(d.id, k.manifest.id);
                assert!(d.url.starts_with("https://") && d.sha256.len() == 64 && d.size_bytes > 0);
                // a name that cannot leave the staging folder
                assert!(!d.file_name.contains(['/', '\\']) && !d.file_name.starts_with('.'));
            }
            // a model that may not be used commercially is never offered for download
            if k.manifest.licence.commercial == Commercial::No {
                assert!(k.download.is_none(), "{}", k.manifest.id);
            }
        }
        assert_eq!(find("rawnind-bayer").is_some(), cfg!(feature = "rawnind-model"));
        assert_eq!(find("lightcraft-bayer").is_some(), cfg!(feature = "lightcraft-model"));
        assert!(find("nothing").is_none());
        assert_eq!(
            by_sha256("da27509dab6a2915da67e988acd86cf71f9d5bbc8d1aa0ed32933578a887b901").map(|m| m.id),
            if cfg!(feature = "rawnind-model") { Some("rawnind-bayer".into()) } else { None }
        );
    }

    #[test]
    fn lightcraft_model_is_permissive_and_its_download_is_the_onnx_it_names() {
        let k = lightcraft_bayer();
        let (m, d) = (&k.manifest, k.download.as_ref().unwrap());
        crate::manifest::validate(m).unwrap();
        assert_eq!(m.licence.commercial, Commercial::Yes);
        assert_eq!(m.licence.name, "MIT OR Apache-2.0");
        assert!(m.provenance.contains("CC0") && m.licence.notice.contains("CC0"), "the data credit is shown");
        // the download is the model file itself, so its hash and size are the manifest's
        assert!(k.entry.is_none());
        assert_eq!(m.sha256.as_deref(), Some(d.sha256.as_str()));
        assert_eq!(m.size_bytes, Some(d.size_bytes));
        assert!(d.url.starts_with("https://") && d.file_name.ends_with(".onnx"));
        assert_eq!(m.gain, Gain::None);
    }

    #[test]
    fn the_model_comes_from_the_project_then_its_mirror_or_one_address_given() {
        let both = vec![LIGHTCRAFT_BAYER_URL.to_string(), LIGHTCRAFT_BAYER_FALLBACK_URL.to_string()];
        assert_eq!(urls_or_default(None), both);
        assert_eq!(host(LIGHTCRAFT_BAYER_URL), "github.com");
        assert_eq!(host(LIGHTCRAFT_BAYER_FALLBACK_URL), "radfilms.com.au");
        // one address given replaces both: no silent second source
        assert_eq!(urls_or_default(Some("https://example.org/m.onnx")), vec!["https://example.org/m.onnx".to_string()]);
        assert_eq!(urls_or_default(Some("http://example.org/m.onnx")), both, "only https");
        assert_eq!(urls_or_default(Some("https://")), both);
        assert_eq!(urls_or_default(Some("https://a b")), both);
        assert_eq!(urls_or_default(Some("https://user:secret@example.org/m.onnx")), both, "no credentials in the address");
        // (not `lightcraft_bayer()`: it reads LIGHTCRAFT_DENOISE_MODEL_URL, which a maintainer testing a copy of
        // their own may have set)
        let k = lightcraft_bayer_from(urls_or_default(None));
        let d = k.download.as_ref().unwrap();
        assert_eq!((d.url.as_str(), d.fallbacks.as_slice()), (LIGHTCRAFT_BAYER_URL, &[LIGHTCRAFT_BAYER_FALLBACK_URL.to_string()][..]));
        let own = lightcraft_bayer_from(urls_or_default(Some("https://example.org/m.onnx")));
        let d = own.download.as_ref().unwrap();
        assert_eq!((d.url.as_str(), d.fallbacks.len()), ("https://example.org/m.onnx", 0), "one address given: no other source");
        // the terms and the status name every site the file may come from (CodeRabbit on #2929)
        assert_eq!(hosts(k.download.as_ref().unwrap()), "github.com or radfilms.com.au");
        assert_eq!(hosts(d), "example.org");
        // an address's user name and password are never shown with its host
        assert_eq!(host("https://user:secret@example.org/m.onnx"), "example.org");
        assert_eq!(host("https://example.org?x=1"), "example.org");
        // the same file wherever it comes from: both end in its name
        assert!(both.iter().all(|u| u.ends_with(&format!("/{}", d.file_name))));
        assert!(!lightcraft_bayer().manifest.licence.notice.contains("  "), "one space between words");
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Download {
    pub id: String,
    pub url: String,
    /// Further sources of the same file, tried in order when the one before fails (unreachable, refused, or a file
    /// whose size or SHA-256 isn't this one's). The file is kept only when its SHA-256 is [`Download::sha256`].
    pub fallbacks: Vec<String>,
    /// The name the file is kept under while it waits for the user to accept its terms.
    pub file_name: String,
    pub size_bytes: u64,
    pub sha256: String,
}

/// The host an `https://` address names, for display: never any `user:password@` in front of it (an address given in
/// `LIGHTCRAFT_DENOISE_MODEL_URL` could carry one, and the model list and download status show the host).
pub fn host(url: &str) -> &str {
    url.strip_prefix("https://")
        .and_then(|r| r.split(['/', '?', '#']).next())
        .map(|authority| authority.rsplit_once('@').map_or(authority, |(_, host)| host))
        .unwrap_or("")
}

/// Every site `d` may come from, in the order they are tried ("github.com or radfilms.com.au"), so the terms and the
/// download's status name the fallback that may supply the file too.
pub fn hosts(d: &Download) -> String {
    let mut all: Vec<&str> = Vec::new();
    for h in std::iter::once(&d.url).chain(d.fallbacks.iter()).map(|u| host(u)) {
        if !h.is_empty() && !all.contains(&h) {
            all.push(h);
        }
    }
    all.join(" or ")
}
