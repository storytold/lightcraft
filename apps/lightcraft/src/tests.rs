use super::*;

#[test]
fn desktop_clear_color_stays_opaque_with_a_modal_open() {
    let mut app = App(
        LightcraftApp::new(Session::new(), Services::default()),
        PrefsWriter::default(),
        #[cfg(target_os = "macos")]
        None,
    );
    app.0.run("app.settings", serde_json::json!({})).unwrap();
    assert!(app.0.ui.dialog.is_some());
    for visuals in [egui::Visuals::dark(), egui::Visuals::light()] {
        assert_eq!(eframe::App::clear_color(&app, &visuals)[3], 1.0);
    }
}

fn dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-app-prefs-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Issue #374: launch arguments that would walk a whole drive or home folder are reported,
/// not imported; empty ones are ignored; the rest are imported as absolute paths.
#[test]
fn launch_arguments_never_walk_a_drive_or_the_home_folder() {
    let home = dir("launch");
    std::fs::create_dir_all(home.join("Trip")).unwrap();
    let args: Vec<String> = ["", "  ", ".", "Trip", &home.to_string_lossy()].iter().map(|s| s.to_string()).collect();
    let LaunchArgs { files, notices, library } = launch_imports_in(&args, Some(&home), Some(&home));
    assert_eq!(library, None);
    assert_eq!(files, [home.join("Trip").to_string_lossy().to_string()]);
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert!(notices.iter().all(|n| n.contains("was not imported")), "{notices:?}");
    // the working folder is a drive root (a launcher's choice): `.` is never the whole drive
    let root = home.ancestors().last().unwrap().to_path_buf();
    let LaunchArgs { files, notices, .. } = launch_imports_in(&[".".to_string()], Some(&root), None);
    assert!(files.iter().all(|f| f.ends_with("DCIM")) && files.len() + notices.len() == 1, "{files:?} {notices:?}");
    let _ = std::fs::remove_dir_all(&home);
}

/// `--library` and `LIGHTCRAFT_LIBRARY` win over a library given as an argument, which is
/// then not remembered either.
#[test]
fn the_library_flag_and_variable_win_over_a_library_argument() {
    use std::path::PathBuf;
    let (flag, env, arg) = (PathBuf::from("flag"), PathBuf::from("env"), PathBuf::from("arg"));
    assert_eq!(launch_library_choice(Some(flag.clone()), Some(&env), Some(arg.clone())), (Some(flag.clone()), false));
    assert_eq!(launch_library_choice(None, Some(&env), Some(arg.clone())), (None, false), "the variable is passed on as itself");
    assert_eq!(launch_library_choice(None, None, Some(arg.clone())), (Some(arg), true));
    assert_eq!(launch_library_choice(None, None, None), (None, false));
    assert_eq!(launch_library_choice(Some(flag.clone()), None, None), (Some(flag), false));
}

/// A library's catalog.snap (double-clicked in Explorer) or its folder given at
/// launch opens that library and is never imported as photos; a second library is left with a notice.
#[test]
fn a_librarys_catalog_given_at_launch_opens_that_library() {
    let home = dir("launch-library");
    let lib = home.join("Wedding LC");
    let mut s = lightcraft_engine::Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.close_library().unwrap();
    let snap = lib.join("catalog.snap");
    assert!(snap.is_file(), "a library keeps its catalog.snap");
    let other = home.join("Other LC");
    let mut s = lightcraft_engine::Session::new().with_fs();
    s.open_library(&other, false).unwrap();
    s.close_library().unwrap();
    for arg in [snap.to_string_lossy().to_string(), lib.to_string_lossy().to_string(), "Wedding LC/catalog.snap".to_string()] {
        let got = launch_imports_in(std::slice::from_ref(&arg), Some(&home), Some(&home.join("elsewhere")));
        assert_eq!(got.library.as_deref(), Some(lib.as_path()), "{arg}");
        assert!(got.files.is_empty() && got.notices.is_empty(), "{arg}: never imported: {got:?}");
    }
    let got = launch_imports_in(&[snap.to_string_lossy().to_string(), other.to_string_lossy().to_string()], Some(&home), None);
    assert_eq!(got.library.as_deref(), Some(lib.as_path()), "the first one");
    assert_eq!(got.notices.len(), 1, "{got:?}");
    let _ = std::fs::remove_dir_all(&home);
}

