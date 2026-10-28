//! ComfyUI's documented HTTP API. No Python, custom nodes, shell or browser automation.
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::Settings;

const JSON_LIMIT: u64 = 4 << 20;
const IMAGE_LIMIT: u64 = 32 << 20;

pub(super) struct Client {
    agent: ureq::Agent,
    endpoint: String,
}

impl Client {
    pub fn new(endpoint: &str) -> Result<Self, String> {
        let endpoint = endpoint.trim().trim_end_matches('/');
        let uri: ureq::http::Uri = endpoint.parse().map_err(|_| "invalid ComfyUI URL")?;
        if !matches!(uri.scheme_str(), Some("http" | "https"))
            || uri.host().is_none()
            || uri.authority().is_none_or(|a| a.as_str().contains('@'))
            || uri.query().is_some()
            || endpoint.contains('#')
            || endpoint.len() > 2048
        {
            return Err("use an http:// or https:// ComfyUI URL without credentials, query or fragment".into());
        }
        let agent = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(30)))
            .tls_config(ureq::tls::TlsConfig::builder().unversioned_rustls_crypto_provider(Arc::new(rustls_rustcrypto::provider())).build())
            .build()
            .new_agent();
        Ok(Self { agent, endpoint: endpoint.to_string() })
    }

    fn url(&self, route: &str) -> String {
        format!("{}{route}", self.endpoint)
    }

    fn post(&self, route: &str, body: &Value) -> Result<Value, String> {
        self.agent
            .post(self.url(route))
            .send_json(body)
            .and_then(|mut r| r.body_mut().with_config().limit(JSON_LIMIT).read_json())
            .map_err(|e| format!("ComfyUI {route}: {e}"))
    }

    /// Return the server's actual filename, which can differ if another client used the name.
    pub fn upload(&self, name: &str, png: &[u8]) -> Result<String, String> {
        // The boundary must not occur in the PNG's compressed bytes.
        let mut boundary = "LightcraftMaskingUpload".to_string();
        while png.windows(boundary.len()).any(|w| w == boundary.as_bytes()) {
            boundary.push('x');
        }
        let mut body =
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"{name}\"\r\nContent-Type: image/png\r\n\r\n")
                .into_bytes();
        body.extend_from_slice(png);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let v: Value = self
            .agent
            .post(self.url("/upload/image"))
            .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
            .send(&body)
            .and_then(|mut r| r.body_mut().with_config().limit(JSON_LIMIT).read_json())
            .map_err(|e| format!("ComfyUI upload: {e}"))?;
        let name = v.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or("ComfyUI upload returned no filename")?;
        let dir = v.get("subfolder").and_then(Value::as_str).unwrap_or("");
        Ok(if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") })
    }

    pub fn generate(&self, settings: &Settings, image: &str, mask: &str, cancelled: impl Fn() -> bool) -> Result<Vec<u8>, String> {
        let graph = workflow(settings, image, mask)?;
        if cancelled() {
            return Err("generation cancelled".into());
        }
        let submitted = self.post("/prompt", &json!({"prompt": graph}))?;
        if submitted.get("node_errors").and_then(Value::as_object).is_some_and(|e| !e.is_empty()) {
            return Err(format!("ComfyUI workflow validation failed: {}", submitted["node_errors"]));
        }
        let id = submitted
            .get("prompt_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 128 && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'))
            .ok_or_else(|| format!("ComfyUI did not accept the workflow: {submitted}"))?;
        let started = Instant::now();
        loop {
            if cancelled() || started.elapsed() > Duration::from_secs(600) {
                // Delete only our queued job. /interrupt would stop another client's current job.
                let _ = self.post("/queue", &json!({"delete": [id]}));
                return Err(if cancelled() { "generation cancelled" } else { "ComfyUI timed out after 10 minutes" }.into());
            }
            let history: Value = self
                .agent
                .get(self.url(&format!("/history/{id}")))
                .call()
                .and_then(|mut r| r.body_mut().with_config().limit(JSON_LIMIT).read_json())
                .map_err(|e| format!("ComfyUI history: {e}"))?;
            if let Some(job) = history.get(id) {
                if job.pointer("/status/status_str").and_then(Value::as_str) == Some("error") {
                    return Err(format!("ComfyUI generation failed: {}", job.get("status").unwrap_or(&Value::Null)));
                }
                if job.pointer("/status/completed").and_then(Value::as_bool) == Some(true) {
                    let out = job.pointer("/outputs/10/images/0").ok_or("ComfyUI completed without a Lightcraft-masking output image")?;
                    let name = out.get("filename").and_then(Value::as_str).ok_or("ComfyUI output has no filename")?;
                    let dir = out.get("subfolder").and_then(Value::as_str).unwrap_or("");
                    let kind = out.get("type").and_then(Value::as_str).unwrap_or("output");
                    return self
                        .agent
                        .get(self.url("/view"))
                        .query("filename", name)
                        .query("subfolder", dir)
                        .query("type", kind)
                        .call()
                        .and_then(|mut r| r.body_mut().with_config().limit(IMAGE_LIMIT).read_to_vec())
                        .map_err(|e| format!("ComfyUI output: {e}"));
                }
            }
            std::thread::sleep(Duration::from_millis(300));
        }
    }
}

