//! Auto-tags: the vocabulary's embeddings (cached per model, only new tags embedded again), the library's tags with
//! their levels and cut-off, accepting (keywords under `Contents|`, one undo step) and dismissing (remembered beside
//! the catalog), the Library Filter's Contents column, the Smart Collection rule, and the speed of scoring a large
//! library. A stand-in model does the embedding: an image's embedding is its colour cast, a text's the colour it names.
//! How the real model tags the demo scenes is checked by `the_real_model_tags_the_demo_scenes` when
//! `LIGHTCRAFT_VISION_DIR` points at it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lightcraft_catalog::PhotoId;
use lightcraft_vision::{ContentModel, Manifest, Rgb8, Scoring, Towers, manifest};
use serde_json::{Value, json};

use crate::Session;
use crate::content_models as cm;
use crate::content_worker::Loader;

const DIM: usize = 8;

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-contenttags-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[derive(Default)]
struct Calls {
    texts: AtomicUsize,
    loads: AtomicUsize,
}

/// The stand-in: an image's embedding is its colour cast; a text's is the colour it names (`a photo of green.`).
struct Fake {
    manifest: Manifest,
    calls: Arc<Calls>,
    /// Milliseconds per text.
    slow: u64,
}

fn normalised(mut v: Vec<f32>) -> Vec<f32> {
    lightcraft_vision::normalize(&mut v);
    v
}

fn colour(name: &str) -> Vec<f32> {
    normalised(match name {
        "green" => vec![-0.5, 1.0, -0.5, 0.0, 0.0, 0.0, 0.0, 0.0],
        "blue" => vec![-0.5, -0.5, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        "red" => vec![1.0, -0.5, -0.5, 0.0, 0.0, 0.0, 0.0, 0.0],
        "yellow" => vec![0.5, 0.5, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        _ => vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0],
    })
}

const SCORING: Scoring = Scoring { scale: 10.0, bias: -5.0, sigmoid: true };

impl ContentModel for Fake {
    fn embed_images(&self, batch: &[Rgb8]) -> lightcraft_vision::Result<Vec<Vec<f32>>> {
        Ok(batch
            .iter()
            .map(|img| {
                let n = img.data.len().max(1) as f32;
                let mean = |c: usize| img.data.iter().map(|p| f32::from(p[c]) / 255.0).sum::<f32>() / n;
                let (r, g, b) = (mean(0), mean(1), mean(2));
                let m = (r + g + b) / 3.0;
                self.sized(normalised(vec![r - m, g - m, b - m, m - 0.4, 0.02, 0.0, 0.0, 0.0]))
            })
            .collect())
    }

    fn embed_texts(&self, texts: &[&str]) -> lightcraft_vision::Result<Vec<Vec<f32>>> {
        std::thread::sleep(Duration::from_millis(self.slow * texts.len() as u64));
        self.calls.texts.fetch_add(texts.len(), Ordering::SeqCst);
        Ok(texts
            .iter()
            .map(|t| {
                let word = ["green", "blue", "red", "yellow"].into_iter().find(|w| t.contains(w)).unwrap_or("void");
                self.sized(colour(word))
            })
            .collect())
    }

    fn score_cosine(&self, cosine: f32) -> f32 {
        self.scoring_now().score(cosine)
    }

    fn scoring(&self) -> Option<Scoring> {
        Some(self.scoring_now())
    }

    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
}

impl Fake {
    /// Its vectors at the manifest's dimension (the colours are in the first four numbers, nothing past the sixth).
    fn sized(&self, mut v: Vec<f32>) -> Vec<f32> {
        v.resize(self.manifest.embedding.dim as usize, 0.0);
        v
    }

    /// As the real families: a CLIP model's score is its scaled cosine plus the manifest's offset, through a sigmoid.
    fn scoring_now(&self) -> Scoring {
        match self.manifest.family {
            manifest::Family::Siglip => SCORING,
            manifest::Family::Clip => Scoring { scale: SCORING.scale, bias: self.manifest.score.softmax_offset(), sigmoid: true },
        }
    }
}

fn loader(calls: Arc<Calls>, slow: u64) -> Loader {
    Arc::new(move |_: &Path, m: &Manifest, _: Towers| {
        calls.loads.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Fake { manifest: m.clone(), calls: calls.clone(), slow }) as Box<dyn ContentModel>)
    })
}

fn install(content: &Path, id: &str) {
    install_manifest(content, id, manifest::builtin_siglip2_b16_256());
}

/// A stand-in of the CLIP family: its own tokenizer kind and softmax scoring, 6 numbers per embedding.
fn install_clip(content: &Path, id: &str) {
    let mut m = manifest::builtin_clip_vit_b32_openai();
    m.score.offset = Some(SCORING.bias);
    install_manifest(content, id, m);
    let file = content.join(id).join(cm::MANIFEST_FILE);
    let mut m: Manifest = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    m.embedding.dim = 6;
    std::fs::write(file, serde_json::to_vec(&m).unwrap()).unwrap();
}

