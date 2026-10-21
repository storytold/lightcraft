//! Bounded ComfyUI search index. Graphs are discarded after extraction; values are lower-cased once.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const JSON_LIMIT: usize = 4 * 1024 * 1024;
const INDEX_LIMIT: usize = 64 * 1024;
const STRING_LIMIT: usize = 8192;
const VISIT_LIMIT: usize = 30_000;

/// Compact search values, grouped by semantic field. `prompt` contains unclassified prompt text;
/// `positive` and `negative` contain text reachable from those conditioning inputs.
/// Unknown node parameters remain searchable in `other`. No original workflow JSON is retained.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ComfyMetadata {
    pub fields: BTreeMap<String, Vec<String>>,
}

impl ComfyMetadata {
    pub fn values(&self, field: &str) -> impl Iterator<Item = &str> {
        self.fields.get(field).into_iter().flatten().map(String::as_str)
    }

    /// `text` must already be lower-case, as catalog search tokens are.
    pub fn contains(&self, text: &str) -> bool {
        self.fields.iter().any(|(key, values)| key.contains(text) || values.iter().any(|v| v.contains(text)))
    }
}

#[derive(Default)]
pub(crate) struct PngIndex {
    fields: BTreeMap<String, BTreeSet<String>>,
    bytes: usize,
    visits: usize,
    original: bool,
    found: bool,
}

impl PngIndex {
    pub(crate) fn field(&mut self, field: &crate::PngTextField) {
        if !matches!(field.name.as_str(), "prompt" | "workflow") {
            return;
        }
        let Ok(graph) = serde_json::from_str::<Value>(&field.value) else { return };
        self.graph(&graph);
    }

    fn add(&mut self, field: &str, text: &str) {
        let text = text.trim();
        if text.is_empty() || self.bytes >= INDEX_LIMIT {
            return;
        }
        let text = if self.original { std::borrow::Cow::Borrowed(text) } else { std::borrow::Cow::Owned(text.to_lowercase()) };
        let text: String = text
            .chars()
            .scan(0, |bytes, c| {
                *bytes += c.len_utf8();
                (*bytes <= STRING_LIMIT).then_some(c)
            })
            .collect();
        if self.bytes + field.len() + text.len() > INDEX_LIMIT {
            return;
        }
        let values = self.fields.entry(field.to_string()).or_default();
        let len = text.len();
        if values.insert(text) {
            self.bytes += field.len() + len;
        }
    }

    fn parameter(&mut self, key: &str, value: &Value, role: &str, depth: usize) {
        if depth > 32 || self.visits >= VISIT_LIMIT || self.bytes >= INDEX_LIMIT {
            return;
        }
        self.visits += 1;
        let key = key.to_lowercase();
        let field = match key.as_str() {
            "positive" | "positive_prompt" => "positive",
            "negative" | "negative_prompt" => "negative",
            "text" | "text_g" | "text_l" | "prompt" => role,
            "ckpt_name" | "checkpoint" | "checkpoint_name" | "model" | "model_name" | "unet_name" | "clip_name" => "model",
            "lora" | "lora_name" => "lora",
            "sampler_name" | "sampler" => "sampler",
            "scheduler" => "scheduler",
            "seed" | "noise_seed" => "seed",
            "steps" => "steps",
            "cfg" | "guidance" => "cfg",
            "denoise" => "denoise",
            "vae" | "vae_name" => "vae",
            "width" => "width",
            "height" => "height",
            _ if key.starts_with("clip_name") => "model",
            _ => "other",
        };
        match value {
            Value::String(v) => {
                self.add(field, v);
                if field == "other" {
                    self.add("other", &key);
                }
            }
            Value::Number(v) => {
                self.add(field, &v.to_string());
                if field == "other" {
                    self.add("other", &key);
                }
            }
            Value::Bool(v) => self.add("other", &format!("{key}={v}")),
            Value::Object(o) => {
                for (k, v) in o {
                    self.parameter(k, v, role, depth + 1);
                }
            }
            Value::Array(a) => {
                for v in a {
                    self.parameter(&key, v, role, depth + 1);
                }
            }
            Value::Null => {}
        }
    }

