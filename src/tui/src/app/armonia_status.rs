//! Read-only organizer observations. No environment, title credentials, or controller writes.
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
type Result<T> = std::result::Result<T, String>;
#[cfg(unix)]
use serde::Deserialize;
use serde_json::{Value, json};
#[cfg(unix)]
use std::path::PathBuf;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
#[cfg(unix)]
use std::{fs, io::Read};

fn document(path: &Path) -> Result<Option<Value>> {
    super::telegram_config::read_private(path, 1024 * 1024)?
        .map(|bytes| {
            serde_json::from_slice(&bytes).map_err(|_| "Invalid organizer document".into())
        })
        .transpose()
}
fn label(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|s| s.len() <= 256 && !s.chars().any(char::is_control))
}
fn policy(directory: &Path) -> Value {
    let data = match document(&directory.join("armonia-layout-policy.json")) {
        Ok(None) => return json!({"exists":false,"error":"","counts":{},"entries":{}}),
        Ok(Some(value)) => value,
        Err(_) => return json!({"exists":true,"error":"Unsafe or malformed policy"}),
    };
    let Some(entries) = data["entries"].as_object() else {
        return json!({"exists":true,"error":"Missing entries object"});
    };
    let mut counts = BTreeMap::<String, usize>::new();
    let mut projected = BTreeMap::new();
    for (key, entry) in entries {
        let name = entry.get("policy").and_then(label).unwrap_or("managed");
        if !matches!(name, "managed" | "manual" | "preserve" | "hold")
            || key.len() > 128
            || !entry.is_object()
        {
            return json!({"exists":true,"error":"Invalid policy entry"});
        }
        *counts.entry(name.to_owned()).or_default() += 1;
        projected.insert(key, name);
    }
    json!({"exists":true,"error":"","server":label(&data["server"]).unwrap_or(""),"counts":counts,"entries":projected})
}
fn cycle(directory: &Path, now: f64) -> Value {
    let data = match document(&directory.join("armonia-continuous-state.json")) {
        Ok(None) => return json!({"error":"","last_poll":null}),
        Ok(Some(value)) if value.is_object() => value,
        _ => return json!({"error":"Unsafe or malformed cycle"}),
    };
    let stamp = data["last_poll"]
        .as_f64()
        .filter(|t| t.is_finite() && *t >= 0.0 && *t <= now);
    if stamp.is_none() {
        return json!({"error":"Invalid cycle timestamp"});
    }
    let result = &data["result"];
    let count = |key: &str| result[key].as_array().map(Vec::len);
    if !result.is_object() || label(&result["state"]).is_none() {
        return json!({"error":"Invalid cycle result"});
    }
    json!({"error":"","last_poll":stamp,"age":stamp.map(|s|now-s),"state":label(&result["state"]),"agents":result["agents"].as_u64(),"holds":count("holds"),"skipped":count("skipped"),"discovered":count("discovered"),"ordering_hold":data["ordering_hold"].as_bool().unwrap_or(false),"protected":data["protected_terminal_ids"].as_array().map(Vec::len)})
}
fn journal(directory: &Path) -> Value {
    let bytes = match super::telegram_config::read_private(
        &directory.join("armonia-continuous.jsonl"),
        8 * 1024 * 1024,
    ) {
        Ok(None) => return json!({"events":0,"verified":0,"open_plans":0,"unresolved":0}),
        Ok(Some(bytes)) => bytes,
        Err(_) => return json!({"error":"Unsafe or oversized journal","unresolved":null}),
    };
    let mut started = BTreeSet::new();
    let mut verified = BTreeSet::new();
    let mut operations = BTreeMap::new();
    let mut events = 0;
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let Ok(event) = serde_json::from_slice::<Value>(line) else {
            return json!({"error":"Malformed journal","unresolved":null});
        };
        let Some(state) = label(&event["state"]) else {
            continue;
        };
        events += 1;
        if let Some(plan) = label(&event["plan"]) {
            if state == "started" {
                started.insert(plan.to_owned());
            }
            if state == "verified" {
                verified.insert(plan.to_owned());
            }
        }
        if let Some(op) = label(&event["op"]) {
            operations.insert(op.to_owned(), state.to_owned());
        }
    }
    json!({"events":events,"verified":verified.len(),"open_plans":started.difference(&verified).count(),"unresolved":operations.values().filter(|state|matches!(state.as_str(),"intent"|"unknown")).count()})
}
#[cfg(unix)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Observer {
    executable: PathBuf,
    script: Option<PathBuf>,
}
#[cfg(unix)]
fn bounded(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|_| "Process observation unavailable")?
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| "Process observation unavailable")?;
    if bytes.len() > 65536 {
        return Err("Process record oversized".into());
    }
    Ok(bytes)
}
#[cfg(unix)]
fn incarnation(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let fields = text
        .rsplit_once(')')?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    if matches!(*fields.first()?, "Z" | "X") {
        return None;
    }
    Some(fields.get(19)?.to_string())
}
#[cfg(unix)]
fn services(directory: &Path, proc_root: &Path) -> Result<Value> {
    let Some(value) = document(&directory.join("armonia-observer.json"))? else {
        return Ok(Value::Null);
    };
    let observer: Observer =
        serde_json::from_value(value).map_err(|_| "Invalid observer configuration")?;
    if !observer.executable.is_absolute()
        || observer.script.as_ref().is_some_and(|p| !p.is_absolute())
    {
        return Err("Observer paths must be absolute".into());
    }
    let executable =
        fs::metadata(&observer.executable).map_err(|_| "Observer executable unavailable")?;
    if !executable.is_file() {
        return Err("Invalid observer executable".into());
    }
    let script = observer
        .script
        .as_ref()
        .map(fs::canonicalize)
        .transpose()
        .map_err(|_| "Observer script unavailable")?;
    let config = fs::canonicalize(directory).map_err(|_| "Configuration unavailable")?;
    let mut rows = Vec::new();
    let entries = fs::read_dir(proc_root).map_err(|_| "Process observation unavailable")?;
    for (index, entry) in entries.enumerate() {
        if index >= 8192 {
            return Err("Process observation limit exceeded".into());
        }
        let entry = entry.map_err(|_| "Process observation incomplete")?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let process = entry.path();
        let Ok(owner) = fs::metadata(&process) else {
            continue;
        };
        if owner.uid() != unsafe { libc::geteuid() } {
            continue;
        }
        let Ok(exe) = fs::metadata(process.join("exe")) else {
            continue;
        };
        if (exe.dev(), exe.ino()) != (executable.dev(), executable.ino()) {
            continue;
        }
        let before = bounded(&process.join("stat"))?;
        let Some(start) = incarnation(&before) else {
            continue;
        };
        let command = bounded(&process.join("cmdline"))?;
        let args = command
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(std::str::from_utf8)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| "Invalid process arguments")?;
        let offset = if let Some(script) = &script {
            if args.get(1).and_then(|p| fs::canonicalize(p).ok()).as_ref() != Some(script) {
                continue;
            }
            2
        } else {
            1
        };
        let mut options = BTreeMap::new();
        let mut once = false;
        let mut cursor = offset;
        let mut valid = true;
        while cursor < args.len() {
            if args[cursor] == "--once" {
                once = true;
                cursor += 1;
                continue;
            }
            let (key, value, step) = match args[cursor].split_once('=') {
                Some((key, value)) => (key, value, 1),
                None if cursor + 1 < args.len() => (args[cursor], args[cursor + 1], 2),
                _ => {
                    valid = false;
                    break;
                }
            };
            if !matches!(key, "--config" | "--session" | "--interval")
                || options.insert(key, value).is_some()
            {
                valid = false;
                break;
            }
            cursor += step;
        }
        if !valid {
            continue;
        }
        let Some(path) = options
            .get("--config")
            .filter(|p| Path::new(p).is_absolute())
        else {
            continue;
        };
        if fs::canonicalize(path).ok().as_ref() != Some(&config) {
            continue;
        }
        let interval = options
            .get("--interval")
            .unwrap_or(&"15")
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite() && *n > 0.0);
        let Some(interval) = interval else { continue };
        let after = fs::metadata(process.join("exe")).map_err(|_| "Process disappeared")?;
        if incarnation(&bounded(&process.join("stat"))?).as_ref() != Some(&start)
            || bounded(&process.join("cmdline"))? != command
            || (exe.dev(), exe.ino()) != (after.dev(), after.ino())
        {
            return Err("Process changed during observation".into());
        }
        rows.push(json!({"pid":pid,"start":start,"mine":true,"session":options.get("--session").unwrap_or(&"unknown (environment not read)"),"interval":interval,"once":once}));
    }
    Ok(json!(rows))
}
fn pending_renames(directory: &Path) -> Option<i64> {
    super::diagnostics::with_database(&directory.join("armonia.sqlite3"), |con| {
        let table: bool = con.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='naming_operations' AND rootpage>0)",
            [], |row| row.get(0),
        ).map_err(|_| "Registry schema unavailable")?;
        if !table { return Ok(0); }
        con.query_row("SELECT count(*) FROM naming_operations WHERE state IN ('intent','unknown')", [], |row| row.get(0))
            .map_err(|_| "Pending naming outcomes unavailable".into())
    }).ok().flatten()
}
pub fn inspect(directory: &Path, proc_root: &Path, now: f64) -> Value {
    let registry = super::diagnostics::organization(directory, now)
        .unwrap_or_else(|_| json!({"status":"unknown"}));
    let agents = registry["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| {
            let mut entry = entry.clone();
            entry["availability"] = json!("unverified");
            entry
        })
        .collect::<Vec<_>>();
    let log = journal(directory);
    let mut cycle = cycle(directory, now);
    if log.get("error").is_some() {
        cycle["error"] = json!("Journal outcomes unavailable");
    }
    let (services, error) = match services(directory, proc_root) {
        Ok(value) => (value, ""),
        Err(_) => (Value::Null, "Service identity unavailable"),
    };
    json!({"registry":registry,"agents":agents,"services":services,"service_error":error,"policy":policy(directory),"cycle":cycle,"journal":log,"pending_renames":pending_renames(directory),"observation":"Read-only; registry availability is unverified; no environment or credentials read"})
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::core::layouts::write_private;
    use std::os::unix::fs::symlink;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "daedalus-armonia-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            write_private(&path.join("marker"), b"").unwrap();
            fs::create_dir(path.join("proc")).unwrap();
            Self(path)
        }
        fn write(&self, name: &str, value: Value) {
            write_private(&self.0.join(name), &serde_json::to_vec(&value).unwrap()).unwrap();
        }
        fn view(&self) -> Value {
            inspect(&self.0, &self.0.join("proc"), 1000.0)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn missing_malformed_and_unsafe_files_are_distinct_and_do_not_create_state() {
        let f = Fixture::new();
        let before = fs::read_dir(&f.0).unwrap().count();
        let view = f.view();
        assert_eq!(view["policy"]["exists"], false);
        assert!(view["services"].is_null());
        assert_eq!(fs::read_dir(&f.0).unwrap().count(), before);
        f.write("armonia-layout-policy.json", json!({"entries":[]}));
        assert_eq!(f.view()["policy"]["error"], "Missing entries object");
        symlink(
            f.0.join("marker"),
            f.0.join("armonia-continuous-state.json"),
        )
        .unwrap();
        assert_eq!(f.view()["cycle"]["error"], "Unsafe or malformed cycle");
        f.write("armonia-observer.json", json!({"executable":"relative"}));
        assert!(f.view()["services"].is_null());
    }
    #[test]
    fn held_stale_cycles_and_policy_counts_preserve_projection_without_payloads() {
        let f = Fixture::new();
        f.write(
            "armonia-layout-policy.json",
            json!({"server":"fixture","entries":{"1":{"policy":"hold"},"2":{"policy":"manual"}}}),
        );
        f.write("armonia-continuous-state.json",json!({"last_poll":1,"ordering_hold":true,"result":{"state":"held","agents":2,"holds":["SECRET"],"skipped":[],"discovered":[]}}));
        write_private(
            &f.0.join("armonia-continuous.jsonl"),
            b"{\"op\":\"one\",\"state\":\"unknown\",\"payload\":\"SECRET\"}\n",
        )
        .unwrap();
        let view = f.view();
        assert_eq!(view["cycle"]["age"], 999.0);
        assert_eq!(view["cycle"]["holds"], 1);
        assert_eq!(view["journal"]["unresolved"], 1);
        assert_eq!(view["policy"]["counts"]["hold"], 1);
        assert!(!view.to_string().contains("SECRET"));
        let rows = super::super::presentation::armonia_rows(&view, true);
        let row = rows.iter().find(|r| r.id == "armonia:cycle").unwrap();
        let rendered = serde_json::to_string(row).unwrap();
        assert!(rendered.contains("Stale"));
        assert!(rendered.contains("Held: outcome unknown"));
        write_private(&f.0.join("armonia-continuous.jsonl"), b"broken").unwrap();
        assert_eq!(f.view()["cycle"]["error"], "Journal outcomes unavailable");
    }
    #[test]
    fn service_requires_exact_executable_script_configuration_and_live_incarnation() {
        let f = Fixture::new();
        let binary = f.0.join("interpreter");
        let script = f.0.join("organizer.py");
        write_private(&binary, b"fixture").unwrap();
        write_private(&script, b"fixture").unwrap();
        f.write(
            "armonia-observer.json",
            json!({"executable":binary,"script":script}),
        );
        let process = f.0.join("proc/17");
        write_private(&process.join("marker"), b"").unwrap();
        symlink(&binary, process.join("exe")).unwrap();
        let command = format!(
            "{}\0{}\0--config\0{}\0--session\0fixture\0",
            binary.display(),
            script.display(),
            f.0.display()
        );
        write_private(&process.join("cmdline"), command.as_bytes()).unwrap();
        let mut fields = vec!["0"; 20];
        fields[0] = "S";
        fields[19] = "42";
        write_private(
            &process.join("stat"),
            format!("17 (organizer) {}", fields.join(" ")).as_bytes(),
        )
        .unwrap();
        assert_eq!(f.view()["services"][0]["pid"], 17);
        assert_eq!(f.view()["services"][0]["start"], "42");
        fields[0] = "Z";
        write_private(
            &process.join("stat"),
            format!("17 (organizer) {}", fields.join(" ")).as_bytes(),
        )
        .unwrap();
        assert_eq!(f.view()["services"], json!([]));
        fields[0] = "S";
        write_private(
            &process.join("stat"),
            format!("17 (organizer) {}", fields.join(" ")).as_bytes(),
        )
        .unwrap();
        write_private(
            &process.join("cmdline"),
            command.replace("--config\0", "--other\0").as_bytes(),
        )
        .unwrap();
        assert_eq!(f.view()["services"], json!([]));
        write_private(&process.join("cmdline"), command.as_bytes()).unwrap();
        fs::remove_file(process.join("exe")).unwrap();
        symlink(&script, process.join("exe")).unwrap();
        assert_eq!(f.view()["services"], json!([]));
    }
}

#[cfg(windows)]
fn services(_directory: &Path, _proc_root: &Path) -> Result<Value> {
    Err("Optional Armonia service process observation is unavailable on Windows".into())
}
