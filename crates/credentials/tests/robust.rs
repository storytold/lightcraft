//! Never-crash (P6.2): a damaged or doctored encrypted credentials file is an error, never a panic
//! or an unbounded key derivation.

use dac_credentials::{FileStore, Key, Secret, SecretStore};

#[test]
fn damaged_credentials_file_never_panics() {
    let dir = std::env::temp_dir().join(format!("dac-cred-robust-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("credentials.json");
    let pass = Secret::new("pw");
    let store = FileStore::open(&path, &pass).unwrap();
    store.set(&Key::new("immich", "https://x|me"), &Secret::new("k")).unwrap();
    let seed = std::fs::read(&path).unwrap();
    let probe = dir.join("probe.json");
    // Each open runs Argon2: keep the loop short and skip mutants that ask for more work than the
    // default parameters (the cap itself is covered below).
    dac_fuzzkit::run("credentials.file", &[&seed], 120, |b| {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(b) {
            let cost = |k: &str| v.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0);
            if cost("m_kib") > 19 * 1024 || cost("t") > 2 || cost("p") > 1 {
                return;
            }
        }
        std::fs::write(&probe, b).unwrap();
        if let Ok(s) = FileStore::open(&probe, &pass) {
            let _ = s.get(&Key::new("immich", "https://x|me"));
        }
    });
    // doctored cost parameters are refused before any work
    for (m, t, p) in [(u32::MAX, 2, 1), (19456, u32::MAX, 1), (19456, 2, 0), (0, 0, 0)] {
        let mut v: serde_json::Value = serde_json::from_slice(&seed).unwrap();
        v["m_kib"] = m.into();
        v["t"] = t.into();
        v["p"] = p.into();
        std::fs::write(&probe, serde_json::to_vec(&v).unwrap()).unwrap();
        assert!(FileStore::open(&probe, &pass).is_err());
    }
    let _ = std::fs::remove_dir_all(&dir);
}
