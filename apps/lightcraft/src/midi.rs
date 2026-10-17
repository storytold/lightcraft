//! MIDI controller input (Windows and macOS; see `docs/midi.md`). Without a `midi.toml` nothing is
//! opened: MIDI ports are exclusive on Windows, so LightCraft doesn't take them from other programs
//! unless the user asks. With one, a background thread keeps the ports the map wants open,
//! re-scanning every few seconds so a controller plugged in later is picked up, and sends what the
//! map makes of each message to the UI thread, which applies it as a develop edit or a command.
//! Failing to open MIDI at all just means no controller: it never stops the app.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, PoisonError, RwLock};

use lightcraft_engine::midi::{Action, Map};

/// Re-reads the map file and swaps it in; the message says what happened.
pub type Reload = Box<dyn FnMut() -> Result<String, String>>;

/// The map the port thread is using now.
type Shared = Arc<RwLock<Arc<Map>>>;

/// Where the user's map lives: `$LIGHTCRAFT_MIDI_MAP`, else `midi.toml` in the config folder.
fn map_path() -> Option<PathBuf> {
    std::env::var_os("LIGHTCRAFT_MIDI_MAP")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| lightcraft_engine::config::config_dir().map(|d| d.join("midi.toml")))
}

/// The map in `path`: `Ok(None)` when there is no file, an error saying what is wrong with it otherwise.
fn read_map(path: Option<PathBuf>) -> Result<Option<(Map, PathBuf)>, String> {
    let Some(path) = path.filter(|p| p.exists()) else { return Ok(None) };
    let text = std::fs::read_to_string(&path).map_err(|e| format!("MIDI map {} could not be read: {e}", path.display()))?;
    let map = Map::from_toml(&text).map_err(|e| format!("MIDI map {} was not used: {e}", path.display()))?;
    Ok(Some((map, path)))
}

/// The map at startup: the user's file, or an empty map (MIDI off) without one or with a broken one,
/// plus a message for the user in that last case.
fn load_map() -> (Map, Option<String>) {
    match read_map(map_path()) {
        Ok(Some((map, path))) => {
            log::info!("MIDI map loaded from {} ({} bindings)", path.display(), map.len());
            (map, None)
        }
        Ok(None) => (Map::default(), None),
        Err(e) => (Map::default(), Some(format!("{e}. MIDI input is off."))),
    }
}

/// Re-read the map into `shared`. A file with a mistake leaves the map in use as it was.
fn reload(path: Option<PathBuf>, shared: &Shared) -> Result<String, String> {
    let (map, said) = match read_map(path)? {
        Some((map, path)) => {
            let said = format!("MIDI map reloaded from {} ({} bindings)", path.display(), map.len());
            (map, said)
        }
        None => (Map::default(), "No MIDI map file: MIDI input is off".to_string()),
    };
    *shared.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(map);
    Ok(said)
}

/// Start MIDI input. Returns the actions to apply, a reload function for the `midi.reload` command and
/// a message to show the user if the map file at startup was unusable.
pub fn start(ctx: egui::Context) -> (Receiver<Action>, Reload, Option<String>) {
    let (map, problem) = load_map();
    let shared: Shared = Arc::new(RwLock::new(Arc::new(map)));
    let (tx, rx) = channel::<Action>();
    spawn(ctx, tx, Arc::clone(&shared));
    (rx, Box::new(move || reload(map_path(), &shared)), problem)
}

