//! `cargo xtask android package`: the Android app as an APK (GitHub releases, sideloading) and,
//! with `--aab`, an Android App Bundle (the Play Store), in `<target>/android/`.
//!
//! `--abi` (repeatable: `arm64`, `x86_64`) picks the processors; every one is built in a single cargo run
//! (`--target` once per ABI), which shares the host-side build and keeps all cores busy. One APK holds every
//! ABI, or, with `--split`, one APK per ABI (what a release ships: phones take arm64, the emulator and
//! Chromebooks x86_64); the bundle always holds them all and the Play Store hands each device its own.
//!
//! No Gradle: the activity is one Java file on the platform's `NativeActivity` (no AndroidX), so
//! the SDK's own tools do it directly: `javac` + `d8` for the activity, `aapt2` for the manifest
//! and launcher icon, the native library from `cargo build`, `zipalign -P 16` (16 KB pages, as
//! Play requires) and `apksigner`. The bundle is built from the same parts by `bundletool`
//! (downloaded once, pinned by sha256) and signed with `jarsigner`.
//!
//! Two distributions differ only in the manifest ([`Store`]): the APKs are the GitHub build (`--store github`,
//! the default), which may ask for "All files access" (`MANAGE_EXTERNAL_STORAGE`) to read picked folders in place;
//! the bundle is always the Play build, which has neither that permission nor `requestLegacyExternalStorage` (Play
//! allows All files access only for file managers, backup and similar apps). `--store play` makes the APKs the Play
//! build too, to try it by sideloading.
//!
//! Signing: `ANDROID_KEYSTORE` (a keystore file), `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS`
//! (default `upload`) and `ANDROID_KEY_PASSWORD` (default: the keystore's). Without them a debug
//! key in `<target>/android/debug.keystore` signs the build (installable for testing; the Play
//! Store refuses it), with a warning.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::android::{Abi, MIN_API, configure, ndk_bin, ndk_root, target_installed};
use crate::{brand, cargo, dist, root, run as step};

/// The API level the app targets (what the Play Store asks for in 2026).
const TARGET_SDK: u32 = 36;
const BUNDLETOOL_VERSION: &str = "1.18.1";
const BUNDLETOOL_SHA256: &str = "675786493983787ffa11550bdb7c0715679a44e1643f3ff980a529e9c822595c";

/// Which distribution a manifest is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Store {
    /// GitHub releases (sideloaded): may ask for All files access, so picked folders are read in place.
    Github,
    /// Google Play: no All files access; folders are copied, the camera roll goes through MediaStore.
    Play,
}

impl Store {
    fn parse(s: &str) -> Option<Store> {
        match s {
            "github" => Some(Store::Github),
            "play" => Some(Store::Play),
            _ => None,
        }
    }

    /// The manifest template's per-distribution placeholders.
    fn manifest_vars(self, vars: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut v = vars.clone();
        let (perm, legacy) = match self {
            Store::Github => {
                (r#"<uses-permission android:name="android.permission.MANAGE_EXTERNAL_STORAGE" />"#, r#"android:requestLegacyExternalStorage="true""#)
            }
            Store::Play => ("", ""),
        };
        v.insert("all_files_permission".into(), perm.into());
        v.insert("legacy_storage".into(), legacy.into());
        v
    }
}

struct Options {
    abis: Vec<Abi>,
    aab: bool,
    /// the distribution the APKs are for (the bundle is always [`Store::Play`])
    store: Store,
    /// one APK per ABI instead of one holding them all
    split: bool,
    skip_build: bool,
    /// `--version-code`: replaces the code derived from the version (a Play re-upload of the same version)
    version_code: Option<u32>,
}

/// The largest version code Google Play accepts.
const MAX_VERSION_CODE: u32 = 2_100_000_000;

fn parse_version_code(v: &str) -> Result<u32, String> {
    let n: u32 = v.trim().parse().map_err(|_| format!("--version-code: `{v}` is not a positive whole number"))?;
    if n == 0 || n > MAX_VERSION_CODE {
        return Err(format!("--version-code: {n} is out of range (1 to {MAX_VERSION_CODE})"));
    }
    Ok(n)
}

fn parse(args: &[&str]) -> Result<Options, String> {
    let mut o = Options { abis: Vec::new(), aab: false, store: Store::Github, split: false, skip_build: false, version_code: None };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--abi" => {
                let v = it.next().ok_or("--abi needs arm64 or x86_64")?;
                let abi = Abi::parse(v).ok_or_else(|| format!("--abi: unknown `{v}` (arm64 or x86_64)"))?;
                if !o.abis.contains(&abi) {
                    o.abis.push(abi);
                }
            }
            "--aab" => o.aab = true,
            "--store" => {
                let v = it.next().ok_or("--store needs github or play")?;
                o.store = Store::parse(v).ok_or_else(|| format!("--store: unknown `{v}` (github or play)"))?;
            }
            "--split" => o.split = true,
            "--skip-build" => o.skip_build = true,
            "--version-code" => {
                let v = it.next().ok_or("--version-code needs a number")?;
                o.version_code = Some(parse_version_code(v)?);
            }
            other => return Err(format!("android package: unknown option `{other}`")),
        }
    }
    if o.abis.is_empty() {
        o.abis.push(Abi::Arm64);
    }
    Ok(o)
}

