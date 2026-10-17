//! Authentication for the desktop app's loopback control port (`lightcraft --control PORT`).
//!
//! Binding to `127.0.0.1` keeps other machines out, but not other processes on this one, nor a web
//! page that gets a browser to send a line to the port. So every connection must open with
//!
//! ```text
//! {"id": 1, "method": "auth", "params": {"token": "<64 hex digits>"}}
//! ```
//!
//! before any other request runs, as in PhotoCraft. The app takes the token from
//! `--control-token` / `$LIGHTCRAFT_CONTROL_TOKEN`, or makes a fresh one for the run, and writes it
//! to [`token_file`] in the settings folder (one file per port), where `lightcraft-cli --connect` and
//! `lightcraft-cli mcp --connect` read it. A page in a browser can't read that file. See
//! `docs/control-protocol.md` → Authentication.
//!
//! The token itself is made by the desktop app (`apps/lightcraft`), which has the random source;
//! this module only checks, compares, stores and finds tokens, so the CLI and MCP client share it.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The method of the first request on every control connection.
pub const AUTH_METHOD: &str = "auth";
/// Environment variable naming the token, read by the app (as `--control-token`) and by clients.
pub const TOKEN_ENV: &str = "LIGHTCRAFT_CONTROL_TOKEN";
/// Hex digits in a token: 32 random bytes.
pub const TOKEN_HEX_LEN: usize = 64;

/// A well-formed token, lower case: exactly [`TOKEN_HEX_LEN`] hex digits after trimming.
pub fn validate(token: &str) -> Result<String, String> {
    let token = token.trim();
    if token.len() != TOKEN_HEX_LEN || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("a control token is exactly {TOKEN_HEX_LEN} hexadecimal digits"));
    }
    Ok(token.to_ascii_lowercase())
}

