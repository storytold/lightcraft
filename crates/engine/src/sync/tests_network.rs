//! A folder listing and a previous lookup cannot authorize later redirected sidecar reads.
use super::*;
use crate::doc_path::network::tests::Fixture;

/// Exercise both discovery and worker reads after replacing a discovered sidecar's directory or leaf.
fn replaced_sidecar_is_refused(leaf: bool) {
    let f = Fixture::new();
    let Some(unc) = f.unc().filter(|p| std::fs::metadata(p).is_ok()) else {
        lightcraft_testkit::skip!(System, "loopback administrative share unavailable");
        return;
    };
    for (folder, rating) in [("selected/safe", 4), ("unselected", 5)] {
        let packet = format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="{rating}"/></rdf:RDF></x:xmpmeta>"#
        );
        std::fs::write(f.0.join(folder).join("photo.xmp"), packet).unwrap();
    }
    let folder = unc.join("selected/safe");
    let original = folder.join("photo.jpg").to_string_lossy().into_owned();
    let mut catalog = Catalog::default();
    let p = Photo::new(catalog.alloc_photo_id(), Source::File { path: original.clone() }, "photo.jpg", "JPEG", 32, 24, "2020-01-01T00:00:00");
    let lookup = SidecarLookup { naming: SidecarNaming::Stem, owns_stem: true };
    let mut listing = Listing::default();
    listing.note(&folder, vec![("photo.xmp".into(), crate::walk::EntryKind::File)], true);
    let found = lookup.find(&original).unwrap();
    let target = crate::crs::Target::for_photo(&p);
    assert!(sidecar_has_news(&catalog, &p, lookup, &listing));
    assert_eq!(read_sidecar(&original, lookup, target).unwrap().rating, Some(4));
    if leaf {
        let redirect = f.0.join("selected/safe/redirect.xmp");
        if let Err(error) = std::os::windows::fs::symlink_file(unc.join("unselected/photo.xmp"), &redirect) {
            lightcraft_testkit::skip!(System, "Windows symlink creation unavailable: {error}");
            return;
        }
        std::fs::remove_file(f.0.join("selected/safe/photo.xmp")).unwrap();
        std::fs::rename(redirect, f.0.join("selected/safe/photo.xmp")).unwrap();
    } else {
        std::fs::rename(f.0.join("selected/safe"), f.0.join("selected/before")).unwrap();
        junction::create(f.0.join("unselected"), f.0.join("selected/safe")).unwrap();
    }
    assert!(!sidecar_has_news(&catalog, &p, lookup, &listing), "cached listing must not bypass redirect checks");
    assert!(read_sidecar_file(&original, &found, target).is_err(), "a previous lookup must not authorize a redirected read");
    assert!(read_sidecar(&original, lookup, target).is_err());
}

/// An existing scan result becomes unusable when its directory is replaced with a junction.
#[test]
fn synchronize_refuses_directory_replacement_after_lookup() {
    replaced_sidecar_is_refused(false);
}

/// The same refusal applies when just the sidecar leaf becomes a symbolic link.
#[test]
fn synchronize_refuses_leaf_replacement_after_lookup() {
    replaced_sidecar_is_refused(true);
}