/// Issue #236: the window answers control requests on its UI thread, so the desktop session
/// adds to Apple Photos on a worker (headless sessions wait instead).
#[test]
fn the_desktop_session_adds_to_apple_photos_in_the_background() {
    let (s, problem) = open_session(true, &Launch::Open { dir: None, alt_asks: false, remembered: false }, false);
    assert!(problem.is_none());
    assert!(s.apple_photos_background);
    assert!(!lightcraft_engine::Session::new().apple_photos_background);
}

/// Issue #1290: with Settings ▸ At launch ▸ Ask me nothing is opened (nor seeded with demo
/// photos) before the user chooses, and the settings are still saved while the list shows (a
/// removed entry is kept), unlike the failed-library window's temporary session.
#[test]
fn asking_at_launch_opens_nothing_and_still_saves_the_settings() {
    let (s, problem) = open_session(false, &Launch::Ask, true);
    assert!(problem.is_none() && s.library.is_none() && s.catalog.is_empty());
    assert!(s.apple_photos_background, "the same desktop session as an opened one");

    let d = dir("writer-ask");
    let path = d.join("ui.json");
    let mut app = LightcraftApp::new(s, Services::default());
    app.ui.settings.recent_libraries = vec!["/gone/a".into(), "/gone/b".into()];
    app.library_problem = Some(LibraryProblem::choose_later(&app.ui.settings));
    let mut w = PrefsWriter { path: Some(path.clone()), ..Default::default() };
    app.ui.settings.recent_libraries.remove(0);
    w.save(&app).unwrap();
    let saved: UiState = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved.settings.recent_libraries, ["/gone/b"]);
    // the failed-library window still writes nothing
    app.library_problem = Some(LibraryProblem::new("/gone/b", "not there"));
    app.ui.settings.recent_libraries.clear();
    w.save(&app).unwrap();
    let saved: UiState = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved.settings.recent_libraries, ["/gone/b"]);
    let _ = std::fs::remove_dir_all(&d);
}

/// Issue #3239: the library remembered in Settings whose folder is missing at launch (a disk or
/// network share not connected) is reported in the library problem window. Nothing is created
/// there (not the folder, not its parents) and no demo photos are added.
#[test]
fn a_remembered_library_missing_at_launch_is_reported_not_made_anew() {
    let d = dir("missing-library");
    let gone = d.join("gone");
    let lib = gone.join("Lib");
    let launch = Launch::Open { dir: Some(lib.clone()), alt_asks: true, remembered: true };
    let (s, problem) = open_session(false, &launch, true);
    let problem = problem.expect("the missing library is reported");
    assert_eq!(problem.path, lib.to_string_lossy());
    assert!(problem.can_retry, "Try Again once the drive is back");
    assert!(problem.existing_only, "Try Again makes no library there either");
    assert!(!gone.exists(), "nothing is created where the library was");
    assert!(s.library.is_none() && s.catalog.is_empty(), "no library, no demo photos");
    // a remembered folder that is there but holds no library is reported too
    std::fs::create_dir_all(&lib).unwrap();
    let (s, problem) = open_session(false, &launch, true);
    assert!(problem.as_ref().is_some_and(|p| p.existing_only) && s.library.is_none() && s.catalog.is_empty(), "{problem:?}");
    assert!(!lightcraft_engine::library::is_library(&lib), "nothing written in it");
    // the default location (or --library) is still made when it's missing, with the demo photos
    let fresh = d.join("fresh").join("Lib");
    let (s, problem) = open_session(false, &Launch::Open { dir: Some(fresh.clone()), alt_asks: true, remembered: false }, true);
    assert!(problem.is_none() && s.library.is_some() && !s.catalog.is_empty(), "{problem:?}");
    assert!(lightcraft_engine::library::is_library(&fresh));
    drop(s);
    let _ = std::fs::remove_dir_all(&d);
}

