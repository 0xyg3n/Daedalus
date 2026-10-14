//! Packaged entry-point checks: these paths cannot resolve Python or a harness.
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "daedalus-native-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        Self(directory)
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_daedalus-tui"))
            .env_clear()
            .env("PATH", "/nonexistent")
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap()
    }
    fn input(&self, args: &[&str], input: &[u8]) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_daedalus-tui"))
            .env_clear()
            .env("PATH", "/nonexistent")
            .current_dir(&self.0)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Err(error) = child.stdin.take().unwrap().write_all(input) {
            // Validation may reject the command before consuming stdin.
            assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        }
        child.wait_with_output().unwrap()
    }
}

#[test]
fn native_backend_emits_existing_protocol_without_python() {
    let fixture = Fixture::new();
    let destination = fixture.0.join("instance");
    let source = fixture.0.join("settings.json");
    fs::write(&source, serde_json::to_vec(&json!({"version":1,"agent_name":"example","agent_home":fixture.0,"telegram":{},"discovery":{"automatic":false,"herdr_sessions":[]},"tmux_sockets":[]})).unwrap()).unwrap();
    let imported = fixture.run(&[
        "config",
        "import",
        "--config-dir",
        destination.to_str().unwrap(),
        "--from",
        source.to_str().unwrap(),
        "--apply",
    ]);
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_daedalus-tui"))
        .env_clear()
        .env("PATH", "/nonexistent")
        .current_dir(&fixture.0)
        .args([
            "backend",
            "--stdio",
            "--config-dir",
            destination.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"version\":2,\"op\":\"refresh\"}\n{\"version\":2,\"op\":\"quit\"}\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let frames: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(frames.len() >= 2);
    for frame in frames.iter().filter(|frame| frame["kind"] == "state") {
        assert_eq!(frame["version"], 2);
        assert_eq!(frame["terminals"], 0);
        assert_eq!(frame["sections"].as_array().unwrap().len(), 11);
        assert!(frame["terminal_actions"].is_array());
        assert!(frame["workspace"].is_object());
    }
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame["kind"] == "state")
            .count(),
        2
    );
    assert!(!frames.iter().any(|frame| frame["kind"] == "error"));
    assert!(!destination.join("feeder-process.json").exists());
}

