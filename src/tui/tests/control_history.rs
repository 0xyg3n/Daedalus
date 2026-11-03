#![cfg(unix)]
//! Real packaged, offline receipt/policy and import paths.
use daedalus_tui::app::{
    config::ControlConfig,
    history::{self, State},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
fn private(path: &Path, bytes: &[u8]) {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path.parent().unwrap())
        .unwrap();
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "daedalus-history-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        private(&root.join("marker"), b"fixture");
        private(
            &root.join(".config/daedalus/dashboard.json"),
            br#"{"discovery":{"scan_processes":false,"scan_default_roots":false}}"#,
        );
        Self(root)
    }
    fn run(&self, args: &[&str], input: &str, success: bool) -> Value {
        let mut child = Command::new(env!("CARGO_BIN_EXE_daedalus-tui"))
            .args(args)
            .env_clear()
            .env("PATH", "/nonexistent")
            .env("HOME", &self.0)
            .current_dir(&self.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if !input.is_empty() {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        } else {
            drop(child.stdin.take());
        }
        let result = child.wait_with_output().unwrap();
        assert_eq!(
            result.status.success(),
            success,
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        if !success {
            return Value::Null;
        }
        if args[0] == "backend" {
            Value::Array(
                String::from_utf8(result.stdout)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect(),
            )
        } else {
            serde_json::from_slice(&result.stdout).unwrap()
        }
    }
    fn instance(&self, name: &str) -> PathBuf {
        let directory = self.0.join(name);
        let mut config = ControlConfig::fresh(&self.0).unwrap();
        config.discovery.automatic = false;
        private(&directory.join("control.json"), &config.encode().unwrap());
        directory
    }
    fn seed(&self, directory: &Path, count: usize) {
        let entries=(0..count).map(|i|json!({"version":1,"operation":if i==0{"deadbeef".into()}else{format!("{i:x}")},"action":"configure","state":if i==0{"unconfirmed"}else{"completed"},"time":1.0})).collect::<Vec<_>>();
        private(
            &directory.join("control-audit.json"),
            &serde_json::to_vec(&entries).unwrap(),
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn malformed_policy_keeps_receipts_intact_and_opens_history_recovery() {
    let f = Fixture::new();
    let directory = f.instance("instance");
    f.seed(&directory, 4);
    let original = b"{broken-HISTORY_PRIVATE_SENTINEL";
    private(&directory.join("control-history.json"), original);
    let before = fs::read(directory.join("control-audit.json")).unwrap();
    let requests = [
        json!({"version":2,"op":"action","page":4,"action":"settings.history"}),
        json!({"version":2,"op":"quit"}),
    ]
    .iter()
    .map(|value| format!("{value}\n"))
    .collect::<String>();
    let frames = f.run(
        &[
            "backend",
            "--stdio",
            "--config-dir",
            directory.to_str().unwrap(),
        ],
        &requests,
        true,
    );
    assert_eq!(
        fs::read(directory.join("control-audit.json")).unwrap(),
        before
    );
    assert_eq!(
        fs::read(directory.join("control-history.json")).unwrap(),
        original
    );
    assert!(!frames.to_string().contains("HISTORY_PRIVATE_SENTINEL"));
    assert!(
        frames
            .as_array()
            .unwrap()
            .iter()
            .any(|frame| frame["kind"] == "dialog" && frame["title"] == "Control action history"),
        "Damaged policy blocks the recovery menu"
    );
}
#[test]
fn packaged_policy_recovery_and_saved_archive_review_preserve_private_outcomes() {
    let f = Fixture::new();
    let directory = f.instance("instance");
    let dir = directory.to_str().unwrap();
    f.seed(&directory, 4);
    let receipts = fs::read(directory.join("control-audit.json")).unwrap();
    let original = b"{broken-HISTORY_PRIVATE_SENTINEL";
    private(&directory.join("control-history.json"), original);
    let shown = f.run(
        &["history", "config", "show", "--config-dir", dir],
        "",
        true,
    );
    assert_eq!(shown["policy_valid"], false);
    assert!(!shown.to_string().contains("HISTORY_PRIVATE_SENTINEL"));
    let page = f.run(
        &["history", "list", "--config-dir", dir, "--unresolved"],
        "",
        true,
    );
    assert_eq!(page["policy_valid"], false);
    assert_eq!(page["events"][0]["operation"], "deadbeef");
    let args = [
        "history",
        "config",
        "recover",
        "--config-dir",
        dir,
        "--entry-limit",
        "8",
        "--byte-limit",
        "8192",
        "--display-limit",
        "1",
        "--archive-scan-limit",
        "4",
    ];
    let preview = f.run(&args, "", true);
    assert_eq!(preview["saved"], false);
    assert!(preview["review"]["previous_configuration"].is_null());
    assert_eq!(
        fs::read(directory.join("control-history.json")).unwrap(),
        original
    );
    f.run(
        &[args.as_slice(), &["--apply", "--expect-sha", "wrong"]].concat(),
        "",
        false,
    );
    let sha = preview["review"]["approval"].as_str().unwrap();
    f.run(
        &[args.as_slice(), &["--apply", "--expect-sha", sha]].concat(),
        "",
        true,
    );
    assert_eq!(
        fs::read(preview["review"]["backup_file"].as_str().unwrap()).unwrap(),
        original
    );
    assert_eq!(
        fs::read(directory.join("control-audit.json")).unwrap(),
        receipts
    );
    let settings = State::load(&directory).unwrap().settings;
    assert_eq!(
        (
            settings.entry_limit,
            settings.byte_limit,
            settings.display_limit,
            settings.archive_scan_limit
        ),
        (8, 8192, 1, 4)
    );
    let args = ["history", "archive", "--config-dir", dir];
    let preview = f.run(&args, "", true);
    f.run(
        &[
            args.as_slice(),
            &[
                "--apply",
                "--expect-sha",
                preview["review"]["approval"].as_str().unwrap(),
            ],
        ]
        .concat(),
        "",
        true,
    );
    let archive = PathBuf::from(preview["review"]["archive_file"].as_str().unwrap());
    let name = archive.file_name().unwrap().to_str().unwrap();
    private(&directory.join("control-history.json"), b"bad policy again");
    private(
        &directory.join("control-audit.json"),
        b"unreadable active receipts",
    );
    let catalog = f.run(&["history", "archives", "--config-dir", dir], "", true);
    assert!(
        catalog["archives"]
            .as_array()
            .unwrap()
            .contains(&json!(name))
    );
    assert_eq!(catalog["policy_valid"], false);
    let archived = f.run(
        &[
            "history",
            "list",
            "--config-dir",
            dir,
            "--archive",
            name,
            "--unresolved",
        ],
        "",
        true,
    );
    assert_eq!(archived["source"], "archive");
    assert_eq!(archived["events"][0]["operation"], "deadbeef");
    assert_eq!(archived["policy_valid"], false);
    assert_eq!(fs::read(&archive).unwrap(), receipts);
    f.run(
        &[
            "history",
            "list",
            "--config-dir",
            dir,
            "--archive",
            "../control-audit.json",
        ],
        "",
        false,
    );
}

#[test]
fn packaged_raw_receipt_recovery_keeps_exact_bytes_and_requires_review() {
    let f = Fixture::new();
    let directory = f.instance("instance");
    let dir = directory.to_str().unwrap();
    let raw = b"{HISTORY_PRIVATE_RECEIPTS";
    private(&directory.join("control-audit.json"), raw);
    private(&directory.join("control-history.json"), b"bad policy");
    let shown = f.run(
        &["history", "config", "show", "--config-dir", dir],
        "",
        true,
    );
    assert_eq!(shown["history_valid"], false);
    assert!(shown["protected_operations"].is_null());
    assert!(!shown.to_string().contains("HISTORY_PRIVATE_RECEIPTS"));
    f.run(
        &["history", "config", "recover", "--config-dir", dir],
        "",
        false,
    );
    assert_eq!(fs::read(directory.join("control-audit.json")).unwrap(), raw);
    let args = ["history", "archive", "--config-dir", dir];
    let preview = f.run(&args, "", true);
    assert_eq!(preview["review"]["history_valid"], false);
    assert!(!preview.to_string().contains("HISTORY_PRIVATE_RECEIPTS"));
    f.run(
        &[
            args.as_slice(),
            &[
                "--apply",
                "--expect-sha",
                preview["review"]["approval"].as_str().unwrap(),
            ],
        ]
        .concat(),
        "",
        true,
    );
    assert_eq!(
        fs::read(preview["review"]["archive_file"].as_str().unwrap()).unwrap(),
        raw
    );
    assert_eq!(
        fs::read(directory.join("control-audit.json")).unwrap(),
        b"[]"
    );
    assert_eq!(
        fs::read(directory.join("control-history.json")).unwrap(),
        b"bad policy"
    );
    let args = ["history", "config", "recover", "--config-dir", dir];
    let preview = f.run(&args, "", true);
    f.run(
        &[
            args.as_slice(),
            &[
                "--apply",
                "--expect-sha",
                preview["review"]["approval"].as_str().unwrap(),
            ],
        ]
        .concat(),
        "",
        true,
    );
    assert!(State::load(&directory).is_ok());
}

#[test]
fn explicit_instance_history_can_be_edited_without_a_home_environment() {
    let f = Fixture::new();
    let directory = f.instance("instance");
    let mut child = Command::new(env!("CARGO_BIN_EXE_daedalus-tui"))
        .args([
            "backend",
            "--stdio",
            "--config-dir",
            directory.to_str().unwrap(),
        ])
        .env_clear()
        .env("PATH", "/nonexistent")
        .current_dir(&f.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let requests = [
        json!({"version":2,"op":"action","page":4,"action":"settings.history"}),
        json!({"version":2,"op":"reply","id":1,"value":"entry_limit"}),
        json!({"version":2,"op":"reply","id":2,"value":"8"}),
        json!({"version":2,"op":"reply","id":3,"value":"yes"}),
        json!({"version":2,"op":"quit"}),
    ]
    .iter()
    .map(|value| format!("{value}\n"))
    .collect::<String>();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(requests.as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("History settings saved"));
    assert_eq!(State::load(&directory).unwrap().settings.entry_limit, 8);
}
#[test]
fn full_old_history_keeps_unconfirmed_receipt_during_real_reviewed_backend_save() {
    let f = Fixture::new();
    let directory = f.instance("instance");
    f.seed(&directory, 1000);
    let requests = [
        json!({"version":2,"op":"action","page":4,"action":"settings.interface"}),
        json!({"version":2,"op":"reply","id":1,"value":"colors"}),
        json!({"version":2,"op":"reply","id":2,"value":"off"}),
        json!({"version":2,"op":"reply","id":3,"value":"yes"}),
        json!({"version":2,"op":"quit"}),
    ]
    .iter()
    .map(|value| format!("{value}\n"))
    .collect::<String>();
    let frames = f.run(
        &[
            "backend",
            "--stdio",
            "--config-dir",
            directory.to_str().unwrap(),
        ],
        &requests,
        true,
    );
    assert!(
        frames
            .as_array()
            .unwrap()
            .iter()
            .any(|frame| frame["message"] == "Configuration saved privately")
    );
    assert!(
        !ControlConfig::parse(&fs::read(directory.join("control.json")).unwrap())
            .unwrap()
            .ui
            .colors
    );
    let entries: Vec<Value> =
        serde_json::from_slice(&fs::read(directory.join("control-audit.json")).unwrap()).unwrap();
    assert_eq!(entries.len(), 1000);
    assert!(
        entries
            .iter()
            .any(|e| e["operation"] == "deadbeef" && e["state"] == "unconfirmed")
    );
    let report = f.run(
        &[
            "history",
            "list",
            "--config-dir",
            directory.to_str().unwrap(),
            "--unresolved",
        ],
        "",
        true,
    );
    assert_eq!(report["events"][0]["operation"], "deadbeef");
    assert!(!directory.join("delivery.sqlite3").exists());
}
#[test]
fn packaged_policy_preview_apply_pages_and_archive_preserve_exact_prior_bytes() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let directory = f.instance("instance");
    let dir = directory.to_str().unwrap();
    f.seed(&directory, 10);
    let before = fs::read(directory.join("control-audit.json")).unwrap();
    let shown = f.run(
        &["history", "config", "show", "--config-dir", dir],
        "",
        true,
    );
    assert_eq!(shown["editable_settings"].as_array().unwrap().len(), 4);
    for (key, value) in [
        ("entry_limit", "8"),
        ("byte_limit", "8192"),
        ("display_limit", "1"),
    ] {
        let args = [
            "history",
            "config",
            "set",
            "--config-dir",
            dir,
            "--key",
            key,
            "--value",
            value,
        ];
        let preview = f.run(&args, "", true);
        assert_eq!(preview["saved"], false);
        f.run(&[args.as_slice(), &["--apply"]].concat(), "", true);
        assert_eq!(
            State::load(&directory)
                .unwrap()
                .settings
                .display(key)
                .unwrap(),
            value
        );
        assert_eq!(
            fs::read(directory.join("control-audit.json")).unwrap(),
            before
        );
    }
    let first = f.run(&["history", "list", "--config-dir", dir], "", true);
    assert_eq!(first["events"].as_array().unwrap().len(), 1);
    assert_eq!(first["next_offset"], 1);
    let second = f.run(
        &["history", "list", "--config-dir", dir, "--offset", "1"],
        "",
        true,
    );
    assert_ne!(first["events"][0], second["events"][0]);
    for extra in [
        vec!["--unrecognized"],
        vec!["--apply", "--apply"],
        vec!["--expect-sha", "stale"],
    ] {
        f.run(
            &[
                &["history", "archive", "--config-dir", dir],
                extra.as_slice(),
            ]
            .concat(),
            "",
            false,
        );
    }
    let args = ["history", "archive", "--config-dir", dir];
    let review = f.run(&args, "", true);
    let digest = review["review"]["approval"].as_str().unwrap();
    let archive = PathBuf::from(review["review"]["archive_file"].as_str().unwrap());
    assert!(!archive.exists());
    f.run(
        &[args.as_slice(), &["--apply", "--expect-sha", "wrong"]].concat(),
        "",
        false,
    );
    assert_eq!(
        fs::read(directory.join("control-audit.json")).unwrap(),
        before
    );
    f.run(
        &[args.as_slice(), &["--apply", "--expect-sha", digest]].concat(),
        "",
        true,
    );
    assert_eq!(fs::read(&archive).unwrap(), before);
    assert_eq!(
        fs::metadata(&archive).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::read(directory.join("control-audit.json")).unwrap(),
        b"[]"
    );
    f.run(
        &[args.as_slice(), &["--apply", "--expect-sha", digest]].concat(),
        "",
        false,
    );
    assert!(!directory.join("telegram-service.json").exists());
}
#[test]
fn portable_policy_import_archives_current_source_receipts_without_replaying_them() {
    let f = Fixture::new();
    let source = f.instance("source");
    let destination = f.0.join("destination");
    f.seed(&source, 4);
    let before = fs::read(source.join("control-audit.json")).unwrap();
    let state = State::load(&source).unwrap();
    let updated = state.settings.set("display_limit", "2").unwrap();
    let plan = history::Plan::edit(state, updated).unwrap();
    let approval = plan.approval();
    plan.apply(&approval).unwrap();
    f.run(
        &[
            "config",
            "import-instance",
            "--config-dir",
            destination.to_str().unwrap(),
            "--from",
            source.to_str().unwrap(),
            "--apply",
        ],
        "",
        true,
    );
    assert_eq!(State::load(&destination).unwrap().settings.display_limit, 2);
    assert!(!destination.join("control-audit.json").exists());
    let archives = fs::read_dir(&destination)
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("control-audit.archive-")
                .then_some(path)
        })
        .collect::<Vec<_>>();
    assert_eq!(archives.len(), 1);
    assert_eq!(fs::read(&archives[0]).unwrap(), before);
    assert_eq!(fs::read(source.join("control-audit.json")).unwrap(), before);
}
