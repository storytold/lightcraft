use lightcraft_meta::{ComfyMetadata, extract};
use miniz_oxide::deflate::compress_to_vec_zlib;

const API: &str = include_str!("fixtures/comfy-api.json");
const WORKFLOW: &str = include_str!("fixtures/comfy-workflow.json");

fn chunk(png: &mut Vec<u8>, typ: &[u8], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    png.extend_from_slice(typ);
    png.extend_from_slice(data);
    let mut crc = crc32fast::Hasher::new();
    crc.update(typ);
    crc.update(data);
    png.extend_from_slice(&crc.finalize().to_be_bytes());
}

fn png(entries: &[(&[u8], &str, &[u8])]) -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = 1024u32.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&768u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &ihdr);
    for (typ, keyword, text) in entries {
        let mut data = keyword.as_bytes().to_vec();
        data.push(0);
        match *typ {
            b"zTXt" => {
                data.push(0);
                data.extend(compress_to_vec_zlib(text, 6));
            }
            b"iTXt" => {
                data.extend_from_slice(&[1, 0, 0, 0]);
                data.extend(compress_to_vec_zlib(text, 6));
            }
            _ => data.extend_from_slice(text),
        }
        chunk(&mut png, typ, &data);
    }
    chunk(&mut png, b"IEND", &[]);
    png
}

fn has(m: &ComfyMetadata, field: &str, value: &str) -> bool {
    m.values(field).any(|v| v == value)
}

