//! Run the real callback from a worker to the main thread in a disposable process, with no OS dialog.
#![forbid(unsafe_code)]
#[cfg(target_os = "macos")]
#[path = "../src/native_dialog.rs"]
mod native_dialog;

#[cfg(target_os = "macos")]
fn main() {
    if std::env::args().any(|a| a == "--callback-child") {
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(20));
            eprintln!("native dialog callback timed out");
            std::process::exit(2);
        });
        std::thread::spawn(|| {
            let (tx, rx) = std::sync::mpsc::channel();
            let answer: Option<String> = native_dialog::legacy(&tx, || {
                assert!(objc2::MainThreadMarker::new().is_some());
                panic!("fake NSOpenPanel constructor returned nil");
            });
            assert!(answer.is_none());
            assert!(rx.recv().unwrap().contains("fake NSOpenPanel constructor returned nil"));
            assert_eq!(native_dialog::run(Vec::<String>::new), Ok(vec![]));
            assert_eq!(native_dialog::run(|| vec!["chosen.jpg".to_string()]), Ok(vec!["chosen.jpg".to_string()]));
            assert_eq!(native_dialog::run(|| 42), Ok(42));
            println!("caught constructor panic; main queue answered next command: 42");
            std::process::exit(0);
        });
        loop {
            objc2_core_foundation::CFRunLoop::run();
        }
    }
    let result = std::process::Command::new(std::env::current_exe().unwrap()).arg("--callback-child").output().unwrap();
    assert!(result.status.success(), "callback child failed: {result:?}");
    assert!(String::from_utf8_lossy(&result.stdout).contains("main queue answered next command: 42"));
    println!("native dialog callback regression passed");
}

#[cfg(not(target_os = "macos"))]
fn main() {
    lightcraft_testkit::skip!(System, "the native-dialog dispatch callback is specific to macOS");
}
