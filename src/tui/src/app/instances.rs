//! Bounded discovery of private native instances. Process names and saved PID
//! records are candidates only; the supervisor proves ownership and incarnation.
use super::{config::ControlConfig, telegram_config, telegram_supervisor};
use crate::core::layouts;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Read,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, String>;
const MAX_INSTANCES: usize = 128;
const MAX_PROCESSES: usize = 4096;

fn private_directory(path: &Path) -> Result<()> {
    layouts::check_path(path)?;
    let meta = fs::symlink_metadata(path).map_err(|_| "Instance directory is unavailable")?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err("Instance directory must be private and owned".into());
    }
    Ok(())
}
fn configuration(path: &Path) -> Result<ControlConfig> {
    private_directory(path)?;
    ControlConfig::parse(
        &telegram_config::read_private(&path.join("control.json"), 1024 * 1024)?
            .ok_or("Instance configuration is missing")?,
    )
}
fn preferences(directory: &Path) -> Result<Value> {
    let Some(bytes) = layouts::read_private_limit(&directory.join("dashboard.json"), 64 * 1024)?
    else {
        return Ok(json!({}));
    };
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid dashboard preferences")?;
    if !value.is_object() {
        return Err("Invalid dashboard preferences".into());
    }
    Ok(value)
}
/// Remember only an explicitly selected, valid native configuration. Keep other
/// dashboard keys; serialize updates with the stable configuration lock.
pub fn remember(preference_directory: &Path, selected: &Path) -> Result<()> {
    configuration(selected)?;
    let _lock = telegram_config::configuration_lock(preference_directory)?;
    let mut value = preferences(preference_directory)?;
    let mut paths = BTreeSet::new();
    for path in value["instances"].as_array().into_iter().flatten() {
        if let Some(path) = path.as_str() {
            let path = PathBuf::from(path);
            if layouts::check_path(&path).is_ok() {
                paths.insert(path);
            }
        }
    }
    paths.insert(selected.into());
    if paths.len() > MAX_INSTANCES {
        return Err("Saved instance limit reached".into());
    }
    value["last_instance"] = json!(selected);
    value["instances"] = json!(paths);
    layouts::write_private(
        &preference_directory.join("dashboard.json"),
        &serde_json::to_vec_pretty(&value).map_err(|_| "Cannot encode dashboard preferences")?,
    )
}
/// A saved selection never turns a deleted or malformed configuration into a
/// first-run target. Startup falls back to the default instance without writes.
pub fn remembered(preference_directory: &Path) -> Result<Option<PathBuf>> {
    let value = preferences(preference_directory)?;
    let Some(path) = value["last_instance"].as_str() else {
        return Ok(None);
    };
    let path = PathBuf::from(path);
    Ok(configuration(&path).is_ok().then_some(path))
}
fn process_candidate(process: &Path) -> Result<Option<PathBuf>> {
    let meta = fs::symlink_metadata(process).map_err(|_| "Process unavailable")?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } {
        return Ok(None);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(process.join("cmdline"))
        .map_err(|_| "Process unavailable")?;
    if file.metadata().map_err(|_| "Process unavailable")?.uid() != unsafe { libc::geteuid() } {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(8193)
        .read_to_end(&mut bytes)
        .map_err(|_| "Process unavailable")?;
    if bytes.len() > 8192 {
        return Ok(None);
    }
    let args = bytes
        .split(|b| *b == 0)
        .filter(|b| !b.is_empty())
        .collect::<Vec<_>>();
    if args.len() != 5
        || args[1] != b"telegram"
        || args[2] != b"serve"
        || args[3] != b"--config-dir"
    {
        return Ok(None);
    }
    let path = PathBuf::from(std::ffi::OsStr::from_bytes(args[4]));
    if layouts::check_path(&path).is_err() {
        return Ok(None);
    }
    Ok(Some(path))
}
/// All inputs are explicit, so tests use isolated roots and synthetic /proc.
/// Only one directory level is scanned; no home tree or environment is scraped.
pub fn discover(
    current: &Path,
    preference_directory: &Path,
    config_roots: &[PathBuf],
    proc_root: &Path,
) -> Result<Value> {
    let mut candidates = BTreeSet::from([current.to_path_buf()]);
    let mut warnings = Vec::new();
    match preferences(preference_directory) {
        Ok(value) => {
            for path in value["instances"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(std::iter::once(&value["last_instance"]))
            {
                if let Some(path) = path.as_str() {
                    if candidates.len() >= MAX_INSTANCES {
                        warnings.push("Saved instance limit reached");
                        break;
                    }
                    candidates.insert(path.into());
                }
            }
        }
        Err(_) => warnings.push("Saved instance preferences could not be read"),
    }
    for root in config_roots.iter().take(16) {
        if layouts::check_path(root).is_err() {
            warnings.push("Unsafe configuration search root skipped");
            continue;
        }
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for (index, entry) in entries.enumerate() {
            if index >= 1024 || candidates.len() >= MAX_INSTANCES {
                warnings.push("Configuration discovery limit reached");
                break;
            }
            let Ok(entry) = entry else {
                continue;
            };
            if entry.file_type().is_ok_and(|kind| kind.is_dir())
                && entry.path().join("control.json").exists()
            {
                candidates.insert(entry.path());
            }
        }
    }
    let mut process_candidates = Vec::new();
    if let Ok(entries) = fs::read_dir(proc_root) {
        for (index, entry) in entries.enumerate() {
            if index >= MAX_PROCESSES {
                warnings.push("Process discovery limit reached");
                break;
            }
            let Ok(entry) = entry else {
                continue;
            };
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            if let Ok(Some(path)) = process_candidate(&entry.path()) {
                if process_candidates.len() >= MAX_INSTANCES {
                    warnings.push("Process instance limit reached");
                    break;
                }
                process_candidates.push((path, pid));
            }
        }
    } else {
        warnings.push("Process discovery unavailable");
    }
    // Outside known roots, only verified native owners may introduce a candidate.
    for (path, pid) in process_candidates {
        if candidates.len() >= MAX_INSTANCES {
            break;
        }
        if telegram_supervisor::inspect_at(&path, proc_root).is_ok_and(|status| {
            status.state == "running" && status.process.is_some_and(|record| record.pid == pid)
        }) {
            candidates.insert(path);
        }
    }
    let mut instances = Vec::new();
    for path in candidates {
        let Ok(config) = configuration(&path) else {
            continue;
        };
        let status = telegram_supervisor::inspect_at(&path, proc_root);
        let (services, service_status) = match status {
            Ok(status) if status.state == "running" => {
                let verified = status.process.filter(|record| {
                    telegram_config::ServiceConfig::load(&path.join("telegram-service.json"))
                        .is_ok_and(|service| {
                            service.instance == record.instance && service.bot_id == record.bot_id
                        })
                });
                match verified {
                    Some(record) => (
                        json!([{"kind":"Telegram feeder","state":"Running","pid":record.pid,"start":record.start_ticks.to_string()}]),
                        "running",
                    ),
                    None => (Value::Null, "unknown"),
                }
            }
            Ok(status) if status.state == "stopped" => (json!([]), "stopped"),
            _ => (Value::Null, "unknown"),
        };
        instances.push(json!({"directory":path,"name":config.agent_name,"files":["control.json"],"services":services,"service_status":service_status}));
    }
    instances.sort_by(|a, b| a["directory"].as_str().cmp(&b["directory"].as_str()));
    warnings.sort();
    warnings.dedup();
    Ok(json!({"instances":instances,"warnings":warnings}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeSet, os::unix::fs::symlink};
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "daedalus-instances-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            layouts::write_private(&root.join("marker"), b"fixture").unwrap();
            fs::create_dir(root.join("proc")).unwrap();
            Self(root)
        }
        fn config(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            let mut config = ControlConfig::fresh(&self.0).unwrap();
            config.agent_name = relative.replace('/', "-");
            layouts::write_private(
                &path.join("control.json"),
                &serde_json::to_vec(&config).unwrap(),
            )
            .unwrap();
            path
        }
        fn service(&self, directory: &Path) -> super::super::telegram_service::Owner {
            let config = telegram_config::ServiceConfig {
                version: 1,
                instance: "fixture".into(),
                bot_id: 123,
                token_file: directory.join("token"),
                grants: vec![],
                listen_chats: BTreeSet::new(),
                registrations: vec![],
                hook_bindings: vec![],
            };
            layouts::write_private(
                &directory.join("telegram-service.json"),
                &serde_json::to_vec(&config).unwrap(),
            )
            .unwrap();
            let owner = super::super::telegram_service::Owner::acquire(
                directory,
                &self.0.join("locks"),
                123,
            )
            .unwrap();
            let proc_root = self.0.join("proc");
            layouts::write_private(
                &proc_root.join("sys/kernel/random/boot_id"),
                b"fixture-boot",
            )
            .unwrap();
            self.stat(42);
            let command = format!(
                "fixture-binary\0telegram\0serve\0--config-dir\0{}\0",
                directory.display()
            );
            layouts::write_private(&proc_root.join("17/cmdline"), command.as_bytes()).unwrap();
            layouts::write_private(
                &proc_root.join("17/environ"),
                b"PRIVATE_TOKEN=must-never-leak\0",
            )
            .unwrap();
            symlink("/fixture/daedalus", proc_root.join("17/exe")).unwrap();
            let record = telegram_supervisor::Record {
                version: 1,
                pid: 17,
                start_ticks: 42,
                boot_id: "fixture-boot".into(),
                executable: "/fixture/daedalus".into(),
                directory: directory.into(),
                instance: "fixture".into(),
                bot_id: 123,
                phase: telegram_supervisor::Phase::Running,
            };
            layouts::write_private(
                &directory.join("telegram-runtime.json"),
                &serde_json::to_vec(&record).unwrap(),
            )
            .unwrap();
            owner
        }
        fn stat(&self, ticks: u64) {
            let mut fields = vec!["0".to_owned(); 20];
            fields[0] = "S".into();
            fields[19] = ticks.to_string();
            layouts::write_private(
                &self.0.join("proc/17/stat"),
                format!("17 (fixture worker) {}", fields.join(" ")).as_bytes(),
            )
            .unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn saved_configuration_is_not_running_and_invalid_candidates_are_skipped() {
        let f = Fixture::new();
        let current = f.config("config/current");
        let saved = f.config("config/saved");
        let target = f.config("outside");
        symlink(&target, f.0.join("config/link")).unwrap();
        layouts::write_private(
            &f.0.join("config/invalid/control.json"),
            b"PRIVATE_TOKEN=must-never-leak",
        )
        .unwrap();
        let report =
            discover(&current, &current, &[f.0.join("config")], &f.0.join("proc")).unwrap();
        let rows = report["instances"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|r| r["directory"] == json!(saved)));
        assert!(rows.iter().all(|r| r["services"] == json!([])));
        assert!(!report.to_string().contains("must-never-leak"));
        assert!(!current.join("dashboard.json").exists());
    }
    #[test]
    fn external_service_discovery_requires_owner_and_exact_process_incarnation() {
        let f = Fixture::new();
        let current = f.config("current");
        let external = f.config("external");
        let owner = f.service(&external);
        let inspect = || discover(&current, &current, &[], &f.0.join("proc")).unwrap();
        let live = inspect();
        assert_eq!(live["instances"].as_array().unwrap().len(), 2);
        let running = live["instances"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["directory"] == json!(external))
            .unwrap();
        assert_eq!(running["services"][0]["start"], "42");
        assert!(!live.to_string().contains("must-never-leak"));
        f.stat(43);
        assert_eq!(inspect()["instances"].as_array().unwrap().len(), 1);
        // Once explicitly remembered, an unknown process remains a saved
        // configuration, never a live instance inferred from its stale record.
        remember(&current, &external).unwrap();
        let stale = inspect();
        let saved = stale["instances"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["directory"] == json!(external))
            .unwrap();
        assert_eq!(saved["service_status"], "unknown");
        assert!(saved["services"].is_null());
        f.stat(42);
        drop(owner);
        let stopped = inspect();
        let saved = stopped["instances"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["directory"] == json!(external))
            .unwrap();
        assert_eq!(saved["service_status"], "stopped");
        assert_eq!(saved["services"], json!([]));
    }
    #[test]
    fn preferences_preserve_other_keys_and_deleted_selection_does_not_redirect_startup() {
        let f = Fixture::new();
        let base = f.config("base");
        let first = f.config("first");
        let second = f.config("second");
        layouts::write_private(&base.join("dashboard.json"), b"{\"theme\":\"existing\"}").unwrap();
        remember(&base, &first).unwrap();
        remember(&base, &second).unwrap();
        let value = preferences(&base).unwrap();
        assert_eq!(value["theme"], "existing");
        assert_eq!(value["instances"].as_array().unwrap().len(), 2);
        assert_eq!(remembered(&base).unwrap(), Some(second.clone()));
        fs::remove_file(second.join("control.json")).unwrap();
        assert_eq!(remembered(&base).unwrap(), None);
        let before = fs::read(base.join("dashboard.json")).unwrap();
        assert!(remember(&base, &second).is_err());
        assert_eq!(fs::read(base.join("dashboard.json")).unwrap(), before);
        let report = discover(&first, &base, &[], &f.0.join("proc")).unwrap();
        assert_eq!(report["instances"].as_array().unwrap().len(), 1);
    }
}
