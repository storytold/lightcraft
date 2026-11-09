use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

#[cfg(not(target_arch = "wasm32"))]
fn mock(responses: Vec<(&'static str, Vec<u8>)>) -> (String, std::thread::JoinHandle<Vec<(String, Vec<u8>)>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let thread = std::thread::spawn(move || {
        let mut received = Vec::new();
        for (status, response) in responses {
            let start = Instant::now();
            let mut stream = loop {
                if let Ok((s, _)) = listener.accept() {
                    break s;
                }
                assert!(start.elapsed() < Duration::from_secs(30), "client did not send request {}", received.len());
                std::thread::sleep(Duration::from_millis(10));
            };
            // Accepted sockets can inherit the listener's nonblocking mode on macOS.
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut raw = Vec::new();
            let mut buffer = [0; 8192];
            let end = loop {
                let n = stream.read(&mut buffer).unwrap();
                assert!(n > 0);
                raw.extend_from_slice(&buffer[..n]);
                if let Some(i) = raw.windows(4).position(|b| b == b"\r\n\r\n") {
                    break i + 4;
                }
                assert!(raw.len() < 64 * 1024);
            };
            let headers = String::from_utf8(raw[..end].to_vec()).unwrap();
            let length = headers
                .lines()
                .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                .unwrap_or(0);
            assert!(length < 32 << 20);
            while raw.len() < end + length {
                let n = stream.read(&mut buffer).unwrap();
                assert!(n > 0);
                raw.extend_from_slice(&buffer[..n]);
            }
            received.push((headers, raw[end..end + length].to_vec()));
            write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len()).unwrap();
            stream.write_all(&response).unwrap();
        }
        received
    });
    (endpoint, thread)
}

#[test]
fn validates_prompt_and_model_before_starting() {
    assert!(Settings::default().validate().is_err());
    let s = Settings { prompt: "new background".into(), model: String::new(), ..Default::default() };
    assert!(s.validate().is_err());
}

#[test]
fn qwen_instruction_is_editable_and_must_keep_the_prompt_placeholder() {
    let qwen = Settings { prompt: "a red door".into(), model: "qwen-image-2.1-UC-BF16.gguf".into(), ..Default::default() };
    assert!(qwen.validate().is_ok());
    assert!(qwen.resolved_prompt().starts_with("Replace the entire white region"));
    assert!(qwen.resolved_prompt().contains("with the following: a red door. Remove the existing objects"));
    let custom = Settings { qwen_instruction: "Paint {prompt} into <image2>".into(), ..qwen.clone() };
    assert_eq!(custom.resolved_prompt(), "Paint a red door into <image2>");
    let missing = Settings { qwen_instruction: "Paint something".into(), ..qwen.clone() };
    assert!(missing.validate().unwrap_err().contains("{prompt}"));
    let huge = Settings { qwen_instruction: format!("{{prompt}}{}", "x".repeat(9000)), ..qwen };
    assert!(huge.validate().is_err());
    // SDXL checkpoints get the prompt alone, whatever the Qwen instruction says.
    let sdxl = Settings { prompt: "a red door".into(), qwen_instruction: "Paint {prompt}".into(), ..Default::default() };
    assert_eq!(sdxl.resolved_prompt(), "a red door");
}

