#![cfg(unix)]
//! Offline settings and import checks against the actual packaged executable.
use daedalus_tui::app::gateway;
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "daedalus-gateway-cli-{}-{}",
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
            .args(args)
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            success,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("do-not-echo-fixture-secret"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("do-not-echo-fixture-secret"));
        if success {
            serde_json::from_slice(&output.stdout).unwrap()
        } else {
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
fn packaged_gateway_settings_and_import_keep_explicit_provider_state_and_credential_consent() {
    let f = Fixture::new();
    let source = f.0.join("source");
    let dir = source.to_str().unwrap();
    f.call(
        &[
            "config",
            "init",
            "--config-dir",
            dir,
            "--workspace",
            f.0.to_str().unwrap(),
            "--apply",
        ],
        true,
    );
    let key = source.join("provider-key");
    fs::write(&key, b"fixture-private-provider-key").unwrap();
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    let args = [
        "gateway",
        "config",
        "init",
        "--config-dir",
        dir,
        "--provider",
        "claude",
        "--endpoint",
        "https://gateway.example.invalid",
        "--model",
        "fixture-model",
        "--credential-file",
        key.to_str().unwrap(),
    ];
    assert_eq!(f.call(&args, true)["saved"], false);
    assert!(!source.join("gateway.json").exists());
    let mut apply = args.to_vec();
    apply.push("--apply");
    f.call(&apply, true);
    assert!(!gateway::load(&source).unwrap().unwrap().enabled);
    for (field, value) in [
        ("provider", "codex"),
        ("endpoint", "http://localhost:8999/v1"),
        ("model", "changed-model"),
        ("credential_file", key.to_str().unwrap()),
        ("enabled", "true"),
    ] {
        let before = fs::read(source.join("gateway.json")).unwrap();
        let args = [
            "gateway",
            "config",
            "set",
            "--config-dir",
            dir,
            "--key",
            field,
            "--value",
            value,
        ];
        assert_eq!(f.call(&args, true)["saved"], false);
        assert_eq!(fs::read(source.join("gateway.json")).unwrap(), before);
        let mut apply = args.to_vec();
        apply.push("--apply");
        f.call(&apply, true);
    }
    let original = gateway::load(&source).unwrap().unwrap();
    assert!(original.enabled);
    assert_eq!(original.format, gateway::Format::OpenaiResponses);
    assert_eq!(original.harness, "codex");
    for args in [
        vec!["gateway", "config", "show", "--config-dir", dir, "--apply"],
        vec![
            "gateway",
            "config",
            "show",
            "--config-dir",
            dir,
            "--config-dir",
            dir,
        ],
        vec![
            "gateway",
            "config",
            "set",
            "--config-dir",
            dir,
            "--unknown",
            "value",
        ],
        vec![
            "gateway",
            "config",
            "set",
            "--config-dir",
            dir,
            "--key",
            "enabled",
            "--value",
        ],
        vec![
            "gateway",
            "config",
            "set",
            "--config-dir",
            dir,
            "--key",
            "enabled",
            "--value",
            "false",
            "--apply",
            "--apply",
        ],
    ] {
        f.call(&args, false);
        assert_eq!(gateway::load(&source).unwrap().unwrap(), original);
    }
    for (field, value) in [
        ("provider", "cursor"),
        (
            "endpoint",
            "https://user:do-not-echo-fixture-secret@example.invalid",
        ),
        ("credential_file", "do-not-echo-fixture-secret"),
    ] {
        f.call(
            &[
                "gateway",
                "config",
                "set",
                "--config-dir",
                dir,
                "--key",
                field,
                "--value",
                value,
                "--apply",
            ],
            false,
        );
        assert_eq!(gateway::load(&source).unwrap().unwrap(), original);
    }
    let destination = f.0.join("without-key");
    f.call(
        &[
            "config",
            "import-instance",
            "--config-dir",
            destination.to_str().unwrap(),
            "--from",
            dir,
            "--apply",
        ],
        true,
    );
    let imported = gateway::load(&destination).unwrap().unwrap();
    assert!(imported.enabled);
    assert_eq!(imported.model, original.model);
    assert_eq!(imported.endpoint, original.endpoint);
    assert!(!imported.credential_file.exists());
    let destination = f.0.join("with-key");
    f.call(
        &[
            "config",
            "import-instance",
            "--config-dir",
            destination.to_str().unwrap(),
            "--from",
            dir,
            "--credentials",
            "--apply",
        ],
        true,
    );
    assert_eq!(
        fs::read(destination.join("gateway-token.txt")).unwrap(),
        b"fixture-private-provider-key"
    );
    assert_eq!(
        fs::metadata(source.join("gateway.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(fs::read_dir(&source).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("gateway.backup-")
    }));
    assert!(!source.join("delivery.sqlite3").exists());
}
