#![cfg(unix)]
//! Packaged offline global-preference paths; no providers are installed here.
use daedalus_tui::app::{config::ControlConfig, dashboard::State};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
fn private_file(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path.parent().unwrap())?;
    fs::write(path, bytes)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "daedalus-dashboard-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        private_file(&path.join("marker"), b"fixture").unwrap();
        Self(path)
    }
    fn call(&self, args: &[&str], success: bool) -> Value {
        let out = Command::new(env!("CARGO_BIN_EXE_daedalus-tui"))
            .env_clear()
            .env("HOME", &self.0)
            .env("PATH", "/nonexistent")
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap();
        assert_eq!(
            out.status.success(),
            success,
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!String::from_utf8_lossy(&out.stdout).contains("DASHBOARD_PRIVATE_SENTINEL"));
        assert!(!String::from_utf8_lossy(&out.stderr).contains("DASHBOARD_PRIVATE_SENTINEL"));
        if success {
            serde_json::from_slice(&out.stdout).unwrap()
        } else {
            Value::Null
        }
    }
    fn instance(&self, name: &str) -> PathBuf {
        let root = self.0.join(name);
        let mut config = ControlConfig::fresh(&self.0).unwrap();
        config.agent_name = name.into();
        private_file(&root.join("control.json"), &config.encode().unwrap()).unwrap();
        root
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn packaged_dashboard_recovery_previews_retains_backup_and_refuses_unsafe_requests() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let f = Fixture::new();
    let root = f.0.join("preferences");
    let path = root.join("dashboard.json");
    let directory = root.to_str().unwrap();
    let original = b"{BROKEN-DASHBOARD_PRIVATE_SENTINEL";
    let instance = f.instance("preserved");
    let control = fs::read(instance.join("control.json")).unwrap();
    f.call(
        &["dashboard", "config", "recover", "--config-dir", directory],
        false,
    );
    assert!(!root.exists());
    private_file(&path, original).unwrap();
    let args = ["dashboard", "config", "recover", "--config-dir", directory];
    let preview = f.call(&args, true);
    assert_eq!(preview["saved"], false);
    assert_eq!(preview["review"]["recovery"], true);
    assert_eq!(preview["review"]["configuration"]["scan_processes"], false);
    let backup = PathBuf::from(preview["review"]["backup_file"].as_str().unwrap());
    assert!(!backup.exists());
    assert_eq!(fs::read(&path).unwrap(), original);
    for extra in [
        vec!["--key", "scan_processes"],
        vec!["--path", instance.to_str().unwrap()],
        vec!["--apply", "--apply"],
        vec!["--config-dir", directory],
    ] {
        f.call(&[args.as_slice(), extra.as_slice()].concat(), false);
        assert_eq!(fs::read(&path).unwrap(), original);
    }
    let saved = f.call(&[args.as_slice(), &["--apply"]].concat(), true);
    assert_eq!(saved["saved"], true);
    assert_eq!(fs::read(&backup).unwrap(), original);
    assert_eq!(
        fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::read(instance.join("control.json")).unwrap(), control);
    let state = State::load(&root).unwrap();
    assert!(!state.settings.scan_processes);
    assert!(!state.settings.scan_default_roots);
    assert!(state.saved_instances().is_empty());
    let recovered = fs::read(&path).unwrap();
    f.call(&[args.as_slice(), &["--apply"]].concat(), false);
    assert_eq!(fs::read(&path).unwrap(), recovered);
    fs::remove_file(&path).unwrap();
    symlink(&backup, &path).unwrap();
    f.call(&[args.as_slice(), &["--apply"]].concat(), false);
    assert_eq!(fs::read(&backup).unwrap(), original);
    assert!(!root.join("telegram-service.json").exists());
    assert!(!root.join("control.json").exists());
}

#[test]
fn packaged_dashboard_recovery_keeps_oversized_legacy_bytes_before_replacement() {
    let f = Fixture::new();
    let root = f.0.join("preferences");
    let path = root.join("dashboard.json");
    let original = serde_json::to_vec(&json!({"unrelated":"x".repeat(70_000)})).unwrap();
    private_file(&path, &original).unwrap();
    let args = [
        "dashboard",
        "config",
        "recover",
        "--config-dir",
        root.to_str().unwrap(),
    ];
    let preview = f.call(&args, true);
    assert_eq!(preview["review"]["original_bytes"], original.len());
    assert_eq!(fs::read(&path).unwrap(), original);
    f.call(&[args.as_slice(), &["--apply"]].concat(), true);
    assert_eq!(
        fs::read(preview["review"]["backup_file"].as_str().unwrap()).unwrap(),
        original
    );
    assert!(State::load(&root).is_ok());
}

