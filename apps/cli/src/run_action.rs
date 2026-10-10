//! `run-action NAME [OPTIONS] [param=value…]` (P4.5): play a saved action, the same as
//! `run [OPTIONS] actions.play name=NAME args={…}`.

use serde_json::{Map, Value, json};

/// The `run` arguments that play action `NAME` with the given parameters.
pub fn to_run_args(args: &[String]) -> Result<Vec<String>, String> {
    let mut opts = Vec::new();
    let mut name: Option<String> = None;
    let mut params = Map::new();
    let mut ids: Option<Value> = None;
    let mut i = 0;
    while let Some(a) = args.get(i) {
        if a.starts_with("--") {
            opts.push(a.clone());
            let takes = matches!(a.as_str(), "--library" | "--import" | "--script")
                || (a == "--connect" && args.get(i + 1).is_some_and(|n| n.contains(':') && !n.contains('=')));
            if takes {
                i += 1;
                opts.push(args.get(i).cloned().ok_or_else(|| format!("{a} needs a value"))?);
            }
        } else if let Some((k, v)) = a.split_once('=') {
            let v = serde_json::from_str(v).unwrap_or_else(|_| json!(v));
            if k.trim() == "ids" {
                ids = Some(v);
            } else {
                params.insert(k.trim().to_string(), v);
            }
        } else if name.is_none() {
            name = Some(a.clone());
        } else {
            return Err(format!("run-action: unexpected `{a}` (parameters are key=value)"));
        }
        i += 1;
    }
    let name = name.ok_or("run-action: no action name (actions.list shows them)")?;
    let mut p = json!({"name": name, "args": params});
    if let Some(ids) = ids {
        p["ids"] = ids;
    }
    opts.push("actions.play".into());
    opts.push(p.to_string());
    Ok(opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn builds_a_run_line() {
        let r = to_run_args(&s(&["--library", "/l", "Warm", "ev=0.5", "tag=night", "ids=[1,2]"])).unwrap();
        assert_eq!(&r[..3], &s(&["--library", "/l", "actions.play"])[..]);
        let p: Value = serde_json::from_str(&r[3]).unwrap();
        assert_eq!(p, json!({"name": "Warm", "args": {"ev": 0.5, "tag": "night"}, "ids": [1, 2]}));
        assert!(to_run_args(&s(&["--library"])).is_err());
        assert!(to_run_args(&s(&[])).is_err());
        assert!(to_run_args(&s(&["a", "b"])).is_err());
    }
}