    fn graph(&mut self, graph: &Value) {
        // UI workflows describe widget positions; API graphs describe named inputs.
        let mut nodes = BTreeMap::new();
        if let Some(ui) = graph.get("nodes").and_then(Value::as_array) {
            for node in ui.iter().take(VISIT_LIMIT) {
                let Some(kind) = node.get("type").and_then(Value::as_str) else { continue };
                let Some(id) = node.get("id") else { continue };
                let mut inputs = serde_json::Map::new();
                if let Some(widgets) = node.get("widgets_values") {
                    if let Some(named) = widgets.as_object() {
                        inputs.extend(named.clone());
                    } else if let Some(values) = widgets.as_array() {
                        let names = widget_names(kind);
                        for (i, value) in values.iter().enumerate().take(VISIT_LIMIT) {
                            let name = names.get(i).copied().unwrap_or("other");
                            if name == "other" {
                                self.parameter(name, value, "other", 0);
                            } else {
                                inputs.insert(name.to_string(), value.clone());
                            }
                        }
                    }
                }
                for input in node.get("inputs").and_then(Value::as_array).into_iter().flatten() {
                    let Some(name) = input.get("name").and_then(Value::as_str) else { continue };
                    if let Some(link) = input.get("link").filter(|v| !v.is_null()) {
                        inputs.insert(name.into(), serde_json::json!([link, 0]));
                    }
                }
                self.parameter("other", &node["properties"], "other", 0);
                self.parameter("other", &node["title"], "other", 0);
                nodes.insert(id_string(id), Node { kind: kind.to_string(), inputs });
            }
            // Replace UI link ids with their source node ids, just like an API connection.
            let links: BTreeMap<_, _> = graph
                .get("links")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|l| Some((id_string(l.get(0).or_else(|| l.get("id"))?), l.get(1).or_else(|| l.get("origin_id"))?.clone())))
                .collect();
            for node in nodes.values_mut() {
                for value in node.inputs.values_mut() {
                    if let Some(source) = value.get(0).and_then(|id| links.get(&id_string(id))) {
                        *value = serde_json::json!([source, 0]);
                    }
                }
            }
        } else if let Some(api) = graph.as_object() {
            for (id, node) in api.iter().take(VISIT_LIMIT) {
                let (Some(kind), Some(inputs)) = (node.get("class_type").and_then(Value::as_str), node.get("inputs").and_then(Value::as_object))
                else {
                    continue;
                };
                self.parameter("other", &node["_meta"], "other", 0);
                nodes.insert(id.clone(), Node { kind: kind.to_string(), inputs: inputs.clone() });
            }
        }
        if nodes.is_empty() {
            return;
        }
        self.found = true;
        let mut roles: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
        for node in nodes.values() {
            for role in ["positive", "negative"] {
                if let Some(id) = node.inputs.get(role).and_then(connection) {
                    let mut pending = vec![id];
                    while let Some(id) = pending.pop() {
                        if let Some(node) = nodes.get(&id) {
                            if !roles.entry(id).or_default().insert(role) {
                                continue;
                            }
                            pending.extend(node.inputs.values().filter_map(connection));
                        }
                    }
                }
            }
        }
        for (id, node) in &nodes {
            self.add("node", &node.kind);
            for (key, value) in &node.inputs {
                if connection(value).is_some_and(|id| nodes.contains_key(&id)) {
                    continue;
                }
                if let Some(roles) = roles.get(id) {
                    for role in roles {
                        self.parameter(key, value, role, 0);
                    }
                } else {
                    self.parameter(key, value, "prompt", 0);
                }
            }
        }
        for key in ["extra", "groups"] {
            self.parameter("other", &graph[key], "other", 0);
        }
    }

    pub(crate) fn finish(mut self, dimensions: Option<(u32, u32)>) -> Option<ComfyMetadata> {
        if !self.found {
            return None;
        }
        if let Some((w, h)) = dimensions {
            self.add("width", &w.to_string());
            self.add("height", &h.to_string());
        }
        Some(ComfyMetadata { fields: self.fields.into_iter().map(|(k, v)| (k, v.into_iter().collect())).collect() })
    }
}

struct Node {
    kind: String,
    inputs: serde_json::Map<String, Value>,
}

fn id_string(v: &Value) -> String {
    v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())
}

fn connection(v: &Value) -> Option<String> {
    let a = v.as_array()?;
    let first = a.first()?;
    (a.len() == 2 && a.get(1)?.is_u64() && (first.is_string() || first.is_u64())).then(|| id_string(first))
}

// shortcut: positional widgets are named only for standard nodes; custom widget values remain
// free-text searchable until the workflow supplies named widgets or a mapping is added here.
fn widget_names(kind: &str) -> &'static [&'static str] {
    match kind {
        "CLIPTextEncode" => &["text"],
        "CLIPTextEncodeSDXL" => &["width", "height", "crop_w", "crop_h", "target_width", "target_height", "text_g", "text_l"],
        "CheckpointLoaderSimple" => &["ckpt_name"],
        "CheckpointLoader" => &["config_name", "ckpt_name"],
        "UNETLoader" => &["unet_name", "weight_dtype"],
        "CLIPLoader" => &["clip_name", "type", "device"],
        "DualCLIPLoader" => &["clip_name1", "clip_name2", "type", "device"],
        "TripleCLIPLoader" => &["clip_name1", "clip_name2", "clip_name3"],
        "LoraLoader" => &["lora_name", "strength_model", "strength_clip"],
        "LoraLoaderModelOnly" => &["lora_name", "strength_model"],
        "VAELoader" => &["vae_name"],
        "KSampler" => &["seed", "control_after_generate", "steps", "cfg", "sampler_name", "scheduler", "denoise"],
        "KSamplerAdvanced" => &[
            "add_noise",
            "noise_seed",
            "control_after_generate",
            "steps",
            "cfg",
            "sampler_name",
            "scheduler",
            "start_at_step",
            "end_at_step",
            "return_with_leftover_noise",
        ],
        "EmptyLatentImage" | "EmptySD3LatentImage" => &["width", "height", "batch_size"],
        _ => &[],
    }
}

/// Readable generation fields preserve original case; raw graphs remain in the PNG text group.
pub fn comfy_display_fields(fields: &[crate::PngTextField], dimensions: Option<(u32, u32)>) -> BTreeMap<String, Vec<String>> {
    let mut index = PngIndex { original: true, ..Default::default() };
    let mut total = 0usize;
    for field in fields.iter().take(32).filter(|f| matches!(f.name.as_str(), "prompt" | "workflow")) {
        total = total.saturating_add(field.value.len());
        if field.value.len() > JSON_LIMIT || total > 2 * JSON_LIMIT {
            continue;
        }
        if let Ok(graph) = serde_json::from_str::<Value>(&field.value) {
            index.graph(&graph);
        }
    }
    index.finish(dimensions).map(|m| m.fields).unwrap_or_default()
}