#[test]
fn native_goal_controls_preserve_waiting_state_without_launching_a_process() {
    let fixture = Fixture::new();
    let source = fixture.0.join("registration.json");
    fs::write(&source,serde_json::to_vec(&json!({
        "version":1,"id":"fixture","directory":fixture.0,
        "scope":{"instance":"example","terminal":{"backend":"tmux","endpoint":"/tmp/mock.sock","pane":"%1","generation":"fixture-generation"},"conversation":"fixture-session","harness":"claude"},
        "goal_id":"fixture-goal","transcript":null,"events":["Stop"],
        "options":{"telegram_delivery":false,"telegram_progress":false,"notifications":false,"hindsight":false},
        "activity_lease_ms":30000,"pending_timeout_ms":30000,"hook_timeout_ms":10000
    })).unwrap()).unwrap();
    let installed = fixture.run(&["hook", "install", "--from", source.to_str().unwrap()]);
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let path = fixture.0.join("hooks/fixture.json");
    let armed = fixture.input(
        &["goal", "arm", "--registration", path.to_str().unwrap()],
        b"Complete the synthetic fixture",
    );
    assert!(
        armed.status.success(),
        "{}",
        String::from_utf8_lossy(&armed.stderr)
    );
    let goal: Value = serde_json::from_slice(&armed.stdout).unwrap();
    assert_eq!(goal["goal"]["status"]["state"], "armed");
    for (command, expected) in [
        ("status", "armed"),
        ("pause", "paused"),
        ("resume", "armed"),
        ("complete", "completed"),
        ("status", "completed"),
    ] {
        let result = fixture.run(&["goal", command, "--registration", path.to_str().unwrap()]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let goal: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(goal["goal"]["status"]["state"], expected);
    }
    assert!(!fixture.0.join("feeder-process.json").exists());
}

#[test]
fn native_hook_errors_use_blocking_exit_code_and_never_emit_success_json() {
    let fixture = Fixture::new();
    let missing = fixture.0.join("missing.json");
    let output = fixture.input(
        &[
            "hook",
            "run",
            "--registration",
            missing.to_str().unwrap(),
            "--event-kind",
            "PreToolUse",
        ],
        b"{\"hook_event_name\":\"PreToolUse\",\"session_id\":\"fixture\"}",
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("missing.json"));
    let stopped = fixture.input(
        &[
            "hook",
            "run",
            "--registration",
            missing.to_str().unwrap(),
            "--event-kind",
            "Stop",
        ],
        b"{\"hook_event_name\":\"Stop\",\"session_id\":\"fixture\"}",
    );
    assert_eq!(stopped.status.code(), Some(1));
    assert!(stopped.stdout.is_empty());
    let help = fixture.run(&["setup", "--cli", "--help"]);
    assert!(help.status.success());
}

#[test]
fn native_diagnostics_without_python_does_not_create_runtime_state() {
    let fixture = Fixture::new();
    let output = fixture.run(&[
        "diagnostics",
        "--config-dir",
        fixture.0.to_str().unwrap(),
        "--json",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let state: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(state["live_probes"], false);
    assert_eq!(state["goal_recovery"], false);
    assert_eq!(state["configuration"]["status"], "missing");
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 0);
    assert!(!String::from_utf8_lossy(&output.stdout).contains(fixture.0.to_str().unwrap()));
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn instance_import_runs_without_python_and_does_not_activate_services() {
    let fixture = Fixture::new();
    let source = fixture.0.join("old");
    let destination = fixture.0.join("new");
    let init = fixture.run(&[
        "config",
        "init",
        "--config-dir",
        source.to_str().unwrap(),
        "--workspace",
        fixture.0.to_str().unwrap(),
        "--apply",
    ]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    fs::write(source.join("tg_offset"), b"12345\n").unwrap();
    fs::write(
        source.join("pending_feed.jsonl"),
        b"{\"win\":\"missing-pane\",\"text\":\"pending fixture\"}\n",
    )
    .unwrap();
    let args = [
        "config",
        "import-instance",
        "--config-dir",
        destination.to_str().unwrap(),
        "--from",
        source.to_str().unwrap(),
    ];
    let preview = fixture.run(&args);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    assert!(!destination.exists());
    let mut apply = args.to_vec();
    apply.push("--apply");
    let applied = fixture.run(&apply);
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let result: Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(result["result"]["requires_cutover"], true);
    let cursor: Value =
        serde_json::from_slice(&fs::read(destination.join("legacy-polling.json")).unwrap())
            .unwrap();
    assert_eq!(cursor["next_offset"], 12345);
    assert_eq!(
        fs::read(destination.join("legacy-pending-feed.jsonl")).unwrap(),
        fs::read(source.join("pending_feed.jsonl")).unwrap()
    );
    assert!(!destination.join("feeder-process.json").exists());
}

#[test]
fn fresh_and_import_work_without_python_and_do_not_probe_disabled_backends() {
    let fixture = Fixture::new();
    let destination = fixture.0.join("instance");
    let path = destination.to_str().unwrap();
    let preview = fixture.run(&[
        "config",
        "init",
        "--config-dir",
        path,
        "--workspace",
        "/tmp/example",
    ]);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    assert!(!destination.exists());
    let applied = fixture.run(&[
        "config",
        "init",
        "--config-dir",
        path,
        "--workspace",
        "/tmp/example",
        "--apply",
    ]);
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let source = fixture.0.join("import.json");
    fs::write(&source, serde_json::to_vec(&json!({"version":1,"agent_name":"example","agent_home":"/tmp/example","subscriptions":["claude"],"telegram":{},"discovery":{"automatic":false,"herdr_sessions":[]},"tmux_sockets":[]})).unwrap()).unwrap();
    let imported = fixture.run(&[
        "config",
        "import",
        "--config-dir",
        path,
        "--from",
        source.to_str().unwrap(),
        "--apply",
    ]);
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let inventory = fixture.run(&["inventory", "--config-dir", path, "--json"]);
    assert!(
        inventory.status.success(),
        "{}",
        String::from_utf8_lossy(&inventory.stderr)
    );
    let state: Value = serde_json::from_slice(&inventory.stdout).unwrap();
    assert_eq!(state["panes"], json!([]));
    assert_eq!(state["errors"], json!([]));
    let repeated = fixture.run(&[
        "config",
        "import",
        "--config-dir",
        path,
        "--from",
        source.to_str().unwrap(),
        "--apply",
    ]);
    assert!(repeated.status.success());
    let result: Value = serde_json::from_slice(&repeated.stdout).unwrap();
    assert_eq!(result["result"]["changed"], false);
}

#[test]
fn telegram_configuration_preview_import_and_check_are_private_offline_and_redacted() {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let fixture = Fixture::new();
    let help = fixture.run(&["telegram", "config", "--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("TELEGRAM_BOT_TOKEN"));
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 0);
    let path = fixture.0.join("telegram-service.json");
    let token = fixture.0.join("token");
    let args = [
        "telegram",
        "config",
        "init",
        "--file",
        path.to_str().unwrap(),
        "--instance",
        "example",
        "--bot-id",
        "123",
        "--token-file",
        token.to_str().unwrap(),
        "--owner",
        "7",
        "--chat",
        "42",
    ];
    let preview = fixture.run(&args);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    assert!(!path.exists());
    assert!(!token.exists());
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 0);
    let mut apply = args.to_vec();
    apply.push("--apply");
    let applied = fixture.run(&apply);
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        config["grants"][0]["permissions"],
        json!(["read", "send", "manage"])
    );
    let mut secret = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&token)
        .unwrap();
    secret
        .write_all(b"123:synthetic_fixture_not_a_real_token\n")
        .unwrap();
    drop(secret);
    let count = fs::read_dir(&fixture.0).unwrap().count();
    let checked = fixture.run(&[
        "telegram",
        "config",
        "check",
        "--file",
        path.to_str().unwrap(),
    ]);
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    let result: Value = serde_json::from_slice(&checked.stdout).unwrap();
    assert_eq!(result["valid"], true);
    assert_eq!(result["network_checked"], false);
    assert!(!String::from_utf8_lossy(&checked.stdout).contains("synthetic_fixture"));
    assert!(!String::from_utf8_lossy(&checked.stderr).contains("synthetic_fixture"));
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), count);
    let imported = fixture.0.join("imported.json");
    let import = fixture.run(&[
        "telegram",
        "config",
        "import",
        "--file",
        imported.to_str().unwrap(),
        "--from",
        path.to_str().unwrap(),
        "--apply",
    ]);
    assert!(
        import.status.success(),
        "{}",
        String::from_utf8_lossy(&import.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(imported).unwrap()).unwrap(),
        config
    );
    for runtime in [
        "delivery.sqlite3",
        "routes.sqlite3",
        "telegram-controls.sqlite3",
        ".telegram-service.lock",
        "attachments",
    ] {
        assert!(!fixture.0.join(runtime).exists());
    }
}

