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
        .filter(|(make, model)| {
            !lcc_camdb::find(Some(make), model).is_some_and(|e| lcc_raw::color::has_matrix(&e.color_data()))
        })
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