/// Hex-encode random bytes into a token.
pub fn encode(bytes: &[u8; TOKEN_HEX_LEN / 2]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut token = String::with_capacity(TOKEN_HEX_LEN);
    for b in bytes {
        token.push(char::from(HEX[usize::from(b >> 4)]));
        token.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
    token
}

/// Compare a supplied token with the expected one without stopping at the first different byte, so
/// the time taken says nothing about how much of a guess was right.
pub fn matches(expected: &str, supplied: &str) -> bool {
    if expected.len() != TOKEN_HEX_LEN || supplied.len() != TOKEN_HEX_LEN {
        return false;
    }
    expected.bytes().zip(supplied.bytes()).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
}

/// Where the app keeps the token for `port`: `control-token-<port>` in the settings folder.
/// `None` without a settings folder (the web build, or `LIGHTCRAFT_CONFIG_DIR` set empty).
pub fn token_file(port: u16) -> Option<PathBuf> {
    token_file_in(crate::config::config_dir().as_deref(), port)
}

fn token_file_in(config_dir: Option<&Path>, port: u16) -> Option<PathBuf> {
    config_dir.map(|dir| dir.join(format!("control-token-{port}")))
}

/// Store `token` for clients, replacing an older run's. On Unix only the owner may read it.
pub fn write_token_file(path: &Path, token: &str) -> Result<(), String> {
    use std::io::Write;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    writeln!(file, "{token}").map_err(|e| format!("{}: {e}", path.display()))
}

/// Read and check a token file.
pub fn read_token_file(path: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    validate(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The token a client should send to the app at `addr` (`host:port`): `$LIGHTCRAFT_CONTROL_TOKEN`,
/// else the app's token file for that port. `Ok(None)` when there is neither (the request then
/// gets the app's "authentication required" reply, which says what to do).
pub fn client_token(addr: &str) -> Result<Option<String>, String> {
    client_token_with(addr, std::env::var(TOKEN_ENV).ok(), crate::config::config_dir().as_deref())
}

fn client_token_with(addr: &str, env: Option<String>, config_dir: Option<&Path>) -> Result<Option<String>, String> {
    if let Some(token) = env.filter(|t| !t.trim().is_empty()) {
        return validate(&token).map(Some).map_err(|e| format!("{TOKEN_ENV}: {e}"));
    }
    let Some(path) = port_of(addr).and_then(|port| token_file_in(config_dir, port)) else { return Ok(None) };
    if !path.exists() {
        return Ok(None);
    }
    read_token_file(&path).map(Some)
}

fn port_of(addr: &str) -> Option<u16> {
    addr.rsplit_once(':').and_then(|(_, port)| port.trim().parse().ok())
}

/// The request a client sends first.
pub fn auth_request(id: Value, token: &str) -> Value {
    json!({"id": id, "method": AUTH_METHOD, "params": {"token": token}})
}

/// The app's answer to a connection's first request, and whether the connection may go on.
/// Anything other than an `auth` request with the right token is refused; the reply never contains
/// the expected token.
pub fn check_auth_request(method: &str, params: &Value, expected: &str) -> (Value, bool) {
    let supplied = params.get("token").and_then(Value::as_str).unwrap_or("");
    if method == AUTH_METHOD && matches(expected, &supplied.to_ascii_lowercase()) {
        return (json!({"ok": true, "result": {"authenticated": true}}), true);
    }
    let why = if method == AUTH_METHOD { "wrong control token" } else { "authentication required" };
    let error = format!(
        "{why}: open the connection with {{\"method\": \"auth\", \"params\": {{\"token\": …}}}}; the app writes its token to \
         control-token-<port> in its settings folder (or set {TOKEN_ENV}); closing the connection"
    );
    (json!({"ok": false, "error": error}), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    #[test]
    fn tokens_are_64_hex_digits_in_lower_case() {
        assert_eq!(validate(&format!(" {} \n", TOKEN.to_uppercase())).unwrap(), TOKEN);
        assert!(validate(&TOKEN[1..]).is_err());
        assert!(validate(&format!("{TOKEN}0")).is_err());
        assert!(validate(&TOKEN.replace('a', "g")).is_err());
        assert!(validate("").is_err());
        assert_eq!(encode(&[0xab; 32]), "ab".repeat(32));
    }

    #[test]
    fn only_the_same_token_matches() {
        assert!(matches(TOKEN, TOKEN));
        let mut other = TOKEN.to_string();
        other.replace_range(63.., "0");
        assert!(!matches(TOKEN, &other));
        assert!(!matches(TOKEN, ""));
        assert!(!matches(TOKEN, &TOKEN[..32]));
    }

    #[test]
    fn the_first_request_must_be_auth_with_the_right_token() {
        let (reply, ok) = check_auth_request("auth", &json!({"token": TOKEN.to_uppercase()}), TOKEN);
        assert!(ok);
        assert_eq!(reply["ok"], true);
        for (method, params) in [
            ("engine.execute", json!({"command": "folder.move"})),
            ("auth", json!({})),
            ("auth", json!({"token": 5})),
            ("auth", json!({"token": "0".repeat(64)})),
        ] {
            let (reply, ok) = check_auth_request(method, &params, TOKEN);
            assert!(!ok, "{method} {params}");
            assert_eq!(reply["ok"], false);
            let text = reply.to_string();
            assert!(!text.contains(TOKEN), "the reply must not reveal the token: {text}");
            assert!(text.contains("closing the connection"), "{text}");
        }
    }

    #[test]
    fn clients_find_the_token_in_the_environment_or_the_ports_file() {
        let dir = std::env::temp_dir().join(format!("lightcraft-control-auth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // nothing anywhere: no token, not an error
        assert_eq!(client_token_with("127.0.0.1:18431", None, Some(&dir)), Ok(None));
        // the app's file for that port
        let path = token_file_in(Some(&dir), 18431).unwrap();
        write_token_file(&path, TOKEN).unwrap();
        assert_eq!(client_token_with("127.0.0.1:18431", None, Some(&dir)), Ok(Some(TOKEN.into())));
        // another port's file is not used
        assert_eq!(client_token_with("127.0.0.1:18432", None, Some(&dir)), Ok(None));
        // the environment wins, and a bad value is an error naming the variable
        let other = "f".repeat(64);
        assert_eq!(client_token_with("127.0.0.1:18431", Some(other.clone()), Some(&dir)), Ok(Some(other)));
        assert!(client_token_with("127.0.0.1:18431", Some("short".into()), Some(&dir)).unwrap_err().contains(TOKEN_ENV));
        // an empty variable counts as unset
        assert_eq!(client_token_with("127.0.0.1:18431", Some(String::new()), Some(&dir)), Ok(Some(TOKEN.into())));
        // a damaged file is an error naming it
        std::fs::write(&path, "nope").unwrap();
        assert!(client_token_with("127.0.0.1:18431", None, Some(&dir)).unwrap_err().contains("control-token-18431"));
        // no settings folder, or no port in the address
        assert_eq!(client_token_with("127.0.0.1:18431", None, None), Ok(None));
        assert_eq!(client_token_with("localhost", None, Some(&dir)), Ok(None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_token_file_is_private_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("lightcraft-control-token-mode-{}", std::process::id()));
        write_token_file(&path, TOKEN).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_file(&path);
    }
}