#[test]
fn telegram_serve_fails_before_ownership_or_network_when_token_identity_is_invalid() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let config = fixture.0.join("telegram-service.json");
    let token = fixture.0.join("token");
    fs::write(&config,serde_json::to_vec(&json!({"version":1,"instance":"example","bot_id":123,"token_file":token,"grants":[],"listen_chats":[],"registrations":[],"hook_bindings":[]})).unwrap()).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(fixture.0.join("control.json"),serde_json::to_vec(&json!({"version":1,"agent_name":"example","agent_home":fixture.0,"telegram":{},"discovery":{"automatic":false,"herdr_sessions":[]},"tmux_sockets":[]})).unwrap()).unwrap();
    fs::write(&token, b"999:synthetic_wrong_bot").unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let count = fs::read_dir(&fixture.0).unwrap().count();
    let output = fixture.run(&[
        "telegram",
        "serve",
        "--config-dir",
        fixture.0.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic_wrong_bot"));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Token does not match configured bot identity")
    );
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), count);
    assert!(!fixture.0.join(".telegram-service.lock").exists());
    assert!(!fixture.0.join("delivery.sqlite3").exists());
}

#[test]
fn gateway_exec_reads_private_key_only_inside_verified_fake_harness_process() {
    use daedalus_tui::{
        app::{
            config::ControlConfig,
            gateway::{self, Config, Format},
        },
        core::checkpoints::{LaunchPlan, Operation},
    };
    use std::{collections::BTreeMap, os::unix::fs::PermissionsExt};
    let fixture = Fixture::new();
    let key = fixture.0.join("credential");
    fs::write(&key, b"PRIVATE-GATEWAY-KEY").unwrap();
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    let control = ControlConfig::fresh(&fixture.0).unwrap();
    fs::write(
        fixture.0.join("control.json"),
        serde_json::to_vec(&control).unwrap(),
    )
    .unwrap();
    let config = Config {
        version: 1,
        enabled: true,
        format: Format::AnthropicMessages,
        endpoint: "https://gateway.example.invalid".into(),
        model: "fixture-model".into(),
        credential_file: key,
        harness: "claude".into(),
    };
    gateway::save(&fixture.0, &None, &config).unwrap();
    let fake = fixture.0.join("claude");
    fs::write(&fake,"#!/bin/sh\nprintf '%s\\n' \"$ANTHROPIC_AUTH_TOKEN\" \"$CLAUDE_CONFIG_DIR\" \"$ANTHROPIC_BASE_URL\" \"$@\" > observed\n").unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
    let profile = fixture.0.join("checkpoint-profile");
    fs::create_dir(&profile).unwrap();
    let environment: BTreeMap<String, String> = [
        ("HOME".into(), fixture.0.to_string_lossy().into_owned()),
        ("PATH".into(), fixture.0.to_string_lossy().into_owned()),
        (
            "CLAUDE_CONFIG_DIR".into(),
            profile.to_string_lossy().into_owned(),
        ),
    ]
    .into_iter()
    .collect();
    let mut plan = LaunchPlan {
        binary: fake,
        args: vec!["--resume".into(), "fixture-session".into()],
        environment,
        cwd: fixture.0.clone(),
        operation: Operation::Resume,
    };
    gateway::wrap(
        &fixture.0,
        "claude",
        &mut plan,
        std::path::Path::new(env!("CARGO_BIN_EXE_daedalus-tui")),
    )
    .unwrap();
    assert!(!plan.args.join(" ").contains("PRIVATE-GATEWAY-KEY"));
    assert!(
        !plan
            .environment
            .values()
            .any(|v| v.contains("PRIVATE-GATEWAY-KEY"))
    );
    let execute = || {
        Command::new(&plan.binary)
            .args(&plan.args)
            .env_clear()
            .envs(&plan.environment)
            .current_dir(&plan.cwd)
            .output()
            .unwrap()
    };
    let result = execute();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.stdout.is_empty());
    let observed = fs::read_to_string(fixture.0.join("observed")).unwrap();
    assert!(observed.contains("PRIVATE-GATEWAY-KEY"));
    assert!(observed.contains(profile.to_str().unwrap()));
    assert!(observed.contains("fixture-session"));
    fs::remove_file(fixture.0.join("observed")).unwrap();
    let mut changed = config.clone();
    changed.model = "changed".into();
    gateway::save(&fixture.0, &Some(config), &changed).unwrap();
    let result = execute();
    assert!(!result.status.success());
    assert!(!fixture.0.join("observed").exists());
    assert!(!String::from_utf8_lossy(&result.stderr).contains("PRIVATE-GATEWAY-KEY"));
}

