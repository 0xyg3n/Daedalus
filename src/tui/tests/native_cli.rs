#![cfg(unix)]
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

fn observation_backend_fixture(data: Value) -> Fixture {
    use daedalus_tui::app::config::ControlConfig;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let f = Fixture::new();
    let state = f.0.join("state");
    assert!(
        f.run(&[
            "config",
            "init",
            "--config-dir",
            state.to_str().unwrap(),
            "--workspace",
            f.0.to_str().unwrap(),
            "--apply"
        ])
        .status
        .success()
    );
    let path = state.join("control.json");
    let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    config.discovery.automatic = false;
    config.discovery.herdr_sessions = vec!["fixture".into()];
    fs::write(path, config.encode().unwrap()).unwrap();
    let dashboard = f.0.join(".config/daedalus");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dashboard)
        .unwrap();
    fs::write(
        dashboard.join("dashboard.json"),
        br#"{"discovery":{"scan_processes":false,"scan_default_roots":false}}"#,
    )
    .unwrap();
    fs::set_permissions(
        dashboard.join("dashboard.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    fs::write(f.0.join("payload"), serde_json::to_vec(&data).unwrap()).unwrap();
    let script = f.0.join("inert-herdr");
    fs::write(&script,"#!/bin/sh\nprintf '%s\\n' \"$*\" >>\"$FIXTURE_QUERIES\"\nexec /bin/cat \"$FIXTURE_PAYLOAD\"\n").unwrap();
    fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();
    f
}
fn observation_backend(f: &Fixture) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_daedalus-tui"));
    command
        .env_clear()
        .env("PATH", "/nonexistent")
        .env("HOME", &f.0)
        .env("DAEDALUS_HERDR_BIN", f.0.join("inert-herdr"))
        .env("FIXTURE_PAYLOAD", f.0.join("payload"))
        .env("FIXTURE_QUERIES", f.0.join("queries"))
        .current_dir(&f.0)
        .arg("backend")
        .arg("--stdio")
        .arg("--config-dir")
        .arg(f.0.join("state"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}
#[test]
fn checkpoint_strict_loading_and_import_preserve_removed_references_after_context_relocation() {
    use daedalus_tui::{
        app::config::ControlConfig,
        core::checkpoints::{self, Registry},
    };
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let state = f.0.join("state");
    assert!(
        f.run(&[
            "config",
            "init",
            "--config-dir",
            state.to_str().unwrap(),
            "--workspace",
            f.0.to_str().unwrap(),
            "--apply"
        ])
        .status
        .success()
    );
    let path = state.join("control.json");
    let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    config.discovery.automatic = false;
    fs::write(&path, config.encode().unwrap()).unwrap();
    let context_dir = state.join("checkpoints");
    fs::create_dir(&context_dir).unwrap();
    fs::set_permissions(&context_dir, fs::Permissions::from_mode(0o700)).unwrap();
    let context = context_dir.join("captured.md");
    fs::write(&context, b"Synthetic captured context").unwrap();
    fs::set_permissions(&context, fs::Permissions::from_mode(0o600)).unwrap();
    let registry = state.join("dashboard-checkpoints.json");
    let raw = serde_json::to_vec(&json!({"checkpoints":[
        {"name":"Removed native", "harness":"codex", "sid":"fixture-one", "cwd":f.0, "profile":""},
        {"name":"Removed context", "harness":"codex", "sid":"fixture-two", "cwd":f.0, "context_file":context, "profile":""},
        {"name":"Retained native", "harness":"codex", "sid":"fixture-three", "cwd":f.0, "profile":""}
    ]})).unwrap();
    fs::write(&registry, &raw).unwrap();
    fs::set_permissions(&registry, fs::Permissions::from_mode(0o600)).unwrap();
    let listed = f.run(&[
        "checkpoint",
        "list",
        "--config-dir",
        state.to_str().unwrap(),
    ]);
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let rows: Vec<Value> = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(rows.len(), 3);
    let removed_keys: Vec<_> = rows
        .iter()
        .take(2)
        .map(|r| r["key"].as_str().unwrap().to_owned())
        .collect();
    for key in &removed_keys {
        assert!(
            f.run(&[
                "checkpoint",
                "remove",
                "--config-dir",
                state.to_str().unwrap(),
                "--key",
                key,
                "--apply"
            ])
            .status
            .success()
        );
    }
    let removals = fs::read(state.join("checkpoint-removals.json")).unwrap();
    let destination = f.0.join("destination");
    let imported = f.run(&[
        "config",
        "import-instance",
        "--config-dir",
        destination.to_str().unwrap(),
        "--from",
        state.to_str().unwrap(),
        "--apply",
    ]);
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let listed = f.run(&[
        "checkpoint",
        "list",
        "--config-dir",
        destination.to_str().unwrap(),
    ]);
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let active: Vec<Value> = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0]["name"], "Retained native");
    let imported: Registry =
        serde_json::from_slice(&fs::read(destination.join("dashboard-checkpoints.json")).unwrap())
            .unwrap();
    assert_eq!(
        imported.checkpoints.len(),
        3,
        "Removed evidence is retained"
    );
    let relocated = imported
        .checkpoints
        .iter()
        .find(|r| r.name == "Removed context")
        .unwrap();
    assert!(std::path::Path::new(&relocated.context_file).starts_with(&destination));
    let destination_removals: Vec<String> =
        serde_json::from_slice(&fs::read(destination.join("checkpoint-removals.json")).unwrap())
            .unwrap();
    assert!(
        removed_keys
            .iter()
            .all(|key| destination_removals.contains(key))
    );
    assert!(destination_removals.contains(&checkpoints::key(relocated).unwrap()));
    assert_eq!(fs::read(&registry).unwrap(), raw);
    assert_eq!(
        fs::read(state.join("checkpoint-removals.json")).unwrap(),
        removals
    );
    let ambiguous = String::from_utf8(raw.clone()).unwrap().replacen(
        "\"profile\":\"\"",
        "\"profile\":\"/synthetic/a\",\"profile\":\"/synthetic/b\"",
        1,
    );
    fs::write(&registry, ambiguous.as_bytes()).unwrap();
    let refused_path = f.0.join("refused");
    for args in [
        vec![
            "checkpoint",
            "list",
            "--config-dir",
            state.to_str().unwrap(),
        ],
        vec![
            "checkpoint",
            "remove",
            "--config-dir",
            state.to_str().unwrap(),
            "--key",
            &removed_keys[0],
            "--apply",
        ],
        vec![
            "config",
            "import-instance",
            "--config-dir",
            refused_path.to_str().unwrap(),
            "--from",
            state.to_str().unwrap(),
            "--apply",
        ],
    ] {
        let result = f.run(&args);
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("ambiguous"));
    }
    assert_eq!(fs::read(&registry).unwrap(), ambiguous.as_bytes());
    assert_eq!(
        fs::read(state.join("checkpoint-removals.json")).unwrap(),
        removals
    );
    assert!(!f.0.join("refused").exists());
    assert!(!fs::read_dir(&f.0).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".daedalus-import-")
    }));
}

#[test]
fn checkpoint_saved_limits_and_shared_discovery_admission_reach_real_cli_and_backend() {
    use daedalus_tui::app::config::ControlConfig;
    use std::os::unix::fs::PermissionsExt;
    let f = observation_backend_fixture(json!({"result":{"panes":[]}}));
    let state = f.0.join("state");
    let path = state.join("control.json");
    for (key, value) in [
        ("checkpoint_file_limit_bytes", "8192"),
        ("checkpoint_work_limit_bytes", "65536"),
        ("checkpoint_entry_limit", "17"),
        ("checkpoint_value_limit", "512"),
        ("checkpoint_timeout_ms", "1234"),
    ] {
        let key = format!("runtime.{key}");
        let before = fs::read(&path).unwrap();
        let args = [
            "config",
            "set",
            "--config-dir",
            state.to_str().unwrap(),
            "--key",
            &key,
            "--value",
            value,
        ];
        assert!(f.run(&args).status.success());
        assert_eq!(fs::read(&path).unwrap(), before);
        let applied = f.run(&[args.as_slice(), &["--apply"]].concat());
        assert!(
            applied.status.success(),
            "{}",
            String::from_utf8_lossy(&applied.stderr)
        );
        let receipt: Value = serde_json::from_slice(&applied.stdout).unwrap();
        assert_eq!(
            fs::read(receipt["result"]["backup"].as_str().unwrap()).unwrap(),
            before
        );
    }
    let registry = state.join("dashboard-checkpoints.json");
    let raw = serde_json::to_vec(&json!({"checkpoints":[{"name":"Synthetic checkpoint", "harness":"codex", "sid":"fixture-id", "cwd":f.0, "origin":"x".repeat(5000)}]})).unwrap();
    fs::write(&registry, &raw).unwrap();
    fs::set_permissions(&registry, fs::Permissions::from_mode(0o600)).unwrap();
    for (whole, file, expected) in [(4096, 8192, 0), (32768, 4096, 0), (32768, 8192, 1)] {
        let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
        config.runtime.discovery_metadata_limit_bytes = whole;
        config.runtime.checkpoint_file_limit_bytes = file;
        fs::write(&path, config.encode().unwrap()).unwrap();
        let mut child = observation_backend(&f).spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"{\"version\":2,\"op\":\"quit\"}\n")
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
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        let frame = frames.iter().find(|v| v["kind"] == "state").unwrap();
        assert_eq!(frame["checkpoints"], expected);
        assert_eq!(frame["terminals"], 0);
        if expected == 0 {
            assert!(frame["freshness"].as_str().unwrap().starts_with("PARTIAL"));
            assert!(frame["warnings"].as_u64().unwrap() > 0);
        }
        assert_eq!(fs::read(&registry).unwrap(), raw);
    }
    let listed = f.run(&[
        "checkpoint",
        "list",
        "--config-dir",
        state.to_str().unwrap(),
    ]);
    assert!(listed.status.success());
    let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    config.runtime.checkpoint_work_limit_bytes = 4096;
    fs::write(&path, config.encode().unwrap()).unwrap();
    let refused = f.run(&[
        "checkpoint",
        "list",
        "--config-dir",
        state.to_str().unwrap(),
    ]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("byte-work limit"));
    assert_eq!(fs::read(&registry).unwrap(), raw);
    assert!(!state.join("telegram-service.json").exists());
}

