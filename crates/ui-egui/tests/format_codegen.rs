//! A plural message must work before any translation has been added.

#[allow(dead_code)]
#[path = "../build.rs"]
mod build_script;

#[test]
fn english_only_plural_messages_generate_form_selection() {
    let root = std::env::temp_dir().join(format!("lc-english-plurals-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("ja-formats.json"), "{}").unwrap();
    std::fs::write(
        root.join("en-formats.json"),
        r#"{"Photos: {count}": {"one": "{count} photo", "few": "{count} photos", "many": "{count} photos", "other": "{count} photos"}}"#,
    )
    .unwrap();
    let generated = build_script::generate(&root).unwrap();
    assert!(generated.contains("(\"Photos: {count}\", count = $count:expr"), "the English-only key gets a plural arm");
    assert!(generated.contains("(_, \"one\") => format!(\"{count} photo\""));
    assert!(generated.contains("(_, _) => format!(\"{count} photos\""), "other forms fall back to English too");
    std::fs::remove_dir_all(root).unwrap();
}