#[test]
fn native_package_installs_one_binary_and_fresh_config_without_python() {
    use sha2::{Digest, Sha256};
    let fixture = Fixture::new();
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_daedalus-tui"));
    let digest = format!("{:x}", Sha256::digest(fs::read(&binary).unwrap()));
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let prefix = fixture.0.join("native package");
    let installed = Command::new("/bin/bash")
        .arg(root.join("install.sh"))
        .args(["--native", "--binary"])
        .arg(&binary)
        .args(["--sha256", &digest, "--yes", "--prefix"])
        .arg(&prefix)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &fixture.0)
        .output()
        .unwrap();
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let config = fixture.0.join("fresh instance");
    let result = Command::new(prefix.join("bin/daedalus"))
        .env_clear()
        .env("PATH", "/nonexistent")
        .args(["config", "init", "--config-dir"])
        .arg(&config)
        .arg("--workspace")
        .arg(&fixture.0)
        .arg("--apply")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(config.join("control.json").exists());
    assert!(!config.join("hindsight.json").exists());
    assert!(!config.join("telegram-service.json").exists());
    assert_eq!(fs::read_dir(prefix.join("bin")).unwrap().count(), 1);
}

#[test]
fn checkpoint_and_terminal_commands_preview_without_python_or_live_discovery() {
    let fixture = Fixture::new();
    let directory = fixture.0.join("private");
    let init = fixture.run(&[
        "config",
        "init",
        "--config-dir",
        directory.to_str().unwrap(),
        "--workspace",
        fixture.0.to_str().unwrap(),
        "--apply",
    ]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let target = directory.join("target.json");
    use std::os::unix::fs::OpenOptionsExt;
    let mut target_file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&target)
        .unwrap();
    target_file.write_all(&serde_json::to_vec(&json!({"backend":"herdr","session":"fixture","pane_id":"pane","terminal_id":"generation"})).unwrap()).unwrap();
    drop(target_file);
    for verb in ["checkpoint", "ckpt"] {
        let list = fixture.run(&[verb, "list", "--config-dir", directory.to_str().unwrap()]);
        assert!(
            list.status.success(),
            "{}",
            String::from_utf8_lossy(&list.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&list.stdout).unwrap(),
            json!([])
        );
    }
    let preview = fixture.run(&[
        "terminal",
        "send",
        "--config-dir",
        directory.to_str().unwrap(),
        "--target",
        target.to_str().unwrap(),
        "--text",
        "fixture literal",
    ]);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let value: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(value["apply"], false);
    assert_eq!(value["target"]["generation"], "generation");
    let save = fixture.run(&[
        "checkpoint",
        "save",
        "--config-dir",
        directory.to_str().unwrap(),
        "--target",
        target.to_str().unwrap(),
        "--name",
        "fixture",
    ]);
    assert!(
        save.status.success(),
        "{}",
        String::from_utf8_lossy(&save.stderr)
    );
    assert!(!directory.join("dashboard-checkpoints.json").exists());
    assert!(!directory.join("telegram-service.json").exists());
}

#[test]
fn native_job_retained_aliases_reject_missing_identity_without_python() {
    let fixture = Fixture::new();
    let help = fixture.run(&["job", "--help"]);
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(text.contains("resume, fork, handoff, delegate, status"));
    for operation in ["resume", "fork", "handoff"] {
        let output = fixture.run(&[
            "job",
            operation,
            "--config-dir",
            fixture.0.to_str().unwrap(),
        ]);
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("Options do not match checkpoint operation"),
            "{operation}: {error}"
        );
        assert!(!error.contains("Python"));
        assert!(!error.contains("migration"));
    }
    for operation in ["shard", "gc", "gc-plan", "register", "reconcile"] {
        let output = fixture.run(&["job", operation]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("retired"));
    }
}
