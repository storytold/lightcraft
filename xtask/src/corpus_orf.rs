//! Hash-pinned CC0 Olympus samples; never fetch these during ordinary builds/tests.
use crate::{root, run};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::process::Command;

fn checksum(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(crate) fn download() -> Result<(), String> {
    let manifest: serde_json::Value = serde_json::from_str(include_str!("../../docs/orf-corpus.json")).map_err(|e| e.to_string())?;
    let entries = manifest.get("files").and_then(|v| v.as_array()).ok_or("ORF manifest without files")?;
    let dest = root().join("corpus/raw");
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    for entry in entries {
        let field = |name: &str| entry.get(name).and_then(|v| v.as_str()).ok_or_else(|| format!("ORF manifest missing {name}"));
        let (name, url, expected) = (field("name")?, field("url")?, field("sha256")?);
        if name.contains(['/', '\\']) || !name.starts_with("orf-") || !name.ends_with(".orf") || !url.starts_with("https://raw.pixls.us/") {
            return Err("invalid ORF manifest path or URL".into());
        }
        let out = dest.join(name);
        if out.exists() {
            if checksum(&out)? != expected {
                return Err(format!("{} differs from its pinned SHA-256; remove it and download again", out.display()));
            }
            continue;
        }
        let part = dest.join(format!("{name}.part"));
        let mut curl = Command::new("curl");
        curl.args(["-fsSL", "--retry", "3", "--connect-timeout", "30", "--max-time", "300", "-o"]).arg(&part).arg(url);
        run(curl, &format!("fetch {name}"))?;
        if checksum(&part)? != expected {
            return Err(format!("{name}: downloaded bytes differ from pinned SHA-256; not installed"));
        }
        std::fs::rename(&part, &out).map_err(|e| format!("install {name}: {e}"))?;
    }
    Ok(())
}