/// The Android SDK: `ANDROID_HOME`, `ANDROID_SDK_ROOT`, `~/Android/Sdk` or `/opt/android-sdk`.
fn sdk_root() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = ["ANDROID_HOME", "ANDROID_SDK_ROOT"].iter().filter_map(|v| std::env::var_os(v).map(PathBuf::from)).collect();
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join("Android/Sdk"));
    }
    candidates.push(PathBuf::from("/opt/android-sdk"));
    candidates.into_iter().find(|p| p.join("build-tools").is_dir())
}

/// A version-named folder's numeric parts, for picking the newest.
fn version_key(name: &str) -> Vec<u32> {
    name.split(|c: char| !c.is_ascii_digit()).filter_map(|p| p.parse().ok()).collect()
}

/// The newest build-tools folder that has `aapt2`.
fn build_tools(sdk: &Path) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> =
        std::fs::read_dir(sdk.join("build-tools")).ok()?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.join("aapt2").is_file()).collect();
    dirs.sort_by_key(|p| version_key(&p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()));
    dirs.pop()
}

/// `android.jar` of the target platform, else of the newest one installed.
fn android_jar(sdk: &Path) -> Option<PathBuf> {
    let wanted = sdk.join(format!("platforms/android-{TARGET_SDK}/android.jar"));
    if wanted.is_file() {
        return Some(wanted);
    }
    let mut jars: Vec<PathBuf> =
        std::fs::read_dir(sdk.join("platforms")).ok()?.filter_map(|e| e.ok().map(|e| e.path().join("android.jar"))).filter(|p| p.is_file()).collect();
    jars.sort_by_key(|p| version_key(&p.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default()));
    jars.pop()
}

/// A JDK tool: `$JAVA_HOME/bin/<name>`, else from `PATH`.
fn jdk(name: &str) -> Command {
    match std::env::var_os("JAVA_HOME").map(|h| PathBuf::from(h).join("bin").join(name)).filter(|p| p.is_file()) {
        Some(p) => Command::new(p),
        None => Command::new(name),
    }
}

/// An SDK build tool. `d8` and `apksigner` are shell scripts that run `java` from `PATH`, so `JAVA_HOME`'s
/// `bin` goes first on it: on a machine with no system Java (the CI runners, where
/// `install-android-sdk.sh` sets only `JAVA_HOME`) `d8` otherwise failed with exit status 127.
fn sdk_tool(tool: PathBuf) -> Command {
    let mut c = Command::new(tool);
    if let Some(path) = path_with_java(std::env::var_os("JAVA_HOME"), std::env::var_os("PATH")) {
        c.env("PATH", path);
    }
    c
}

/// `PATH` with `$JAVA_HOME/bin` in front, or `None` without a `JAVA_HOME`.
fn path_with_java(java_home: Option<std::ffi::OsString>, path: Option<std::ffi::OsString>) -> Option<std::ffi::OsString> {
    let bin = PathBuf::from(java_home.filter(|h| !h.is_empty())?).join("bin");
    let rest = path.map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default();
    std::env::join_paths(std::iter::once(bin).chain(rest)).ok()
}