fn install_manifest(content: &Path, id: &str, mut m: Manifest) {
    m.id = id.into();
    m.name = format!("Fake {id}");
    m.embedding.dim = DIM as u32;
    m.match_threshold = 0.5;
    m.tag_threshold = 0.5;
    let bytes = format!("weights of {id}").into_bytes();
    m.files =
        vec![manifest::FileSpec { name: "model.safetensors".into(), size: bytes.len() as u64, sha256: lightcraft_faces::hash::sha256_hex(&bytes) }];
    let home = content.join(id);
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("model.safetensors"), &bytes).unwrap();
    std::fs::write(home.join(cm::MANIFEST_FILE), serde_json::to_vec(&m).unwrap()).unwrap();
    std::fs::write(home.join(cm::RECORD_FILE), br#"{"acceptedAt": "2026-10-11T00:00:00Z"}"#).unwrap();
}

/// The test vocabulary: four colours under `colour|`, a warm group, and a tag nothing shows.
const VOCAB: &str = "colour|green\ncolour|blue = blue, sea blue\ncolour|warm|red\ncolour|warm|yellow\nnothing|void\n";

/// A session on the demo library at `d/lib` with the stand-in installed, the test vocabulary, the library indexed.
fn session(d: &Path, calls: &Arc<Calls>, slow: u64) -> Session {
    let content = d.join("content");
    if !content.join("fake").exists() {
        install(&content, "fake");
        let st = cm::Settings { enabled: true, model: Some("fake".into()), pace: Some("low".into()), ..Default::default() };
        cm::write_settings(&content, &st).unwrap();
        std::fs::write(content.join(crate::content_tags::VOCAB_FILE), VOCAB).unwrap();
    }
    let mut s = Session::new().with_fs();
    s.content_models_dir = Some(content);
    s.content.loader = Some(loader(calls.clone(), slow));
    s.open_library(d.join("lib"), true).unwrap();
    s.execute("content.pump", &json!({"wait": true})).unwrap();
    s
}

fn tags(s: &mut Session, ids: &[u64]) -> Value {
    s.execute("content.tags", &json!({"ids": ids, "wait": true})).unwrap()
}

fn all_ids(s: &Session) -> Vec<u64> {
    let mut v: Vec<u64> = s.catalog.photos().filter(|p| p.in_library() && !p.deleted).map(|p| p.id.0).collect();
    v.sort();
    v
}

/// The stand-in's score of photo `id` for colour `name`.
fn score(s: &Session, id: u64, name: &str) -> f32 {
    let v = s.content.index.as_ref().unwrap().vector(id).unwrap();
    // (the tag's embedding: the mean of its sentences, all the same colour here)
    SCORING.score(v.iter().zip(colour(name)).map(|(a, b)| a * b).sum())
}

/// What the stand-in says photo `id` shows at cut-off `thr`: (tag, score), best first.
fn expected(s: &Session, id: u64, thr: f32) -> Vec<String> {
    let mut v: Vec<(String, f32)> =
        [("colour|green", "green"), ("colour|blue", "blue"), ("colour|warm|red", "red"), ("colour|warm|yellow", "yellow")]
            .into_iter()
            .map(|(t, c)| (t.to_string(), score(s, id, c)))
            .filter(|(_, sc)| *sc >= thr)
            .collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v.into_iter().map(|(t, _)| t).collect()
}

fn photo_tags(r: &Value, i: usize) -> Vec<String> {
    r["photos"][i]["tags"].as_array().unwrap().iter().map(|t| t["tag"].as_str().unwrap().to_string()).collect()
}

