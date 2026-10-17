use std::collections::BTreeSet;

use plist::Value;

#[test]
fn macos_document_types_match_import_extensions_and_use_alternate_rank() {
    let plist = Value::from_reader_xml(include_bytes!("../../../packaging/macos/Info.plist.in").as_slice()).unwrap();
    let plist = plist.as_dictionary().unwrap();
    let mut registered = BTreeSet::new();
    let mut identifiers = BTreeSet::new();
    for entry in plist["CFBundleDocumentTypes"].as_array().unwrap() {
        let entry = entry.as_dictionary().unwrap();
        assert_eq!(entry["LSHandlerRank"].as_string(), Some("Alternate"));
        assert!(matches!(entry["CFBundleTypeRole"].as_string(), Some("Editor" | "Viewer")));
        registered.extend(strings(&entry["CFBundleTypeExtensions"]));
        identifiers.extend(strings(&entry["LSItemContentTypes"]));
    }
    let expected: BTreeSet<_> = lightcraft_engine::import::EXTENSIONS.iter().copied().collect();
    assert_eq!(registered, expected, "run python3 packaging/macos/document_types.py packaging/macos/Info.plist.in");
    // These types already have declarations in supported macOS CoreTypes/RawCameraTypes bundles.
    // The document list must reference them without replacing the system's extension tags.
    let system: BTreeSet<_> = [
        "public.jpeg",
        "public.png",
        "public.tiff",
        "org.webmproject.webp",
        "public.jpeg-xl",
        "public.avif",
        "com.adobe.raw-image",
        "com.canon.cr2-raw-image",
        "com.canon.cr3-raw-image",
        "com.nikon.raw-image",
        "com.nikon.nrw-raw-image",
        "com.sony.arw-raw-image",
        "com.fuji.raw-image",
        "com.olympus.raw-image",
        "com.panasonic.rw2-raw-image",
        "com.leica.rwl-raw-image",
        "com.panasonic.raw-image",
        "com.pentax.raw-image",
        "com.samsung.raw-image",
        "com.adobe.photoshop-image",
        "com.compuserve.gif",
        "com.microsoft.bmp",
        "public.heic",
        "public.heif",
        "com.phaseone.raw-image",
        "com.canon.crw-raw-image",
        "com.konicaminolta.raw-image",
        "com.leafamerica.raw-image",
        "com.epson.raw-image",
        "com.hasselblad.3fr-raw-image",
        "com.hasselblad.fff-raw-image",
    ]
    .into_iter()
    .collect();
    let fallbacks = [
        ("ai.storyteller.lightcraft.image.x3f", "x3f"),
        ("ai.storyteller.lightcraft.image.kdc", "kdc"),
        ("ai.storyteller.lightcraft.image.mef", "mef"),
    ];
    let mut declared = BTreeSet::new();
    let mut tagged = BTreeSet::new();
    for entry in plist["UTImportedTypeDeclarations"].as_array().unwrap() {
        let entry = entry.as_dictionary().unwrap();
        let uti = entry["UTTypeIdentifier"].as_string().unwrap();
        assert!(!system.contains(uti), "system type must not be redeclared: {uti}");
        assert_ne!(uti, "public.camera-raw-image", "never redeclare the system's raw base type");
        assert!(declared.insert(uti), "duplicate UTI: {uti}");
        assert!(identifiers.contains(uti), "unused UTI: {uti}");
        let tags = entry["UTTypeTagSpecification"].as_dictionary().unwrap();
        let (_, expected_tag) = fallbacks.iter().find(|(id, _)| *id == uti).unwrap();
        assert_eq!(strings(&tags["public.filename-extension"]), [*expected_tag], "wrong filename tag for {uti}");
        for ext in strings(&tags["public.filename-extension"]) {
            assert!(tagged.insert(ext), "duplicate extension: {ext}");
        }
        let conforms = strings(&entry["UTTypeConformsTo"]);
        assert!(conforms.contains(&"public.data"));
        assert!(conforms.contains(&"public.image") || conforms.contains(&"public.camera-raw-image"));
    }
    assert_eq!(declared, fallbacks.iter().map(|(uti, _)| *uti).collect());
    assert_eq!(tagged, fallbacks.iter().map(|(_, ext)| *ext).collect());
    assert_eq!(identifiers, system.union(&declared).copied().collect());
    assert!(tagged.is_subset(&expected));
}

fn strings(value: &Value) -> Vec<&str> {
    value.as_array().unwrap().iter().map(|v| v.as_string().unwrap()).collect()
}

#[test]
fn supported_macos_system_image_types_are_not_redeclared() {
    let plist = Value::from_reader_xml(include_bytes!("../../../packaging/macos/Info.plist.in").as_slice()).unwrap();
    for entry in plist.as_dictionary().unwrap()["UTImportedTypeDeclarations"].as_array().unwrap() {
        let id = entry.as_dictionary().unwrap()["UTTypeIdentifier"].as_string().unwrap();
        assert!(!["public.jpeg-xl", "public.avif"].contains(&id), "supported macOS versions already declare {id}");
    }
}

/// Run on each supported macOS release to check the system metadata used by packaging.
#[cfg(target_os = "macos")]
#[test]
fn macos_coretypes_supplies_avif_and_jpeg_xl() {
    let system = Value::from_file("/System/Library/CoreServices/CoreTypes.bundle/Contents/Info.plist").unwrap();
    let system = system.as_dictionary().unwrap();
    for (id, parent, ext, mime) in
        [("public.avif", "public.heif-standard", "avif", "image/avif"), ("public.jpeg-xl", "public.image", "jxl", "image/jxl")]
    {
        let declaration = ["UTExportedTypeDeclarations", "UTImportedTypeDeclarations"]
            .into_iter()
            .flat_map(|key| system.get(key).and_then(Value::as_array).into_iter().flatten())
            .filter_map(Value::as_dictionary)
            .find(|entry| entry.get("UTTypeIdentifier").and_then(Value::as_string) == Some(id))
            .unwrap_or_else(|| panic!("CoreTypes does not declare {id}"));
        let values = |v: &Value| -> Vec<String> {
            if let Some(value) = v.as_string() {
                vec![value.to_string()]
            } else {
                v.as_array().unwrap().iter().map(|v| v.as_string().unwrap().to_string()).collect()
            }
        };
        assert_eq!(values(&declaration["UTTypeConformsTo"]), [parent]);
        let tags = declaration["UTTypeTagSpecification"].as_dictionary().unwrap();
        assert_eq!(values(&tags["public.filename-extension"]), [ext]);
        assert_eq!(values(&tags["public.mime-type"]), [mime]);
        if id == "public.avif" {
            assert_eq!(values(&tags["com.apple.ostype"]), ["AVIF"]);
        }
    }
}