#[test]
fn general_search_reads_generator_text_custom_json_and_compressed_xmp() {
    let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description xmlns:tool="https://example.org/annotations/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:lc="http://ns.lightcraft.app/lc/1.0/">
<tool:tags><rdf:Bag><rdf:li>Cat</rdf:li><rdf:li>Chair</rdf:li></rdf:Bag></tool:tags>
<dc:subject><rdf:Bag><rdf:li>Human keyword</rdf:li></rdf:Bag></dc:subject><lc:settings>private develop blob</lc:settings>
</rdf:Description></rdf:RDF></x:xmpmeta>"#;
    for typ in [b"tEXt", b"zTXt", b"iTXt"] {
        let bytes = png(&[
            (typ, "parameters", b"Church in a forest\nNegative prompt: blur\nSteps: 30, Model: Dreamshaper"),
            (typ, "invokeai_metadata", br#"{"positive_prompt":"Moonlit lake","model":{"name":"SDXL"},"seed":42,"base64":"skip-me"}"#),
            (typ, "XML:com.adobe.xmp", xmp.as_bytes()),
        ]);
        let index = lightcraft_meta::search_metadata(&bytes);
        let (known, combined) = lightcraft_meta::extract_with_search(&bytes);
        assert_eq!(known, extract(&bytes));
        assert_eq!(combined, index);
        assert!(!index.comfy);
        for value in ["church", "forest", "dreamshaper", "moonlit lake", "sdxl", "42", "cat", "chair"] {
            assert!(index.contains(value), "{value}: {index:?}");
        }
        assert!(index.matches("parameters=dreamshaper"));
        assert!(!index.matches("parameters=sdxl"));
        assert!(index.valid());
        assert!(!index.contains("human keyword"), "catalog keywords must remain authoritative");
        assert!(!index.contains("private develop blob"));
        assert!(!index.contains("skip-me"));
    }
}

#[test]
fn general_search_reads_exif_software_serial_and_generator_user_comments() {
    use lightcraft_tiff::{IfdBuilder, TiffWriter, Value};
    for comment in [
        [b"ASCII\0\0\0".as_slice(), b"Cat on a chair, Model: Flux"].concat(),
        [b"UNICODE\0".as_slice(), "Cat on a chair, café".encode_utf16().flat_map(u16::to_be_bytes).collect::<Vec<_>>().as_slice()].concat(),
        [b"UNICODE\0\xff\xfe".as_slice(), "Cat on a chair, café".encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>().as_slice()].concat(),
    ] {
        let mut ifd = IfdBuilder::new();
        ifd.set(0x0131, Value::Ascii("Forge Test Exporter".into()));
        let mut exif = IfdBuilder::new();
        exif.set(0xa431, Value::Ascii("CameraSerial123".into()));
        exif.set(0x9286, Value::Undefined(comment));
        ifd.set_child(0x8769, exif);
        let bytes = TiffWriter::default().write(&[ifd]).unwrap();
        let index = lightcraft_meta::search_metadata(&bytes);
        assert!(index.contains("forge"));
        assert!(index.matches("software=exporter"));
        assert!(index.contains("cameraserial123"));
        assert!(index.contains("cat on a chair"));
    }
}

#[test]
fn general_search_bounds_custom_metadata_and_replaces_sidecar_values() {
    let big = format!(r#"{{"strings":[{}]}}"#, (0..1000).map(|i| format!("\"{i}{}\"", "x".repeat(100))).collect::<Vec<_>>().join(","));
    let bytes = png(&[(b"zTXt", "custom", big.as_bytes()), (b"tEXt", "bad", b"{broken JSON")]);
    let index = lightcraft_meta::search_metadata(&bytes);
    assert!(index.valid());
    assert!(index.byte_len() <= lightcraft_meta::SEARCH_BYTES);
    assert!(!index.contains("broken"));
    for n in (0..bytes.len()).step_by(7) {
        assert!(lightcraft_meta::search_metadata(&bytes[..n]).valid());
    }
    let mut index = lightcraft_meta::SearchMetadata::default();
    let packet = r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:a="https://example.org/ai/" a:tags="Cat"/></rdf:RDF>"#;
    index.replace_sidecar(Some(packet));
    assert!(index.contains("cat"));
    index.replace_sidecar(Some(&packet.replace("Cat", "Chair")));
    assert!(!index.contains("cat"));
    assert!(index.contains("chair"));
    index.replace_sidecar(None);
    assert!(!index.contains("chair"));
}

#[test]
fn metadata_viewer_reads_generator_text_and_preserves_original_case() {
    let bytes = png(&[
        (b"tEXt", "prompt", API.as_bytes()),
        (b"zTXt", "workflow", WORKFLOW.as_bytes()),
        (b"iTXt", "parameters", "Church in a Forest, café\nNegative prompt: blur\nSteps: 30, Model: Qwen".as_bytes()),
    ]);
    let fields = lightcraft_meta::png_text_fields(&bytes);
    assert_eq!(fields.len(), 3);
    assert_eq!(fields[0].value, API);
    assert!(fields[2].value.starts_with("Church in a Forest"));
    let readable = lightcraft_meta::comfy_display_fields(&fields, None);
    assert!(readable["node"].iter().any(|v| v == "MyCustomNode"));
    assert!(readable["positive"].iter().any(|v| v.contains("Cyberpunk")));
    assert!(!readable.values().flatten().any(|v| v.contains("class_type")));
    assert!(lightcraft_meta::png_text_fields(b"not a PNG").is_empty());
    let mut bad = png(&[(b"tEXt", "parameters", b"hidden")]);
    let at = bad.windows(6).position(|b| b == b"hidden").unwrap();
    bad[at] = b'X';
    assert!(lightcraft_meta::png_text_fields(&bad).is_empty());
}

#[test]
fn comfy_api_prompt_only_and_custom_nodes() {
    let m = extract(&png(&[(b"tEXt", "prompt", API.as_bytes())])).comfy.unwrap();
    for (field, value) in [
        ("positive", "cyberpunk neon city, café at night"),
        ("negative", "blurry, watermark"),
        ("model", "flux/flux-dev.safetensors"),
        ("lora", "detail.safetensors"),
        ("lora", "cinema.safetensors"),
        ("sampler", "euler"),
        ("scheduler", "karras"),
        ("seed", "18446744073709551614"),
        ("seed", "123456"),
        ("steps", "30"),
        ("cfg", "7.5"),
        ("denoise", "0.85"),
        ("vae", "ae.safetensors"),
        ("width", "1024"),
        ("height", "768"),
        ("node", "mycustomnode"),
    ] {
        assert!(has(&m, field, value), "{field}: {value}: {m:?}");
    }
    for term in ["0.7", "strength_clip", "stormy", "magenta", "custom atmospheric control"] {
        assert!(m.contains(term), "{term}");
    }
    assert!(!m.values("prompt").any(|v| v.contains("class_type")));
}

#[test]
fn comfy_ui_workflow_only_in_all_text_encodings() {
    for typ in [b"tEXt", b"zTXt", b"iTXt"] {
        let m = extract(&png(&[(typ, "workflow", WORKFLOW.as_bytes())])).comfy.unwrap();
        for (field, value) in [
            ("positive", "cyberpunk neon city, café at night"),
            ("negative", "blurry, watermark"),
            ("seed", "123456"),
            ("steps", "28"),
            ("cfg", "6.5"),
            ("sampler", "euler_ancestral"),
            ("model", "flux-dev.safetensors"),
            ("lora", "custom-detail.safetensors"),
            ("vae", "ae.safetensors"),
            ("prompt", "a distant moon"),
        ] {
            assert!(has(&m, field, value), "{field}: {value}: {m:?}");
        }
        for term in ["0.42", "stormy", "atmosphere v2", "night photography experiment", "lighting"] {
            assert!(m.contains(term), "{term}");
        }
    }
    let mut modern: serde_json::Value = serde_json::from_str(WORKFLOW).unwrap();
    modern["version"] = 1.into();
    modern["links"] = serde_json::json!([
        {"id":20,"origin_id":6,"origin_slot":0,"target_id":3,"target_slot":1,"type":"CONDITIONING"},
        {"id":21,"origin_id":7,"origin_slot":0,"target_id":3,"target_slot":2,"type":"CONDITIONING"}
    ]);
    let m = extract(&png(&[(b"tEXt", "workflow", modern.to_string().as_bytes())])).comfy.unwrap();
    assert!(has(&m, "positive", "cyberpunk neon city, café at night"));
    assert!(has(&m, "negative", "blurry, watermark"));
}

#[test]
fn comfy_combines_deduplicates_and_ignores_bad_json() {
    let m = extract(&png(&[(b"tEXt", "prompt", API.as_bytes()), (b"zTXt", "workflow", WORKFLOW.as_bytes())])).comfy.unwrap();
    assert_eq!(m.values("positive").count(), 1);
    assert_eq!(m.values("lora").count(), 3);
    for (a, b) in [("prompt", "workflow"), ("workflow", "prompt")] {
        assert!(extract(&png(&[(b"tEXt", a, b"{bad json"), (b"tEXt", b, API.as_bytes())])).comfy.is_some());
    }
    for invalid in ["null", "[]", "{}", "{\"text\":\"ordinary caption\"}", "{\"nodes\":false}"] {
        assert!(extract(&png(&[(b"tEXt", "prompt", invalid.as_bytes())])).comfy.is_none());
    }
    assert!(extract(&png(&[(b"tEXt", "Comment", API.as_bytes())])).comfy.is_none());
}

#[test]
fn comfy_crc_truncation_and_compression_damage_are_safe() {
    let p = png(&[(b"zTXt", "prompt", API.as_bytes())]);
    for n in 0..p.len() {
        let _ = extract(&p[..n]);
    }
    let mut damaged = p.clone();
    let payload_end = damaged.len() - 17;
    damaged[payload_end] ^= 1;
    assert!(extract(&damaged).comfy.is_none());
    let mut p = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut p, b"zTXt", b"prompt\0\0not zlib");
    chunk(&mut p, b"iTXt", b"workflow\0\x02\0\0\0invalid flag");
    chunk(&mut p, b"tEXt", b"prompt");
    assert!(extract(&p).comfy.is_none());
}

#[test]
fn comfy_limits_inflation_json_depth_and_index_size() {
    let bomb = vec![b' '; 4 * 1024 * 1024 + 1];
    for typ in [b"tEXt", b"zTXt", b"iTXt"] {
        assert!(extract(&png(&[(typ, "prompt", &bomb)])).comfy.is_none());
    }
    let deep = format!("{}0{}", "[".repeat(200), "]".repeat(200));
    assert!(extract(&png(&[(b"tEXt", "prompt", deep.as_bytes())])).comfy.is_none());
    let inputs: serde_json::Map<String, serde_json::Value> =
        (0..100).map(|i| (format!("param{i}"), format!("{i}{}", "é".repeat(5000)).into())).collect();
    let graph = serde_json::json!({"1":{"class_type":"Custom", "inputs":inputs}}).to_string();
    let m = extract(&png(&[(b"tEXt", "prompt", graph.as_bytes())])).comfy.unwrap();
    assert!(serde_json::to_vec(&m).unwrap().len() < 70 * 1024);
    assert!(m.fields.values().flatten().all(|s| s.len() <= 8192));
}

#[test]
fn comfy_cyclic_and_dangling_links_are_safe() {
    let graph = br#"{"1":{"class_type":"Sampler","inputs":{"positive":["2",0]}},"2":{"class_type":"Custom","inputs":{"text":"connected text","conditioning":["2",0],"missing":["404",0],"colors":["scarlet",7],"dimensions":[256,128]}}}"#;
    let m = extract(&png(&[(b"tEXt", "prompt", graph)])).comfy.unwrap();
    assert!(has(&m, "positive", "connected text"));
    assert!(m.contains("scarlet") && m.contains("256") && m.contains("128"));
}

#[test]
fn comfy_uncompressed_international_text_preserves_unicode() {
    let mut p = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut data = b"prompt\0\0\0en\0Prompt\0".to_vec();
    data.extend_from_slice(API.as_bytes());
    chunk(&mut p, b"iTXt", &data);
    let m = extract(&p);
    assert!(m.comfy.unwrap().contains("café"));
    assert_eq!(m.caption, None);
}

#[test]
fn comfy_latin1_text_and_utf8_international_text() {
    let latin1: Vec<u8> = API.chars().map(|c| c as u8).collect();
    for typ in [b"tEXt", b"zTXt"] {
        let m = extract(&png(&[(typ, "prompt", &latin1)])).comfy.unwrap();
        assert!(m.contains("café"));
    }
    assert!(extract(&png(&[(b"iTXt", "prompt", &latin1)])).comfy.is_none());
}

#[test]
fn comfy_legacy_checkpoint_clip_loaders_and_nested_custom_loras() {
    let graph = br#"{"nodes":[
        {"id":1,"type":"CheckpointLoader","widgets_values":["v1-inference.yaml","old-model.ckpt"]},
        {"id":2,"type":"DualCLIPLoader","widgets_values":["t5xxl.safetensors","clip_l.safetensors","flux"]},
        {"id":3,"type":"PowerLoraLoader","widgets_values":{"lora_1":{"lora":"nested-detail.safetensors","strength":0.75}}}
    ]}"#;
    let m = extract(&png(&[(b"tEXt", "workflow", graph)])).comfy.unwrap();
    for value in ["old-model.ckpt", "t5xxl.safetensors", "clip_l.safetensors"] {
        assert!(has(&m, "model", value));
    }
    assert!(!has(&m, "model", "v1-inference.yaml"));
    assert!(has(&m, "lora", "nested-detail.safetensors"));
    assert!(m.contains("0.75"));
}
