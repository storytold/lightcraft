use std::sync::Mutex;

use super::*;

const SECRET: &str = "immich-api-key-7f3a9c";

fn tempdir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("dac-cred-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

struct Capture;
static LOGS: Mutex<String> = Mutex::new(String::new());

impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, r: &log::Record) {
        LOGS.lock().unwrap().push_str(&format!("{} {}\n", r.target(), r.args()));
    }
    fn flush(&self) {}
}

fn capture_logs() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = log::set_logger(&Capture);
        log::set_max_level(log::LevelFilter::Trace);
    });
}

#[test]
fn file_store_round_trip_and_wrong_passphrase() {
    let dir = tempdir("rt");
    let path = dir.join("sub/credentials.json");
    let pass = Secret::new("correct horse");
    let key = Key::new("immich", "https://photos.home|me@example.org");
    {
        let s = FileStore::open(&path, &pass).unwrap();
        assert_eq!(s.get(&key).unwrap(), None);
        s.set(&key, &Secret::new(SECRET)).unwrap();
        s.set(&Key::new("other", "x"), &Secret::new("y")).unwrap();
    }
    let s = FileStore::open(&path, &pass).unwrap();
    assert_eq!(s.get(&key).unwrap().unwrap().expose(), SECRET);
    assert!(s.delete(&Key::new("other", "x")).unwrap());
    assert!(!s.delete(&Key::new("other", "x")).unwrap());
    assert_eq!(s.get(&key).unwrap().unwrap().expose(), SECRET);

    // the file never holds the plaintext
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(!raw.contains(SECRET) && !raw.contains("photos.home"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    assert_eq!(FileStore::open(&path, &Secret::new("wrong")).unwrap_err(), CredError::WrongPassphrase);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn hostile_files_are_errors() {
    let dir = tempdir("hostile");
    let pass = Secret::new("p");
    let path = dir.join("c.json");
    let s = FileStore::open(&path, &pass).unwrap();
    s.set(&Key::new("a", "b"), &Secret::new("v")).unwrap();
    let good = std::fs::read_to_string(&path).unwrap();

    // flipped ciphertext byte
    let mut env: serde_json::Value = serde_json::from_str(&good).unwrap();
    let data = env["data"].as_str().unwrap().to_string();
    let flipped = format!("{}{}", if data.starts_with('0') { "1" } else { "0" }, &data[1..]);
    env["data"] = flipped.into();
    std::fs::write(&path, env.to_string()).unwrap();
    assert_eq!(FileStore::open(&path, &pass).unwrap_err(), CredError::WrongPassphrase);

    // absurd KDF cost
    let mut env: serde_json::Value = serde_json::from_str(&good).unwrap();
    env["m_kib"] = (u32::MAX).into();
    std::fs::write(&path, env.to_string()).unwrap();
    assert!(matches!(FileStore::open(&path, &pass), Err(CredError::Corrupt(_))));

    for junk in ["", "{}", "not json", r#"{"format":1,"kdf":"argon2id","m_kib":64,"t":1,"p":1,"salt":"zz","nonce":"","data":""}"#] {
        std::fs::write(&path, junk).unwrap();
        assert!(FileStore::open(&path, &pass).is_err(), "{junk}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn secrets_never_reach_debug_output_or_logs() {
    capture_logs();
    let secret = Secret::new(SECRET);
    assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
    assert_eq!(format!("{secret}"), "<redacted>");

    let dir = tempdir("logs");
    let path = dir.join("c.json");
    let s = FileStore::open(&path, &Secret::new("passphrase-xyz")).unwrap();
    let key = Key::new("immich", "acct");
    s.set(&key, &secret).unwrap();
    let got = s.get(&key).unwrap();
    s.delete(&key).unwrap();
    let wrong = FileStore::open(&path, &Secret::new("nope")).err();
    let debug = format!("{s:?} {got:?} {wrong:?}");
    // the OS store may or may not exist here; either way its output must not leak
    let os = os_store().map(|st| st.name()).map_err(|e| e.to_string());
    let logs = LOGS.lock().unwrap().clone();
    assert!(logs.contains("credentials: stored a secret"), "logging works: {logs}");
    for text in [&logs, &debug, &format!("{os:?}")] {
        assert!(!text.contains(SECRET), "secret leaked into {text}");
        assert!(!text.contains("passphrase-xyz"), "passphrase leaked into {text}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// Exercises the real keychain only when asked (it may prompt or touch the user's keyring).
#[test]
fn os_store_round_trip_when_enabled() {
    if std::env::var_os("DAC_TEST_OS_KEYCHAIN").is_none() {
        return;
    }
    let store = os_store().unwrap();
    let key = Key::new("test-service", &format!("test-{}", std::process::id()));
    store.set(&key, &Secret::new(SECRET)).unwrap();
    assert_eq!(store.get(&key).unwrap().unwrap().expose(), SECRET);
    assert!(store.delete(&key).unwrap());
    assert_eq!(store.get(&key).unwrap(), None);
}
