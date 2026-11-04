//! The denoise and upscaling models LightCraft knows about: what each one is, who may use it, and where to fetch it.
//!
//! None is part of LightCraft. A model with an address here can be downloaded when the user asks and has accepted
//! its terms; anything else is brought by the user as an `.onnx` file with a `denoise-model.json` beside it.

#[cfg(any(feature = "rawnind-model", feature = "realesrgan-model", test))]
use crate::licence::{Commercial, Licence};

use crate::manifest::DenoiserManifest;
#[cfg(any(feature = "rawnind-model", feature = "realesrgan-model", test))]
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
        file_name: "rawdenoise-nind.dtmodel".into(),
        size_bytes: 57_700_134,
        sha256: "d71b5f1e727c85a359e6f74dca9e2016c9d8fc3e2f7ac3e9b347d80ceca969af".into(),
    };
    Known { manifest, download: Some(download), entry: Some("model_bayer.onnx") }
}

/// NIND's U-Net for finished pictures (JPEG, PNG, TIFF …), as published for darktable (`denoise-nind.dtmodel`). It takes
/// and gives gamma-encoded sRGB, so it is the model for photos that have no sensor data left. Its weights are GPL-3.0;
/// the training pictures are CC BY 4.0 and CC0.
#[cfg(any(feature = "rawnind-model", test))]
fn nind_rgb() -> Known {
    let manifest = DenoiserManifest {
        id: "nind-rgb".into(),
        name: "NIND U-Net (finished pictures)".into(),
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
        source: Some("https://github.com/darktable-org/darktable-ai/tree/master/models/denoise-nind".into()),
        sha256: Some("5d615c5026a3c579455b9082f72b91e109adb54f4125b5b5cde45983bd6776b2".into()),
        size_bytes: Some(59_138_823),
        provenance: "Trained on NIND, real photographs of the same scenes taken noisy and clean (images CC BY 4.0 and CC0, \
                     published on Wikimedia Commons). Paper: arXiv 1906.00270."
            .into(),
        domain: Domain::RgbToRgb,
        tile: 768,
        overlap: 64,
        // its pictures come out a little brighter and cooler than they went in: the means are matched again
        gain: Gain::MatchMean { nominal: 1.0, max_deviation: 0.1 },
    };
    let download = Download {
        id: manifest.id.clone(),
        url: "https://github.com/darktable-org/darktable-ai/releases/download/release-5.6.0/denoise-nind.dtmodel".into(),
        file_name: "denoise-nind.dtmodel".into(),
        size_bytes: 55_008_210,
        sha256: "825b3657cbb5193a67432a2f0b44ab86531cc7337f89e8e6c17d93db9665708a".into(),
    };
    Known { manifest, download: Some(download), entry: Some("model.onnx") }
}

/// Real-ESRGAN's compact general-purpose 4× upscaler (`realesr-general-x4v3`, SRVGGNetCompact), as an ONNX export
/// re-hosted on Hugging Face (pinned to a revision). Code and weights are BSD-3-Clause.
#[cfg(any(feature = "realesrgan-model", test))]
fn realesr_general_x4v3() -> Known {
    let manifest = DenoiserManifest {
        id: "realesr-general-x4v3".into(),
        name: "Real-ESRGAN General v3 (4× upscaler)".into(),
        version: "3".into(),
        licence: Licence {
            name: "BSD-3-Clause".into(),
            commercial: Commercial::Yes,
            url: Some("https://github.com/xinntao/Real-ESRGAN/blob/master/LICENSE".into()),
            notice: "Copyright (c) 2021, Xintao Wang. The weights are published under the BSD 3-Clause licence. LightCraft does \
                     not include them: they are downloaded to your computer at your request and used by it only. If you pass the \
                     model file on, keep its copyright notice and licence with it."
                .into(),
        },
        source: Some("https://github.com/xinntao/Real-ESRGAN".into()),
        sha256: Some("09b757accd747d7e423c1d352b3e8f23e77cc5742d04bae958d4eb8082b76fa4".into()),
        size_bytes: Some(4_871_181),
        provenance: "Trained by Xintao Wang et al. (Tencent ARC Lab) on the DF2K (DIV2K and Flickr2K) and OST picture sets, \
                     made blurry, noisy and compressed on purpose (synthetic degradations); those sets are published for \
                     research. Paper: arXiv 2107.10833."
            .into(),
        domain: Domain::RgbToRgb4x,
        tile: 256,
        // its 18 convolutions see 18 pixels far: the quarter of the overlap that counts for nothing (20) covers that
        overlap: 80,
        gain: Gain::None,
    };
    let download = Download {
        id: manifest.id.clone(),
        url: "https://huggingface.co/Heliosoph/realesrgan-onnx/resolve/488e5dda07333179f229a6205d92135eea4c25e9/realesr-general-x4v3.onnx".into(),
        file_name: "realesr-general-x4v3.onnx".into(),
        size_bytes: 4_871_181,
        sha256: "09b757accd747d7e423c1d352b3e8f23e77cc5742d04bae958d4eb8082b76fa4".into(),
    };
    Known { manifest, download: Some(download), entry: None }
}

/// Every model LightCraft knows: each product-policy feature adds its own.
pub fn all() -> Vec<Known> {
    rawnind().into_iter().chain(realesrgan()).collect()
}

#[cfg(feature = "rawnind-model")]
fn rawnind() -> Vec<Known> {
    vec![rawnind_bayer(), nind_rgb()]
}
#[cfg(not(feature = "rawnind-model"))]
fn rawnind() -> Vec<Known> {
    Vec::new()
}

#[cfg(feature = "realesrgan-model")]
fn realesrgan() -> Vec<Known> {
    vec![realesr_general_x4v3()]
}
#[cfg(not(feature = "realesrgan-model"))]
fn realesrgan() -> Vec<Known> {
    Vec::new()
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
        assert_eq!(all.is_empty(), !cfg!(any(feature = "rawnind-model", feature = "realesrgan-model")));
        validate(&rawnind_bayer().manifest).unwrap();
        validate(&nind_rgb().manifest).unwrap();
        validate(&realesr_general_x4v3().manifest).unwrap();
        assert_eq!(nind_rgb().manifest.domain, Domain::RgbToRgb);
        assert_eq!(realesr_general_x4v3().manifest.domain, Domain::RgbToRgb4x);
        assert_eq!(find("realesr-general-x4v3").is_some(), cfg!(feature = "realesrgan-model"));
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
        assert!(find("nothing").is_none());
        assert_eq!(
            by_sha256("da27509dab6a2915da67e988acd86cf71f9d5bbc8d1aa0ed32933578a887b901").map(|m| m.id),
            if cfg!(feature = "rawnind-model") { Some("rawnind-bayer".into()) } else { None }
        );
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Download {
    pub id: String,
    pub url: String,
    /// The name the file is kept under while it waits for the user to accept its terms.
    pub file_name: String,
    pub size_bytes: u64,
    pub sha256: String,
}

pub fn host(url: &str) -> &str {
    url.strip_prefix("https://").and_then(|r| r.split('/').next()).unwrap_or("")
}
