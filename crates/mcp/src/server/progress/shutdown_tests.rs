use super::*;
use crate::Backend;
use std::io::{BufReader, Cursor, Read};
use std::process::{Command, Stdio};

struct ExportUntilCancelled {
    hook: Option<ProgressHook>,
    retain_hook: bool,
}

impl Backend for ExportUntilCancelled {
    fn call(&mut self, _: &str, _: Value) -> Result<Value, String> {
        if self.retain_hook {
            return Ok(json!({}));
        }
        let hook = self.hook.as_mut().expect("export installs a progress hook");
        while hook(0, 2, "photo.png") {
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(json!({"cancelled": true}))
    }

    fn has_ui(&self) -> bool {
        false
    }

    fn describe(&self) -> String {
        "test".into()
    }

    fn set_progress(&mut self, hook: Option<ProgressHook>) {
        assert!(!self.retain_hook || hook.is_some(), "synthetic clear-hook panic");
        self.hook = hook;
    }
}

struct FailedOutput {
    flush: bool,
    initialized: bool,
}

impl Write for FailedOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if !self.initialized || self.flush { Ok(bytes.len()) } else { Err(std::io::Error::other("synthetic output failure")) }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.initialized {
            Err(std::io::Error::other("synthetic output failure"))
        } else {
            self.initialized = true;
            Ok(())
        }
    }
}

// The child owns the blocked read; a regression cannot strand the test runner in a thread join.
#[test]
#[ignore = "subprocess helper for the bounded shutdown regression"]
fn output_failure_child() {
    let mode = std::env::var("LIGHTCRAFT_MCP_SHUTDOWN_CASE").unwrap();
    let mut server = Server::new(Box::new(ExportUntilCancelled { hook: None, retain_hook: mode == "retained-hook" }));
    if mode == "retained-hook" {
        let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"export","arguments":{"dir":"unused"}}});
        let mut output = Vec::new();
        server.serve(Cursor::new(format!("{request}\n")), &mut output).unwrap();
        let reply: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(reply["result"]["isError"], true);
        assert!(reply.to_string().contains("synthetic clear-hook panic"));
    } else {
        let error = server.serve_stdio(FailedOutput { flush: mode.ends_with("flush"), initialized: false }).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert_eq!(error.to_string(), "synthetic output failure");
        assert!(server.wire.is_none());
        let mut another = Server::new(Box::new(ExportUntilCancelled { hook: None, retain_hook: false }));
        let error = another.serve_stdio(std::io::sink()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    }
    eprintln!("MCP shutdown verified");
}

#[test]
fn idle_write_failure_does_not_wait_for_input_eof() {
    run_shutdown_case("idle-write");
}

#[test]
fn idle_flush_failure_does_not_wait_for_input_eof() {
    run_shutdown_case("idle-flush");
}

#[test]
fn progress_write_failure_does_not_wait_for_input_eof() {
    run_shutdown_case("progress-write");
}

#[test]
fn progress_flush_failure_does_not_wait_for_input_eof() {
    run_shutdown_case("progress-flush");
}

fn run_shutdown_case(mode: &str) {
    let _serial = crate::serial_sockets_and_children();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "server::progress::shutdown_tests::output_failure_child", "--ignored", "--nocapture"])
        .env("LIGHTCRAFT_MCP_SHUTDOWN_CASE", mode)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    if mode != "retained-hook" {
        let request = if mode.starts_with("progress") {
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"export","arguments":{"dir":"unused"},"_meta":{"progressToken":"photos"}}})
        } else {
            json!({"jsonrpc":"2.0","id":2,"method":"ping"})
        };
        let initialize = json!({"jsonrpc":"2.0","id":0,"method":"initialize"});
        let requests = format!("{initialize}\n{request}\n{{\"jsonrpc\":");
        input.write_all(requests.as_bytes()).unwrap();
        input.flush().unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("{mode}: MCP shutdown waited for input EOF");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut stderr = String::new();
    child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
    assert!(status.success(), "{mode}: {status}: {stderr}");
    assert!(stderr.contains("MCP shutdown verified"), "child did not exercise shutdown: {stderr}");
    drop(input);
}

#[test]
fn retained_progress_hook_does_not_keep_output_alive() {
    run_shutdown_case("retained-hook");
}

#[test]
fn input_errors_and_eof_keep_their_meaning() {
    let mut server = Server::new(Box::new(ExportUntilCancelled { hook: None, retain_hook: false }));
    let error = server.serve(Cursor::new(vec![0xff]), std::io::sink()).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(server.wire.is_none());
    server.serve(Cursor::new(b""), std::io::sink()).unwrap();
}

#[test]
fn reader_panic_is_an_input_error() {
    struct PanickingRead;
    impl Read for PanickingRead {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("synthetic input panic");
        }
    }
    let mut server = Server::new(Box::new(ExportUntilCancelled { hook: None, retain_hook: false }));
    let error = server.serve(BufReader::new(PanickingRead), std::io::sink()).unwrap_err();
    assert_eq!(error.to_string(), "MCP input thread panicked");
}

#[test]
fn output_error_takes_precedence_over_input_error() {
    let mut server = Server::new(Box::new(ExportUntilCancelled { hook: None, retain_hook: false }));
    let mut input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n".to_vec();
    input.push(0xff);
    let error = server.serve(Cursor::new(input), FailedOutput { flush: false, initialized: true }).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Other);
    assert_eq!(error.to_string(), "synthetic output failure");
}