#[test]
fn saved_local_navigation_settings_preview_validate_backup_and_roundtrip_in_packaged_cli() {
    use daedalus_tui::app::config::ControlConfig;
    let fixture = Fixture::new();
    let state = fixture.0.join("state");
    assert!(
        fixture
            .run(&[
                "config",
                "init",
                "--config-dir",
                state.to_str().unwrap(),
                "--workspace",
                fixture.0.to_str().unwrap(),
                "--apply"
            ])
            .status
            .success()
    );
    let path = state.join("control.json");
    for (key, value, above) in [
        ("ui.list_page_rows", "4", "1001"),
        ("ui.details_page_rows", "6", "1001"),
        ("ui.options_page_items", "2", "1001"),
        ("ui.mouse_scroll_rows", "2", "1001"),
        ("ui.choice_wheel_items", "3", "1001"),
        ("ui.search_query_limit_chars", "7", "16385"),
        ("ui.shortcut_query_limit_chars", "5", "16385"),
    ] {
        let before = fs::read(&path).unwrap();
        for invalid in ["0", above] {
            assert!(
                !fixture
                    .run(&[
                        "config",
                        "set",
                        "--config-dir",
                        state.to_str().unwrap(),
                        "--key",
                        key,
                        "--value",
                        invalid,
                        "--apply"
                    ])
                    .status
                    .success()
            );
            assert_eq!(fs::read(&path).unwrap(), before);
        }
        let args = [
            "config",
            "set",
            "--config-dir",
            state.to_str().unwrap(),
            "--key",
            key,
            "--value",
            value,
        ];
        let preview = fixture.run(&args);
        assert!(
            preview.status.success(),
            "{}",
            String::from_utf8_lossy(&preview.stderr)
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        let applied = fixture.run(&[args.as_slice(), &["--apply"]].concat());
        assert!(
            applied.status.success(),
            "{}",
            String::from_utf8_lossy(&applied.stderr)
        );
        let receipt: Value = serde_json::from_slice(&applied.stdout).unwrap();
        assert_eq!(
            fs::read(receipt["result"]["backup"].as_str().unwrap()).unwrap(),
            before
        );
        let saved = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.ui.value(key).unwrap(), value.parse::<u64>().unwrap());
    }
    let show = fixture.run(&["config", "show", "--config-dir", state.to_str().unwrap()]);
    assert!(show.status.success());
    let shown = String::from_utf8(show.stdout).unwrap();
    assert!(shown.contains("shortcut_query_limit_chars") && shown.contains("choice_wheel_items"));
}

#[test]
fn viewer_history_resource_controls_preserve_known_ids_and_saved_bytes_in_the_real_backend() {
    use daedalus_tui::app::config::ControlConfig;
    use std::os::unix::fs::PermissionsExt;
    let panes:Vec<_>=["source","viewer"].into_iter().map(|id|json!({"pane_id":id,"terminal_id":format!("generation-{id}"),"workspace_id":"work","tab_id":"tab"})).collect();
    let f = observation_backend_fixture(json!({"result":{"panes":panes}}));
    let path = f.0.join("state/control.json");
    let history_path = f.0.join("state/views.json");
    let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    let id = |pane| json!({"backend":"herdr","endpoint":"fixture","pane":pane,"generation":format!("generation-{pane}")});
    for mode in 0..6 {
        let mut row = json!({"version":2,"source":id("source"),"viewer":id("viewer"),"source_label":"Fixture source","mechanism":"x".repeat(5000)});
        config.runtime.discovery_metadata_limit_bytes = if mode == 0 { 4096 } else { 2097152 };
        config.runtime.terminal_view_history_limit_bytes = if mode == 2 { 4096 } else { 8192 };
        config.runtime.terminal_view_history_value_limit = if mode == 4 { 128 } else { 1024 };
        if mode >= 4 {
            row["mechanism"] = json!("Fixture");
            row["extra"] = json!(vec![0; 200]);
        }
        let history = serde_json::to_vec(&json!({"version":2,"views":[row]})).unwrap();
        fs::write(&path, config.encode().unwrap()).unwrap();
        let saved = fs::read(&path).unwrap();
        fs::write(&history_path, &history).unwrap();
        fs::set_permissions(&history_path, fs::Permissions::from_mode(0o600)).unwrap();
        let mut child = observation_backend(&f).spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"{\"version\":2,\"op\":\"quit\"}\n")
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let frame: Value = serde_json::from_str(
            String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        let limited = mode % 2 == 0;
        assert_eq!(frame["terminals"], 2);
        assert_eq!(frame["relationship_limited"] == true, limited);
        let panes: Vec<_> = frame["tree"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["kind"] == "pane")
            .collect();
        assert_eq!(panes.len(), 2);
        for pane in panes {
            let key: Vec<Value> = serde_json::from_str(pane["id"].as_str().unwrap()).unwrap();
            let native = key[key.len() - 2].as_str().unwrap();
            assert_eq!(key[key.len() - 1], format!("generation-{native}"));
            assert_eq!(
                pane["tile"]["views"],
                if native == "source" && !limited { 2 } else { 1 }
            );
            assert_eq!(pane["tile"]["viewer"], native == "viewer" && !limited);
        }
        assert_eq!(fs::read(&history_path).unwrap(), history);
        assert_eq!(fs::read(&path).unwrap(), saved);
    }
    assert!(!f.0.join("state/telegram-service.json").exists());
}
#[test]
fn repeated_and_conflicting_observations_reach_real_backend_refusal_and_recovery() {
    use std::io::{BufRead, BufReader, Read};
    let first = json!({"pane_id":"one","terminal_id":"generation-one","workspace_id":"work","tab_id":"tab","agent":"codex","agent_session":{"agent":"codex","kind":"id","value":"one"}});
    let healthy = json!({"pane_id":"healthy","terminal_id":"generation-healthy","workspace_id":"work","tab_id":"tab"});
    let data = |conflict| {
        let mut second = first.clone();
        if conflict {
            second["agent_session"]["value"] = json!("two");
        }
        json!({"result":{"panes":[first.clone(),second,healthy.clone()],"layouts":[{"workspace_id":"work","tab_id":"tab","area":{"width":100,"height":24},"panes":[{"pane_id":"one","rect":{"x":0,"y":0,"width":49,"height":24}},{"pane_id":"healthy","rect":{"x":50,"y":0,"width":50,"height":24}}]}]}})
    };
    let f = observation_backend_fixture(data(true));
    let path = f.0.join("state/control.json");
    let before = fs::read(&path).unwrap();
    for conflict in [true, false] {
        fs::write(
            f.0.join("payload"),
            serde_json::to_vec(&data(conflict)).unwrap(),
        )
        .unwrap();
        let mut child = observation_backend(&f).spawn().unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        let frame: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(frame["terminals"], 2);
        assert_eq!(frame["views"], 2);
        let panes: Vec<_> = frame["tree"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["kind"] == "pane")
            .collect();
        assert_eq!(panes.len(), 2);
        let one = panes
            .iter()
            .find(|row| row["id"].as_str().unwrap().contains("generation-one"))
            .unwrap();
        let good = panes
            .iter()
            .find(|row| row["id"].as_str().unwrap().contains("generation-healthy"))
            .unwrap();
        assert_eq!(one["actionable"], !conflict);
        assert_eq!(good["actionable"], true);
        assert_eq!(one["tile"]["views"], 1);
        let group = frame["tree"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == one["parent"])
            .unwrap();
        assert_eq!(group["layout"].is_null(), conflict);
        assert_eq!(frame["observation_warnings"].is_array(), conflict);
        let mut stdin = child.stdin.take().unwrap();
        if conflict {
            writeln!(
                stdin,
                "{}",
                json!({"version":2,"op":"previews","nodes":[one["id"]],"rows":24})
            )
            .unwrap();
            writeln!(stdin,"{}",json!({"version":2,"op":"action","page":1,"action":"terminal.resize","node":one["id"]})).unwrap();
        }
        writeln!(stdin, "{{\"version\":2,\"op\":\"quit\"}}").unwrap();
        drop(stdin);
        let mut output = String::new();
        reader.read_to_string(&mut output).unwrap();
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(child.wait().unwrap().success(), "{stderr}");
        if conflict {
            let frames: Vec<Value> = output
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert!(frames.iter().any(|frame| {
                frame["kind"] == "previews"
                    && frame["items"][0]["error"]
                        .as_str()
                        .unwrap_or("")
                        .contains("Conflicting terminal observations")
            }));
            assert!(frames.iter().any(|frame| {
                frame["kind"] == "error"
                    && frame["message"]
                        .as_str()
                        .unwrap_or("")
                        .contains("Conflicting terminal observations")
            }));
        }
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    let queries = fs::read_to_string(f.0.join("queries")).unwrap();
    assert!(
        !queries.contains("pane read")
            && !queries.contains("pane resize")
            && !queries.contains("send-keys")
    );
    assert!(!f.0.join("state/telegram-service.json").exists());
}
#[test]
fn checkpoint_tree_mode_is_persistent_and_real_backend_keeps_saved_profile_records() {
    use daedalus_tui::{adapters::terminals, app::config::ControlConfig};
    use std::os::unix::fs::PermissionsExt;
    let pane = json!({"pane_id":"pane","terminal_id":"generation","generation":"extra","workspace_id":"work","tab_id":"tab","agent":"codex","agent_session":{"agent":"codex","kind":"id","value":"same-native-id"}});
    let f = observation_backend_fixture(json!({"result":{"panes":[pane.clone()]}}));
    let state = f.0.join("state");
    let path = state.join("control.json");
    let live = json!({"backend":"herdr","server":"fixture","session":"fixture","pane_id":"pane","terminal_id":"generation","generation":"extra"});
    let source = terminals::reference(&live).unwrap();
    let records:Vec<_>=["profile-one","profile-two"].iter().enumerate().map(|(i,profile)|json!({"name":profile,"harness":"codex","sid":"same-native-id","cwd":f.0,"profile":f.0.join(profile),"created_at":"2026-01-01T00:00:00Z","source":if i==0{serde_json::to_value(&source).unwrap()}else{Value::Null}})).collect();
    let registry = state.join("dashboard-checkpoints.json");
    fs::write(
        &registry,
        serde_json::to_vec(&json!({"checkpoints":records})).unwrap(),
    )
    .unwrap();
    fs::set_permissions(&registry, fs::Permissions::from_mode(0o600)).unwrap();
    let records_before = fs::read(&registry).unwrap();
    let initial = fs::read(&path).unwrap();
    assert!(
        !f.run(&[
            "config",
            "set",
            "--config-dir",
            state.to_str().unwrap(),
            "--key",
            "ui.checkpoint_matches",
            "--value",
            "unknown",
            "--apply"
        ])
        .status
        .success()
    );
    assert_eq!(fs::read(&path).unwrap(), initial);
    for (mode, count) in [("all", 2), ("source", 1), ("off", 0)] {
        let args = [
            "config",
            "set",
            "--config-dir",
            state.to_str().unwrap(),
            "--key",
            "ui.checkpoint_matches",
            "--value",
            mode,
        ];
        let before = fs::read(&path).unwrap();
        assert!(f.run(&args).status.success());
        assert_eq!(fs::read(&path).unwrap(), before);
        let applied = f.run(&[args.as_slice(), &["--apply"]].concat());
        assert!(
            applied.status.success(),
            "{}",
            String::from_utf8_lossy(&applied.stderr)
        );
        let receipt: Value = serde_json::from_slice(&applied.stdout).unwrap();
        let saved = fs::read(&path).unwrap();
        if saved != before {
            assert_eq!(
                fs::read(receipt["result"]["backup"].as_str().unwrap()).unwrap(),
                before
            );
        } else {
            assert!(
                receipt["result"]["backup"].is_null(),
                "A default all-mode no-op needs no backup"
            );
        }
        assert_eq!(
            serde_json::to_value(ControlConfig::parse(&saved).unwrap().ui).unwrap()["checkpoint_matches"],
            mode
        );
        let mut child = observation_backend(&f).spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"{\"version\":2,\"op\":\"quit\"}\n")
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let frame: Value = serde_json::from_str(
            String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(frame["checkpoints"], 2);
        let matches: Vec<_> = frame["tree"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["kind"] == "checkpoint")
            .collect();
        assert_eq!(matches.len(), count);
        for row in matches {
            let fields = row["fields"].as_array().unwrap();
            assert!(fields.iter().any(|field| field[0] == "Saved profile"));
            assert!(fields.iter().any(
                |field| field[0] == "Link" && field[1].as_str().unwrap().contains("unverified")
            ));
        }
        assert_eq!(fs::read(&path).unwrap(), saved);
        assert_eq!(fs::read(&registry).unwrap(), records_before);
    }
    assert!(!state.join("telegram-service.json").exists());
}

#[test]
fn viewer_identity_recovery_uses_the_real_backend_without_mutating_saved_history() {
    use daedalus_tui::app::config::ControlConfig;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path.parent().unwrap())?;
        fs::write(path, bytes)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
    }
    let f = Fixture::new();
    let directory = f.0.join("state");
    let state = directory.to_str().unwrap();
    assert!(
        f.run(&[
            "config",
            "init",
            "--config-dir",
            state,
            "--workspace",
            f.0.to_str().unwrap(),
            "--apply"
        ])
        .status
        .success()
    );
    let path = directory.join("control.json");
    let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    config.discovery.automatic = false;
    config.discovery.herdr_sessions = vec!["fixture".into()];
    fs::write(&path, config.encode().unwrap()).unwrap();
    let config_before = fs::read(&path).unwrap();
    write_private(
        &f.0.join(".config/daedalus/dashboard.json"),
        br#"{"discovery":{"scan_processes":false,"scan_default_roots":false}}"#,
    )
    .unwrap();
    let panes:Vec<_>=[("a/b","c"),("a","b/c"),("viewer","viewer-generation")].into_iter().map(|(pane,generation)|json!({"pane_id":pane,"terminal_id":generation,"workspace_id":"work","tab_id":"tab"})).collect();
    let payload = f.0.join("payload");
    fs::write(
        &payload,
        serde_json::to_vec(&json!({"result":{"panes":panes}})).unwrap(),
    )
    .unwrap();
    let executable = f.0.join("inert-herdr");
    fs::write(
        &executable,
        "#!/bin/sh\nexec /bin/cat \"$FIXTURE_PAYLOAD\"\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let id = |pane: &str, generation: &str| json!({"backend":"herdr","endpoint":"fixture","pane":pane,"generation":generation});
    let valid = json!({"version":2,"source":id("a/b","c"),"viewer":id("viewer","viewer-generation"),"source_label":"Fixture source"});
    let mut conflict = valid.clone();
    conflict["source"] = id("a", "b/c");
    let legacy = json!({"source":"herdr/fixture/a/b/c","backend":"herdr","server":"fixture","pane_id":"viewer","viewer_terminal":"viewer-generation"});
    for (registry, limited, count) in [
        (json!({"views":[legacy]}), true, 1),
        (json!({"views":[valid.clone()]}), false, 2),
        (json!({"views":[valid, conflict]}), true, 1),
        (json!({"version":3,"views":[]}), true, 1),
    ] {
        let history = serde_json::to_vec(&registry).unwrap();
        write_private(&directory.join("views.json"), &history).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_daedalus-tui"))
            .env_clear()
            .env("PATH", "/nonexistent")
            .env("HOME", &f.0)
            .env("DAEDALUS_HERDR_BIN", &executable)
            .env("FIXTURE_PAYLOAD", &payload)
            .current_dir(&f.0)
            .args(["backend", "--stdio", "--config-dir", state])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"{\"version\":2,\"op\":\"action\",\"page\":6,\"action\":\"activity.discovery\"}\n{\"version\":2,\"op\":\"quit\"}\n").unwrap();
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
        let frame = &frames[0];
        assert_eq!(frame["kind"], "state");
        assert_eq!(frame["terminals"], 3);
        assert_eq!(frame["relationship_limited"] == true, limited);
        let rows: Vec<_> = frame["tree"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["kind"] == "pane")
            .collect();
        assert_eq!(rows.len(), 3);
        for row in rows {
            let key: Vec<Value> = serde_json::from_str(row["id"].as_str().unwrap()).unwrap();
            let pane = &key[key.len() - 2];
            assert_eq!(row["tile"]["views"], if pane == "a/b" { count } else { 1 });
            assert_eq!(row["tile"]["viewer"], !limited && pane == "viewer");
            let views = row["fields"]
                .as_array()
                .unwrap()
                .iter()
                .find(|field| field[0] == "Views")
                .unwrap();
            assert_eq!(
                views[1].as_str().unwrap().contains("details incomplete"),
                limited
            );
        }
        assert_eq!(
            frames.iter().any(|frame| frame["text"]
                .as_str()
                .is_some_and(|text| text.contains(
                    "Saved viewer records contain invalid, ambiguous or unsupported identities"
                ) || text.contains("Saved view identities could not be read"))),
            limited
        );
        assert_eq!(fs::read(directory.join("views.json")).unwrap(), history);
        assert_eq!(fs::read(&path).unwrap(), config_before);
    }
    assert!(!directory.join("telegram-service.json").exists());
}