/// `versionCode`: grows with every release, a prerelease below its release
/// (`1.2.3-rc4` → 1_02_03_04, `1.2.3` → 1_02_03_50). Prereleases use 01–49; 51–99 are left free for
/// re-uploads of the same release to Google Play (`--version-code`), since every upload consumes its code.
pub fn version_code(version: &str) -> Result<u32, String> {
    let (main, pre) = version.split_once('-').map_or((version, None), |(m, p)| (m, Some(p)));
    let nums: Vec<u32> =
        main.split('.').map(|p| p.parse::<u32>().map_err(|_| format!("version `{version}`: `{p}` is not a number"))).collect::<Result<_, _>>()?;
    let [major, minor, patch] = nums[..] else { return Err(format!("version `{version}` is not X.Y.Z")) };
    if minor > 99 || patch > 99 || major > 2000 {
        return Err(format!("version `{version}`: too large for a versionCode"));
    }
    let build = match pre {
        None => 50,
        Some(p) => {
            let n: u32 = p.trim_start_matches(|c: char| !c.is_ascii_digit()).parse().unwrap_or(0);
            n.clamp(1, 49)
        }
    };
    Ok(major * 1_000_000 + minor * 10_000 + patch * 100 + build)
}

struct Key {
    keystore: PathBuf,
    store_pass: String,
    alias: String,
    key_pass: String,
    debug: bool,
}

fn signing_key(out: &Path) -> Result<Key, String> {
    if let Some(ks) = std::env::var_os("ANDROID_KEYSTORE").map(PathBuf::from).filter(|p| !p.as_os_str().is_empty()) {
        let store_pass = std::env::var("ANDROID_KEYSTORE_PASSWORD").map_err(|_| "ANDROID_KEYSTORE is set but ANDROID_KEYSTORE_PASSWORD is not")?;
        let alias = std::env::var("ANDROID_KEY_ALIAS").ok().filter(|a| !a.is_empty()).unwrap_or_else(|| "upload".into());
        let key_pass = std::env::var("ANDROID_KEY_PASSWORD").ok().filter(|p| !p.is_empty()).unwrap_or_else(|| store_pass.clone());
        return Ok(Key { keystore: ks, store_pass, alias, key_pass, debug: false });
    }
    let ks = out.join("debug.keystore");
    if !ks.is_file() {
        let mut c = jdk("keytool");
        c.args(["-genkeypair", "-noprompt", "-keystore"]).arg(&ks).args([
            "-storepass",
            "android",
            "-keypass",
            "android",
            "-alias",
            "androiddebugkey",
            "-keyalg",
            "RSA",
            "-keysize",
            "2048",
            "-validity",
            "10000",
            "-dname",
            "CN=Android Debug,O=Android,C=US",
        ]);
        step(c, "keytool -genkeypair (debug key)")?;
    }
    eprintln!("android: WARNING: no ANDROID_KEYSTORE; signed with a debug key (fine for testing, refused by the Play Store)");
    Ok(Key { keystore: ks, store_pass: "android".into(), alias: "androiddebugkey".into(), key_pass: "android".into(), debug: true })
}

fn mkdir(p: &Path) -> Result<(), String> {
    std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))
}

fn fresh_dir(p: &Path) -> Result<(), String> {
    if p.exists() {
        std::fs::remove_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    mkdir(p)
}

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(dir) = to.parent() {
        mkdir(dir)?;
    }
    std::fs::copy(from, to).map(|_| ()).map_err(|e| format!("copy {} → {}: {e}", from.display(), to.display()))
}

/// Every file below `dir`, sorted (for a reproducible order).
fn files_below(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() { stack.push(p) } else { out.push(p) }
        }
    }
    out.sort();
    out
}