/// Issue #3239: a remembered library whose folder can't be looked into (no permission) is
/// reported as unreadable at launch, not as gone, and nothing is opened or written there.
#[cfg(unix)]
#[test]
fn a_remembered_library_it_may_not_read_at_launch_says_so() {
    use std::os::unix::fs::PermissionsExt;
    let d = dir("denied-library");
    let lib = d.join("Lib");
    let mut s = lightcraft_engine::Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.close_library().unwrap();
    drop(s);
    let names = |dir: &std::path::Path| {
        let mut names: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name()).collect();
        names.sort();
        names
    };
    let before = names(&lib);
    std::fs::set_permissions(&lib, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&lib).is_ok() {
        std::fs::set_permissions(&lib, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&d);
        lightcraft_testkit::skip!(System, "running as root: a folder with mode 000 is still readable");
        return;
    }
    let launch = Launch::Open { dir: Some(lib.clone()), alt_asks: true, remembered: true };
    let (s, problem) = open_session(false, &launch, true);
    std::fs::set_permissions(&lib, std::fs::Permissions::from_mode(0o755)).unwrap();
    let problem = problem.expect("the unreadable library is reported");
    assert!(problem.error.starts_with("This folder can't be read:"), "{}", problem.error);
    assert!(problem.error.to_lowercase().contains("permission denied"), "{}", problem.error);
    assert!(problem.existing_only, "Try Again makes no library there either");
    assert!(s.library.is_none() && s.catalog.is_empty(), "no library, no demo photos");
    assert_eq!(names(&lib), before, "nothing written there");
    drop(s);
    let _ = std::fs::remove_dir_all(&d);
}

