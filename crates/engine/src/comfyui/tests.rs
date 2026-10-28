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

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn http_error_validation_failure_and_cancellation_are_errors_not_panics() {
    let (url, t) = mock(vec![("500 Internal Server Error", b"failure".to_vec())]);
    assert!(client::Client::new(&url).unwrap().upload("input.png", b"png").unwrap_err().contains("upload"));
    t.join().unwrap();
    let (url, t) = mock(vec![("200 OK", br#"{"node_errors":{"1":{"errors":["missing checkpoint"]}}}"#.to_vec())]);
    let c = client::Client::new(&url).unwrap();
    assert!(c.generate(&Settings::default(), "a", "b", || false).unwrap_err().contains("validation"));
    t.join().unwrap();
    let (url, t) = mock(vec![("200 OK", br#"{"prompt_id":"test-job"}"#.to_vec()), ("200 OK", b"{}".to_vec())]);
    let count = std::cell::Cell::new(0);
    let error = client::Client::new(&url)
        .unwrap()
        .generate(&Settings::default(), "a", "b", || {
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
    let dir = std::env::temp_dir().join(format!("lightcraft-comfyui-test-{}", std::process::id()));
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
    let settings = json!({"endpoint": url, "prompt": "blue vase", "seed": 42});
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
    let requests = thread.join().unwrap();
    let graph: Value = serde_json::from_slice(&requests[2].1).unwrap();
    assert_eq!(graph.pointer("/prompt/2/inputs/image"), Some(&json!("inputs/actual-image.png")));
    assert_eq!(graph.pointer("/prompt/3/inputs/image"), Some(&json!("actual-mask.png")));
    assert!(s.generation.is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}