/// Copies `src`'s entries (as they are) to `dst` under `prefix`, renamed by `rename`, then adds
/// `extra` (zip path, file) stored uncompressed.
fn rezip(src: &Path, dst: &Path, rename: impl Fn(&str) -> Option<String>, extra: &[(String, PathBuf)]) -> Result<(), String> {
    let err = |e: zip::result::ZipError| format!("{}: {e}", dst.display());
    let input = std::fs::File::open(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let mut archive = zip::ZipArchive::new(input).map_err(|e| format!("{}: {e}", src.display()))?;
    let output = std::fs::File::create(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    let mut w = zip::ZipWriter::new(output);
    for i in 0..archive.len() {
        let entry = archive.by_index_raw(i).map_err(|e| format!("{}: {e}", src.display()))?;
        let name = entry.name().map_err(|e| format!("{}: {e}", src.display()))?;
        let Some(name) = rename(&name) else { continue };
        w.raw_copy_file_rename(entry, name).map_err(err)?;
    }
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, file) in extra {
        let mut bytes = Vec::new();
        std::fs::File::open(file).and_then(|mut f| f.read_to_end(&mut bytes)).map_err(|e| format!("{}: {e}", file.display()))?;
        w.start_file(name.as_str(), stored).map_err(err)?;
        w.write_all(&bytes).map_err(|e| format!("{}: {e}", dst.display()))?;
    }
    w.finish().map_err(err)?;
    Ok(())
}

/// bundletool, downloaded once into the cache and checked against its pinned sha256.
fn bundletool(out: &Path) -> Result<PathBuf, String> {
    use sha2::{Digest, Sha256};
    let jar = out.join(format!("bundletool-all-{BUNDLETOOL_VERSION}.jar"));
    let ok = |p: &Path| std::fs::read(p).is_ok_and(|b| format!("{:x}", Sha256::digest(&b)) == BUNDLETOOL_SHA256);
    if ok(&jar) {
        return Ok(jar);
    }
    let url = format!("https://github.com/google/bundletool/releases/download/{BUNDLETOOL_VERSION}/bundletool-all-{BUNDLETOOL_VERSION}.jar");
    let mut c = Command::new("curl");
    c.args(["-sSfL", "-o"]).arg(&jar).arg(&url);
    step(c, &format!("download bundletool {BUNDLETOOL_VERSION}"))?;
    if !ok(&jar) {
        let _ = std::fs::remove_file(&jar);
        return Err(format!("bundletool {BUNDLETOOL_VERSION}: the download doesn't match its pinned sha256"));
    }
    Ok(jar)
}

/// The APKs to write: file name and the ABI folders (`lib/<dir>/`) each one holds. One APK with every
/// ABI, or with `split` one per ABI, named after the ABIs it carries.
fn apk_outputs(binary: &str, version: &str, abis: &[Abi], split: bool) -> Vec<(String, Vec<&'static str>)> {
    let name = |dirs: &[&str]| format!("{binary}-{version}-android-{}.apk", dirs.join("-"));
    if split {
        abis.iter().map(|a| (name(&[a.dir()]), vec![a.dir()])).collect()
    } else {
        let dirs: Vec<&'static str> = abis.iter().map(|a| a.dir()).collect();
        vec![(name(&dirs), dirs)]
    }
}

pub fn package(args: &[&str]) -> Result<(), String> {
    let opts = parse(args)?;
    let root = root();
    let brand = brand::Brand::current()?;
    let vars = dist::vars(&root, &brand)?;
    let version = vars.get("version").cloned().ok_or("no version")?;
    let code = match opts.version_code {
        Some(c) => c,
        None => version_code(&version)?,
    };
    let binary = brand.req("binary")?.to_string();

    let sdk = sdk_root().ok_or("android: the Android SDK was not found (set ANDROID_HOME)")?;
    let tools = build_tools(&sdk).ok_or_else(|| format!("android: no build-tools with aapt2 in {}", sdk.display()))?;
    let jar = android_jar(&sdk).ok_or_else(|| format!("android: no platform android.jar in {}", sdk.display()))?;
    let target = dist::target_dir(&root);
    let out = target.join("android");
    let work = out.join("work");
    fresh_dir(&work)?;

    // 1. the native library, per processor: the `dist` profile (thin LTO, no debug info), stripped
    let ndk = ndk_root().as_deref().and_then(ndk_bin).ok_or("android: the Android NDK was not found (set ANDROID_NDK_HOME)")?;
    if !opts.skip_build {
        if let Some(abi) = opts.abis.iter().find(|a| !target_installed(a.triple())) {
            return Err(format!("android: `rustup target add {}` first", abi.triple()));
        }
        let mut c = cargo();
        c.args(["build", "--profile", "dist", "-p", "lcc-android"]);
        for abi in &opts.abis {
            c.args(["--target", abi.triple()]);
            configure(&mut c, &ndk, *abi);
        }
        let targets = opts.abis.iter().map(|a| format!("--target {}", a.triple())).collect::<Vec<_>>().join(" ");
        step(c, &format!("cargo build --profile dist {targets} -p lcc-android"))?;
    }
    let mut libs = Vec::new();
    for abi in &opts.abis {
        let so = target.join(abi.triple()).join("dist/libapp.so");
        if !so.is_file() {
            return Err(format!("android: {} is missing (build without --skip-build)", so.display()));
        }
        let stripped = work.join("lib").join(abi.dir()).join("libapp.so");
        mkdir(stripped.parent().ok_or("no parent")?)?;
        let mut c = Command::new(ndk.join("llvm-strip"));
        c.arg("--strip-all").arg("-o").arg(&stripped).arg(&so);
        step(c, "llvm-strip")?;
        libs.push((abi.dir(), stripped));
    }

    // 2. the manifest and activity, from packaging/android
    let render = |rel: &str, dest_rel: &str, vars: &BTreeMap<String, String>| -> Result<PathBuf, String> {
        let src = root.join("packaging/android").join(format!("{rel}.in"));
        let text = std::fs::read_to_string(&src).map_err(|e| format!("{}: {e}", src.display()))?;
        let text = brand::render(&text, vars).map_err(|e| format!("{}: {e}", src.display()))?;
        let dest = work.join("gen").join(dest_rel);
        if let Some(d) = dest.parent() {
            mkdir(d)?;
        }
        std::fs::write(&dest, text).map_err(|e| format!("{}: {e}", dest.display()))?;
        Ok(dest)
    };
    let manifest_for = |store: Store| {
        let dir = match store {
            Store::Github => "github",
            Store::Play => "play",
        };
        render("AndroidManifest.xml", &format!("{dir}/AndroidManifest.xml"), &store.manifest_vars(&vars))
    };
    let activity = render("java/MainActivity.java", "java/MainActivity.java", &vars)?;

    // 3. the activity: javac → d8
    let classes = work.join("classes");
    mkdir(&classes)?;
    let mut c = jdk("javac");
    c.args(["--release", "11", "-nowarn", "-encoding", "UTF-8", "-classpath"]).arg(&jar).arg("-d").arg(&classes).arg(&activity);
    step(c, "javac MainActivity.java")?;
    let dex = work.join("dex");
    mkdir(&dex)?;
    let mut c = sdk_tool(tools.join("d8"));
    c.args(["--release", "--min-api", &MIN_API.to_string(), "--lib"]).arg(&jar).arg("--output").arg(&dex).args(files_below(&classes));
    step(c, "d8")?;

    // 4. resources: the launcher icon (the brand's generated PNGs)
    let res = work.join("res");
    for (density, size) in [("mdpi", 48), ("xhdpi", 128), ("xxxhdpi", 256)] {
        copy(&root.join(format!("assets/app-icon/hicolor/{size}x{size}/apps/app.png")), &res.join(format!("mipmap-{density}/ic_launcher.png")))?;
    }
    let compiled = work.join("res.zip");
    let mut c = sdk_tool(tools.join("aapt2"));
    c.args(["compile", "--dir"]).arg(&res).arg("-o").arg(&compiled);
    step(c, "aapt2 compile")?;
    let link = |store: Store, proto: bool, dest: &Path| -> Result<(), String> {
        let manifest = manifest_for(store)?;
        let mut c = sdk_tool(tools.join("aapt2"));
        c.args(["link", "-o"]).arg(dest).arg("-I").arg(&jar).arg("--manifest").arg(&manifest).args([
            "--min-sdk-version",
            &MIN_API.to_string(),
            "--target-sdk-version",
            &TARGET_SDK.to_string(),
            "--version-code",
            &code.to_string(),
            "--version-name",
            &version,
        ]);
        if proto {
            c.arg("--proto-format");
        }
        c.arg(&compiled);
        step(c, if proto { "aapt2 link --proto-format" } else { "aapt2 link" })
    };

    let key = signing_key(&out)?;
    let abis = opts.abis.iter().map(|a| a.dir()).collect::<Vec<_>>().join("-");
    let stem = format!("{binary}-{version}-android-{abis}");

    // 5. the APK(s): linked resources + dex + native libraries, aligned and signed
    let linked = work.join("linked.apk");
    link(opts.store, false, &linked)?;
    for (name, dirs) in apk_outputs(&binary, &version, &opts.abis, opts.split) {
        let mut extra = vec![("classes.dex".to_string(), dex.join("classes.dex"))];
        extra.extend(libs.iter().filter(|(dir, _)| dirs.contains(dir)).map(|(dir, so)| (format!("lib/{dir}/libapp.so"), so.clone())));
        let unaligned = work.join("unaligned.apk");
        rezip(&linked, &unaligned, |n| Some(n.to_string()), &extra)?;
        let aligned = work.join("aligned.apk");
        let mut c = sdk_tool(tools.join("zipalign"));
        c.args(["-P", "16", "-f", "4"]).arg(&unaligned).arg(&aligned);
        step(c, "zipalign")?;
        let apk = out.join(&name);
        let mut c = sdk_tool(tools.join("apksigner"));
        c.args(["sign", "--ks"])
            .arg(&key.keystore)
            .args(["--ks-key-alias", &key.alias, "--ks-pass", "env:LCC_APK_STORE_PASS", "--key-pass", "env:LCC_APK_KEY_PASS", "--out"])
            .arg(&apk)
            .arg(&aligned)
            .env("LCC_APK_STORE_PASS", &key.store_pass)
            .env("LCC_APK_KEY_PASS", &key.key_pass);
        step(c, "apksigner sign")?;
        let mut c = sdk_tool(tools.join("apksigner"));
        c.args(["verify"]).arg(&apk);
        step(c, "apksigner verify")?;
        println!("android: {}{}", apk.display(), if key.debug { " (debug key)" } else { "" });
    }

    // 6. the bundle (Play Store): the same parts in bundletool's module layout
    if opts.aab {
        let proto = work.join("proto.apk");
        link(Store::Play, true, &proto)?;
        let mut extra = vec![("dex/classes.dex".to_string(), dex.join("classes.dex"))];
        extra.extend(libs.iter().map(|(dir, so)| (format!("lib/{dir}/libapp.so"), so.clone())));
        let module = work.join("base.zip");
        rezip(
            &proto,
            &module,
            |n| match n {
                "AndroidManifest.xml" => Some("manifest/AndroidManifest.xml".into()),
                "resources.pb" => Some("resources.pb".into()),
                n if n.starts_with("res/") => Some(n.to_string()),
                _ => None,
            },
            &extra,
        )?;
        let tool = bundletool(&out)?;
        let aab = out.join(format!("{stem}.aab"));
        let _ = std::fs::remove_file(&aab);
        let mut c = jdk("java");
        c.arg("-jar").arg(&tool).arg("build-bundle").arg(format!("--modules={}", module.display())).arg(format!("--output={}", aab.display()));
        step(c, "bundletool build-bundle")?;
        let mut c = jdk("jarsigner");
        c.args(["-keystore"])
            .arg(&key.keystore)
            .args(["-storepass:env", "LCC_APK_STORE_PASS", "-keypass:env", "LCC_APK_KEY_PASS", "-sigalg", "SHA256withRSA", "-digestalg", "SHA-256"])
            .arg(&aab)
            .arg(&key.alias)
            .env("LCC_APK_STORE_PASS", &key.store_pass)
            .env("LCC_APK_KEY_PASS", &key.key_pass);
        step(c, "jarsigner (bundle)")?;
        println!("android: {}{}", aab.display(), if key.debug { " (debug key)" } else { "" });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_codes_grow_and_prereleases_sort_first() {
        let order = [
            "0.9.0-rc1",
            "0.9.0-rc2",
            "0.9.0",
            "0.9.1",
            "0.9.6",
            "0.10.0",
            "1.0.0-rc1",
            "1.0.0",
            "26.43.4-rc1",
            "26.43.4",
            "26.43.5",
            "26.53.1",
            "27.1.1",
            "99.53.99",
        ];
        let codes: Vec<u32> = order.iter().map(|v| version_code(v).unwrap()).collect();
        assert!(codes.windows(2).all(|w| w[0] < w[1]), "{codes:?}");
        // a release leaves codes free above it, below the next version's first prerelease
        let (release, next) = (version_code("26.43.5").unwrap(), version_code("26.43.6-rc1").unwrap());
        assert!(next - release > 10, "{release} {next}");
        assert!(version_code("26.43.6-rc99").unwrap() < version_code("26.43.6").unwrap());
    }

    #[test]
    fn split_writes_one_apk_per_abi_and_the_default_one_for_all() {
        let both = [Abi::Arm64, Abi::X86_64];
        let split = apk_outputs("app", "1.2.3", &both, true);
        assert_eq!(
            split,
            vec![("app-1.2.3-android-arm64-v8a.apk".to_string(), vec!["arm64-v8a"]), ("app-1.2.3-android-x86_64.apk".to_string(), vec!["x86_64"]),]
        );
        assert_eq!(
            apk_outputs("app", "1.2.3", &both, false),
            vec![("app-1.2.3-android-arm64-v8a-x86_64.apk".to_string(), vec!["arm64-v8a", "x86_64"])]
        );
        let opts = parse(&["--abi", "arm64", "--abi", "x86_64", "--split", "--aab"]).unwrap();
        assert!(opts.split && opts.aab && opts.abis == both);
    }

    #[test]
    fn the_play_manifest_never_asks_for_all_files_access() {
        let template = std::fs::read_to_string(root().join("packaging/android/AndroidManifest.xml.in")).unwrap();
        let mut vars = BTreeMap::new();
        for k in ["android_package", "display_name"] {
            vars.insert(k.to_string(), "x".to_string());
        }
        let play = brand::render(&template, &Store::Play.manifest_vars(&vars)).unwrap();
        let github = brand::render(&template, &Store::Github.manifest_vars(&vars)).unwrap();
        // only comments may name them in the Play build
        let elements = |m: &str| m.split("<!--").map(|c| c.split_once("-->").map_or(c, |(_, rest)| rest)).collect::<String>();
        for gone in ["MANAGE_EXTERNAL_STORAGE", "requestLegacyExternalStorage"] {
            assert!(!elements(&play).contains(gone), "{gone} in the Play manifest");
            assert!(elements(&github).contains(gone), "{gone} missing from the GitHub manifest");
        }
        // the camera roll needs the photo permissions in both
        for both in ["READ_MEDIA_IMAGES", "READ_MEDIA_VISUAL_USER_SELECTED"] {
            assert!(elements(&play).contains(both) && elements(&github).contains(both), "{both}");
        }
        assert_eq!(parse(&["--aab"]).unwrap().store, Store::Github);
        assert_eq!(parse(&["--store", "play"]).unwrap().store, Store::Play);
        assert!(parse(&["--store", "amazon"]).is_err());
    }

    #[test]
    fn sdk_tools_find_the_jdk_first_on_path() {
        let home = std::env::temp_dir().join("jdk");
        let path = path_with_java(Some(home.clone().into()), Some(std::env::join_paths([std::env::temp_dir().join("usr-bin")]).unwrap())).unwrap();
        let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
        assert_eq!(dirs, vec![home.join("bin"), std::env::temp_dir().join("usr-bin")]);
        assert_eq!(path_with_java(None, Some("x".into())), None);
        assert_eq!(path_with_java(Some("".into()), Some("x".into())), None);
    }

    #[test]
    fn version_codes_are_validated() {
        // version code override is validated
        {
            let code = parse(&["--aab", "--version-code", "26430600"]).unwrap().version_code.unwrap();
            assert!(code > version_code("26.43.5").unwrap() && code < version_code("26.43.6-rc1").unwrap());
            assert_eq!(parse(&["--aab"]).unwrap().version_code, None);
            for bad in ["0", "-5", "abc", "2100000001", "99999999999", ""] {
                assert!(parse(&["--version-code", bad]).is_err(), "{bad}");
            }
            assert!(parse(&["--version-code"]).is_err());
        }
        // unusable versions are errors
        {
            for v in ["", "1.2", "1.2.x", "1.100.0", "1.2.3.4"] {
                assert!(version_code(v).is_err(), "{v}");
            }
        }
    }
}
