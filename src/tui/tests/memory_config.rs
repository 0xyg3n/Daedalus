#![cfg(unix)]
//! Exercise the packaged CLI with private state and no external programs in PATH.
use daedalus_tui::app::memory::{self, Config};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "daedalus-memory-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self(root)
    }
    fn call(&self, args: &[&str], success: bool) -> Value {
        let output = Command::new(env!("CARGO_BIN_EXE_daedalus-tui"))
            .env_clear()
            .env("HOME", &self.0)
            .env("PATH", "/nonexistent")
            .current_dir(&self.0)
            .args([
                "memory",
                "config",
                args[0],
                "--config-dir",
                self.0.to_str().unwrap(),
            ])
            .args(&args[1..])
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            success,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if success {
            serde_json::from_slice(&output.stdout).unwrap()
        } else {
            assert!(!String::from_utf8_lossy(&output.stderr).contains("do-not-echo-this-token"));
            Value::Null
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn packaged_memory_configuration_reviews_saves_and_binds_without_network_or_implicit_enable() {
    let f = Fixture::new();
    let path = f.0.join("hindsight.json");
    assert_eq!(f.call(&["show"], true)["configured"], false);
    let preview = f.call(
        &[
            "set",
            "--key",
            "endpoint",
            "--value",
            "https://memory.example.invalid",
        ],
        true,
    );
    assert_eq!(preview["saved"], false);
    assert!(!path.exists());
    f.call(
        &[
            "set",
            "--key",
            "endpoint",
            "--value",
            "https://memory.example.invalid",
            "--apply",
        ],
        true,
    );
    assert!(!memory::load(&f.0).unwrap().unwrap().enabled);
    let query = "  Literal query with 界\nand \\\"quoted content\\\"  ";
    for (key, value) in [
        ("recall_query", query),
        ("timeout_ms", "2500"),
        ("credential_file", "none"),
    ] {
        let before = fs::read(&path).unwrap();
        f.call(&["set", "--key", key, "--value", value], true);
        assert_eq!(fs::read(&path).unwrap(), before);
        f.call(&["set", "--key", key, "--value", value, "--apply"], true);
    }
    let before = fs::read(&path).unwrap();
    f.call(
        &["set", "--key", "timeout_ms", "--value", "99", "--apply"],
        false,
    );
    f.call(
        &[
            "set",
            "--key",
            "credential_file",
            "--value",
            "do-not-echo-this-token",
            "--apply",
        ],
        false,
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    let registration = f.0.join("hooks/fixture.json");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(registration.parent().unwrap())
        .unwrap();
    let value = json!({"version":1,"id":"fixture","directory":f.0,
        "scope":{"instance":"fixture","terminal":{"backend":"herdr","endpoint":"fixture","pane":"pane","generation":"generation"},"conversation":"session","harness":"claude"},
        "goal_id":null,"harness_process":null,"transcript":null,"events":["Stop"],
        "options":{"telegram_delivery":false,"telegram_progress":false,"notifications":false,"hindsight":true},
        "activity_lease_ms":30000,"pending_timeout_ms":30000,"hook_timeout_ms":12000});
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&registration)
        .unwrap()
        .write_all(&serde_json::to_vec(&value).unwrap())
        .unwrap();
    f.call(
        &[
            "bind",
            "--registration",
            registration.to_str().unwrap(),
            "--bank",
            "explicit-bank",
            "--apply",
        ],
        true,
    );
    let config = memory::load(&f.0).unwrap().unwrap();
    assert!(!config.enabled);
    assert_eq!(config.recall_query, query);
    assert_eq!(config.bindings[0].bank, "explicit-bank");
    f.call(
        &["set", "--key", "enabled", "--value", "true", "--apply"],
        true,
    );
    assert!(memory::load(&f.0).unwrap().unwrap().enabled);
    f.call(&["unbind", "--registration-id", "fixture", "--apply"], true);
    let config: Config = memory::load(&f.0).unwrap().unwrap();
    assert!(config.enabled);
    assert!(config.bindings.is_empty());
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(fs::read_dir(&f.0).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("hindsight.backup-")
    }));
    assert!(!f.0.join("delivery.sqlite3").exists());
    assert!(!f.0.join("telegram-runtime.json").exists());
}