/// Issue #103: a damaged ui.json (which holds the library location) is kept aside and
/// reported, not silently replaced by the defaults.
#[test]
fn damaged_ui_json_is_kept_and_reported() {
    let d = dir("damaged");
    let path = d.join("ui.json");
    std::fs::write(&path, br#"{"settings": {"library_path": "/Volumes/Photos/Lib"#).unwrap();
    let (ui, warning, keep) = load_prefs_at(&path, true);
    assert!(ui.is_none() && !keep);
    let warning = warning.unwrap();
    assert!(warning.contains("damaged") && warning.contains("ui.json.corrupt-"), "{warning}");
    assert!(!path.exists());
    let kept: Vec<_> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
    assert!(kept.len() == 1 && kept[0].starts_with("ui.json.corrupt-"), "{kept:?}");
    // a missing file is just "no settings yet"
    assert_eq!(load_prefs_at(&path, true).1, None);
    let _ = std::fs::remove_dir_all(&d);
}

/// Issue #164: a `--memory` session reads the settings but writes nothing — a damaged file is
/// reported and left where it is, not set aside.
#[test]
fn memory_session_leaves_a_damaged_ui_json_alone() {
    let d = dir("damaged-memory");
    let path = d.join("ui.json");
    let bytes = br#"{"settings": {"library_path": "/Volumes/Photos/Lib"#;
    std::fs::write(&path, bytes).unwrap();
    let (ui, warning, keep) = load_prefs_at(&path, false);
    assert!(ui.is_none() && keep);
    let warning = warning.unwrap();
    assert!(warning.contains("damaged") && warning.contains("nothing is saved"), "{warning}");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn docking_survives_native_settings_file_reload() {
    use lightcraft_ui_egui::docking::Panel;
    let d = dir("docking-reload");
    let path = d.join("ui.json");
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    app.run("ui.dock", serde_json::json!({"operation":"float", "panel":"tools", "rect":[70,80,310,500]})).unwrap();
    app.run("ui.dock", serde_json::json!({"operation":"close", "panel":"tools"})).unwrap();
    let mut writer = PrefsWriter { path: Some(path.clone()), ..Default::default() };
    writer.save_ui(&app.ui, false).unwrap();
    let (restored, warning, keep) = load_prefs_at(&path, false);
    assert!(warning.is_none());
    assert!(!keep);
    let mut restored_app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    restored_app.ui = restored.unwrap();
    restored_app.run("ui.dock", serde_json::json!({"operation":"open", "panel":"tools"})).unwrap();
    assert_eq!(
        restored_app.ui.docking.as_ref().unwrap().layout.floating.iter().find(|g| g.contains(&Panel::Tools)).unwrap().rect,
        [70.0, 80.0, 310.0, 500.0]
    );
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn future_docking_preserves_library_settings_and_the_original_preferences_file() {
    let d = dir("future-docking");
    let path = d.join("ui.json");
    let mut ui = UiState { thumb_size: 210.0, filmstrip: false, ..Default::default() };
    ui.settings.library_path = "/Volumes/Photos/Synthetic Library".into();
    for docking in [serde_json::json!({"layout":{"root":{"FutureLayout":{}}}}), serde_json::json!(false)] {
        let mut saved = serde_json::to_value(&ui).unwrap();
        saved["docking"] = docking;
        assert!(serde_json::from_value::<UiState>(saved.clone()).is_err(), "runtime state parsing stays strict");
        let bytes = serde_json::to_vec(&saved).unwrap();
        for set_aside in [false, true] {
            std::fs::write(&path, &bytes).unwrap();
            let (loaded, warning, keep) = load_prefs_at(&path, set_aside);
            let loaded = loaded.unwrap();
            assert_eq!(loaded.settings.library_path, ui.settings.library_path);
            assert_eq!(loaded.thumb_size, ui.thumb_size);
            assert!(!loaded.filmstrip);
            assert!(loaded.docking.is_none());
            assert!(warning.unwrap().contains("Other settings were loaded"));
            assert!(keep);
            let mut writer = PrefsWriter { path: Some(path.clone()), keep_file: keep, ..Default::default() };
            writer.save_ui(&loaded, false).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1, "a future layout does not rename or replace the settings file");
        }
    }
    std::fs::remove_dir_all(d).unwrap();
}

/// Issue #164: the writer saves changed settings to its file — and nothing at all without one
/// (`--memory`) or while the session is the temporary one offered when the library can't open.
#[test]
fn prefs_writer_writes_nothing_in_a_memory_or_temporary_session() {
    let d = dir("writer");
    let path = d.join("sub").join("ui.json");
    let mut ui = UiState::default();
    // an ordinary session: a change is written, the folder created on the way
    let mut w = PrefsWriter { path: Some(path.clone()), ..Default::default() };
    ui.thumb_size += 50.0;
    w.save_ui(&ui, false).unwrap();
    let saved = std::fs::read(&path).unwrap();
    assert!(String::from_utf8_lossy(&saved).contains("thumbSize"), "{}", String::from_utf8_lossy(&saved));
    // the temporary session (library problem): a further change is not written…
    ui.thumb_size += 50.0;
    w.save_ui(&ui, true).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), saved);
    // …until a library opens
    w.save_ui(&ui, false).unwrap();
    assert_ne!(std::fs::read(&path).unwrap(), saved);
    // --memory: no path, so no file and no folder, whatever changes
    let d2 = dir("writer-memory");
    let _ = std::fs::remove_dir_all(&d2);
    let mut w = PrefsWriter { path: None, ..Default::default() };
    ui.thumb_size += 50.0;
    w.save_ui(&ui, false).unwrap();
    assert!(w.written.is_empty() && !d2.exists());
    let _ = std::fs::remove_dir_all(&d);
}

/// Issue #169: a `--memory` session neither arms the GPU crash sentinel (which would create
/// the settings folder) nor removes a marker a crashed launch left; it still reports it, and
/// the next ordinary launch finds it, reports it and clears it.
#[test]
fn memory_session_leaves_the_gpu_marker_and_folder_alone() {
    let d = dir("gpu-marker-memory");
    let _ = std::fs::remove_dir_all(&d);
    let m = d.join("gpu-init.marker");
    // nothing left behind: no notice, and the folder is not created
    assert_eq!(gpu_crash_check_at(Some(m.clone()), true), None);
    assert!(!d.exists(), "a --memory session must not create the settings folder");
    // a marker left by a crashed launch: reported, kept
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(&m, "GPU device creation started (backends METAL)").unwrap();
    let notice = gpu_crash_check_at(Some(m.clone()), true).unwrap();
    assert!(notice.contains("GPU rendering is now off"), "{notice}");
    assert!(m.exists(), "the marker stays for the next ordinary launch");
    // the next ordinary launch reports it once and removes it
    assert!(gpu_crash_check_at(Some(m.clone()), false).is_some());
    lightcraft_engine::gpu::backend::set_init_marker(None);
    assert!(!m.exists());
    assert_eq!(gpu_crash_check_at(Some(m.clone()), false), None);
    lightcraft_engine::gpu::backend::set_init_marker(None);
    let _ = std::fs::remove_dir_all(&d);
}

