//! Real CLI previews on both host platforms; no harness or network is used.
use daedalus_tui::{
    app::config::ControlConfig,
    core::checkpoints::{self, Checkpoint},
    platform,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output},
};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let mut nonce = [0; 8];
        platform::random_bytes(&mut nonce).unwrap();
        let nonce: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let root = std::env::temp_dir().join(format!("daedalus-fork-license-cli-{nonce}"));
        directory(&root);
        Self(root)
    }
    fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_daedalus-tui"));
        command
            .env_clear()
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("PATH", self.0.join("missing-bin"))
            .current_dir(&self.0)
            .args(args);
        for name in ["SYSTEMROOT", "SystemRoot", "WINDIR", "COMSPEC"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command.output().unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn directory(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .unwrap();
    }
    #[cfg(windows)]
    platform::ensure_private_dir(path).unwrap();
}
fn write(path: &Path, bytes: &[u8]) {
    directory(path.parent().unwrap());
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap()
    };
    #[cfg(windows)]
    let mut file = platform::create_private_new(path).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}
#[test]
fn license_catalog_and_fork_preview_select_exact_profile_without_reading_auth_or_mutating_source() {
    let f = Fixture::new();
    let state = f.0.join("state");
    let source = f.0.join("source");
    let target = f.0.join("target");
    directory(&state);
    directory(&source);
    directory(&target);
    let config = ControlConfig::fresh(&f.0).unwrap();
    write(&state.join("control.json"), &config.encode().unwrap());
    let checkpoint:Checkpoint=serde_json::from_value(json!({"name":"Fixture conversation","harness":"codex","sid":"12345678-abcd-4000-8000-123456789abc","cwd":f.0,"profile":source})).unwrap();
    let key = checkpoints::key(&checkpoint).unwrap();
    let saved = serde_json::to_vec(&json!({"checkpoints":[checkpoint]})).unwrap();
    write(&state.join("dashboard-checkpoints.json"), &saved);
    write(
        &state.join("codex-licenses.json"),
        &serde_json::to_vec(&json!({"licenses":{"fixture-license":{"home":target}}})).unwrap(),
    );
    for profile in [&source, &target] {
        write(
            &profile.join("auth.json"),
            b"Not valid authentication; this fixture must never be parsed",
        );
    }
    let observation = f.0.join("destination.json");
    write(&observation,&serde_json::to_vec(&json!({"backend":"herdr","session":"fixture","pane_id":"pane","terminal_id":"generation"})).unwrap());
    let list = f.run(&[
        "checkpoint",
        "licenses",
        "--config-dir",
        state.to_str().unwrap(),
    ]);
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    let listed: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(listed[0]["name"], "fixture-license");
    assert_eq!(listed[0]["home"], target.to_string_lossy().as_ref());
    let mut args = vec![
        "checkpoint",
        "spawn",
        "--config-dir",
        state.to_str().unwrap(),
        "--key",
        &key,
        "--target",
        observation.to_str().unwrap(),
        "--operation",
        "fork",
        "--harness",
        "codex",
        "--license",
        "fixture-license",
    ];
    let preview = f.run(&args);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let preview: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(preview["license"]["name"], "fixture-license");
    assert_eq!(
        preview["license"]["home"],
        target.to_string_lossy().as_ref()
    );
    assert_eq!(preview["apply"], false);
    *args.last_mut().unwrap() = "unknown-license";
    let refused = f.run(&args);
    assert!(!refused.status.success());
    assert_eq!(
        fs::read(state.join("dashboard-checkpoints.json")).unwrap(),
        saved
    );
    assert!(!source.join("sessions").exists() && !target.join("sessions").exists());
    for profile in [&source, &target] {
        assert_eq!(
            fs::read(profile.join("auth.json")).unwrap(),
            b"Not valid authentication; this fixture must never be parsed"
        );
    }
}
