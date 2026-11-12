//! `lightcraft-cli mcp --automation-read-root --automation-write-root`, the real binary over stdio
//! (so the roots are in force in every thread): every command and every tool, given paths that
//! leave the roots in each way there is, changes nothing outside them and shows nothing from there.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use serde_json::{Value, json};

use super::binary;

/// In a file outside the roots, and in the names of files there: a reply showing one has read it.
const SECRET: &str = "SECRETCANARY";

/// Parameter and tool argument names that name a file or folder, as a command might spell them.
const READ_NAMES: &[&str] = &["path", "paths", "folder", "file", "files", "source", "dataset", "image", "listing"];
const WRITE_NAMES: &[&str] = &["path", "dir", "destination", "folder", "into", "parent", "previewPath", "output", "dataset"];

/// Fetches from the network whatever it is given, into the app's own folder.
const NOT_WALKED: &[&str] = &["lens.downloadDatabase"];

/// Write a small PNG at `path`.
fn png(path: &Path) {
    let img = lightcraft_raster::Rgba8::from_fn(48, 32, |x, y| [(x * 5) as u8, (y * 7) as u8, 100, 255]);
    std::fs::write(path, lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &Default::default()).unwrap()).unwrap();
}

/// Every file and folder under `dir` with its contents; links are listed, not followed.
fn snapshot(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            out.insert(path.clone(), vec![]);
            snapshot(&path, out);
        } else if kind.is_file() {
            out.insert(path.clone(), std::fs::read(&path).unwrap());
        } else {
            out.insert(path.clone(), std::fs::read_link(&path).unwrap().to_string_lossy().into_owned().into_bytes());
        }
    }
}

/// `--library` is the operator's choice, so roots elsewhere don't stop it, and don't blind the guard
/// that keeps a library out of a Lightroom catalog's folder.
#[test]
fn library_outside_the_roots_opens_unless_it_is_a_lightroom_folder() {
    let base = std::env::temp_dir().join(format!("lc-cli-library-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    for d in ["in", "out", "lib", "lightroom"] {
        std::fs::create_dir_all(base.join(d)).unwrap();
    }
    std::fs::write(base.join("lightroom/Cat.lrcat"), b"not opened").unwrap();
    let open = |library: &str| {
        let mut cmd = Command::new(binary());
        cmd.arg("mcp").arg("--library").arg(base.join(library));
        cmd.arg("--automation-read-root").arg(base.join("in")).arg("--automation-write-root").arg(base.join("out"));
        cmd.stdin(Stdio::null()).output().unwrap()
    };
    let opened = open("lib");
    assert!(opened.status.success(), "{}", String::from_utf8_lossy(&opened.stderr));
    let refused = open("lightroom");
    let why = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success() && why.contains("library.importLightroom"), "{why}");
    let names: Vec<_> = std::fs::read_dir(base.join("lightroom")).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(names, ["Cat.lrcat"], "nothing is added to the catalog's folder");
    let _ = std::fs::remove_dir_all(&base);
}

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    replies: Receiver<String>,
    next: u64,
}

impl Mcp {
    /// Start the real binary over stdio, with the roots asked for.
    fn start(base: &Path, read: bool, write: bool) -> Self {
        let mut cmd = Command::new(binary());
        // (a path with leading blanks is relative: with no roots a write lands here, not in the repository)
        cmd.arg("mcp").current_dir(base);
        if read {
            cmd.arg("--automation-read-root").arg(base.join("in"));
        }
        if write {
            cmd.arg("--automation-write-root").arg(base.join("out"));
        }
        // The app's own folders (not confined) stay out of the way of the test machine's.
        for var in ["HOME", "USERPROFILE", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "APPDATA", "LOCALAPPDATA"] {
            cmd.env(var, base.join("home"));
        }
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let (tx, replies) = channel();
        std::thread::spawn(move || {
            for line in stdout.lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut mcp = Self { child, stdin, replies, next: 0 };
        mcp.rpc("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "escape", "version": "1"}}));
        mcp
    }

    /// Send one JSON-RPC request and wait for its reply, passing over progress notifications.
    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let line = json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params});
        writeln!(self.stdin, "{line}").unwrap();
        self.stdin.flush().unwrap();
        loop {
            let reply = self.replies.recv_timeout(Duration::from_secs(120)).unwrap_or_else(|_| panic!("no reply to {line}"));
            let reply: Value = serde_json::from_str(&reply).unwrap();
            // (notifications about progress come in between)
            if reply["id"] == self.next {
                return reply;
            }
        }
    }