/// Stable bindings for our original, bundled native-node workflow; the browser workflow is
/// a standalone demonstration. Save raw decoded pixels: LightCraft composites exactly once.
pub(super) fn workflow(settings: &Settings, image: &str, mask: &str) -> Result<Value, String> {
    // The supported GGUF preset is Qwen Image 2.1; safetensors checkpoints retain the SDXL
    // workflow. Both expose the same image/mask/seed/output bindings to the caller.
    let qwen = settings.model.to_ascii_lowercase().ends_with(".gguf");
    let template = if qwen { include_str!("Lightcraft-masking-qwen21-api.json") } else { include_str!("Lightcraft-masking-api.json") };
    let mut v: Value = serde_json::from_str(template).map_err(|e| e.to_string())?;
    let (model, positive, negative, prompt) = if qwen {
        (
            "/1/inputs/unet_name",
            "/4/inputs/prompt",
            "/4/inputs/negative_prompt",
            format!(
                "Replace the entire white region shown in <image2> of <image1> with the following: {}. \
                Remove the existing objects in that white region and reconstruct it as the requested replacement. \
                Black regions are context: keep them unchanged. Return an opaque photograph.",
                settings.prompt
            ),
        )
    } else {
        ("/1/inputs/ckpt_name", "/4/inputs/text", "/5/inputs/text", settings.prompt.clone())
    };
    for (path, value) in [
        (model, json!(settings.model)),
        ("/2/inputs/image", json!(image)),
        ("/3/inputs/image", json!(mask)),
        (positive, json!(prompt)),
        (negative, json!(settings.negative)),
        ("/7/inputs/seed", json!(settings.seed)),
    ] {
        *v.pointer_mut(path).ok_or("invalid bundled ComfyUI workflow")? = value;
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ambiguous_endpoints_and_binds_inputs() {
        for url in
            ["file:///tmp/test", "http://user:secret@localhost:8188", "http://localhost:8188?token=x", "http://localhost/#secret", "localhost:8188"]
        {
            assert!(Client::new(url).is_err(), "{url}");
        }
        assert!(Client::new("http://127.0.0.1:8188").is_ok());
        let s = Settings { prompt: "blue vase".into(), seed: 99, ..Default::default() };
        let v = workflow(&s, "photo.png", "mask.png").unwrap();
        assert_eq!(v.pointer("/4/inputs/text"), Some(&json!("blue vase")));
        assert_eq!(v.pointer("/3/inputs/channel"), Some(&json!("red")));
        assert_eq!(v.pointer("/7/inputs/seed"), Some(&json!(99)));
        assert_eq!(v.pointer("/10/inputs/images"), Some(&json!(["8", 0])));
    }

    #[test]
    fn qwen21_binds_edit_reference_mask_and_installed_gguf_without_sdxl_nodes() {
        let s = Settings {
            model: "qwen-image-2.1-UC-BF16.gguf".into(),
            prompt: "replace the block with a blue vase".into(),
            seed: 43,
            ..Default::default()
        };
        let v = workflow(&s, "photo.png", "mask.png").unwrap();
        assert_eq!(v.pointer("/1/inputs/unet_name"), Some(&json!(s.model)));
        assert_eq!(v.pointer("/4/inputs/images.image_1"), Some(&json!(["2", 0])));
        assert_eq!(v.pointer("/4/inputs/images.image_2"), Some(&json!(["17", 0])));
        assert_eq!(v.pointer("/17/class_type"), Some(&json!("MaskToImage")));
        assert_eq!(v.pointer("/17/inputs/mask"), Some(&json!(["3", 0])));
        assert!(v.pointer("/4/inputs/prompt").unwrap().as_str().unwrap().contains(&s.prompt));
        assert!(v.pointer("/4/inputs/prompt").unwrap().as_str().unwrap().contains("Replace the entire white region"));
        assert_eq!(v.pointer("/3/inputs/image"), Some(&json!("mask.png")));
        assert_eq!(v.pointer("/9/inputs/mask"), Some(&json!(["3", 0])));
        assert_eq!(v.pointer("/9/inputs/samples"), Some(&json!(["6", 0])));
        assert_eq!(v.pointer("/6/class_type"), Some(&json!("VAEEncode")));
        assert_eq!(v.pointer("/7/inputs/seed"), Some(&json!(43)));
        assert_eq!(v.pointer("/7/inputs/cfg"), Some(&json!(1.0)));
        assert_eq!(v.pointer("/7/inputs/steps"), Some(&json!(25)));
        assert_eq!(v.pointer("/10/inputs/images"), Some(&json!(["15", 0])));
        assert_eq!(v.pointer("/15/class_type"), Some(&json!("SplitImageWithAlpha")));
        assert!(!v.to_string().contains("CheckpointLoaderSimple"));
    }
}