/// Issue #1337: the window stopped drawing after the graphics driver reset the GPU during an
/// export, and the dialog said the window "could not open". The stop is recorded; the next
/// launch says why once and renders on the CPU (a `--memory` session leaves the record).
#[test]
fn a_gpu_lost_while_running_turns_the_gpu_off_next_time() {
    let d = dir("gpu-lost");
    let m = d.join("gpu-lost.marker");
    assert_eq!(gpu_lost_check_at(Some(&m), false), None, "nothing recorded");
    assert_eq!(gpu_lost_check_at(None, false), None, "no settings folder");
    record_gpu_lost(Some(&m), "device lost (Unknown): Device is lost");
    let notice = gpu_lost_check_at(Some(&m), true).unwrap();
    assert!(
        notice.contains("(device lost (Unknown): Device is lost)")
            && notice.contains("GPU rendering is now off")
            && notice.contains("Use the GPU for rendering"),
        "{notice}"
    );
    assert!(m.exists(), "a --memory session leaves it");
    assert!(gpu_lost_check_at(Some(&m), false).is_some());
    assert!(!m.exists());
    assert_eq!(gpu_lost_check_at(Some(&m), false), None, "said once");
    // the message box: LightCraft stopped (not "could not open its window"), and why
    let text = stopped_text("its window stopped drawing (the renderer panicked)", Some("device lost (Unknown): Device is lost"));
    assert!(text.contains("reset the GPU (device lost (Unknown)") && text.contains("renders on the CPU from its next start"), "{text}");
    assert_eq!(stopped_text("its window stopped drawing", None), "its window stopped drawing");
    let (title, body) = lightcraft_ui_egui::i18n::stopped_message(&text, Some("/logs/lightcraft.log"));
    assert_eq!(title, "LightCraft stopped");
    assert!(body.starts_with("LightCraft stopped unexpectedly: its window stopped drawing") && body.ends_with("/logs/lightcraft.log"), "{body}");
    let _ = std::fs::remove_dir_all(&d);
}

/// Issue #136: a GPU init marker left by a crashed launch is reported once and removed.
#[test]
fn gpu_crash_marker_is_reported_once() {
    let d = dir("gpu-marker");
    let m = d.join("gpu-init.marker");
    assert_eq!(lightcraft_engine::gpu::backend::take_init_marker(&m), None);
    std::fs::write(&m, "GPU device creation started (backends VULKAN | DX12)").unwrap();
    let what = lightcraft_engine::gpu::backend::take_init_marker(&m).unwrap();
    let notice = gpu_crash_notice(&what);
    assert!(notice.contains("GPU rendering is now off") && notice.contains("Use the GPU for rendering"), "{notice}");
    assert!(!m.exists());
    let _ = std::fs::remove_dir_all(&d);
}

/// Issue #136: Windows windows render with DX12 alone (no Vulkan driver loaded) by default.
#[test]
fn window_backends_follow_the_platform_default() {
    let eframe::egui_wgpu::WgpuSetup::CreateNew(n) = window_wgpu_options().wgpu_setup else { panic!("expected CreateNew") };
    let b = n.instance_descriptor.backends;
    if std::env::var_os("LIGHTCRAFT_GPU_BACKEND").is_none() && std::env::var_os("WGPU_BACKEND").is_none() {
        if cfg!(windows) {
            assert_eq!(b, eframe::wgpu::Backends::DX12);
        } else if cfg!(target_os = "macos") {
            assert_eq!(b, eframe::wgpu::Backends::METAL);
        }
    }
    assert!(!b.is_empty());
}

/// Issue #471: the window's DX12 shaders compile with FXC — wgpu's default loaded any
/// `dxcompiler.dll` on the search path, and one without `dxil.dll` kept the window from opening.
#[test]
fn window_compiles_dx12_shaders_with_fxc() {
    let eframe::egui_wgpu::WgpuSetup::CreateNew(n) = window_wgpu_options().wgpu_setup else { panic!("expected CreateNew") };
    if std::env::var_os("WGPU_DX12_COMPILER").is_none() {
        assert!(matches!(n.instance_descriptor.backend_options.dx12.shader_compiler, eframe::wgpu::Dx12Compiler::Fxc));
    }
}

