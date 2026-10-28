use super::{cargo, layers, metadata, run};

/// Workspace packages that must build for wasm32: all L0–L5 crates plus
/// the egui shell and the web app.
fn wasm_set() -> Result<Vec<String>, String> {
    let crates = layers::from_metadata(&metadata()?)?;
    Ok(crates
        .into_iter()
        .filter(|c| match layers::classify(&c.name) {
            Some(layers::Class::Layer(l)) => l <= 5,
            Some(layers::Class::Standalone) => true,
            _ => false,
        })
        .map(|c| c.name)
        .chain(std::iter::once("lightcraft-web".to_string()))
        .collect())
}

/// One `cargo check` over the whole set; `--keep-going` reports every crate that fails.
pub(crate) fn cmd_wasm() -> Result<(), String> {
    cmd_wasm_for(None)
}

/// The wasm check of the wasm-safe crates, or only those of them in `only` (`ci --quick`).
pub(crate) fn cmd_wasm_for(only: Option<&std::collections::BTreeSet<String>>) -> Result<(), String> {
    let set: Vec<String> = wasm_set()?.into_iter().filter(|p| only.is_none_or(|o| o.contains(p))).collect();
    if set.is_empty() {
        println!("wasm32-unknown-unknown check: no changed crate builds for wasm32");
        return Ok(());
    }
    let mut c = cargo();
    c.args(["check", "--target", "wasm32-unknown-unknown", "--keep-going"]);
    for pkg in &set {
        c.args(["-p", pkg]);
    }
    run(c, &format!("cargo check --target wasm32-unknown-unknown --keep-going -p {}", set.join(" -p ")))
}