#[test]
fn saved_resize_policy_reaches_real_backend_admission_and_submits_once_to_an_inert_adapter() {
    use daedalus_tui::app::config::ControlConfig;
    use std::{
        io::{BufRead, BufReader, Read},
        os::unix::fs::{DirBuilderExt, PermissionsExt},
    };
    let f = Fixture::new();
    let directory = f.0.join("state");
    let state = directory.to_str().unwrap();
    assert!(
        f.run(&[
            "config",
            "init",
            "--config-dir",
            state,
            "--workspace",
            f.0.to_str().unwrap(),
            "--apply"
        ])
        .status
        .success()
    );
    let path = directory.join("control.json");
    let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    config.discovery.automatic = false;
    config.discovery.herdr_sessions = vec!["fixture".into()];
    let preferences = f.0.join(".config/daedalus");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&preferences)
        .unwrap();
    fs::write(
        preferences.join("dashboard.json"),
        br#"{"discovery":{"scan_processes":false,"scan_default_roots":false}}"#,
    )
    .unwrap();
    fs::set_permissions(
        preferences.join("dashboard.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let left = json!({"pane_id":"left","terminal_id":"g-left","workspace_id":"work","tab_id":"tab","pid":1});
    let right = json!({"pane_id":"right","terminal_id":"g-right","workspace_id":"work","tab_id":"tab","pid":1});
    let payload = f.0.join("payload");
    let layout = json!({"workspace_id":"work","tab_id":"tab","area":{"x":0,"y":0,"width":100,"height":40},"zoomed":false,"panes":[{"pane_id":"left","rect":{"x":0,"y":0,"width":50,"height":40}},{"pane_id":"right","rect":{"x":50,"y":0,"width":50,"height":40}}],"splits":[{"id":"root","direction":"right","ratio":0.5,"rect":{"x":0,"y":0,"width":100,"height":40}}]});
    fs::write(
        &payload,
        serde_json::to_vec(
            &json!({"result":{"panes":[left.clone(),right.clone()],"layouts":[layout.clone()]}}),
        )
        .unwrap(),
    )
    .unwrap();
    // Independently specified native reply: two cells move50/50 to52/48.
    let mut after = layout;
    after["panes"][0]["rect"]["width"] = json!(52);
    after["panes"][1]["rect"]["x"] = json!(52);
    after["panes"][1]["rect"]["width"] = json!(48);
    after["splits"][0]["ratio"] = json!(0.52);
    let reply = f.0.join("resize-reply");
    fs::write(&reply,serde_json::to_vec(&json!({"result":{"type":"pane_resize","resize":{"pane_id":"left","changed":true,"layout":after}}})).unwrap()).unwrap();
    let left_file = f.0.join("left");
    let right_file = f.0.join("right");
    fs::write(
        &left_file,
        serde_json::to_vec(&json!({"result":{"pane":left}})).unwrap(),
    )
    .unwrap();
    fs::write(
        &right_file,
        serde_json::to_vec(&json!({"result":{"pane":right}})).unwrap(),
    )
    .unwrap();
    let effects = f.0.join("effects");
    let executable = f.0.join("inert-herdr");
    fs::write(&executable,"#!/bin/sh\ncase \"$3 $4\" in\n 'api snapshot') exec /bin/cat \"$FIXTURE_PAYLOAD\";;\n 'pane get') if [ \"$5\" = left ]; then exec /bin/cat \"$FIXTURE_LEFT\"; else exec /bin/cat \"$FIXTURE_RIGHT\"; fi;;\n 'pane resize') printf '%s\\n' \"$*\" >>\"$FIXTURE_EFFECTS\"; exec /bin/cat \"$FIXTURE_REPLY\";;\n *) exit 1;;\nesac\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    for limit in [1, 3] {
        config.runtime.terminal_resize_max_cells = limit;
        fs::write(&path, config.encode().unwrap()).unwrap();
        let before = fs::read(&path).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_daedalus-tui"))
            .env_clear()
            .env("PATH", "/nonexistent")
            .env("HOME", &f.0)
            .env("DAEDALUS_HERDR_BIN", &executable)
            .env("FIXTURE_PAYLOAD", &payload)
            .env("FIXTURE_LEFT", &left_file)
            .env("FIXTURE_RIGHT", &right_file)
            .env("FIXTURE_EFFECTS", &effects)
            .env("FIXTURE_REPLY", &reply)
            .current_dir(&f.0)
            .args(["backend", "--stdio", "--config-dir", state])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        let frame: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(frame["kind"], "state");
        assert_eq!(frame["resize_settings"]["max_cells"], limit);
        let panes: Vec<_> = frame["tree"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["kind"] == "pane")
            .collect();
        assert_eq!(panes.len(), 2);
        let a = panes
            .iter()
            .find(|row| row["id"].as_str().unwrap().contains("g-left"))
            .unwrap();
        let b = panes
            .iter()
            .find(|row| row["id"].as_str().unwrap().contains("g-right"))
            .unwrap();
        let parent = frame["tree"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == a["parent"])
            .unwrap();
        let request = json!({"version":2,"op":"action","page":1,"action":"terminal.resize","node":a["id"],"resize":{"peer":b["id"],"axis":"horizontal","cells":2,"layout":parent["layout"]}});
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "{request}").unwrap();
        writeln!(stdin, "{{\"version\":2,\"op\":\"quit\"}}").unwrap();
        drop(stdin);
        let mut rest = String::new();
        reader.read_to_string(&mut rest).unwrap();
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(child.wait().unwrap().success(), "{stderr}");
        let frames: Vec<Value> = rest
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        if limit == 1 {
            assert!(!effects.exists());
            assert!(frames.iter().any(|frame| {
                frame["message"]
                    .as_str()
                    .is_some_and(|text| text.contains("Invalid resize"))
            }));
        } else {
            let log = fs::read_to_string(&effects).unwrap();
            assert_eq!(log.lines().count(), 1);
            assert!(log.contains("pane resize --pane left --direction right"));
            assert!(
                frames
                    .iter()
                    .any(|frame| frame["message"] == "Pane divider resized")
            );
        }
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    assert!(!directory.join("telegram-service.json").exists());
}
#[test]
fn relationship_settings_limit_real_backend_details_and_preserve_all_terminals() {
    use daedalus_tui::app::config::ControlConfig;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let f = Fixture::new();
    let directory = f.0.join("state");
    let state = directory.to_str().unwrap();
    assert!(
        f.run(&[
            "config",
            "init",
            "--config-dir",
            state,
            "--workspace",
            f.0.to_str().unwrap(),
            "--apply"
        ])
        .status
        .success()
    );
    let path = directory.join("control.json");
    let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    config.discovery.automatic = false;
    config.discovery.herdr_sessions = vec!["fixture".into()];
    let preferences = f.0.join(".config/daedalus");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&preferences)
        .unwrap();
    fs::write(
        preferences.join("dashboard.json"),
        br#"{"discovery":{"scan_processes":false,"scan_default_roots":false}}"#,
    )
    .unwrap();
    fs::set_permissions(
        preferences.join("dashboard.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let payload = f.0.join("payload");
    let panes: Vec<_> = (0..100)
        .map(|n| json!({"pane_id":format!("p{n}"),"terminal_id":"generation"}))
        .collect();
    fs::write(
        &payload,
        serde_json::to_vec(&json!({"result":{"panes":panes}})).unwrap(),
    )
    .unwrap();
    let executable = f.0.join("inert-herdr");
    fs::write(
        &executable,
        "#!/bin/sh\nexec /bin/cat \"$FIXTURE_PAYLOAD\"\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let mut original_ids = None;
    for (entries, bytes, limited) in [(1, 2097152, true), (4096, 8192, false), (4096, 1024, true)] {
        config.runtime.discovery_relationship_limit = entries;
        config.runtime.discovery_relationship_limit_bytes = bytes;
        fs::write(&path, config.encode().unwrap()).unwrap();
        let before = fs::read(&path).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_daedalus-tui"))
            .env_clear()
            .env("PATH", "/nonexistent")
            .env("HOME", &f.0)
            .env("DAEDALUS_HERDR_BIN", &executable)
            .env("FIXTURE_PAYLOAD", &payload)
            .current_dir(&f.0)
            .args(["backend", "--stdio", "--config-dir", state])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"{\"version\":2,\"op\":\"action\",\"page\":6,\"action\":\"activity.discovery\"}\n{\"version\":2,\"op\":\"quit\"}\n").unwrap();
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
        let frame = frames
            .iter()
            .find(|frame| frame["kind"] == "state")
            .unwrap();
        assert_eq!(frame["relationship_limited"] == true, limited);
        assert_ne!(frame["display_limited"], true);
        assert_eq!(frame["terminals"], 100);
        assert_eq!(frame["views"], 100);
        assert_eq!(
            frame["freshness"].as_str().unwrap().starts_with("PARTIAL"),
            limited
        );
        let ids = frame["tree"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["kind"] == "pane")
            .map(|row| row["id"].as_str().unwrap().to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), 100);
        if let Some(original) = &original_ids {
            assert_eq!(&ids, original);
        } else {
            original_ids = Some(ids);
        }
        assert!(
            frame["catalog"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["id"] == "action:settings.runtime")
        );
        assert_eq!(
            frames.iter().any(|frame| frame["text"]
                .as_str()
                .is_some_and(|text| text.contains("Relationship display is incomplete"))),
            limited
        );
        assert!(!frames.iter().any(|frame| frame["kind"] == "error"));
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    assert!(!directory.join("delivery.sqlite3").exists());
    assert!(!directory.join("telegram-service.json").exists());
}
#[test]
fn whole_discovery_deadline_stops_owned_queries_without_accepting_late_results() {
    use daedalus_tui::app::config::ControlConfig;
    use std::{
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };
    let f = Fixture::new();
    let directory = f.0.join("state");
    let state = directory.to_str().unwrap();
    assert!(
        f.run(&[
            "config",
            "init",
            "--config-dir",
            state,
            "--workspace",
            f.0.to_str().unwrap(),
            "--apply"
        ])
        .status
        .success()
    );
    let path = directory.join("control.json");
    let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    config.discovery.automatic = false;
    config.discovery.herdr_sessions = vec!["b".into(), "a".into()];
    config.runtime.discovery_workers = 1;
    config.runtime.discovery_timeout_ms = 1000;
    config.runtime.terminal_query_timeout_ms = 5000;
    fs::write(&path, config.encode().unwrap()).unwrap();
    let executable = f.0.join("inert-herdr");
    let calls = f.0.join("calls");
    fs::write(&executable, "#!/bin/sh\nprintf '%s:start\\n' \"$2\" >>\"$FIXTURE_CALLS\"\n/bin/sleep 2\nprintf '{\"result\":{\"panes\":[{\"pane_id\":\"%s\",\"terminal_id\":\"generation\"}]}}\\n' \"$2\"\nprintf '%s:done\\n' \"$2\" >>\"$FIXTURE_CALLS\"\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let run = || {
        let output = Command::new(env!("CARGO_BIN_EXE_daedalus-tui"))
            .env_clear()
            .env("PATH", "/nonexistent")
            .env("HOME", &f.0)
            .env("DAEDALUS_HERDR_BIN", &executable)
            .env("FIXTURE_CALLS", &calls)
            .current_dir(&f.0)
            .args(["inventory", "--config-dir", state, "--json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let before = fs::read(&path).unwrap();
    let started = Instant::now();
    let partial = run();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(partial["panes"].as_array().unwrap().is_empty());
    assert!(partial["errors"].to_string().contains("deadline"));
    assert_eq!(fs::read_to_string(&calls).unwrap(), "a:start\n");
    assert_eq!(fs::read(&path).unwrap(), before);
    config.runtime.discovery_timeout_ms = 8000;
    fs::write(&path, config.encode().unwrap()).unwrap();
    fs::write(&calls, []).unwrap();
    let complete = run();
    assert_eq!(complete["panes"].as_array().unwrap().len(), 2);
    assert!(
        complete["errors"].as_array().unwrap().is_empty(),
        "{complete}"
    );
    assert_eq!(
        fs::read_to_string(&calls).unwrap(),
        "a:start\na:done\nb:start\nb:done\n"
    );
    assert!(!directory.join("delivery.sqlite3").exists());
}
#[test]
fn backend_limits_expanded_terminal_rows_and_keeps_recovery_settings_available() {
    use daedalus_tui::app::config::ControlConfig;
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let directory = f.0.join("state");
    let state = directory.to_str().unwrap();
    assert!(
        f.run(&[
            "config",
            "init",
            "--config-dir",
            state,
            "--workspace",
            f.0.to_str().unwrap(),
            "--apply"
        ])
        .status
        .success()
    );
    let path = directory.join("control.json");
    let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    config.discovery.automatic = false;
    config.discovery.herdr_sessions = vec!["fixture".into()];
    config.runtime.discovery_frame_limit_bytes = 1048576;
    config.runtime.discovery_metadata_limit_bytes = 4194304;
    config.runtime.discovery_metadata_value_limit = 250000;
    fs::write(&path, config.encode().unwrap()).unwrap();
    let before = fs::read(&path).unwrap();
    let preferences = directory.join("dashboard.json");
    fs::write(
        &preferences,
        br#"{"discovery":{"scan_processes":false,"scan_default_roots":false}}"#,
    )
    .unwrap();
    fs::set_permissions(&preferences, fs::Permissions::from_mode(0o600)).unwrap();
    let payload = f.0.join("payload");
    let panes: Vec<_> = (0..5000)
        .map(|n| json!({"pane_id":format!("p{n}"),"terminal_id":"generation","title":"fixture"}))
        .collect();
    fs::write(
        &payload,
        serde_json::to_vec(&json!({"result":{"panes":panes}})).unwrap(),
    )
    .unwrap();
    let executable = f.0.join("inert-herdr");
    fs::write(
        &executable,
        "#!/bin/sh\nexec /bin/cat \"$FIXTURE_PAYLOAD\"\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_daedalus-tui"))
        .env_clear()
        .env("PATH", "/nonexistent")
        .env("HOME", &f.0)
        .env("DAEDALUS_HERDR_BIN", executable)
        .env("FIXTURE_PAYLOAD", payload)
        .current_dir(&f.0)
        .args(["backend", "--stdio", "--config-dir", state])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"{\"version\":2,\"op\":\"action\",\"page\":6,\"action\":\"activity.discovery\"}\n{\"version\":2,\"op\":\"quit\"}\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let frames = stdout
        .lines()
        .map(|line| {
            let frame: Value = serde_json::from_str(line).unwrap();
            if frame["kind"] == "state" {
                assert!(line.len() <= 1048576);
            }
            frame
        })
        .collect::<Vec<_>>();
    let frame = frames
        .iter()
        .find(|frame| frame["kind"] == "state")
        .unwrap();
    assert_eq!(frame["terminals"], 5000);
    assert_eq!(frame["views"], 5000);
    assert_eq!(frame["display_limited"], true);
    assert!(frame["freshness"].as_str().unwrap().starts_with("PARTIAL"));
    assert!(
        frame["catalog"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == "action:settings.runtime")
    );
    let mut seen = std::collections::BTreeSet::new();
    for row in frame["tree"].as_array().unwrap() {
        let parent = row["parent"].as_str().unwrap();
        assert!(parent.is_empty() || seen.contains(parent));
        seen.insert(row["id"].as_str().unwrap());
    }
    assert!(frames.iter().any(|frame| {
        frame["text"]
            .as_str()
            .is_some_and(|text| text.contains("Terminal display is incomplete"))
    }));
    assert!(!frames.iter().any(|frame| frame["kind"] == "error"));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(!directory.join("delivery.sqlite3").exists());
}
#[test]
fn oversized_discovery_remains_a_readable_partial_workspace_inventory() {
    use daedalus_tui::app::config::ControlConfig;
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let directory = f.0.join("state");
    let state = directory.to_str().unwrap();
    assert!(
        f.run(&[
            "config",
            "init",
            "--config-dir",
            state,
            "--workspace",
            f.0.to_str().unwrap(),
            "--apply"
        ])
        .status
        .success()
    );
    let path = directory.join("control.json");
    let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    config.discovery.automatic = false;
    config.discovery.herdr_sessions = vec!["fixture".into()];
    fs::write(&path, config.encode().unwrap()).unwrap();
    let before = fs::read(&path).unwrap();
    let payload = f.0.join("payload");
    let panes: Vec<_> = (0..3000).map(|n| json!({"pane_id":format!("p{n}"),"terminal_id":"generation","title":"x".repeat(2048)})).collect();
    fs::write(
        &payload,
        serde_json::to_vec(&json!({"result":{"panes":panes}})).unwrap(),
    )
    .unwrap();
    let executable = f.0.join("inert-herdr");
    fs::write(
        &executable,
        "#!/bin/sh\nexec /bin/cat \"$FIXTURE_PAYLOAD\"\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_daedalus-tui"))
        .env_clear()
        .env("PATH", "/nonexistent")
        .env("HOME", &f.0)
        .env("DAEDALUS_HERDR_BIN", executable)
        .env("FIXTURE_PAYLOAD", payload)
        .current_dir(&f.0)
        .args(["workspace-usage", "--config-dir", state, "--json"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "large discovery must remain readable and explicitly partial: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(result["discovery_complete"], false);
    assert!(result["discovery_errors"].as_u64().unwrap() > 0);
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(!directory.join("workspace-usage.sqlite3").exists());
}
#[test]
fn discovery_settings_reach_real_cli_queries_and_partial_usage_stays_incomplete() {
    use daedalus_tui::app::config::ControlConfig;
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let directory = f.0.join("state");
    let state = directory.to_str().unwrap();
    assert!(
        f.run(&[
            "config",
            "init",
            "--config-dir",
            state,
            "--workspace",
            f.0.to_str().unwrap(),
            "--apply"
        ])
        .status
        .success()
    );
    let path = directory.join("control.json");
    let shown = f.run(&["config", "show", "--config-dir", state]);
    let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
    let set = |key: &str, value: &str| {
        assert!(
            shown["runtime_settings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field["key"] == key)
        );
        let qualified = format!("runtime.{key}");
        let args = [
            "config",
            "set",
            "--config-dir",
            state,
            "--key",
            &qualified,
            "--value",
            value,
        ];
        let before = fs::read(&path).unwrap();
        assert!(f.run(&args).status.success());
        assert_eq!(fs::read(&path).unwrap(), before);
        let applied = f.run(&[args.as_slice(), &["--apply"]].concat());
        assert!(
            applied.status.success(),
            "{}",
            String::from_utf8_lossy(&applied.stderr)
        );
        let receipt: Value = serde_json::from_slice(&applied.stdout).unwrap();
        assert_eq!(
            fs::read(receipt["result"]["backup"].as_str().unwrap()).unwrap(),
            before
        );
    };
    for (key, value) in [
        ("workspace_usage_max_gap_ms", "1000"),
        ("workspace_usage_lease_ms", "2000"),
        ("workspace_usage_retention_days", "3"),
        ("workspace_usage_entry_limit", "256"),
        ("workspace_usage_work_limit_bytes", "32768"),
        ("workspace_usage_timeout_ms", "1000"),
        ("workspace_usage_sql_step_limit", "200000"),
        ("workspace_usage_database_limit_bytes", "1048576"),
        ("workspace_usage_output_limit_bytes", "32768"),
        ("workspace_usage_busy_wait_ms", "10"),
        ("native_command_argument_limit_bytes", "4096"),
        ("native_command_model_limit_bytes", "256"),
        ("native_metadata_file_limit_bytes", "2097152"),
        ("native_metadata_work_limit_bytes", "33554432"),
        ("native_metadata_entry_limit", "1234"),
        ("native_metadata_value_limit", "524288"),
        ("native_metadata_header_limit_bytes", "32768"),
        ("native_metadata_timeout_ms", "3000"),
        ("native_metadata_process_limit", "256"),
        ("native_metadata_process_scan_limit", "131072"),
        ("native_metadata_depth_limit", "64"),
        ("discovery_workers", "1"),
        ("discovery_endpoint_limit", "2"),
        ("discovery_socket_scan_limit", "1"),
        ("terminal_query_limit_bytes", "1024"),
        ("discovery_timeout_ms", "2500"),
        ("discovery_metadata_limit_bytes", "131072"),
        ("discovery_metadata_value_limit", "200000"),
        ("discovery_frame_limit_bytes", "2097152"),
        ("discovery_relationship_limit", "4096"),
        ("discovery_relationship_limit_bytes", "131072"),
        ("discovery_projection_timeout_ms", "1000"),
        ("discovery_layout_pane_limit", "128"),
        ("discovery_layout_limit_bytes", "65536"),
        ("discovery_layout_timeout_ms", "1000"),
        ("terminal_resize_pane_limit", "256"),
        ("terminal_resize_max_cells", "64"),
        ("terminal_resize_min_cols", "6"),
        ("terminal_resize_min_rows", "4"),
        ("terminal_view_history_limit", "25"),
        ("terminal_view_history_limit_bytes", "16384"),
        ("terminal_view_history_value_limit", "2048"),
        ("terminal_view_history_timeout_ms", "1500"),
        ("instance_import_limit_bytes", "1048576"),
        ("instance_import_entry_limit", "128"),
        ("instance_import_json_value_limit", "2048"),
        ("instance_import_work_limit_bytes", "2097152"),
        ("instance_import_timeout_ms", "10000"),
        ("instance_import_attachment_limit", "16"),
        ("instance_import_attachment_limit_bytes", "65536"),
        ("instance_import_frame_limit_bytes", "8192"),
        ("native_scrollback_rows", "64"),
        ("native_discovery_entry_limit", "16"),
        ("native_discovery_timeout_ms", "500"),
        ("native_discovery_rpc_timeout_ms", "200"),
        ("external_discovery_entry_limit", "128"),
        ("external_discovery_timeout_ms", "500"),
        ("external_discovery_query_limit", "32"),
    ] {
        set(key, value);
    }
    let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        config.runtime.workspace_usage_policy(),
        daedalus_tui::core::usage_budget::Policy {
            max_gap_ms: 1000,
            lease_ms: 2000,
            retention_days: 3,
            entries: 256,
            work_bytes: 32768,
            timeout_ms: 1000,
            sql_steps: 200000,
            database_bytes: 1048576,
            output_bytes: 32768,
            busy_ms: 10
        }
    );
    assert_eq!(config.runtime.native_command_limits().argument_bytes, 4096);
    assert_eq!(config.runtime.native_command_limits().model_bytes, 256);
    assert_eq!(config.runtime.native_metadata_file_limit_bytes, 2097152);
    assert_eq!(config.runtime.native_metadata_work_limit_bytes, 33554432);
    assert_eq!(config.runtime.native_metadata_entry_limit, 1234);
    assert_eq!(config.runtime.native_metadata_value_limit, 524288);
    assert_eq!(config.runtime.native_metadata_header_limit_bytes, 32768);
    assert_eq!(config.runtime.native_metadata_timeout_ms, 3000);
    assert_eq!(config.runtime.native_metadata_process_limit, 256);
    assert_eq!(config.runtime.native_metadata_process_scan_limit, 131072);
    assert_eq!(config.runtime.native_metadata_depth_limit, 64);
    config.discovery.automatic = false;
    config.discovery.herdr_sessions = vec!["c".into(), "a".into(), "b".into()];
    // config init already created this private regular file.
    fs::write(&path, config.encode().unwrap()).unwrap();
    let queries = f.0.join("queries");
    let large = f.0.join("large");
    let executable = f.0.join("inert-herdr");
    fs::write(&executable, "#!/bin/sh\nprintf '%s\\n' \"$2\" >> \"$FIXTURE_QUERY_LOG\"\nprintf '{\"result\":{\"panes\":[{\"pane_id\":\"pane\",\"terminal_id\":\"generation\"}],\"padding\":\"'\nif [ -f \"$FIXTURE_LARGE_OUTPUT\" ]; then printf '%2048s' ''; fi\nprintf '\"}}\\n'\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let run = |usage: bool| {
        let result = Command::new(env!("CARGO_BIN_EXE_daedalus-tui"))
            .env_clear()
            .env("PATH", "/nonexistent")
            .env("HOME", &f.0)
            .env("DAEDALUS_HERDR_BIN", &executable)
            .env("FIXTURE_QUERY_LOG", &queries)
            .env("FIXTURE_LARGE_OUTPUT", &large)
            .current_dir(&f.0)
            .args([
                if usage {
                    "workspace-usage"
                } else {
                    "inventory"
                },
                "--config-dir",
                state,
                "--json",
            ])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        serde_json::from_slice::<Value>(&result.stdout).unwrap()
    };
    let partial = run(false);
    assert_eq!(partial["herdr_sessions"], json!(["a", "b"]));
    assert_eq!(fs::read_to_string(&queries).unwrap(), "a\nb\n");
    assert_eq!(partial["errors"].as_array().unwrap().len(), 1);
    assert!(
        partial["errors"][0]["error"]
            .as_str()
            .unwrap()
            .contains("endpoint limit reached")
    );
    assert_eq!(run(true)["discovery_complete"], false);
    assert!(!directory.join("workspace-usage.sqlite3").exists());
    set("discovery_endpoint_limit", "3");
    fs::write(&large, b"fixture").unwrap();
    let refused = run(false);
    assert_eq!(refused["panes"], json!([]));
    assert_eq!(refused["errors"].as_array().unwrap().len(), 3);
    assert!(
        refused["errors"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["error"].as_str().unwrap().contains("InvalidData"))
    );
    set("terminal_query_limit_bytes", "4096");
    let complete = run(false);
    assert_eq!(complete["herdr_sessions"], json!(["a", "b", "c"]));
    assert_eq!(complete["errors"], json!([]));
    assert_eq!(complete["panes"].as_array().unwrap().len(), 3);
    let saved = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved.runtime.discovery_workers, 1);
    assert_eq!(saved.runtime.discovery_socket_scan_limit, 1);
    assert_eq!(saved.runtime.terminal_query_limit_bytes, 4096);
    assert!(!directory.join("telegram-service.json").exists());
    assert!(!directory.join("delivery.sqlite3").exists());
}

#[test]
fn instance_settings_are_complete_reviewed_private_and_strict_in_the_packaged_cli() {
    use daedalus_tui::app::{config::ControlConfig, control_settings::FIELDS};
    let f = Fixture::new();
    let directory = f.0.join("state");
    let state = directory.to_str().unwrap();
    assert!(
        f.run(&[
            "config",
            "init",
            "--config-dir",
            state,
            "--workspace",
            f.0.to_str().unwrap(),
            "--apply"
        ])
        .status
        .success()
    );
    let workspace = f.0.join("second workspace");
    fs::create_dir(&workspace).unwrap();
    let path = directory.join("control.json");
    let show = f.run(&["config", "show", "--config-dir", state]);
    let metadata: Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(
        metadata["instance_settings"].as_array().unwrap().len(),
        FIELDS.len()
    );
    let socket = f.0.join("tmux, socket").to_str().unwrap().to_owned();
    let other = f.0.join("second socket").to_str().unwrap().to_owned();
    let endpoint = f.0.join("m, s").to_str().unwrap().to_owned();
    let values = [
        ("agent_name", "configured-instance".to_owned()),
        ("agent_home", workspace.to_str().unwrap().to_owned()),
        ("subscriptions", json!(["codex", "gateway"]).to_string()),
        ("tmux_sockets", json!([socket, other]).to_string()),
        ("discovery.automatic", "false".into()),
        ("discovery.native", "true".into()),
        (
            "discovery.herdr_sessions",
            json!(["example", "offline"]).to_string(),
        ),
        (
            "discovery.herdr_sockets",
            json!({"example":endpoint}).to_string(),
        ),
        ("telegram.owner_id", "7".into()),
        ("telegram.bot_id", "123".into()),
        ("telegram.username", "unset".into()),
    ];
    for (key, value) in values {
        assert!(
            metadata["instance_settings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field["key"] == key)
        );
        let args = [
            "config",
            "set",
            "--config-dir",
            state,
            "--key",
            key,
            "--value",
            value.as_str(),
        ];
        let before = fs::read(&path).unwrap();
        let preview = f.run(&args);
        assert!(
            preview.status.success(),
            "{}",
            String::from_utf8_lossy(&preview.stderr)
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        let applied = f.run(&[args.as_slice(), &["--apply"]].concat());
        assert!(
            applied.status.success(),
            "{}",
            String::from_utf8_lossy(&applied.stderr)
        );
        let receipt: Value = serde_json::from_slice(&applied.stdout).unwrap();
        assert_eq!(
            fs::read(receipt["result"]["backup"].as_str().unwrap()).unwrap(),
            before
        );
    }
    let saved = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved.tmux_sockets, vec![socket, other]);
    assert_eq!(saved.discovery.herdr_sockets["example"], endpoint);
    assert_eq!(saved.subscriptions, vec!["codex", "gateway"]);
    assert_eq!(saved.telegram.username.as_deref(), Some("unset"));
    for (key, value) in [
        ("subscriptions", r#"["codex","codex"]"#),
        (
            "discovery.herdr_sockets",
            r#"{"example":"/first","example":"/second"}"#,
        ),
        ("discovery.herdr_sessions", r#"["../bad"]"#),
        ("agent_home", "relative"),
        ("telegram.owner_id", "PRIVATE_INVALID_ID"),
    ] {
        let before = fs::read(&path).unwrap();
        let output = f.run(&[
            "config",
            "set",
            "--config-dir",
            state,
            "--key",
            key,
            "--value",
            value,
            "--apply",
        ]);
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("PRIVATE_INVALID_ID"));
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    let raw = serde_json::to_string(&saved).unwrap();
    let duplicate = raw.replace(
        r#""profiles":{}"#,
        r#""profiles":{"codex":"/first","codex":"/second"}"#,
    );
    assert_ne!(raw, duplicate);
    let invalid = f.0.join("duplicate-config.json");
    fs::write(&invalid, duplicate).unwrap();
    assert!(
        !f.run(&["config", "validate", "--from", invalid.to_str().unwrap()])
            .status
            .success()
    );
    let imported = f.0.join("invalid-import");
    let rejected = f.run(&[
        "config",
        "import",
        "--from",
        invalid.to_str().unwrap(),
        "--config-dir",
        imported.to_str().unwrap(),
        "--apply",
    ]);
    assert!(
        !rejected.status.success(),
        "Ambiguous profile keys must not be collapsed before import validation"
    );
    assert!(!imported.exists());
    for key in ["telegram.owner_id", "telegram.bot_id", "telegram.username"] {
        assert!(
            f.run(&[
                "config",
                "set",
                "--config-dir",
                state,
                "--key",
                key,
                "--value",
                "null",
                "--apply"
            ])
            .status
            .success()
        );
    }
    assert_eq!(
        ControlConfig::parse(&fs::read(&path).unwrap())
            .unwrap()
            .telegram,
        Default::default()
    );
    for name in ["telegram-service.json", "delivery.sqlite3", "goals.sqlite3"] {
        assert!(!directory.join(name).exists());
    }
}
#[test]
fn database_snapshot_runtime_settings_control_real_packaged_import_without_activation() {
    use daedalus_tui::app::config::ControlConfig;
    let f = Fixture::new();
    let source = f.0.join("source");
    let destination = f.0.join("destination");
    assert!(
        f.run(&[
            "config",
            "init",
            "--config-dir",
            source.to_str().unwrap(),
            "--workspace",
            f.0.to_str().unwrap(),
            "--apply"
        ])
        .status
        .success()
    );
    let path = source.join("control.json");
    let show = f.run(&["config", "show", "--config-dir", source.to_str().unwrap()]);
    let metadata: Value = serde_json::from_slice(&show.stdout).unwrap();
    let set = |key: &str, value: &str| {
        let qualified = format!("runtime.{key}");
        assert!(
            metadata["runtime_settings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field["key"] == key)
        );
        let args = [
            "config",
            "set",
            "--config-dir",
            source.to_str().unwrap(),
            "--key",
            qualified.as_str(),
            "--value",
            value,
        ];
        let before = fs::read(&path).unwrap();
        let preview = f.run(&args);
        assert!(
            preview.status.success(),
            "{}",
            String::from_utf8_lossy(&preview.stderr)
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        let applied = f.run(&[args.as_slice(), &["--apply"]].concat());
        assert!(
            applied.status.success(),
            "{}",
            String::from_utf8_lossy(&applied.stderr)
        );
        let receipt: Value = serde_json::from_slice(&applied.stdout).unwrap();
        assert_eq!(
            fs::read(receipt["result"]["backup"].as_str().unwrap()).unwrap(),
            before
        );
    };
    for (key, value) in [
        ("database_snapshot_timeout_ms", "2000"),
        ("database_snapshot_limit_bytes", "4096"),
        ("database_snapshot_pages_per_step", "1"),
        ("database_snapshot_busy_wait_ms", "11"),
        ("database_snapshot_progress_ops", "1"),
    ] {
        set(key, value);
    }
    let db_path = source.join("goals.sqlite3");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE receipts(state TEXT,payload BLOB); INSERT INTO receipts VALUES('unconfirmed',zeroblob(65536));").unwrap();
    let before = fs::read(&db_path).unwrap();
    let wal_path = source.join("goals.sqlite3-wal");
    let wal = fs::read(&wal_path).unwrap();
    assert_eq!(before.len(), 4096);
    let args = [
        "config",
        "import-instance",
        "--config-dir",
        destination.to_str().unwrap(),
        "--from",
        source.to_str().unwrap(),
    ];
    let refused = f.run(&args);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("snapshot size limit"));
    assert!(!destination.exists());
    assert_eq!(fs::read(&db_path).unwrap(), before);
    assert_eq!(fs::read(&wal_path).unwrap(), wal);
    set("database_snapshot_limit_bytes", "131072");
    let selected = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    let preview = f.run(&args);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    assert!(!destination.exists());
    let applied = f.run(&[args.as_slice(), &["--apply"]].concat());
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let copied = rusqlite::Connection::open(destination.join("goals.sqlite3")).unwrap();
    assert_eq!(
        copied
            .query_row("SELECT state FROM receipts", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "unconfirmed"
    );
    assert_eq!(
        ControlConfig::parse(&fs::read(destination.join("control.json")).unwrap())
            .unwrap()
            .runtime,
        selected.runtime
    );
    assert_eq!(fs::read(&db_path).unwrap(), before);
    assert_eq!(fs::read(&wal_path).unwrap(), wal);
    assert!(!source.join("cutover-retired.json").exists());
    assert!(!destination.join("telegram-service.json").exists());
    assert!(!destination.join("delivery.sqlite3").exists());
    assert!(!fs::read_dir(&f.0).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".daedalus-import-")
    }));
}
#[test]
fn native_profile_overrides_preview_apply_and_inherit_for_every_tool_offline() {
    use daedalus_tui::app::config::{ControlConfig, HARNESSES};
    let fixture = Fixture::new();
    let directory = fixture.0.join("state");
    let state = directory.to_str().unwrap();
    let created = fixture.run(&[
        "config",
        "init",
        "--config-dir",
        state,
        "--workspace",
        fixture.0.to_str().unwrap(),
        "--apply",
    ]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let path = directory.join("control.json");
    let show = fixture.run(&["config", "show", "--config-dir", state]);
    assert!(show.status.success());
    let metadata: Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(
        metadata["profile_settings"].as_array().unwrap().len(),
        HARNESSES.len()
    );
    let mut profiles = std::collections::BTreeMap::new();
    for tool in HARNESSES {
        let profile = fixture.0.join(format!("{tool} account profile"));
        fs::create_dir(&profile).unwrap();
        fs::write(profile.join("native-account"), b"fixture-account-preserved").unwrap();
        let key = format!("profiles.{tool}");
        let before = fs::read(&path).unwrap();
        let args = [
            "config",
            "set",
            "--config-dir",
            state,
            "--key",
            &key,
            "--value",
            profile.to_str().unwrap(),
        ];
        let preview = fixture.run(&args);
        assert!(
            preview.status.success(),
            "{}",
            String::from_utf8_lossy(&preview.stderr)
        );
        let preview: Value = serde_json::from_slice(&preview.stdout).unwrap();
        assert_eq!(preview["preview"]["before"], Value::Null);
        assert_eq!(preview["preview"]["after"], json!(profile));
        assert_eq!(fs::read(&path).unwrap(), before);
        let applied = fixture.run(&[args.as_slice(), &["--apply"]].concat());
        assert!(
            applied.status.success(),
            "{}",
            String::from_utf8_lossy(&applied.stderr)
        );
        let result: Value = serde_json::from_slice(&applied.stdout).unwrap();
        assert_eq!(
            fs::read(result["result"]["backup"].as_str().unwrap()).unwrap(),
            before
        );
        profiles.insert(tool.to_string(), profile.to_str().unwrap().to_string());
        assert_eq!(
            ControlConfig::parse(&fs::read(&path).unwrap())
                .unwrap()
                .profiles,
            profiles
        );
    }
    let before = fs::read(&path).unwrap();
    let symlink = fixture.0.join("linked-profile");
    std::os::unix::fs::symlink(&fixture.0, &symlink).unwrap();
    for (key, value) in [
        ("profiles.codex", "relative"),
        ("profiles.codex", symlink.to_str().unwrap()),
        (
            "profiles.codex",
            fixture.0.join("missing").to_str().unwrap(),
        ),
        ("profiles.unknown", "inherit"),
    ] {
        let invalid = fixture.run(&[
            "config",
            "set",
            "--config-dir",
            state,
            "--key",
            key,
            "--value",
            value,
            "--apply",
        ]);
        assert!(!invalid.status.success());
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    for tool in HARNESSES {
        let key = format!("profiles.{tool}");
        let profile = profiles.remove(tool).unwrap();
        let before = fs::read(&path).unwrap();
        let args = [
            "config",
            "set",
            "--config-dir",
            state,
            "--key",
            &key,
            "--value",
            "inherit",
        ];
        let preview = fixture.run(&args);
        assert!(preview.status.success());
        let review: Value = serde_json::from_slice(&preview.stdout).unwrap();
        assert_eq!(review["preview"]["before"], profile);
        assert_eq!(review["preview"]["after"], Value::Null);
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(
            fixture
                .run(&[args.as_slice(), &["--apply"]].concat())
                .status
                .success()
        );
        assert_eq!(
            ControlConfig::parse(&fs::read(&path).unwrap())
                .unwrap()
                .profiles,
            profiles
        );
        assert_eq!(
            fs::read(PathBuf::from(profile).join("native-account")).unwrap(),
            b"fixture-account-preserved"
        );
    }
    for (key, value) in [
        ("native_login_timeout_ms", "250"),
        ("native_login_output_limit_bytes", "4096"),
    ] {
        let key = format!("runtime.{key}");
        let args = [
            "config",
            "set",
            "--config-dir",
            state,
            "--key",
            &key,
            "--value",
            value,
        ];
        let before = fs::read(&path).unwrap();
        assert!(fixture.run(&args).status.success());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(
            fixture
                .run(&[args.as_slice(), &["--apply"]].concat())
                .status
                .success()
        );
    }
    let saved = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved.runtime.native_login_timeout_ms, 250);
    assert_eq!(saved.runtime.native_login_output_limit_bytes, 4096);
    assert!(!directory.join("delivery.sqlite3").exists());
    assert!(!directory.join("telegram-service.json").exists());
}

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
fn telegram_runtime_additions_preview_apply_and_reject_invalid_values_offline() {
    use daedalus_tui::app::telegram_config::{ServiceConfig, ServicePlan};
    let f = Fixture::new();
    let path = f.0.join("telegram-service.json");
    let config: ServiceConfig = serde_json::from_value(json!({"version":1,"instance":"fixture","bot_id":123,"token_file":f.0.join("not-provisioned")})).unwrap();
    let plan = ServicePlan::fresh(&path, config).unwrap();
    plan.apply(&plan.approval_token()).unwrap();
    let cases = [
        ("feedback_typing_enabled", "false"),
        ("feedback_ack_enabled", "false"),
        ("feedback_commands_enabled", "false"),
        ("feedback_queue_limit", "3"),
        ("feedback_typing_interval_ms", "500"),
        ("feedback_typing_ttl_ms", "250"),
        ("feedback_ack_ttl_ms", "750"),
        ("feedback_commands_ttl_ms", "1250"),
        ("progress_interval_ms", "500"),
        ("progress_batch_limit", "1"),
        ("stream_event_batch_limit", "1"),
        ("stream_input_retention", "2"),
        ("stream_subscription_limit", "2"),
        ("stream_text_limit_bytes", "1024"),
        ("stream_chunk_utf16_limit", "8"),
        ("stream_message_limit_per_event", "2"),
        ("outbound_retry_ms", "700"),
        ("outbound_lease_ms", "1200"),
        ("inbound_lease_ms", "1500"),
        ("feeder_retry_ms", "400"),
        ("stage_batch_limit", "1"),
        ("control_batch_limit", "1"),
        ("update_limit_bytes", "1024"),
        ("input_text_limit_bytes", "1000"),
        ("callback_ttl_ms", "2500"),
        ("callback_busy_timeout_ms", "7"),
        ("feeder_registration_limit", "2"),
        ("feeder_prompt_limit_bytes", "1024"),
        ("feedback_ack_buffer_limit", "1"),
    ];
    let shown = f.run(&[
        "telegram",
        "config",
        "show",
        "--file",
        path.to_str().unwrap(),
    ]);
    assert!(
        shown.status.success(),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
    for (key, value) in cases {
        assert!(
            shown["runtime_settings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field["key"] == key)
        );
        let before = fs::read(&path).unwrap();
        let qualified = format!("runtime.{key}");
        let args = [
            "telegram",
            "config",
            "set",
            "--file",
            path.to_str().unwrap(),
            "--key",
            qualified.as_str(),
            "--value",
            value,
        ];
        let preview = f.run(&args);
        assert!(
            preview.status.success(),
            "{}",
            String::from_utf8_lossy(&preview.stderr)
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        let mut apply = args.to_vec();
        apply.push("--apply");
        let applied = f.run(&apply);
        assert!(
            applied.status.success(),
            "{}",
            String::from_utf8_lossy(&applied.stderr)
        );
        assert_eq!(
            ServiceConfig::load(&path)
                .unwrap()
                .runtime
                .display(key)
                .unwrap(),
            value
        );
    }
    let before = fs::read(&path).unwrap();
    for (key, value) in [
        ("feedback_ack_enabled", "yes"),
        ("stream_message_limit_per_event", "0"),
        ("stream_message_limit_per_event", "1025"),
        ("outbound_lease_ms", "0"),
        ("callback_ttl_ms", "0"),
        ("feeder_prompt_limit_bytes", "64001"),
        ("feeder_registration_limit", "1025"),
        ("feedback_queue_limit", "0"),
        ("feedback_typing_interval_ms", "0"),
        ("feedback_ack_ttl_ms", "0"),
        ("progress_batch_limit", "1025"),
        ("stream_event_batch_limit", "0"),
        ("stream_input_retention", "0"),
        ("stream_subscription_limit", "0"),
        ("stream_text_limit_bytes", "64001"),
        ("stream_chunk_utf16_limit", "1"),
        ("stream_chunk_utf16_limit", "4001"),
    ] {
        let result = f.run(&[
            "telegram",
            "config",
            "set",
            "--file",
            path.to_str().unwrap(),
            "--key",
            key,
            "--value",
            value,
            "--apply",
        ]);
        assert!(
            !result.status.success(),
            "Unexpectedly accepted {key}={value}"
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    assert!(!f.0.join("not-provisioned").exists());
    assert!(!f.0.join("telegram-runtime.json").exists());
}

#[test]
fn viewer_history_import_uses_saved_limits_preserves_large_exact_state_and_never_activates() {
    use daedalus_tui::app::config::ControlConfig;
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let source = fixture.0.join("source");
    assert!(
        fixture
            .run(&[
                "config",
                "init",
                "--config-dir",
                source.to_str().unwrap(),
                "--workspace",
                fixture.0.to_str().unwrap(),
                "--apply"
            ])
            .status
            .success()
    );
    let config_path = source.join("control.json");
    let history_path = source.join("views.json");
    let mut config = ControlConfig::parse(&fs::read(&config_path).unwrap()).unwrap();
    config.runtime.terminal_view_history_limit = 1;
    let run_import = |destination: &std::path::Path, apply| {
        let mut args = vec![
            "config",
            "import-instance",
            "--config-dir",
            destination.to_str().unwrap(),
            "--from",
            source.to_str().unwrap(),
        ];
        if apply {
            args.push("--apply");
        }
        fixture.run(&args)
    };
    for (name, history, byte_limit, value_limit, expected) in [
        ("bytes", serde_json::to_vec(&json!({"version":2,"views":[{"annotation":"private-viewer-annotation".repeat(250)}]})).unwrap(),4096,1024,"byte budget"),
        ("values", serde_json::to_vec(&json!({"version":2,"views":[{"annotation":vec![0;200]}]})).unwrap(),8192,128,"value budget"),
        ("future", br#"{"version":3,"views":[]}"#.to_vec(),8192,1024,"Unsupported viewer registry"),
        ("duplicate", br#"{"version":3,"version":2,"views":[]}"#.to_vec(),8192,1024,"Invalid or ambiguous"),
    ] {
        config.runtime.terminal_view_history_limit_bytes=byte_limit;
        config.runtime.terminal_view_history_value_limit=value_limit;
        fs::write(&config_path,config.encode().unwrap()).unwrap();
        fs::write(&history_path,&history).unwrap();
        fs::set_permissions(&history_path,fs::Permissions::from_mode(0o600)).unwrap();
        let destination=fixture.0.join(name);
        for apply in [false,true] {
            let refused=run_import(&destination,apply);
            let error=String::from_utf8_lossy(&refused.stderr);
            assert!(!refused.status.success(),"{name} unexpectedly accepted");
            assert!(error.contains(expected),"{error}");
            assert!(!error.contains("private-viewer-annotation"));
            assert!(!destination.exists());
            assert_eq!(fs::read(&history_path).unwrap(),history);
        }
    }
    config.runtime.terminal_view_history_limit_bytes = 16 * 1024 * 1024;
    config.runtime.terminal_view_history_value_limit = 1024;
    let config_bytes = config.encode().unwrap();
    fs::write(&config_path, &config_bytes).unwrap();
    let history=serde_json::to_vec_pretty(&json!({"version":2,"views":[{"version":2,"source":{"backend":"herdr","endpoint":"fixture","pane":"source","generation":"source-generation"},"viewer":{"backend":"herdr","endpoint":"fixture","pane":"viewer","generation":"viewer-generation"},"annotation":"x".repeat(9*1024*1024)},{"version":99,"unresolved":true}]})).unwrap();
    fs::write(&history_path, &history).unwrap();
    let destination = fixture.0.join("accepted");
    let preview = run_import(&destination, false);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let value: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert!(
        value["preview"]["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "views.json")
    );
    assert!(!destination.exists());
    let applied = run_import(&destination, true);
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let value: Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(value["result"]["requires_cutover"], true);
    assert_eq!(fs::read(destination.join("views.json")).unwrap(), history);
    assert_eq!(fs::read(&history_path).unwrap(), history);
    assert_eq!(
        fs::read(destination.join("control.json")).unwrap(),
        config_bytes
    );
    assert_eq!(fs::read(&config_path).unwrap(), config_bytes);
    assert_eq!(
        fs::metadata(destination.join("views.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(!destination.join("feeder-process.json").exists());
    assert!(!source.join("cutover-retired.json").exists());
    assert!(!fs::read_dir(&fixture.0).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".daedalus-import-")
    }));
}

#[test]
fn instance_import_operating_budgets_refuse_before_publish_and_recover_in_packaged_cli() {
    use daedalus_tui::app::config::ControlConfig;
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let source = f.0.join("source");
    assert!(
        f.run(&[
            "config",
            "init",
            "--config-dir",
            source.to_str().unwrap(),
            "--workspace",
            f.0.to_str().unwrap(),
            "--apply"
        ])
        .status
        .success()
    );
    let path = source.join("control.json");
    let pending = source.join("pending_feed.jsonl");
    fs::write(&pending, vec![b'x'; 7000]).unwrap();
    fs::set_permissions(&pending, fs::Permissions::from_mode(0o600)).unwrap();
    let baseline = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    let run = |destination: &std::path::Path, apply| {
        let mut args = vec![
            "config",
            "import-instance",
            "--from",
            source.to_str().unwrap(),
            "--config-dir",
            destination.to_str().unwrap(),
        ];
        if apply {
            args.push("--apply");
        }
        f.run(&args)
    };
    for key in [
        "instance_import_limit_bytes",
        "instance_import_entry_limit",
        "instance_import_json_value_limit",
        "instance_import_work_limit_bytes",
    ] {
        let mut config = baseline.clone();
        config.runtime = match key {
            "instance_import_limit_bytes" => config.runtime.set(key, "8192").unwrap(),
            "instance_import_entry_limit" => config.runtime.set(key, "1").unwrap(),
            "instance_import_json_value_limit" => config.runtime.set(key, "128").unwrap(),
            _ => config.runtime.set(key, "4096").unwrap(),
        };
        let before = config.encode().unwrap();
        fs::write(&path, &before).unwrap();
        let destination = f.0.join(key);
        for apply in [false, true] {
            let refused = run(&destination, apply);
            assert!(!refused.status.success(), "{key} unexpectedly accepted");
            assert!(String::from_utf8_lossy(&refused.stderr).contains("Instance import"));
            assert!(!destination.exists());
            assert_eq!(fs::read(&path).unwrap(), before);
            assert_eq!(fs::read(&pending).unwrap(), vec![b'x'; 7000]);
        }
    }
    let database = source.join("goals.sqlite3");
    let con = rusqlite::Connection::open(&database).unwrap();
    con.execute_batch("CREATE TABLE inert(value BLOB);INSERT INTO inert VALUES(zeroblob(32768));")
        .unwrap();
    drop(con);
    fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();
    let original_database = fs::read(&database).unwrap();
    let mut config = baseline.clone();
    config.runtime.instance_import_limit_bytes = 16384;
    fs::write(&path, config.encode().unwrap()).unwrap();
    let destination = f.0.join("database");
    let refused = run(&destination, true);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("retained-byte"));
    assert!(!destination.exists());
    config.runtime.instance_import_limit_bytes = 1048576;
    config.runtime.instance_import_work_limit_bytes = 1048576;
    let config_bytes = config.encode().unwrap();
    fs::write(&path, &config_bytes).unwrap();
    let accepted = run(&destination, true);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&accepted.stdout).unwrap()["result"]["requires_cutover"],
        true
    );
    assert_eq!(fs::read(&path).unwrap(), config_bytes);
    assert_eq!(
        fs::read(destination.join("control.json")).unwrap(),
        config_bytes
    );
    assert_eq!(fs::read(&database).unwrap(), original_database);
    let copy = rusqlite::Connection::open(destination.join("goals.sqlite3")).unwrap();
    assert_eq!(
        copy.query_row("SELECT length(value) FROM inert", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        32768
    );
    let repeat = run(&destination, true);
    assert!(repeat.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&repeat.stdout).unwrap()["result"]["changed"],
        false
    );
    assert!(!source.join("cutover-retired.json").exists());
    assert!(!destination.join("feeder-process.json").exists());
    assert!(!fs::read_dir(&f.0).unwrap().any(|p| {
        p.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".daedalus-import-")
    }));
}

#[test]
fn offline_cutover_cli_keeps_producer_attachments_metadata_and_uncertain_rows_under_saved_limits() {
    use daedalus_tui::{
        app::{
            config::ControlConfig,
            migration::ImportPlan,
            setup::SetupPlan,
            telegram::{Attachment, Files, Frame, PrivateFiles},
            telegram_config::{ServiceConfig, ServicePlan},
        },
        core::{
            identity::{Backend, TerminalId},
            journal::Journal,
            routes::{Registry, Scope},
        },
    };
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let source = f.0.join("source");
    let destination = f.0.join("destination");
    let mut config = ControlConfig::fresh(&f.0).unwrap();
    config.discovery.automatic = false;
    config.discovery.herdr_sessions.clear();
    config.tmux_sockets.clear();
    SetupPlan::fresh(&source, config).unwrap().apply().unwrap();
    let bot = 8_100_000_000_000_000i64 + std::process::id() as i64;
    let owner_root = std::fs::canonicalize("/tmp").unwrap_or_else(|_| PathBuf::from("/tmp"));
    let owner = owner_root
        .join(format!("daedalus-service-{}", unsafe { libc::geteuid() }))
        .join(format!("telegram-{bot}.lock"));
    assert!(
        !owner.exists(),
        "Fixture refuses to touch an existing owner identity"
    );
    struct FixtureOwner(PathBuf);
    impl Drop for FixtureOwner {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let _owner = FixtureOwner(owner);
    let token = source.join("credential.txt");
    fs::write(&token, format!("{bot}:synthetic_fixture")).unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let service = ServiceConfig {
        version: 1,
        instance: "fixture".into(),
        bot_id: bot,
        token_file: token,
        grants: vec![],
        listen_chats: Default::default(),
        registrations: vec![],
        hook_bindings: vec![],
        runtime: Default::default(),
    };
    let service_plan = ServicePlan::fresh(&source.join("telegram-service.json"), service).unwrap();
    service_plan.apply(&service_plan.approval_token()).unwrap();
    let routes = source.join("routes.sqlite3");
    fs::write(&routes, b"").unwrap();
    fs::set_permissions(&routes, fs::Permissions::from_mode(0o600)).unwrap();
    drop(Registry::from_connection(rusqlite::Connection::open(&routes).unwrap()).unwrap());
    let mut journal = Journal::open(&source.join("delivery.sqlite3")).unwrap();
    journal
        .admit_and_advance(
            "telegram",
            &bot.to_string(),
            7,
            &json!({"fixture":true}),
            1.0,
        )
        .unwrap();
    let terminal = TerminalId {
        backend: Backend::Herdr,
        endpoint: "fixture-no-server".into(),
        pane: "fixture-pane".into(),
        generation: "fixture-generation".into(),
    };
    let mut files = PrivateFiles {
        directory: source.join("attachments"),
    };
    let mut attachments: Vec<Attachment> = Vec::new();
    for (key, name, state) in [
        ("one", "notes.txt", "pending"),
        ("two", "photo.png", "pending"),
        ("uncertain", "unknown.txt", "unknown"),
    ] {
        let attachment = files.save(name, key.as_bytes()).unwrap();
        attachments.push(attachment.clone());
        let frame = Frame {
            scope: Scope {
                instance: "fixture".into(),
                bot: bot.to_string(),
                chat: "42".into(),
            },
            actor: "7".into(),
            message_id: 5,
            update_id: 8,
            terminal: terminal.clone(),
            route_revision: 1,
            conversation: Some("fixture-conversation".into()),
            text: "x".repeat(2048),
            attachment: Some(attachment),
        };
        let mut payload = serde_json::to_value(frame).unwrap();
        payload["retained_metadata"] = json!({"revision":7,"label":"private fixture"});
        journal
            .enqueue(
                "inbound",
                &serde_json::to_string(&terminal).unwrap(),
                &payload,
                key,
                None,
                1.0,
            )
            .unwrap();
        if state == "unknown" {
            let con = rusqlite::Connection::open(source.join("delivery.sqlite3")).unwrap();
            con.execute("UPDATE delivery SET state='unknown',receipt='{\"fixture\":\"retained\"}' WHERE dedup_key='uncertain'",[]).unwrap();
        }
    }
    drop(journal);
    ImportPlan::prepare(&source, &destination, true)
        .unwrap()
        .apply()
        .unwrap();
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_daedalus-tui"))
            .env_clear()
            .env("PATH", "/nonexistent")
            .env("HOME", &f.0)
            // The child cannot inherit the macOS resolved temp root.
            .env(
                "TMPDIR",
                if cfg!(target_os = "macos") {
                    "/private/tmp"
                } else {
                    "/tmp"
                },
            )
            .current_dir(&f.0)
            .args(args)
            .output()
            .unwrap()
    };
    let source_journal = fs::read(source.join("delivery.sqlite3")).unwrap();
    let destination_control = fs::read(destination.join("control.json")).unwrap();
    let path = source.join("control.json");
    let original = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    for (key, value) in [
        ("instance_import_attachment_limit", "1"),
        ("instance_import_attachment_limit_bytes", "5"),
        ("instance_import_frame_limit_bytes", "1024"),
    ] {
        let mut changed = original.clone();
        changed.runtime = changed.runtime.set(key, value).unwrap();
        fs::write(&path, changed.encode().unwrap()).unwrap();
        let refused = invoke(&[
            "telegram",
            "cutover",
            "preview",
            "--config-dir",
            destination.to_str().unwrap(),
        ]);
        assert!(!refused.status.success(), "{key} unexpectedly accepted");
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains(key),
            "{}",
            String::from_utf8_lossy(&refused.stderr)
        );
        assert!(!source.join("cutover-retired.json").exists());
        assert!(!destination.join("cutover.json").exists());
        assert_eq!(
            fs::read(source.join("delivery.sqlite3")).unwrap(),
            source_journal
        );
        assert_eq!(
            fs::read(destination.join("control.json")).unwrap(),
            destination_control
        );
    }
    fs::write(&path, original.encode().unwrap()).unwrap();
    let preview = invoke(&[
        "telegram",
        "cutover",
        "preview",
        "--config-dir",
        destination.to_str().unwrap(),
    ]);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let preview: Value = serde_json::from_slice(&preview.stdout).unwrap();
    let approval = preview["approval"].as_str().unwrap();
    let applied = invoke(&[
        "telegram",
        "cutover",
        "apply",
        "--config-dir",
        destination.to_str().unwrap(),
        "--approve",
        approval,
    ]);
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let frames: Vec<Value> = serde_json::Deserializer::from_slice(&applied.stdout)
        .into_iter::<Value>()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(frames.last().unwrap()["state"], "committed");
    assert_eq!(frames.last().unwrap()["activated"], false);
    let con = rusqlite::Connection::open(destination.join("delivery.sqlite3")).unwrap();
    for (index, key) in ["one", "two", "uncertain"].into_iter().enumerate() {
        let (state, raw): (String, String) = con
            .query_row(
                "SELECT state,payload FROM delivery WHERE dedup_key=?",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let payload: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(payload["retained_metadata"]["revision"], 7);
        let expected = if key == "uncertain" {
            assert_eq!(state, "unknown");
            attachments[index].path.clone()
        } else {
            assert_eq!(state, "pending");
            destination
                .join("attachments")
                .join(attachments[index].path.file_name().unwrap())
        };
        assert_eq!(payload["attachment"]["path"], json!(expected));
        if key != "uncertain" {
            assert_eq!(fs::read(expected).unwrap(), key.as_bytes());
        }
    }
    drop(con);
    assert_eq!(
        fs::read(source.join("delivery.sqlite3")).unwrap(),
        source_journal
    );
    let rollback = invoke(&[
        "telegram",
        "cutover",
        "rollback",
        "--source",
        source.to_str().unwrap(),
        "--approve",
        approval,
    ]);
    assert!(
        rollback.status.success(),
        "{}",
        String::from_utf8_lossy(&rollback.stderr)
    );
    assert_eq!(
        fs::read(destination.join("control.json")).unwrap(),
        destination_control
    );
    assert_eq!(
        fs::read(source.join("delivery.sqlite3")).unwrap(),
        source_journal
    );
    assert!(!destination.join("cutover-started.json").exists());
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
    let plan = gateway::Plan::prepare(&fixture.0, &None, config.clone()).unwrap();
    let approval = plan.approval();
    plan.apply(&approval).unwrap();
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
    fs::write(fixture.0.join("cutover-retired.json"), b"{}").unwrap();
    let retired = execute();
    assert!(!retired.status.success());
    assert!(String::from_utf8_lossy(&retired.stderr).contains("retired"));
    assert!(!fixture.0.join("observed").exists());
    fs::remove_file(fixture.0.join("cutover-retired.json")).unwrap();
    let mut changed = config.clone();
    changed.model = "changed".into();
    let plan = gateway::Plan::prepare(&fixture.0, &Some(config), changed).unwrap();
    let approval = plan.approval();
    plan.apply(&approval).unwrap();
    let result = execute();
    assert!(!result.status.success());
    assert!(!fixture.0.join("observed").exists());
    assert!(!String::from_utf8_lossy(&result.stderr).contains("PRIVATE-GATEWAY-KEY"));
}

// install.sh is the Linux native installer; macOS has no release installer yet.
#[cfg(target_os = "linux")]
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

#[test]
fn workspace_usage_saved_switch_controls_real_backend_and_read_only_cli_respects_output_limit() {
    use daedalus_tui::app::config::ControlConfig;
    let panes:Vec<_>=(0..12).map(|i|json!({"pane_id":format!("p{i}"),"terminal_id":format!("generation-{i}"),"title":"owned fixture"})).collect();
    let f = observation_backend_fixture(json!({"result":{"panes":panes,"focused_pane_id":"p0"}}));
    let directory = f.0.join("state");
    let state = directory.to_str().unwrap();
    let path = directory.join("control.json");
    let set = |key: &str, value: &str| {
        let before = fs::read(&path).unwrap();
        let args = [
            "config",
            "set",
            "--config-dir",
            state,
            "--key",
            key,
            "--value",
            value,
        ];
        assert!(f.run(&args).status.success());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(
            f.run(&[args.as_slice(), &["--apply"]].concat())
                .status
                .success()
        );
    };
    set("ui.workspace_usage_recording", "false");
    assert!(
        !ControlConfig::parse(&fs::read(&path).unwrap())
            .unwrap()
            .ui
            .workspace_usage_recording
    );
    let refresh = || {
        let mut child = observation_backend(&f).spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"{\"version\":2,\"op\":\"quit\"}\n")
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    refresh();
    let database = directory.join("workspace-usage.sqlite3");
    assert!(!database.exists());
    set("ui.workspace_usage_recording", "true");
    refresh();
    assert!(database.exists());
    let before = fs::read(&database).unwrap();
    let query = || {
        Command::new(env!("CARGO_BIN_EXE_daedalus-tui"))
            .env_clear()
            .env("PATH", "/nonexistent")
            .env("HOME", &f.0)
            .env("DAEDALUS_HERDR_BIN", f.0.join("inert-herdr"))
            .env("FIXTURE_PAYLOAD", f.0.join("payload"))
            .env("FIXTURE_QUERIES", f.0.join("queries"))
            .current_dir(&f.0)
            .args(["workspace-usage", "--config-dir", state, "--json"])
            .output()
            .unwrap()
    };
    let result = query();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let data: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(data["recording_enabled"], true);
    assert_eq!(data["terminals"].as_array().unwrap().len(), 12);
    assert_eq!(fs::read(&database).unwrap(), before);
    set("runtime.workspace_usage_output_limit_bytes", "1024");
    let result = query();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("workspace_usage_output_limit_bytes"));
    assert_eq!(fs::read(&database).unwrap(), before);
}
