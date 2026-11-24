//! Every widely used raw body has measured, spectral or dcraw colour matrices, so none renders through the neutral
//! fallback matrix (gap 1, completion plan 3.1). The list stands in for "top bodies by use": no usage telemetry
//! exists, so it is the most common interchangeable-lens bodies in public raw sample sets (raw.pixls.us) and the
//! bodies named in issues. A body added here that fails needs a row from the dcraw / LibRaw / colormatrices tables
//! (AGENTS.md source ranking) or a chart measurement (`docs/chart-calibration.md`).

/// Exif make and model of the top bodies.
const TOP_BODIES: &[(&str, &str)] = &[
    ("SONY", "ILCE-7M3"),
    ("SONY", "ILCE-7M4"),
    ("SONY", "ILCE-7RM4"),
    ("SONY", "ILCE-1"),
    ("SONY", "ILCE-6400"),
    ("SONY", "ILCE-7RM5"),
    ("Canon", "Canon EOS R5"),
    ("Canon", "Canon EOS R6"),
    ("Canon", "Canon EOS 5D Mark IV"),
    ("Canon", "Canon EOS R"),
    ("Canon", "Canon EOS 90D"),
    ("NIKON CORPORATION", "NIKON Z 6_2"),
    ("NIKON CORPORATION", "NIKON Z 8"),
    ("NIKON CORPORATION", "NIKON D850"),
    ("NIKON CORPORATION", "NIKON D750"),
    ("NIKON CORPORATION", "NIKON Z 6"),
    ("FUJIFILM", "X-T4"),
    ("FUJIFILM", "X-T5"),
    ("OLYMPUS CORPORATION", "E-M1MarkII"),
    ("Panasonic", "DC-G9"),
];

#[test]
fn no_top_body_falls_back_to_the_neutral_matrix() {
    let missing: Vec<String> = TOP_BODIES
        .iter()
        .filter(|(make, model)| !lcc_camdb::find(Some(make), model).is_some_and(|e| lcc_raw::color::has_matrix(&e.color_data())))
        .map(|(make, model)| format!("{make} {model}"))
        .collect();
    assert!(missing.is_empty(), "bodies on the neutral fallback matrix: {missing:?}");
}

/// Report (not a failure): each top body's colour source. Bodies without a `Chart` source still need a chart shot.
#[test]
fn colour_source_of_top_bodies() {
    for (make, model) in TOP_BODIES {
        let source = lcc_camdb::find(Some(make), model).map(|e| e.provenance.source);
        println!("{make} {model}: {source:?}");
    }
}

/// Bodies in `supported_bodies.tsv` that no allowed table (dcraw 9.28, LibRaw 2ab6323) has a matrix for. They need a
/// chart shot (`docs/chart-calibration.md`) or a new allowed source; until then they render through the neutral matrix.
const NO_SOURCE_YET: &[(&str, &str)] = &[("Leica", "D-Lux 7"), ("Nikon", "Coolpix P1100"), ("Canon", "EOS 4000D"), ("Canon", "EOS 2000D")];

fn has_matrix(maker: &str, model: &str) -> bool {
    lcc_camdb::find(Some(maker), model)
        .or_else(|| lcc_camdb::find_camera(&format!("{maker} {model}")))
        .is_some_and(|e| lcc_raw::color::has_matrix(&e.color_data()))
}

/// Every body we decode from a pinned sample (`supported_bodies.tsv`) has colour matrices, apart from the listed
/// bodies with no allowed source; a listed body that gains one must leave the list.
#[test]
fn no_supported_body_falls_back_to_the_neutral_matrix() {
    let bodies: Vec<(&str, &str)> = include_str!("supported_bodies.tsv")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some((f.next()?, f.next()?))
        })
        .collect();
    assert!(bodies.len() > 250, "supported_bodies.tsv read {} bodies", bodies.len());
    let missing: Vec<String> =
        bodies.iter().filter(|b| !NO_SOURCE_YET.contains(b) && !has_matrix(b.0, b.1)).map(|(make, model)| format!("{make} {model}")).collect();
    assert!(missing.is_empty(), "bodies on the neutral fallback matrix: {missing:?}");
    let stale: Vec<_> = NO_SOURCE_YET.iter().filter(|(make, model)| has_matrix(make, model)).collect();
    assert!(stale.is_empty(), "now have matrices, drop them from NO_SOURCE_YET: {stale:?}");
}