/// Issue #250: the window's device is created without wgpu's indirect-call validation pipeline,
/// which Intel Macs' Metal compiler fails to build (the device was lost, the window never opened).
#[test]
fn window_has_no_indirect_call_validation_pipeline() {
    let eframe::egui_wgpu::WgpuSetup::CreateNew(n) = window_wgpu_options().wgpu_setup else { panic!("expected CreateNew") };
    if std::env::var_os("WGPU_VALIDATION_INDIRECT_CALL").is_none() {
        assert!(!n.instance_descriptor.flags.contains(eframe::wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL));
    }
}

/// Issue #234: Windows opens links with the URL protocol handler (the default browser), not
/// File Explorer, and passes the whole map link, `?`, `&` and `#` included, as one argument.
#[test]
fn windows_links_open_in_the_browser() {
    let url = "https://www.openstreetmap.org/?mlat=48.856600&mlon=2.352200#map=15/48.856600/2.352200";
    let c = windows_open_url_command(url);
    assert_eq!(c.get_program(), "rundll32");
    let args: Vec<&std::ffi::OsStr> = c.get_args().collect();
    assert_eq!(args, ["url.dll,FileProtocolHandler", url]);
}

/// Issue #272: Explorer gets the file to select quoted after `/select,`, with backslashes and
/// without the verbatim prefix, even when the path has spaces (else it opens Documents).
#[test]
fn explorer_select_quotes_the_path() {
    assert_eq!(explorer_select_arg(r"C:\Users\Me\My Photos\IMG 1.jpg"), r#"/select,"C:\Users\Me\My Photos\IMG 1.jpg""#);
    assert_eq!(explorer_select_arg("D:/Example/Photos/IMG_0001.CR3"), r#"/select,"D:\Example\Photos\IMG_0001.CR3""#);
    assert_eq!(explorer_select_arg(r"\\server\share\My Photos\a.jpg"), r#"/select,"\\server\share\My Photos\a.jpg""#);
    assert_eq!(explorer_select_arg(r"\\?\D:\My Photos\a.jpg"), r#"/select,"D:\My Photos\a.jpg""#);
    assert_eq!(explorer_select_arg(r"\\?\UNC\server\share\a.jpg"), r#"/select,"\\server\share\a.jpg""#);
    // a `"` can't end the quoted path and add arguments to the raw command line
    assert_eq!(explorer_select_arg(r#"C:\a" "C:\b.jpg"#), r#"/select,"C:\a C:\b.jpg""#);
    let c = explorer_select_command(r"C:\My Photos\a.jpg");
    assert_eq!(c.get_program(), "explorer");
    let args: Vec<&std::ffi::OsStr> = c.get_args().collect();
    assert_eq!(args, [r#"/select,"C:\My Photos\a.jpg""#]);
}

#[test]
fn a_control_port_that_is_not_a_number_is_ignored() {
    assert_eq!(control_port_from("--control", Some("7980".into())), Some(7980));
    assert_eq!(control_port_from("--control", Some(" 18001 ".into())), Some(18001));
    assert_eq!(control_port_from("--control", Some("nope".into())), None);
    assert_eq!(control_port_from("--control", Some(String::new())), None);
    assert_eq!(control_port_from("--control", Some("70000".into())), None);
    assert_eq!(control_port_from("LIGHTCRAFT_CONTROL_PORT", None), None);
}

#[test]
fn ui_json_is_written_atomically() {
    let d = dir("atomic");
    let path = d.join("ui.json");
    std::fs::write(&path, b"old").unwrap();
    write_atomic(&path, b"{\"new\": true}").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"{\"new\": true}");
    assert!(!d.join("ui.json.tmp").exists());
    // a failed write leaves the old file
    let missing = d.join("gone").join("ui.json");
    assert!(write_atomic(&missing, b"x").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"{\"new\": true}");
    let ui = UiState::default();
    write_atomic(&path, &serde_json::to_vec_pretty(&ui).unwrap()).unwrap();
    let (loaded, warning, _) = load_prefs_at(&path, true);
    assert!(loaded.is_some() && warning.is_none());
    let _ = std::fs::remove_dir_all(&d);
}

/// Given what the settings remember of the window, when the app's window is built, then it
/// opens at that size, zoomed if it was, where the system puts it; with nothing remembered it
/// opens at the usual size.
#[test]
fn the_window_opens_as_it_was() {
    use lightcraft_ui_egui::window_place::{DEFAULT_SIZE, WindowPlace};
    let fresh = as_remembered(egui::ViewportBuilder::default(), None);
    assert_eq!((fresh.inner_size, fresh.position, fresh.maximized), (Some(egui::Vec2::from(DEFAULT_SIZE)), None, None));
    let b = as_remembered(egui::ViewportBuilder::default(), Some(WindowPlace { size: [1300.0, 800.0], zoomed: true }));
    assert_eq!((b.inner_size, b.position, b.maximized), (Some(egui::vec2(1300.0, 800.0)), None, Some(true)));
    // (not asked to un-zoom: macOS would then resize a window that fills its screen)
    let b = as_remembered(egui::ViewportBuilder::default(), Some(WindowPlace { size: [1300.0, 800.0], zoomed: false }));
    assert_eq!((b.inner_size, b.position, b.maximized), (Some(egui::vec2(1300.0, 800.0)), None, None));
}

/// Given a window that rests at a size, when the settings are written and read again, then
/// they hold how it was. A window in the file that is not one (a hand edit, another version's)
/// reads as nothing remembered and costs no other setting; and a window resized just before
/// the app quits is remembered at that size.
#[test]
fn the_settings_keep_how_the_window_was() {
    use lightcraft_ui_egui::window_place::{Seen, WindowPlace};
    let d = dir("window");
    let path = d.join("ui.json");
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    app.ui.settings.library_path = "/pictures/library".into();
    let mut writer = PrefsWriter { path: Some(path.clone()), ..Default::default() };
    writer.save(&app).unwrap();
    assert!(!std::fs::read_to_string(&path).unwrap().contains("\"window\""), "nothing to remember yet");
    let window = WindowPlace { size: [1300.0, 800.0], zoomed: true };
    app.ui.window = Some(window);
    writer.save(&app).unwrap();
    let (restored, warning, _) = load_prefs_at(&path, false);
    assert_eq!((restored.unwrap().window, warning), (Some(window), None));

    // a window that is no window: the file is read all the same, with its other settings
    let text = std::fs::read_to_string(&path).unwrap();
    for odd in [r#""window": {}"#, r#""window": "wide""#, r#""window": {"size": [1, 2, 3]}"#, r#""window": {"size": [null, 800.0]}"#] {
        let start = text.find("\"window\"").unwrap();
        let end = start + text[start..].find('}').unwrap() + 1;
        std::fs::write(&path, format!("{}{odd}{}", &text[..start], &text[end..])).unwrap();
        let (restored, warning, keep) = load_prefs_at(&path, true);
        let restored = restored.unwrap_or_else(|| panic!("{odd}: {warning:?}"));
        assert_eq!((restored.window, restored.settings.library_path.as_str(), warning, keep), (None, "/pictures/library", None, false), "{odd}");
    }

    // resized, and the app quits a moment later: the new size is what is written
    let mut writer = PrefsWriter { path: Some(path.clone()), ..Default::default() };
    let rest = Some(WindowPlace { size: [1300.0, 800.0], zoomed: false });
    writer.window.note(rest, &Seen { size: Some([1500.0, 900.0]), ..Default::default() }, 0.0);
    writer.window_since = std::time::Instant::now().checked_sub(std::time::Duration::from_millis(400));
    assert!(writer.window_since.is_some());
    assert_eq!(writer.window_at_exit(rest), Some(WindowPlace { size: [1500.0, 900.0], zoomed: false }));
    assert_eq!(PrefsWriter::default().window_at_exit(rest), rest, "a window that was never seen");
    let _ = std::fs::remove_dir_all(&d);
}

/// Given the app's window at a size, when the settings writer looks at it frame after frame,
/// then it asks to be woken while the size has still to rest, remembers the size once it has,
/// and asks for nothing more. A window in the settings that is not one (a script set it) is
/// put right before it is gone by.
#[test]
fn the_writer_follows_the_window_from_frame_to_frame() {
    use lightcraft_ui_egui::window_place::WindowPlace;
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    let mut writer = PrefsWriter::default();
    let ctx = egui::Context::default();
    let pass = |look: &mut dyn FnMut(&egui::Context)| {
        let raw = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1300.0, 800.0))), ..Default::default() };
        let mut out = ctx.run_ui(raw, |ui| look(ui.ctx()));
        out.textures_delta.clear();
        out.viewport_output.get(&egui::ViewportId::ROOT).map(|v| v.repaint_delay)
    };
    // (egui's own first frames ask for another: let them pass)
    for _ in 0..3 {
        pass(&mut |_| {});
    }
    app.ui.window = Some(WindowPlace { size: [f32::NAN, 800.0], zoomed: true });
    let asked = pass(&mut |ctx| writer.tick(&mut app, ctx));
    assert_eq!(app.ui.window, None, "no window, and not yet at rest");
    assert!(asked.is_some_and(|wait| wait <= std::time::Duration::from_secs(1)), "{asked:?}");
    // a second later
    writer.window_since = writer.window_since.and_then(|since| since.checked_sub(std::time::Duration::from_secs(1)));
    pass(&mut |ctx| writer.tick(&mut app, ctx));
    assert_eq!(app.ui.window, Some(WindowPlace { size: [1300.0, 800.0], zoomed: false }));
    let asked = pass(&mut |ctx| writer.tick(&mut app, ctx));
    assert!(asked.is_none_or(|wait| wait > std::time::Duration::from_secs(3600)), "at rest and remembered: {asked:?}");
}

/// #1331: on macOS the process ends inside `on_exit`, before eframe would tear the window down.
/// The check runs in a child (this test binary again), since on macOS it ends the process.
#[test]
fn on_exit_ends_the_process_on_macos() {
    if std::env::var_os("LIGHTCRAFT_TEST_ON_EXIT").is_some() {
        let app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
        let mut app = App(
            app,
            PrefsWriter::default(),
            #[cfg(target_os = "macos")]
            None,
        );
        println!("calling on_exit");
        eframe::App::on_exit(&mut app, None);
        println!("on_exit returned");
        return;
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "tests::on_exit_ends_the_process_on_macos", "--nocapture"])
        .env("LIGHTCRAFT_TEST_ON_EXIT", "1")
        .env("LIGHTCRAFT_NO_PREFS", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(stdout.contains("calling on_exit"), "the child didn't run the test: {stdout}");
    assert_eq!(stdout.contains("on_exit returned"), !cfg!(target_os = "macos"), "{stdout}");
}

/// Quitting leaves no `catalog.lock.owner` behind, though the process ends without running
/// destructors (macOS): `on_exit` releases the library's lock itself. The child opens a library
/// and calls `on_exit` (on macOS that ends it); the parent looks at the folder afterwards. Other
/// systems drop the app after `on_exit`, which removes the note anyway.
#[test]
fn on_exit_removes_the_lock_owner_note() {
    if let Some(dir) = std::env::var_os("LIGHTCRAFT_TEST_LOCK_DIR") {
        let mut session = lightcraft_engine::Session::new();
        session.open_library(&dir, false).unwrap();
        let mut app = App(
            LightcraftApp::new(session, Services::default()),
            PrefsWriter::default(),
            #[cfg(target_os = "macos")]
            None,
        );
        eframe::App::on_exit(&mut app, None);
        return;
    }
    let dir = std::env::temp_dir().join(format!("lc-app-lock-owner-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "tests::on_exit_removes_the_lock_owner_note", "--nocapture"])
        .env("LIGHTCRAFT_TEST_LOCK_DIR", &dir)
        .env("LIGHTCRAFT_NO_PREFS", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(dir.join("catalog.snap").exists(), "the child didn't open and save the library: {stdout}");
    assert!(!dir.join("catalog.lock.owner").exists(), "the owner note was left behind");
    assert!(lightcraft_engine::Session::new().open_library(&dir, false).is_ok(), "the library is still locked");
    let _ = std::fs::remove_dir_all(&dir);
}