    /// Call a tool.
    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        self.rpc("tools/call", json!({"name": name, "arguments": arguments}))
    }

    /// Call a tool that has to succeed.
    fn ok(&mut self, name: &str, arguments: Value) -> Value {
        let r = self.tool(name, arguments);
        assert_eq!(r["result"]["isError"], false, "{name}: {r}");
        r
    }

    /// Call a tool that has to be refused by the automation roots.
    fn refused(&mut self, name: &str, arguments: Value) {
        let r = self.tool(name, arguments);
        assert_eq!(r["result"]["isError"], true, "{name}: {r}");
        assert!(r.to_string().contains("automation"), "{name}: {r}");
    }
}

impl Drop for Mcp {
    /// Stop the server with the test.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A folder with `in`, `out` and `outside`, and links from the roots to outside.
fn folders(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("lc-escape-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    for d in ["in", "out", "outside/sub", "home"] {
        std::fs::create_dir_all(base.join(d)).unwrap();
    }
    png(&base.join("in/photo.png"));
    png(&base.join("out/photo.png"));
    png(&base.join(format!("outside/{SECRET}.png")));
    png(&base.join(format!("outside/sub/{SECRET}2.png")));
    std::fs::write(base.join(format!("outside/{SECRET}.png.xmp")), format!("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">{SECRET}</x:xmpmeta>")).unwrap();
    std::fs::write(base.join("outside/config.json"), format!("{{\"secret\": \"{SECRET}\"}}")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink(base.join("outside"), base.join("in/linkdir")).unwrap();
        symlink(base.join(format!("outside/{SECRET}.png")), base.join("in/linkfile.png")).unwrap();
        symlink(base.join("outside/nothing-here"), base.join("in/dangling")).unwrap();
        symlink(base.join("outside"), base.join("out/linkdir")).unwrap();
        symlink(base.join("outside/new.png"), base.join("out/dangling.png")).unwrap();
        // and from outside into the write root: writing to one acts on the link, where it stands
        png(&base.join("out/back.png"));
        symlink(base.join("out/back.png"), base.join("outside/back.png")).unwrap();
        symlink(base.join("out"), base.join("outside/backdir")).unwrap();
    }
    base
}

/// The ways out of the roots: to read, and to write.
fn escapes(base: &Path) -> (Vec<String>, Vec<String>) {
    let s = |p: PathBuf| p.to_string_lossy().into_owned();
    let mut read = vec![
        s(base.join("outside")),
        s(base.join(format!("outside/{SECRET}.png"))),
        s(base.join(format!("in/../outside/{SECRET}.png"))),
        format!("  {}", s(base.join(format!("outside/{SECRET}.png")))),
    ];
    let mut write = vec![
        s(base.join("outside/new.png")),
        s(base.join("out/../outside/new.png")),
        s(base.join("out/missing/../../outside/new.png")),
        s(base.join("in/new.png")),
    ];
    if cfg!(unix) {
        read.extend([
            s(base.join("in/linkdir")),
            s(base.join(format!("in/linkdir/{SECRET}.png"))),
            s(base.join("in/linkfile.png")),
            s(base.join("in/dangling")),
        ]);
        write.extend([
            s(base.join("out/linkdir")),
            s(base.join("out/linkdir/new.png")),
            s(base.join("out/dangling.png")),
            s(base.join("outside/back.png")),
            s(base.join("outside/backdir")),
        ]);
    }
    (read, write)
}

/// Parameters with every name for a path set to `target` (a list where a list is meant).
fn path_params(names: &[&str], target: &str) -> Value {
    let mut o = serde_json::Map::new();
    for name in names {
        let value = if matches!(*name, "paths" | "files") {
            json!([target])
        } else if *name == "listing" {
            json!([[target, 1]])
        } else {
            json!(target)
        };
        o.insert((*name).to_string(), value);
    }
    o.insert("identityPlate".to_string(), json!({"image": target}));
    o.insert("watermark".to_string(), json!({"image": target}));
    // `print.set` checks this nested profile path; a flat `image` is the page image, not a file.
    o.insert("job".to_string(), json!({"color": {"profile": {"path": target}}}));
    o.insert("name".to_string(), json!("escape"));
    Value::Object(o)
}

/// Help text names a path-like word, but that word is not a file the roots check. These commands
/// refuse for another reason (a missing id, an album or keyword, an enum, a template token)
/// and never reach the path check. Each one is `Policy::NoPaths`; the walk asserts that, so a
/// command that grows a real path parameter cannot stay on this list.
const REFUSES_EARLIER: &[&str] = &[
    "album.create",
    "album.move",
    "album.reorder",
    "album.sort",
    "beforeAfter.setBefore",
    "keyword.create",
    "keyword.merge",
    "keyword.move",
    "photo.rename",
    "photo.renamePreview",
    "spot.add",
    "spot.update",
];

/// Whether `name` (`job.color.profile.path`) is present in `params`.
fn param_is_set(params: &Value, name: &str) -> bool {
    let mut at = Some(params);
    for part in name.split('.') {
        at = at.and_then(|v| v.get(part));
    }
    at.is_some()
}

/// A read the roots allow. `in/new.png` sits in the read root. `outside/back.png` and
/// `outside/backdir` are links that stand outside and point into the write root, and a read
/// follows the link. A write to either is refused, because a write acts on the link where it stands.
fn escape_is_allowed(access: lightcraft_engine::file_access::Access, target: &str, base: &Path, read: bool, write: bool) -> bool {
    if !matches!(access, lightcraft_engine::file_access::Access::Read) {
        return false;
    }
    let path = Path::new(target.trim());
    (read && path == base.join("in/new.png")) || (write && (path == base.join("outside/back.png") || path == base.join("outside/backdir")))
}

/// The roots check a parameter this call set, and the target is not one that check allows.
fn reaches_path_check(id: &str, params: &Value, base: &Path, target: &str, read: bool, write: bool) -> bool {
    if REFUSES_EARLIER.contains(&id) {
        return false;
    }
    match lightcraft_engine::file_access::commands::policy(id) {
        Some(lightcraft_engine::file_access::commands::Policy::Paths(spec)) => {
            spec.iter().any(|(name, access)| param_is_set(params, name) && !escape_is_allowed(*access, target, base, read, write))
        }
        _ => false,
    }
}

/// Replies may name the path they refuse, never what's in that folder or file.
fn leaks(reply: &Value) -> bool {
    let reply = reply.to_string();
    reply.contains(&format!("{SECRET}2")) || reply.contains(&format!("\\\"secret\\\": \\\"{SECRET}")) || reply.contains(&format!("xmpmeta>{SECRET}"))
}

/// Every command and tool, once per way out. What crossed the roots, in words.
fn walk(tag: &str, read: bool, write: bool) -> Vec<String> {
    let mut found = Vec::new();
    let base = folders(tag);
    let outside = base.join("outside");
    let mut before = BTreeMap::new();
    snapshot(&outside, &mut before);
    let mut mcp = Mcp::start(&base, read, write);
    // The photo has to already sit inside a root. `confine_process` is set before any tool call
    // and cannot be turned off, so this process cannot import a file from outside and then enable
    // the roots. Reads are allowed in the write root, so a write-only server imports from `out`.
    let photo = if read || !write { base.join("in/photo.png") } else { base.join("out/photo.png") };
    mcp.ok("import", json!({"paths": [photo.to_string_lossy()]}));
    let listed = mcp.ok("command_list", json!({}));
    let text = listed["result"]["content"][0]["text"].as_str().unwrap().to_string();
    let commands: Value = serde_json::from_str(&text).unwrap();
    let listed_commands = commands.as_array().or(commands["commands"].as_array()).unwrap();
    let mut ids: Vec<String> =
        listed_commands.iter().filter_map(|c| c["id"].as_str().map(str::to_string)).filter(|id| !NOT_WALKED.contains(&id.as_str())).collect();
    assert!(ids.len() > 300, "the registry: {}", ids.len());
    // those that take the photo away last, so the others have one to work on
    ids.sort_by_key(|id| ["delete", "remove", "forget", "empty"].iter().any(|w| id.to_lowercase().contains(w)));
    for id in REFUSES_EARLIER {
        assert!(
            !matches!(lightcraft_engine::file_access::commands::policy(id), Some(lightcraft_engine::file_access::commands::Policy::Paths(_))),
            "{id} is listed as refusing before the path check, but the roots do check a path of it"
        );
    }
    let (reads, writes) = escapes(&base);
    let targets = reads.iter().map(|t| (READ_NAMES, t)).chain(writes.iter().map(|t| (WRITE_NAMES, t)));
    for (names, target) in targets {
        for id in &ids {
            let params = path_params(names, target);
            let r = mcp.tool("run_command", json!({"command": id, "params": params}));
            if leaks(&r) {
                found.push(format!("{id} {target}: {r}"));
            }
            // A command that reaches the path check has to be refused by the roots, so a missing
            // path and an existing one outside get the same answer. Commands in `REFUSES_EARLIER`
            // fail on a missing id or a non-file parameter first.
            if (read || write) && reaches_path_check(id, &params, &base, target, read, write) && !r.to_string().contains("automation") {
                found.push(format!("{id} {target}: expected an automation refusal: {r}"));
            }
        }
    }
    // the helper tools, with each argument of theirs that names a path
    let tools = mcp.rpc("tools/list", json!({}));
    for tool in tools["result"]["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        if name.starts_with("cmd_") || matches!(name, "run_command" | "command_run" | "command_batch") {
            continue;
        }
        let Some(props) = tool["inputSchema"]["properties"].as_object() else { continue };
        for target in reads.iter().chain(&writes) {
            let mut args = serde_json::Map::new();
            for (key, schema) in props {
                if READ_NAMES.contains(&key.as_str()) || WRITE_NAMES.contains(&key.as_str()) {
                    let list = schema["type"] == "array";
                    args.insert(key.clone(), if list { json!([target]) } else { json!(target) });
                }
            }
            if !args.is_empty() {
                let r = mcp.tool(name, Value::Object(args));
                if leaks(&r) {
                    found.push(format!("{name} {target}: {r}"));
                }
            }
        }
    }
    // nothing from outside got into the library either
    let photos = mcp.tool("query_photos", json!({}));
    if photos.to_string().contains(SECRET) {
        found.push(format!("a photo from outside is in the library: {photos}"));
    }
    let mut after = BTreeMap::new();
    snapshot(&outside, &mut after);
    if before != after {
        let changed: Vec<_> = before.keys().chain(after.keys()).filter(|k| before.get(*k) != after.get(*k)).collect();
        found.push(format!("changed outside the roots: {changed:?}"));
    }
    drop(mcp);
    let _ = std::fs::remove_dir_all(&base);
    found
}

/// No command or tool leaves both roots.
#[test]
fn no_command_or_tool_leaves_both_roots() {
    let found = walk("both", true, true);
    assert!(found.is_empty(), "{found:#?}");
}

/// No command or tool leaves a read only root.
#[test]
fn no_command_or_tool_leaves_a_read_only_root() {
    let found = walk("read", true, false);
    assert!(found.is_empty(), "{found:#?}");
}

/// No command or tool leaves a write only root.
#[test]
fn no_command_or_tool_leaves_a_write_only_root() {
    let found = walk("write", false, true);
    assert!(found.is_empty(), "{found:#?}");
}

/// The walk can see an escape: with no roots the same calls do cross into `outside`.
#[test]
fn without_roots_the_walk_does_find_its_way_out() {
    let found = walk("open", false, false);
    assert!(!found.is_empty(), "the walk reached nothing outside, so its passes would prove nothing");
}

/// Work inside the roots still works and refusals say why.
#[test]
fn work_inside_the_roots_still_works_and_refusals_say_why() {
    let base = folders("work");
    let (photo, out) = (base.join("in/photo.png"), base.join("out"));
    let mut mcp = Mcp::start(&base, true, true);
    mcp.ok("import", json!({"paths": [photo.to_string_lossy()]}));
    let exported = out.join("export.png");
    mcp.ok("export", json!({"path": exported.to_string_lossy()}));
    assert!(exported.is_file());
    let rendered = out.join("render.png");
    let r = mcp.ok("render_photo", json!({"path": rendered.to_string_lossy()}));
    assert_eq!(r["result"]["content"][0]["type"], "image");
    assert!(rendered.is_file());
    mcp.ok("render_photo", json!({}));

    let outside = base.join("outside");
    mcp.refused("import", json!({"paths": [outside.join(format!("{SECRET}.png")).to_string_lossy()]}));
    mcp.refused("export", json!({"path": outside.join("new.png").to_string_lossy()}));
    mcp.refused("export", json!({"dir": outside.to_string_lossy()}));
    mcp.refused("render_photo", json!({"path": outside.join("new.png").to_string_lossy()}));
    // a command nothing knows, and one that is off while confined
    let r = mcp.tool("run_command", json!({"command": "no.such.command"}));
    assert_eq!(r["result"]["isError"], true, "{r}");
    mcp.refused("run_command", json!({"command": "library.devices"}));
    // a graphic and a profile the settings point at, read when the photo is rendered
    let logo = outside.join(format!("{SECRET}.png"));
    mcp.ok("run_command", json!({"command": "slideshow.overlays", "params": {"identityPlate": {"image": ""}}}));
    mcp.refused("run_command", json!({"command": "slideshow.overlays", "params": {"identityPlate": {"image": logo.to_string_lossy()}}}));
    mcp.refused("export", json!({"path": out.join("marked.png").to_string_lossy(), "watermark": {"image": logo.to_string_lossy()}}));
    assert!(!out.join("marked.png").exists());
    assert!(!outside.join("new.png").exists());
    drop(mcp);
    let _ = std::fs::remove_dir_all(&base);
}

/// Links inside a root are followed before they are judged.
#[cfg(unix)]
#[test]
fn links_inside_a_root_are_followed_before_they_are_judged() {
    let base = folders("links");
    let mut mcp = Mcp::start(&base, true, true);
    for path in ["in/linkfile.png", "in/dangling", "in/linkdir"] {
        mcp.refused("import", json!({"paths": [base.join(path).to_string_lossy()]}));
    }
    for path in ["out/linkdir/new.png", "out/dangling.png"] {
        mcp.refused("export", json!({"path": base.join(path).to_string_lossy()}));
    }
    assert!(!base.join("outside/new.png").exists());
    drop(mcp);
    let _ = std::fs::remove_dir_all(&base);
}