#[cfg(any(windows, target_os = "macos"))]
fn spawn(ctx: egui::Context, tx: std::sync::mpsc::Sender<Action>, shared: Shared) {
    use std::collections::HashMap;
    use std::sync::mpsc::Sender;
    use std::time::Duration;

    use midir::{MidiInput, MidiInputConnection};

    /// How often the port list is checked for hot-plugged devices.
    const RESCAN: Duration = Duration::from_secs(3);

    fn scan(tx: &Sender<Action>, ctx: &egui::Context, shared: &Shared, map: &Map, open: &mut HashMap<String, MidiInputConnection<()>>) {
        let Ok(probe) = MidiInput::new("LightCraft probe") else { return };
        let names: Vec<String> = probe.ports().iter().filter_map(|p| probe.port_name(p).ok()).filter(|n| map.wants_input(n)).collect();
        // closing a vanished or no longer wanted port frees its slot, so it can be opened again later
        open.retain(|name, _| names.contains(name));
        for name in names {
            if open.contains_key(&name) {
                continue;
            }
            let Ok(input) = MidiInput::new("LightCraft") else { continue };
            let Some(port) = input.ports().into_iter().find(|p| input.port_name(p).is_ok_and(|n| n == name)) else { continue };
            let (tx, ctx, shared, port_name) = (tx.clone(), ctx.clone(), Arc::clone(shared), name.clone());
            let conn = input.connect(
                &port,
                "lightcraft-in",
                move |_stamp, bytes, _| {
                    log::debug!("MIDI {port_name}: {bytes:02X?}");
                    // the map in use now, so a reload takes effect without reopening the port
                    let map = Arc::clone(&shared.read().unwrap_or_else(PoisonError::into_inner));
                    if let Some(action) = lightcraft_engine::midi::parse(bytes).and_then(|msg| map.action(msg))
                        && tx.send(action).is_ok()
                    {
                        ctx.request_repaint();
                    }
                },
                (),
            );
            match conn {
                Ok(c) => {
                    log::info!("MIDI input opened: {name}");
                    open.insert(name, c);
                }
                Err(e) => log::warn!("MIDI input {name} could not be opened: {e}"),
            }
        }
    }

    let spawned = std::thread::Builder::new().name("midi-input".into()).spawn(move || {
        let mut open = HashMap::new();
        loop {
            let map = Arc::clone(&shared.read().unwrap_or_else(PoisonError::into_inner));
            if map.is_empty() {
                // no map: hold no ports
                open.clear();
            } else {
                scan(&tx, &ctx, &shared, &map, &mut open);
            }
            std::thread::sleep(RESCAN);
        }
    });
    if let Err(e) = spawned {
        log::warn!("MIDI input thread could not start: {e}");
    }
}

/// No MIDI backend on this platform yet (Linux's needs ALSA, a C library).
#[cfg(not(any(windows, target_os = "macos")))]
fn spawn(_ctx: egui::Context, _tx: std::sync::mpsc::Sender<Action>, _shared: Shared) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(name: &str, text: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lightcraft-midi-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    fn shared() -> Shared {
        Arc::new(RwLock::new(Arc::new(Map::default())))
    }

    fn bindings(shared: &Shared) -> usize {
        shared.read().unwrap().len()
    }

    #[test]
    fn no_file_means_no_map() {
        assert_eq!(read_map(None), Ok(None));
        assert_eq!(read_map(Some(PathBuf::from("/no/such/midi.toml"))), Ok(None));
    }

    #[test]
    fn reload_swaps_in_a_good_file_and_turns_midi_off_when_the_file_is_gone() {
        let shared = shared();
        let path = write("good.toml", r#"bindings = [{ on = "note", number = 1, run = "library.next" }]"#);
        assert!(reload(Some(path), &shared).unwrap().contains("1 bindings"));
        assert_eq!(bindings(&shared), 1);
        assert!(reload(Some(PathBuf::from("/no/such/midi.toml")), &shared).unwrap().contains("off"));
        assert_eq!(bindings(&shared), 0);
    }

    #[test]
    fn reload_keeps_the_map_in_use_when_the_file_has_a_mistake() {
        let shared = shared();
        let good = write("keep-good.toml", r#"bindings = [{ on = "note", number = 1, run = "library.next" }]"#);
        reload(Some(good), &shared).unwrap();
        let bad = write("keep-bad.toml", r#"bindings = [{ on = "note", number = 1, run = "no.such.command" }]"#);
        let e = reload(Some(bad), &shared).unwrap_err();
        assert!(e.contains("binding 1") && e.contains("no.such.command"), "{e}");
        assert_eq!(bindings(&shared), 1);
    }
}