#[test]
fn saved_settings_without_an_instruction_get_the_default() {
    let s: Settings = serde_json::from_value(json!({"endpoint": "http://192.168.0.56:8188", "model": "x.gguf", "prompt": "p"})).unwrap();
    assert_eq!(s.qwen_instruction, DEFAULT_QWEN_INSTRUCTION);
    let round: Settings = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
    assert_eq!(round.qwen_instruction, DEFAULT_QWEN_INSTRUCTION);
    assert!(serde_json::to_value(&s).unwrap().get("qwenInstruction").is_some());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn http_error_validation_failure_and_cancellation_are_errors_not_panics() {
    let (url, t) = mock(vec![("500 Internal Server Error", b"failure".to_vec())]);
    assert!(client::Client::new(&url).unwrap().upload("input.png", b"png").unwrap_err().contains("upload"));
    t.join().unwrap();
    let (url, t) = mock(vec![("200 OK", br#"{"node_errors":{"1":{"errors":["missing checkpoint"]}}}"#.to_vec())]);
    let c = client::Client::new(&url).unwrap();
    assert!(c.generate(&client::Pass::fill(&Settings::default()), "a", "b", || false).unwrap_err().contains("validation"));
    t.join().unwrap();
    let (url, t) = mock(vec![("200 OK", br#"{"prompt_id":"test-job"}"#.to_vec()), ("200 OK", b"{}".to_vec())]);
    let count = std::cell::Cell::new(0);
    let error = client::Client::new(&url)
        .unwrap()
        .generate(&client::Pass::fill(&Settings::default()), "a", "b", || {
            count.set(count.get() + 1);
            count.get() >= 2
        })
        .unwrap_err();
    assert!(error.contains("cancelled"));
    let requests = t.join().unwrap();
    assert!(requests[1].0.starts_with("POST /queue "));
    assert_eq!(serde_json::from_slice::<Value>(&requests[1].1).unwrap(), json!({"delete": ["test-job"]}));
    assert!(!requests.iter().any(|(h, _)| h.contains("/interrupt")));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn command_roundtrip_previews_before_accepting_preserves_original_and_stacks_copy() {
    command_roundtrip("Juggernaut-XL_v9_RunDiffusionPhoto_v2.safetensors");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn qwen21_command_roundtrip_previews_before_accepting_preserves_original_and_stacks_copy() {
    command_roundtrip("qwen-image-2.1-UC-BF16.gguf");
}

#[cfg(not(target_arch = "wasm32"))]
fn command_roundtrip(model: &str) {
    let preset = if model.ends_with(".gguf") { "qwen21" } else { "sdxl" };
    let dir = std::env::temp_dir().join(format!("lightcraft-comfyui-test-{}-{preset}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("original.png");
    let mut original = Rgba8::new(64, 64);
    original.data.fill([110, 70, 30, 255]);
    let original_bytes = image::encode(&original).unwrap();
    std::fs::write(&path, &original_bytes).unwrap();
    let mut generated = Rgba8::new(1024, 1024);
    generated.data.fill([10, 20, 240, 255]);
    let (url, thread) = mock(vec![
        ("200 OK", br#"{"name":"actual-image.png","subfolder":"inputs"}"#.to_vec()),
        ("200 OK", br#"{"name":"actual-mask.png"}"#.to_vec()),
        ("200 OK", br#"{"prompt_id":"test-job","node_errors":{}}"#.to_vec()),
        ("200 OK", br#"{"test-job":{"status":{"completed":true,"status_str":"success"},"outputs":{"10":{"images":[{"filename":"result.png","subfolder":"Lightcraft-masking","type":"output"}]}}}}"#.to_vec()),
        ("200 OK", image::encode(&generated).unwrap()),
    ]);
    let mut s = crate::Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [path]})).unwrap();
    let photo = s.active().unwrap();
    s.execute("mask.add", &json!({"kind": "radial", "center": [0.5, 0.5], "rx": 0.2, "ry": 0.2, "feather": 0.0})).unwrap();
    let before = (*s.develop_of(photo).unwrap()).clone();
    let rendered = s.render_now(photo, 64, 64).unwrap().image;
    let settings = json!({"endpoint": url, "model": model, "prompt": "blue vase", "seed": 42});
    s.execute("mask.generate", &settings).unwrap();
    assert_eq!(s.activity.list().len(), 1);
    let deadline = Instant::now();
    while s.generation.as_ref().unwrap().running() {
        assert!(deadline.elapsed() < Duration::from_secs(30), "{:?}", s.generation.as_ref().unwrap().status());
        std::thread::sleep(Duration::from_millis(20));
    }
    let status = s.execute("mask.generationStatus", &json!({})).unwrap();
    assert_eq!(status["ready"], true, "{status}");
    assert!(s.activity.list().is_empty());
    assert_eq!(s.catalog.photos().count(), 1, "preview does not import a file");
    assert_eq!(*s.develop_of(photo).unwrap(), before);
    let mut bright = lightcraft_develop::DevelopSettings::default();
    bright.light.exposure = 2.0;
    s.presets.push(lightcraft_develop::Preset::from_settings(
        "bright-import",
        "Bright",
        "Tests",
        &bright,
        &[lightcraft_develop::SettingsGroup::Light],
    ));
    s.import_defaults.other_preset = Some("bright-import".into());
    let result = s.execute("mask.acceptGeneration", &json!({})).unwrap();
    let new_id = PhotoId(result["id"].as_u64().unwrap());
    assert_eq!(s.catalog.stack_of(new_id).unwrap().top(), new_id);
    assert_eq!(s.catalog.photos().count(), 2);
    assert_eq!(s.develop_of(new_id).unwrap().light.exposure, 0.0, "do not reapply an import preset to an already developed result");
    assert_eq!(s.import_defaults.other_preset.as_deref(), Some("bright-import"));
    assert_eq!(*s.develop_of(photo).unwrap(), before);
    assert_eq!(std::fs::read(&path).unwrap(), original_bytes);
    let png = std::fs::read(result["path"].as_str().unwrap()).unwrap();
    let out = lightcraft_codecs::decode(&png, Default::default()).unwrap().to_srgb8();
    assert_eq!((out.width, out.height), (64, 64));
    assert_eq!(out.data[0], rendered.data[0], "unselected corner stays byte identical");
    assert_ne!(out.data[32 * 64 + 32], rendered.data[32 * 64 + 32]);
    let recipe: Value = serde_json::from_slice(&std::fs::read(result["provenance"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!(recipe["settings"]["seed"], 42);
    assert_eq!(recipe["settings"]["model"], model);
    let requests = thread.join().unwrap();
    let graph: Value = serde_json::from_slice(&requests[2].1).unwrap();
    assert_eq!(graph.pointer("/prompt/2/inputs/image"), Some(&json!("inputs/actual-image.png")));
    assert_eq!(graph.pointer("/prompt/3/inputs/image"), Some(&json!("actual-mask.png")));
    assert!(s.generation.is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn removal_needs_no_prompt_but_bounded_controls_and_instructions() {
    let remove = Settings { mode: Mode::Remove, model: "qwen-image-2.1-UC-BF16.gguf".into(), ..Default::default() };
    assert!(remove.validate().is_ok(), "the subject can come from the mask's name");
    for bad in [
        Settings { context: 0.5, ..remove.clone() },
        Settings { context: f64::NAN, ..remove.clone() },
        Settings { grow: GROW_MAX + 1, ..remove.clone() },
        Settings { detail: -0.1, ..remove.clone() },
        Settings { detail: DETAIL_MAX + 0.01, ..remove.clone() },
        Settings { removal_instruction: " ".into(), ..remove.clone() },
        Settings { refine_instruction: "x".repeat(9000), ..remove.clone() },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
    // Replacement still needs its prompt; removal-only fields do not affect it.
    assert!(Settings { mode: Mode::Replace, context: 0.0, ..remove.clone() }.validate().is_err());
    // Saved settings from before removal existed load as replacement with removal defaults.
    let old: Settings = serde_json::from_value(json!({"model": "x.gguf", "prompt": "p"})).unwrap();
    assert_eq!((old.mode, old.context, old.grow, old.detail), (Mode::Replace, 2.5, 8, 0.35));
    assert_eq!(old.removal_instruction, DEFAULT_REMOVAL_INSTRUCTION);
    assert_eq!(serde_json::to_value(&remove).unwrap()["mode"], "remove");
}

#[test]
fn removal_names_what_it_removes() {
    let qwen = Settings { mode: Mode::Remove, model: "q.gguf".into(), ..Default::default() };
    // Typed text wins; otherwise a named mask says what it selects; "Mask 3" does not.
    assert_eq!(Settings { subject: " the bin ".into(), ..qwen.clone() }.removal_subject("Person in yellow jacket").unwrap(), "the bin");
    // The replacement prompt never leaks into a removal.
    assert_eq!(
        Settings { prompt: "a blue vase".into(), ..qwen.clone() }.removal_subject("Person in yellow jacket").unwrap(),
        "Person in yellow jacket"
    );
    assert_eq!(qwen.removal_subject(" Person in yellow jacket ").unwrap(), "Person in yellow jacket");
    for generic in ["Mask 3", "Mask 12", "", "  "] {
        assert!(qwen.removal_subject(generic).unwrap_err().contains("describe what to remove"), "{generic:?}");
    }
    assert_eq!(qwen.removal_subject("Mask of the bench").unwrap(), "Mask of the bench");
    let named = Settings { subject: "the person in the yellow jacket".into(), ..qwen.clone() };
    assert!(named.resolved_prompt().starts_with("Remove the person in the yellow jacket from <image1>"));
    assert!(!named.resolved_prompt().contains(PROMPT_PLACEHOLDER));
    assert_eq!(named.resolved_refine_prompt(), DEFAULT_REFINE_INSTRUCTION);
    // The removal instruction must say where the subject goes.
    assert!(Settings { removal_instruction: "Erase the white region".into(), ..named.clone() }.validate().unwrap_err().contains("{prompt}"));
    // SDXL is asked for empty background, with what is removed in its negative prompt.
    let sdxl = Settings { mode: Mode::Remove, subject: "trash bins".into(), ..Default::default() };
    assert_eq!(sdxl.resolved_prompt(), SDXL_REMOVAL_PROMPT);
    assert_eq!(sdxl.resolved_refine_prompt(), SDXL_REMOVAL_PROMPT);
    assert!(sdxl.resolved_negative().starts_with("blurry, low quality, text, watermark, trash bins, object, person"));
    assert_eq!(Settings { mode: Mode::Replace, ..sdxl }.resolved_negative(), "blurry, low quality, text, watermark");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn detail_passes_redraw_lightly_with_their_own_seed() {
    use client::{Pass, workflow};
    let sdxl = Settings { mode: Mode::Remove, seed: 7, detail: 0.4, ..Default::default() };
    let v = workflow(&Pass::detail(&sdxl, 2), "tile.png", "mask.png").unwrap();
    assert_eq!(v.pointer("/6/class_type"), Some(&json!("VAEEncode")), "SDXL detail encodes the tile instead of blanking the mask");
    assert_eq!(v.pointer("/9/class_type"), Some(&json!("SetLatentNoiseMask")));
    assert_eq!(v.pointer("/9/inputs/mask"), Some(&json!(["3", 0])));
    assert_eq!(v.pointer("/7/inputs/latent_image"), Some(&json!(["9", 0])));
    assert_eq!(v.pointer("/7/inputs/denoise"), Some(&json!(0.4)));
    assert_eq!(v.pointer("/7/inputs/seed"), Some(&json!(10)));
    assert_eq!(v.pointer("/10/inputs/images"), Some(&json!(["8", 0])));
    assert!(!v.to_string().contains("VAEEncodeForInpaint"));
    // The coarse SDXL fill keeps the inpainting graph at full strength.
    let v = workflow(&Pass::fill(&sdxl), "crop.png", "mask.png").unwrap();
    assert_eq!(v.pointer("/6/class_type"), Some(&json!("VAEEncodeForInpaint")));
    assert_eq!(v.pointer("/7/inputs/denoise"), Some(&json!(1.0)));
    assert_eq!(v.pointer("/4/inputs/text"), Some(&json!(SDXL_REMOVAL_PROMPT)));
    assert!(v.pointer("/5/inputs/text").unwrap().as_str().unwrap().contains("object, person"));
    // Qwen's graph already encodes its input: detail binds strength and the detail instruction.
    let qwen = Settings { model: "q.gguf".into(), ..sdxl };
    let v = workflow(&Pass::detail(&qwen, 0), "tile.png", "mask.png").unwrap();
    assert_eq!(v.pointer("/7/inputs/denoise"), Some(&json!(0.4)));
    assert_eq!(v.pointer("/7/inputs/seed"), Some(&json!(8)));
    assert_eq!(v.pointer("/4/inputs/prompt"), Some(&json!(DEFAULT_REFINE_INSTRUCTION)));
    // Seeds wrap inside ComfyUI's range; denoise outside 0..=1 is refused.
    assert_eq!(Pass::detail(&Settings { seed: MAX_SEED, ..qwen.clone() }, 0).seed, 0);
    assert!(workflow(&Pass { denoise: 1.5, ..Pass::fill(&qwen) }, "a", "b").is_err());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn removal_roundtrip_runs_a_coarse_fill_then_detail_tiles_and_keeps_unselected_pixels() {
    let dir = std::env::temp_dir().join(format!("lightcraft-comfyui-remove-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("original.png");
    // Large enough that the wide context must be scaled down, so a detail tile follows.
    let mut original = Rgba8::new(2000, 1500);
    original.data.fill([110, 70, 30, 255]);
    std::fs::write(&path, image::encode(&original).unwrap()).unwrap();
    let png = |w: usize, h: usize, c: [u8; 4]| {
        let mut img = Rgba8::new(w, h);
        img.data.fill(c);
        image::encode(&img).unwrap()
    };
    let job = |n: &str| {
        format!(r#"{{"{n}":{{"status":{{"completed":true,"status_str":"success"}},"outputs":{{"10":{{"images":[{{"filename":"{n}.png","subfolder":"Lightcraft-masking","type":"output"}}]}}}}}}}}"#)
            .into_bytes()
    };
    let (url, thread) = mock(vec![
        ("200 OK", br#"{"name":"fill-image.png"}"#.to_vec()),
        ("200 OK", br#"{"name":"fill-mask.png"}"#.to_vec()),
        ("200 OK", br#"{"prompt_id":"fill","node_errors":{}}"#.to_vec()),
        ("200 OK", job("fill")),
        // A context of 6× covers the whole 2000×1500 frame: 1024×768 at the model.
        ("200 OK", png(1024, 768, [10, 200, 30, 255])),
        ("200 OK", br#"{"name":"tile-image.png"}"#.to_vec()),
        ("200 OK", br#"{"name":"tile-mask.png"}"#.to_vec()),
        ("200 OK", br#"{"prompt_id":"tile","node_errors":{}}"#.to_vec()),
        ("200 OK", job("tile")),
        ("200 OK", png(1024, 1024, [10, 20, 240, 255])),
    ]);
    let mut s = crate::Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [path]})).unwrap();
    let photo = s.active().unwrap();
    s.execute("mask.add", &json!({"kind": "radial", "center": [0.5, 0.5], "rx": 0.1, "ry": 0.1, "feather": 0.0})).unwrap();
    let rendered = s.render_now(photo, 2000, 1500).unwrap().image;
    // A default mask name says nothing about what to remove: describe it, or name the mask.
    let unnamed = json!({"endpoint": "http://127.0.0.1:1", "model": "q.gguf", "mode": "remove"});
    assert!(s.execute("mask.generate", &unnamed).unwrap_err().to_string().contains("describe what to remove"));
    assert!(s.generation.is_none());
    s.execute("mask.rename", &json!({"name": "the brown block"})).unwrap();
    // A leftover replacement prompt is not what gets removed.
    let settings =
        json!({"endpoint": url, "model": "q.gguf", "mode": "remove", "prompt": "a blue vase", "context": 6.0, "grow": 4, "detail": 0.3, "seed": 5});
    s.execute("mask.generate", &settings).unwrap();
    assert_eq!(s.activity.list()[0].label, "Removing from photo");
    let start = Instant::now();
    while s.generation.as_ref().unwrap().running() {
        assert!(start.elapsed() < Duration::from_secs(60), "{:?}", s.generation.as_ref().unwrap().status());
        std::thread::sleep(Duration::from_millis(20));
    }
    let status = s.execute("mask.generationStatus", &json!({})).unwrap();
    assert_eq!(status["ready"], true, "{status}");
    let result = s.execute("mask.acceptGeneration", &json!({})).unwrap();
    let out = lightcraft_codecs::decode(&std::fs::read(result["path"].as_str().unwrap()).unwrap(), Default::default()).unwrap().to_srgb8();
    assert_eq!((out.width, out.height), (2000, 1500));
    assert_eq!(out.data[0], rendered.data[0], "unselected corner stays byte identical");
    assert_eq!(out.data[750 * 2000 + 1000][..3], [10, 20, 240], "the detail tile has the last word in the middle");
    let recipe: Value = serde_json::from_slice(&std::fs::read(result["provenance"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!(recipe["workflow"], "Lightcraft-remove");
    assert_eq!(recipe["settings"]["subject"], "the brown block", "the recipe records what was removed");
    assert_eq!(recipe["removal"]["context"], json!({"x": 0, "y": 0, "w": 2000, "h": 1500}));
    assert_eq!(recipe["removal"]["tiles"].as_array().unwrap().len(), 1);
    assert_eq!(recipe["removal"]["tileSeeds"], json!([6]));
    let requests = thread.join().unwrap();
    assert_eq!(requests.len(), 10);
    let fill: Value = serde_json::from_slice(&requests[2].1).unwrap();
    assert_eq!(fill.pointer("/prompt/7/inputs/denoise"), Some(&json!(1.0)));
    assert_eq!(fill.pointer("/prompt/4/inputs/prompt"), Some(&json!(DEFAULT_REMOVAL_INSTRUCTION.replace("{prompt}", "the brown block"))));
    let tile: Value = serde_json::from_slice(&requests[7].1).unwrap();
    assert_eq!(tile.pointer("/prompt/2/inputs/image"), Some(&json!("tile-image.png")));
    assert_eq!(tile.pointer("/prompt/7/inputs/denoise"), Some(&json!(0.3)));
    assert_eq!(tile.pointer("/prompt/7/inputs/seed"), Some(&json!(6)));
    assert_eq!(tile.pointer("/prompt/4/inputs/prompt"), Some(&json!(DEFAULT_REFINE_INSTRUCTION)));
    // The tile upload is the coarse result at full resolution, not the original.
    let uploaded = &requests[5].1;
    let start = uploaded.windows(4).position(|w| w == b"\x89PNG").unwrap();
    let tile_in = lightcraft_codecs::decode(&uploaded[start..], Default::default()).unwrap().to_srgb8();
    assert_eq!((tile_in.width, tile_in.height), (1024, 1024));
    assert_eq!(tile_in.data[512 * 1024 + 512][..3], [10, 200, 30]);
    std::fs::remove_dir_all(&dir).unwrap();
}