#[test]
fn packaged_dashboard_settings_roots_instances_and_private_previews_preserve_global_selection() {
    let f = Fixture::new();
    let root = f.0.join("preferences");
    let dir = root.to_str().unwrap();
    let path = root.join("dashboard.json");
    let shown = f.call(&["dashboard", "config", "show", "--config-dir", dir], true);
    assert_eq!(shown["configured"], false);
    assert!(!root.exists());
    private_file(&path, br#"{"unrelated":"DASHBOARD_PRIVATE_SENTINEL"}"#).unwrap();
    for (key, value) in [
        ("scan_processes", "false"),
        ("scan_default_roots", "false"),
        ("instance_limit", "4"),
        ("root_limit", "2"),
        ("entries_per_root", "1"),
        ("process_limit", "1"),
        ("argv_limit_bytes", "1024"),
    ] {
        let before = fs::read(&path).unwrap();
        let args = [
            "dashboard",
            "config",
            "set",
            "--config-dir",
            dir,
            "--key",
            key,
            "--value",
            value,
        ];
        assert_eq!(f.call(&args, true)["saved"], false);
        assert_eq!(fs::read(&path).unwrap(), before);
        let mut applied = args.to_vec();
        applied.push("--apply");
        f.call(&applied, true);
        assert_eq!(
            State::load(&root).unwrap().settings.display(key).unwrap(),
            value
        );
    }
    let first = f.0.join("search-one");
    let second = f.0.join("search-two");
    for operation in [
        ("add-root", &first),
        ("add-root", &second),
        ("remove-root", &first),
    ] {
        let before = fs::read(&path).unwrap();
        let args = [
            "dashboard",
            "config",
            operation.0,
            "--config-dir",
            dir,
            "--path",
            operation.1.to_str().unwrap(),
        ];
        f.call(&args, true);
        assert_eq!(fs::read(&path).unwrap(), before);
        let mut apply = args.to_vec();
        apply.push("--apply");
        f.call(&apply, true);
    }
    assert_eq!(
        State::load(&root).unwrap().settings.search_roots,
        vec![second]
    );
    let instance = f.instance("one");
    let before = fs::read(&path).unwrap();
    f.call(
        &[
            "dashboard",
            "config",
            "add-instance",
            "--config-dir",
            dir,
            "--path",
            instance.to_str().unwrap(),
        ],
        true,
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    f.call(
        &[
            "dashboard",
            "config",
            "add-instance",
            "--config-dir",
            dir,
            "--path",
            instance.to_str().unwrap(),
            "--apply",
        ],
        true,
    );
    assert!(State::load(&root).unwrap().show()["last_instance"].is_null());
    f.call(
        &[
            "dashboard",
            "config",
            "remove-instance",
            "--config-dir",
            dir,
            "--path",
            instance.to_str().unwrap(),
            "--apply",
        ],
        true,
    );
    assert!(instance.join("control.json").exists());
    for (key, value) in [
        ("instance_limit", "0"),
        ("scan_processes", "yes"),
        ("argv_limit_bytes", "private-sentinel"),
    ] {
        let before = fs::read(&path).unwrap();
        f.call(
            &[
                "dashboard",
                "config",
                "set",
                "--config-dir",
                dir,
                "--key",
                key,
                "--value",
                value,
                "--apply",
            ],
            false,
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    f.call(
        &[
            "dashboard",
            "config",
            "show",
            "--config-dir",
            dir,
            "--apply",
        ],
        false,
    );
    f.call(
        &[
            "dashboard",
            "config",
            "show",
            "--config-dir",
            dir,
            "--config-dir",
            dir,
        ],
        false,
    );
    let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["unrelated"], json!("DASHBOARD_PRIVATE_SENTINEL"));
    assert!(!root.join("control.json").exists());
    assert!(!root.join("telegram-runtime.json").exists());
    assert!(fs::read_dir(&root).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("dashboard.backup-")
    }));
}
