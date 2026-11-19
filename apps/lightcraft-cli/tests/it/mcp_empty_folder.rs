//! `lightcraft-cli mcp FOLDER` serves even when the folder holds no photos yet.

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn empty_folder_still_serves() {
    let dir = std::env::temp_dir().join(format!("lightcraft-cli-test-{}", std::process::id())).join("mcp-empty-folder");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut child =
        Command::new(super::binary()).arg("mcp").arg(&dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}\n").unwrap();
    drop(input);
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "an empty folder must not stop the server: {stderr}");
    assert!(stderr.contains("serving MCP on stdio"), "CLI must reach the MCP transport: {stderr}");
    assert!(stdout.contains("\"id\":1") && stdout.contains("\"result\""), "initialize must be answered: {stdout}");
}