#[test]
fn every_photo_gets_the_tags_it_shows_with_their_levels_and_the_cut_off_moves_them_at_once() {
    let d = temp("compute");
    let calls = Arc::new(Calls::default());
    let mut s = session(&d, &calls, 0);
    let ids = all_ids(&s);
    let r = tags(&mut s, &ids);
    assert_eq!(r["state"], "ready", "{r}");
    assert_eq!(r["threshold"].as_f64(), Some(0.5));
    let mut tagged = 0;
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(photo_tags(&r, i), expected(&s, *id, 0.5), "photo {id}");
        tagged += usize::from(!photo_tags(&r, i).is_empty());
    }
    assert!(tagged > 0 && tagged < ids.len(), "some photos show a colour, some none: {tagged} of {}", ids.len());
    // the Contents column: each tag with its photos, the levels above counting those below
    let c = s.execute("content.tagCounts", &json!({})).unwrap();
    let count = |tag: &str| c["tags"].as_array().unwrap().iter().find(|t| t["tag"] == tag).map_or(0, |t| t["count"].as_u64().unwrap());
    let have = |tag: &str| ids.iter().filter(|id| expected(&s, **id, 0.5).iter().any(|t| crate::catalog::keywords::is_under(t, tag))).count() as u64;
    for tag in ["colour", "colour|green", "colour|blue", "colour|warm", "colour|warm|red"] {
        assert_eq!(count(tag), have(tag), "{tag}: {c}");
    }
    assert_eq!(count("colour"), tagged as u64);
    assert_eq!(count("nothing|void"), 0, "a tag nothing shows has no row");
    let paths: Vec<&str> = c["tags"].as_array().unwrap().iter().map(|t| t["tag"].as_str().unwrap()).collect();
    assert_eq!(paths.first(), Some(&"colour"), "in path order, a level before the tags under it: {paths:?}");
    // a lower cut-off (Settings ▸ Search): more tags at once, nothing scored again
    let texts = calls.texts.load(Ordering::SeqCst);
    s.execute("content.tagOptions", &json!({"threshold": 0.2})).unwrap();
    let r = tags(&mut s, &ids);
    assert!(ids.iter().enumerate().all(|(i, id)| photo_tags(&r, i) == expected(&s, *id, 0.2).into_iter().take(8).collect::<Vec<_>>()));
    assert!(s.execute("content.tagCounts", &json!({})).unwrap()["tags"][0]["count"].as_u64().unwrap() >= tagged as u64);
    assert_eq!(calls.texts.load(Ordering::SeqCst), texts);
    // the model's own again
    assert_eq!(s.execute("content.tagOptions", &json!({"threshold": null})).unwrap()["threshold"].as_f64(), Some(0.5));
    assert!(s.execute("content.tagOptions", &json!({"threshold": 3})).is_err());
    let st = s.execute("content.status", &json!({})).unwrap();
    assert_eq!((st["tags"]["state"].as_str(), st["tags"]["vocabulary"].as_u64()), (Some("ready"), Some(5)), "{st}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_vocabulary_is_embedded_once_in_the_background_cached_per_model_and_an_edit_embeds_only_new_tags() {
    let d = temp("vocab");
    let calls = Arc::new(Calls::default());
    // 15 ms a text: preparing the vocabulary takes a moment
    let mut s = session(&d, &calls, 15);
    let r = tags(&mut s, &[]);
    assert_eq!(r["state"], "ready");
    // five tags, six phrases, two templates each
    let sentences = 6 * lightcraft_vision::vocab::TEMPLATES.len();
    assert_eq!(calls.texts.load(Ordering::SeqCst), sentences);
    // a tag with many phrases: in the activity stack while it is prepared, and gone after
    let many = "colour|reddish = red, red one, red two, red three, red four, red five, red six, red seven";
    s.execute("content.setVocabulary", &json!({"text": format!("{VOCAB}{many}")})).unwrap();
    let row = s.activity.list().into_iter().find(|t| t.label == "Preparing auto-tags").expect("a row while the tags are prepared");
    assert!(row.cancellable && row.kind == "content");
    assert_eq!(tags(&mut s, &[])["state"], "ready");
    assert!(s.activity.list().iter().all(|t| t.label != "Preparing auto-tags"), "the row goes");
    let sentences = sentences + 8 * lightcraft_vision::vocab::TEMPLATES.len();
    assert_eq!(calls.texts.load(Ordering::SeqCst), sentences);
    s.execute("content.setVocabulary", &json!({"text": VOCAB})).unwrap();
    let cache = d.join("content").join("content-vocab-fake.bin");
    assert!(cache.is_file(), "cached beside the models");
    // a new tag: only it is embedded
    let v = s.execute("content.setVocabulary", &json!({"text": format!("{VOCAB}colour|deep blue = blue water")})).unwrap();
    assert_eq!((v["count"].as_u64(), v["custom"].as_bool()), (Some(6), Some(true)));
    let ids = all_ids(&s);
    let r = tags(&mut s, &ids);
    assert_eq!(r["state"], "ready");
    assert_eq!(calls.texts.load(Ordering::SeqCst), sentences + lightcraft_vision::vocab::TEMPLATES.len());
    assert!(r["photos"].as_array().unwrap().iter().any(|p| p["tags"].as_array().unwrap().iter().any(|t| t["tag"] == "colour|deep blue")));
    let voc = s.execute("content.vocabulary", &json!({"text": true})).unwrap();
    assert_eq!((voc["count"].as_u64(), voc["embedded"].as_u64()), (Some(6), Some(6)), "{voc}");
    assert!(voc["text"].as_str().unwrap().contains("colour|deep blue = blue water"));
    drop(s);
    // a new session: everything from the cache, so the text model is not even loaded
    let loads = calls.loads.load(Ordering::SeqCst);
    let texts = calls.texts.load(Ordering::SeqCst);
    let mut s = session(&d, &calls, 0);
    assert_eq!(tags(&mut s, &[])["state"], "ready");
    assert_eq!(calls.texts.load(Ordering::SeqCst), texts, "nothing embedded again");
    assert_eq!(calls.loads.load(Ordering::SeqCst), loads, "no model loaded for the tags");
    // a damaged cache is rebuilt
    drop(s);
    std::fs::write(&cache, b"LCCV\x01garbage").unwrap();
    let mut s = session(&d, &calls, 0);
    assert_eq!(tags(&mut s, &[])["state"], "ready");
    assert_eq!(calls.texts.load(Ordering::SeqCst), texts + 7 * lightcraft_vision::vocab::TEMPLATES.len(), "the six tags again");
    // restoring the built-in list
    let v = s.execute("content.setVocabulary", &json!({"reset": true})).unwrap();
    assert_eq!(v["custom"], false);
    assert!(v["count"].as_u64().unwrap() > 1000);
    assert!(!d.join("content").join(crate::content_tags::VOCAB_FILE).exists());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn accepting_tags_makes_keywords_under_contents_in_one_undo_step_and_never_on_deleted_photos() {
    let d = temp("accept");
    let calls = Arc::new(Calls::default());
    let mut s = session(&d, &calls, 0);
    let ids = all_ids(&s);
    // (a photo that already has a keyword named "green" is not suggested it)
    let named = |s: &Session, id: u64| s.catalog.photo(PhotoId(id)).unwrap().meta.keywords.iter().any(|k| k.eq_ignore_ascii_case("green"));
    let green: Vec<u64> = ids.iter().copied().filter(|id| expected(&s, *id, 0.5).contains(&"colour|green".to_string()) && !named(&s, *id)).collect();
    assert!(green.len() >= 2, "{green:?}");
    // the whole library selected: "green" is suggested for the green photos (once the tags are worked out)
    tags(&mut s, &[]);
    let sel: Vec<PhotoId> = ids.iter().map(|i| PhotoId(*i)).collect();
    let sug = s.content_suggestions(&sel);
    let g = sug.iter().find(|x| x.tag == "colour|green").expect("suggested");
    assert_eq!(g.photos.iter().map(|p| p.0).collect::<Vec<_>>(), green);
    assert_eq!(g.label, "green");
    // accepting it for the whole selection: only the photos it is suggested for get it
    let r = s.execute("content.acceptTags", &json!({"ids": ids, "tags": ["green"]})).unwrap();
    assert_eq!((r["changed"].as_u64(), r["keywords"][0].as_str()), (Some(green.len() as u64), Some("Contents|green")));
    for id in &ids {
        let has = s.catalog.photo(PhotoId(*id)).unwrap().meta.keywords.contains(&"Contents|green".to_string());
        assert_eq!(has, green.contains(id));
    }
    // accepted: shown as such, no longer suggested
    assert_eq!(
        tags(&mut s, &[green[0]])["photos"][0]["tags"].as_array().unwrap().iter().find(|t| t["tag"] == "colour|green").unwrap()["accepted"],
        true
    );
    assert!(s.content_suggestions(&sel).iter().all(|x| x.tag != "colour|green"));
    // one undo step takes it off every photo
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(ids.iter().all(|id| !s.catalog.photo(PhotoId(*id)).unwrap().meta.keywords.iter().any(|k| k.starts_with("Contents|"))));
    // another parent, or none
    s.execute("content.tagOptions", &json!({"parent": "Auto"})).unwrap();
    s.execute("content.acceptTags", &json!({"ids": [green[0]], "tags": ["colour|green"]})).unwrap();
    assert!(s.catalog.photo(PhotoId(green[0])).unwrap().meta.keywords.contains(&"Auto|green".to_string()));
    s.execute("content.tagOptions", &json!({"parent": ""})).unwrap();
    // `all`: on every photo given, suggested or not
    let other = *ids.iter().find(|id| !green.contains(id)).unwrap();
    s.execute("content.acceptTags", &json!({"ids": [other], "tags": ["red"], "all": true})).unwrap();
    assert!(s.catalog.photo(PhotoId(other)).unwrap().meta.keywords.contains(&"red".to_string()));
    // never crash: unknown tags, no tags, deleted and unknown photos
    assert!(s.execute("content.acceptTags", &json!({"ids": [green[0]], "tags": ["unicorn"]})).is_err());
    assert!(s.execute("content.acceptTags", &json!({"ids": [green[0]], "tags": []})).is_err());
    s.execute("photo.delete", &json!({"ids": [green[1]]})).unwrap();
    assert!(s.execute("content.acceptTags", &json!({"ids": [green[1], 999_999], "tags": ["green"]})).is_err(), "no photos left to act on");
    assert!(!s.catalog.photo(PhotoId(green[1])).unwrap().meta.keywords.iter().any(|k| k.contains("green")));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_dismissed_suggestion_stays_dismissed_after_reopening_and_a_damaged_file_is_rebuilt() {
    let d = temp("dismiss");
    let calls = Arc::new(Calls::default());
    let mut s = session(&d, &calls, 0);
    let ids = all_ids(&s);
    let id = *ids.iter().find(|id| !expected(&s, **id, 0.5).is_empty()).unwrap();
    let first = expected(&s, id, 0.5)[0].clone();
    let r = s.execute("content.dismissTags", &json!({"ids": [id], "tags": [first]})).unwrap();
    assert_eq!(r["dismissed"], 1);
    assert!(!photo_tags(&tags(&mut s, &[id]), 0).contains(&first));
    let before = s.execute("content.tagCounts", &json!({})).unwrap();
    s.close_library().unwrap();
    drop(s);
    let file = d.join("lib").join("content-dismissed.bin");
    assert!(file.is_file());
    let mut s = session(&d, &calls, 0);
    assert!(!photo_tags(&tags(&mut s, &[id]), 0).contains(&first), "still dismissed");
    assert_eq!(s.execute("content.tagCounts", &json!({})).unwrap()["tags"], before["tags"], "and not counted");
    drop(s);
    // a damaged file: ignored, rebuilt with the next dismissal
    std::fs::write(&file, b"nonsense").unwrap();
    let mut s = session(&d, &calls, 0);
    assert!(photo_tags(&tags(&mut s, &[id]), 0).contains(&first));
    s.execute("content.dismissTags", &json!({"ids": [id], "tags": [first]})).unwrap();
    drop(s);
    let mut s = session(&d, &calls, 0);
    assert!(!photo_tags(&tags(&mut s, &[id]), 0).contains(&first));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_contents_filter_shows_a_tags_photos_and_ands_with_the_other_filters() {
    let d = temp("filter");
    let calls = Arc::new(Calls::default());
    let mut s = session(&d, &calls, 0);
    tags(&mut s, &[]);
    let counts: Vec<(String, usize)> = s.content_tag_counts().to_vec();
    let (tag, n) = counts.iter().find(|(t, _)| t == "colour|green").cloned().unwrap();
    let r = s.execute("library.filter", &json!({"contentTag": tag})).unwrap();
    assert_eq!(r["count"].as_u64(), Some(n as u64), "{r}");
    let shown: Vec<u64> = s.visible().iter().map(|p| p.0).collect();
    assert!(shown.iter().all(|id| expected(&s, *id, 0.5).contains(&"colour|green".to_string())));
    // a level: every tag below it
    let warm = counts.iter().find(|(t, _)| t == "colour").unwrap().1;
    assert_eq!(s.execute("library.filter", &json!({"contentTag": "colour"})).unwrap()["count"].as_u64(), Some(warm as u64));
    // ANDed: a rating on one of them
    s.execute("library.filter", &json!({"contentTag": "colour|green"})).unwrap();
    s.execute("photo.rate", &json!({"ids": [shown[0]], "rating": 4})).unwrap();
    let r = s.execute("library.filter", &json!({"rating": 4})).unwrap();
    assert_eq!(r["count"].as_u64(), Some(1));
    // the cut-off moves the view
    s.execute("library.filter", &json!({"rating": 0})).unwrap();
    s.execute("content.tagOptions", &json!({"threshold": 0.05})).unwrap();
    assert!(s.visible().len() >= n);
    // cleared
    let all = all_ids(&s).len();
    assert_eq!(s.execute("library.filter", &json!({"contentTag": null})).unwrap()["count"].as_u64(), Some(all as u64));
    // a tag nobody has, or no index: nothing (never everything)
    assert_eq!(s.execute("library.filter", &json!({"contentTag": "unicorn"})).unwrap()["count"].as_u64(), Some(0));
    let mut bare = Session::with_demo();
    bare.execute("library.filter", &json!({"contentTag": "colour"})).unwrap();
    assert!(bare.visible().is_empty());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_smart_collection_can_ask_what_photos_show_and_matches_nothing_without_an_index() {
    let d = temp("smart");
    let calls = Arc::new(Calls::default());
    let mut s = session(&d, &calls, 0);
    tags(&mut s, &[]);
    let ids = all_ids(&s);
    let green = ids.iter().filter(|id| expected(&s, **id, 0.5).contains(&"colour|green".to_string())).count();
    let shows = ids.iter().filter(|id| score(&s, **id, "green") >= 0.5).count();
    let tagged = s
        .execute(
            "album.createSmart",
            &json!({"name": "Green", "rules": {"ruleSet": {"rules": [{"field": "contents", "op": "isTagged", "value": "colour|green"}]}}}),
        )
        .unwrap();
    // the reply counts the tag rule's photos already (no view asked for them first)
    assert_eq!(tagged["count"].as_u64(), Some(green as u64), "{tagged}");
    let showing = s
        .execute(
            "album.createSmart",
            &json!({"name": "Shows green", "rules": {"ruleSet": {"rules": [{"field": "contents", "op": "shows", "value": "green"}]}}}),
        )
        .unwrap();
    let strict = s
        .execute(
            "album.createSmart",
            &json!({"name": "Very green", "rules": {"ruleSet": {"rules": [{"field": "contents", "op": "shows", "value": {"text": "green", "min": 0.99}}]}}}),
        )
        .unwrap();
    let album = |v: &Value| lightcraft_catalog::AlbumId(v["id"].as_u64().unwrap());
    // the text is read on the text thread: the album fills once it is
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        s.visible();
        if s.catalog.album_count(album(&showing)) == shows || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(s.catalog.album_count(album(&tagged)), green);
    assert_eq!(s.catalog.album_count(album(&showing)), shows);
    assert!(s.catalog.album_count(album(&strict)) < shows);
    s.execute("library.source", &json!({"kind": "album", "id": album(&tagged).0})).unwrap();
    assert_eq!(s.visible().len(), green);
    assert!(s.content_rules_usable());
    // bad values are refused like any rule's
    assert!(
        s.execute("album.createSmart", &json!({"name": "x", "rules": {"ruleSet": {"rules": [{"field": "contents", "op": "shows", "value": ""}]}}}))
            .is_err()
    );
    assert!(
        s.execute(
            "album.createSmart",
            &json!({"name": "x", "rules": {"ruleSet": {"rules": [{"field": "contents", "op": "shows", "value": {"text": "a", "min": 7}}]}}})
        )
        .is_err()
    );
    // the same library without a content model: the albums hold nothing
    s.close_library().unwrap();
    drop(s);
    let mut bare = Session::new().with_fs();
    bare.open_library(d.join("lib"), true).unwrap();
    bare.visible();
    assert!(!bare.content_rules_usable());
    assert_eq!(bare.catalog.album_count(album(&tagged)), 0);
    assert_eq!(bare.catalog.album_count(album(&showing)), 0);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_vocabulary_edited_or_a_model_switched_while_tags_are_worked_out_never_mixes_them_up() {
    let d = temp("switch");
    let calls = Arc::new(Calls::default());
    let mut s = session(&d, &calls, 10);
    s.execute("content.pump", &json!({})).unwrap();
    // edited while it is prepared: the new list wins
    s.execute("content.setVocabulary", &json!({"terms": ["colour|green", "colour|red"]})).unwrap();
    let v = s.execute("content.setVocabulary", &json!({"terms": ["shade|green", "shade|green", "shade|red"]})).unwrap();
    assert_eq!(v["notes"].as_array().unwrap().len(), 1, "the repeated line is noted: {v}");
    let ids = all_ids(&s);
    let r = tags(&mut s, &ids);
    assert_eq!(r["state"], "ready");
    assert!(
        r["photos"].as_array().unwrap().iter().flat_map(|p| p["tags"].as_array().unwrap()).all(|t| t["tag"].as_str().unwrap().starts_with("shade|"))
    );
    // an empty or unreadable vocabulary is refused, the one in use stays
    assert!(s.execute("content.setVocabulary", &json!({"text": "# nothing\n\n"})).is_err());
    assert!(s.execute("content.setVocabulary", &json!({"path": d.join("missing.txt")})).is_err());
    assert!(s.execute("content.setVocabulary", &json!({})).is_err());
    let huge: Vec<String> = (0..6000).map(|i| format!("t{i}|{}", "x".repeat(10))).collect();
    assert_eq!(s.execute("content.setVocabulary", &json!({"terms": huge})).unwrap()["count"].as_u64(), Some(5000), "capped");
    s.execute("content.setVocabulary", &json!({"terms": ["shade|green", "shade|red"]})).unwrap();
    // another model while it works: its own cache, its own tags
    install(&d.join("content"), "other");
    s.execute("content.pump", &json!({})).unwrap();
    s.execute("content.models.select", &json!({"id": "other"})).unwrap();
    s.execute("content.enable", &json!({"on": true})).unwrap();
    s.execute("content.pump", &json!({"wait": true})).unwrap();
    let ids = all_ids(&s);
    let r = tags(&mut s, &ids);
    assert_eq!(r["state"], "ready", "{r}");
    assert!(d.join("content").join("content-vocab-other.bin").is_file());
    assert!(r["photos"].as_array().unwrap().iter().any(|p| !p["tags"].as_array().unwrap().is_empty()));
    // no model at all: nothing, and no error
    s.execute("content.models.select", &json!({"id": null})).unwrap();
    let ids = all_ids(&s);
    let r = tags(&mut s, &ids);
    assert_eq!(r["state"], "noModel");
    assert!(s.content_tag_counts().is_empty());
    assert!(s.execute("content.dismissTags", &json!({"ids": [1], "tags": ["shade|green"]})).is_ok());
    let _ = std::fs::remove_dir_all(&d);
}

/// The two families side by side: a SigLIP model and a CLIP model (another tokenizer, softmax scoring, another
/// embedding size) each keep their own index and vocabulary cache; search, tags, the Contents filter, a Smart Collection
/// and Find Similar Contents work with either, and switching back costs no embedding at all.
#[test]
fn siglip_and_clip_models_are_interchangeable_and_switching_keeps_both_indexes_and_vocabularies() {
    let d = temp("families");
    let calls = Arc::new(Calls::default());
    let mut s = session(&d, &calls, 0);
    let content = d.join("content");
    install_clip(&content, "clipfake");
    let album = s
        .execute(
            "album.createSmart",
            &json!({"name": "Shows green", "rules": {"ruleSet": {"rules": [{"field": "contents", "op": "shows", "value": "green"}]}}}),
        )
        .unwrap();
    let album = lightcraft_catalog::AlbumId(album["id"].as_u64().unwrap());
    // everything a model gives, checked against what the stand-in says
    let check = |s: &mut Session, model: &str| {
        let st = s.execute("content.status", &json!({})).unwrap();
        assert_eq!(st["model"], model, "{st}");
        let ids = all_ids(s);
        assert_eq!(st["indexed"].as_u64(), Some(ids.len() as u64), "{st}");
        let dim = if model == "clipfake" { 6 } else { DIM };
        assert_eq!(s.content.index.as_ref().unwrap().vector(ids[0]).unwrap().len(), dim);
        // tags
        let r = tags(s, &ids);
        assert_eq!(r["state"], "ready", "{model}: {r}");
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(photo_tags(&r, i), expected(s, *id, 0.5), "{model}: photo {id}");
        }
        let green: Vec<u64> = ids.iter().copied().filter(|id| score(s, *id, "green") >= 0.5).collect();
        assert!(!green.is_empty() && green.len() < ids.len(), "{model}");
        // search
        let r = s.execute("content.search", &json!({"text": "green", "limit": 1000})).unwrap();
        let mut found: Vec<u64> =
            r["results"].as_array().unwrap().iter().filter(|x| x["score"].as_f64().unwrap() >= 0.5).map(|x| x["id"].as_u64().unwrap()).collect();
        found.sort();
        assert_eq!(found, green, "{model}: search");
        let f = s.execute("library.filter", &json!({"contents": "green"})).unwrap();
        assert_eq!(f["count"].as_u64(), Some(green.len() as u64), "{model}: {f}");
        s.execute("content.clearQuery", &json!({})).unwrap();
        // the Contents filter
        let tagged = ids.iter().filter(|id| expected(s, **id, 0.5).contains(&"colour|green".to_string())).count();
        let f = s.execute("library.filter", &json!({"contentTag": "colour|green"})).unwrap();
        assert_eq!(f["count"].as_u64(), Some(tagged as u64), "{model}: {f}");
        s.execute("library.filter", &json!({"contentTag": null})).unwrap();
        // the Smart Collection (its text is embedded on the text thread)
        let deadline = Instant::now() + Duration::from_secs(20);
        while s.catalog.album_count(album) != green.len() && Instant::now() < deadline {
            s.visible();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(s.catalog.album_count(album), green.len(), "{model}: smart collection");
        // Find Similar Contents: the photo first, then others
        let r = s.execute("content.similar", &json!({"id": green[0], "limit": 3, "show": true})).unwrap();
        assert!(r["results"].as_array().unwrap().iter().all(|x| x["id"] != green[0]), "{model}: {r}");
        let q = s.execute("content.query", &json!({})).unwrap();
        assert!(!q.is_null(), "{model}: the view is ranked by similarity");
        assert_eq!(s.visible().first().map(|p| p.0), Some(green[0]), "{model}");
        s.execute("content.clearQuery", &json!({})).unwrap();
    };
    check(&mut s, "fake");
    // to CLIP: indexed once with it, its own vocabulary embedded
    s.execute("content.models.select", &json!({"id": "clipfake"})).unwrap();
    s.execute("content.pump", &json!({"wait": true})).unwrap();
    check(&mut s, "clipfake");
    s.close_library().unwrap();
    for f in ["content-embeddings-fake.bin", "content-embeddings-clipfake.bin"] {
        assert!(d.join("lib").join(f).is_file(), "{f}");
    }
    for f in ["content-vocab-fake.bin", "content-vocab-clipfake.bin"] {
        assert!(content.join(f).is_file(), "{f}");
    }
    // the model list says each model has an index of this library
    s.open_library(d.join("lib"), true).unwrap();
    let l = s.execute("content.models.list", &json!({})).unwrap();
    for id in ["fake", "clipfake"] {
        let m = l["models"].as_array().unwrap().iter().find(|m| m["id"] == id).unwrap();
        assert_eq!(m["libraryIndex"]["photos"].as_u64(), Some(all_ids(&s).len() as u64), "{m}");
    }
    // and back, and forth again: nothing is embedded, images or texts
    let (texts, loads) = (calls.texts.load(Ordering::SeqCst), calls.loads.load(Ordering::SeqCst));
    for model in ["fake", "clipfake"] {
        s.execute("content.models.select", &json!({"id": model})).unwrap();
        s.execute("content.pump", &json!({"wait": true})).unwrap();
        let ids = all_ids(&s);
        let r = tags(&mut s, &ids);
        assert_eq!(r["state"], "ready", "{model}: {r}");
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(photo_tags(&r, i), expected(&s, *id, 0.5), "{model} again: photo {id}");
        }
    }
    assert_eq!(calls.texts.load(Ordering::SeqCst), texts, "no vocabulary embedded again");
    assert_eq!(calls.loads.load(Ordering::SeqCst), loads, "no model loaded to index again or to tag");
    let _ = std::fs::remove_dir_all(&d);
}

/// Scoring a large library against a full vocabulary: 80,000 photos × 1,500 tags × 768 numbers (the size the plan
/// budgets for) must stay well under a second in a release build; the bound here is generous for a loaded test machine.
#[test]
fn scoring_eighty_thousand_photos_against_fifteen_hundred_tags_is_quick() {
    use half::f16;
    let (photos, terms, dim) = (80_000usize, 1_500usize, 768usize);
    // a cheap deterministic generator (no rand dependency)
    let mut x = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x >> 40) as f32 / (1u64 << 24) as f32 - 0.5
    };
    let mut vocab = Vec::with_capacity(terms * dim);
    for _ in 0..terms {
        let row: Vec<f32> = normalised((0..dim).map(|_| next()).collect());
        vocab.extend(row);
    }
    let mut rows = Vec::with_capacity(photos * dim);
    for _ in 0..photos {
        rows.extend(normalised((0..dim).map(|_| next()).collect()).into_iter().map(f16::from_f32));
    }
    let m =
        crate::content_tags::Matrix { tag: "t".into(), vocab: 1, dim, data: vocab, scoring: Scoring { scale: 112.9, bias: -16.77, sigmoid: true } };
    let t = Instant::now();
    let out = crate::content_tags::tag_rows(&rows, &m, crate::content_tags::FLOOR);
    let took = t.elapsed();
    eprintln!("scored {photos} photos × {terms} tags in {:.0} ms", took.as_secs_f64() * 1e3);
    assert_eq!(out.len(), photos);
    assert!(out.iter().all(|r| r.len() <= crate::content_tags::TOP_K));
    assert!(took < Duration::from_secs(30), "{took:?}");
}

/// The real model (`LIGHTCRAFT_VISION_DIR` = SigLIP 2 base 256's files) with the built-in vocabulary: the demo scenes
/// get the tags they show.
#[cfg(unix)]
#[test]
fn the_real_model_tags_the_demo_scenes() {
    let Some(dir) = std::env::var_os("LIGHTCRAFT_VISION_DIR").map(PathBuf::from) else {
        eprintln!("LIGHTCRAFT_VISION_DIR is not set: skipping");
        return;
    };
    let d = temp("real");
    let content = d.join("content");
    let m = manifest::builtin_siglip2_b16_256();
    let home = content.join(&m.id);
    std::fs::create_dir_all(&home).unwrap();
    for f in &m.files {
        std::os::unix::fs::symlink(dir.join(&f.name), home.join(&f.name)).unwrap();
    }
    std::fs::write(home.join(cm::MANIFEST_FILE), serde_json::to_vec(&m).unwrap()).unwrap();
    std::fs::write(home.join(cm::RECORD_FILE), br#"{"acceptedAt": "2026-10-11T00:00:00Z"}"#).unwrap();
    let st = cm::Settings { enabled: true, model: Some(m.id.clone()), pace: Some("fast".into()), ..Default::default() };
    cm::write_settings(&content, &st).unwrap();
    let mut s = Session::new().with_fs();
    s.content_models_dir = Some(content);
    s.open_library(d.join("lib"), true).unwrap();
    s.execute("content.pump", &json!({"wait": true})).unwrap();
    let t = Instant::now();
    let ids = all_ids(&s);
    let r = tags(&mut s, &ids);
    eprintln!("vocabulary prepared and tags worked out in {:.1} s", t.elapsed().as_secs_f64());
    assert_eq!(r["state"], "ready", "{r}");
    let scenes = lightcraft_scenes::demo_library();
    let mut found = 0;
    for (i, id) in ids.iter().enumerate() {
        let lightcraft_catalog::Source::Demo { scene } = s.catalog.photo(PhotoId(*id)).unwrap().source else { continue };
        let kind = scenes.iter().find(|sc| sc.id == scene).unwrap().kind;
        let got = photo_tags(&r, i);
        eprintln!("{kind:?}: {got:?}");
        let want: &[&str] = match kind {
            lightcraft_scenes::Kind::Beach => &["scene|beach", "nature|sea", "scene|coast", "nature|seascape"],
            lightcraft_scenes::Kind::Macro => &["plant|flower"],
            _ => continue,
        };
        assert!(got.iter().any(|g| want.iter().any(|w| crate::catalog::keywords::is_under(g, w))), "{kind:?}: {got:?}");
        found += 1;
    }
    assert!(found >= 2);
    let _ = std::fs::remove_dir_all(&d);
}
