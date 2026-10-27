//! A broken protocol output must not require the client to close its input pipe.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn broken_stdout_exits_with_stdin_still_open() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lightcraft-cli"))
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    drop(child.stdout.take());
    input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n{\"jsonrpc\":").unwrap();
    input.flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("MCP did not exit after stdout failed while stdin stayed open");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut stderr = String::new();
    child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
    assert!(stderr.contains("serving MCP on stdio"), "CLI must reach the MCP transport: {stderr}");
    assert!(!status.success(), "broken protocol output must be reported: {stderr}");
    assert!(stderr.contains("lightcraft-cli:"), "CLI must report the transport error: {stderr}");
    drop(input);
}
