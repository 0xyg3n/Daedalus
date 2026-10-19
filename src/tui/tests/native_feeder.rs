//! Native PTY + hook lifecycle + feeder integration with a private shell fixture.
//! This exercises no provider, Telegram bot, or user terminal.
#![cfg(unix)]
use daedalus_tui::{
    adapters::{
        native, process,
        terminals::{self, Terminals},
    },
    app::{
        hook_runtime::{self, Options, ProcessIdentity, Readiness, Registration},
        hooks::{EventKind, Harness},
        native_terminal::{self, SessionInfo},
        telegram::{Feeder, Frame, Submission},
        telegram_feeder::NativeFeeder,
    },
    core::{goal_store, identity::Backend, routes},
};
use serde_json::json;
use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

fn private_file(path: &Path, bytes: &[u8]) {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
struct Cleanup {
    root: PathBuf,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        let output = Command::new(env!("CARGO_BIN_EXE_daedalus-tui"))
            .env_clear()
            .env("HOME", &self.root)
            .env("PATH", "/usr/bin:/bin")
            .args(["terminal-native", "list"])
            .output();
        if let Ok(output) = output
            && let Ok(sessions) = serde_json::from_slice::<Vec<SessionInfo>>(&output.stdout)
        {
            for session in sessions {
                let _ = Command::new(env!("CARGO_BIN_EXE_daedalus-tui"))
                    .env_clear()
                    .env("HOME", &self.root)
                    .env("PATH", "/usr/bin:/bin")
                    .args(["terminal-native", "close", &session.id, &session.generation])
                    .output();
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[test]
fn native_hooks_feed_only_the_verified_idle_conversation() {
    if std::env::var("DAEDALUS_NATIVE_FEEDER_FIXTURE").as_deref() == Ok("1") {
        run_fixture(&PathBuf::from(std::env::var_os("HOME").unwrap()));
        return;
    }
    let mut nonce = [0; 16];
    daedalus_tui::platform::random_bytes(&mut nonce).unwrap();
    let root = std::env::temp_dir().join(format!(
        "daedalus-native-feeder-flow-{}",
        nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ));
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let fixture = Cleanup { root };
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "native_hooks_feed_only_the_verified_idle_conversation",
            "--nocapture",
        ])
        .env_clear()
        .env("HOME", &fixture.root)
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "xterm-256color")
        .env("DAEDALUS_NATIVE_FEEDER_FIXTURE", "1");
    let output = process::run(&mut command, Duration::from_secs(40), 128 * 1024).unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
fn run_fixture(root: &Path) {
    let binary = env!("CARGO_BIN_EXE_daedalus-tui");
    let output = Command::new(binary)
        .current_dir(root)
        .args([
            "terminal-native",
            "start",
            "--name",
            "Fixture actor",
            "--workspace",
            "Fixture",
            "--",
            "/bin/sh",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let session: SessionInfo = serde_json::from_slice(&output.stdout).unwrap();
    let terminal = terminals::identity(&native::pane(&session)).unwrap();
    assert_eq!(terminal.backend, Backend::Native);
    let directory = root.join("state");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let registration = Registration {
        version: 1,
        id: "fixture".into(),
        directory: directory.clone(),
        scope: goal_store::Scope {
            instance: "fixture".into(),
            terminal: terminal.clone(),
            conversation: "fixture-session".into(),
            harness: Harness::Codex,
        },
        goal_id: None,
        harness_process: Some(ProcessIdentity {
            pid: session.pid,
            start_ticks: terminals::process_start(Path::new("/proc"), &session.pid.to_string())
                .unwrap()
                .parse()
                .unwrap(),
        }),
        server_emitter: None,
        transcript: None,
        events: vec![EventKind::UserPromptSubmit, EventKind::Stop],
        options: Options::default(),
        activity_lease_ms: 30000,
        pending_timeout_ms: 30000,
        hook_timeout_ms: 12000,
    };
    let path = hook_runtime::install(&registration).unwrap();
    let mut feeder = NativeFeeder {
        instance: "fixture".into(),
        registrations: vec![path.clone()],
        terminals: Terminals::default(),
    };
    let mut frame = Frame {
        scope: routes::Scope {
            instance: "fixture".into(),
            bot: "1".into(),
            chat: "2".into(),
        },
        actor: "3".into(),
        message_id: 4,
        update_id: 5,
        terminal,
        route_revision: 1,
        conversation: Some("fixture-session".into()),
        text: "native_fixture_payload".into(),
        attachment: None,
    };
    assert_eq!(feeder.live().unwrap(), vec![frame.terminal.clone()]);
    assert_eq!(feeder.submit(&frame).unwrap(), Submission::Busy);
    for (name, event) in [("begin", "UserPromptSubmit"), ("end", "Stop")] {
        private_file(&root.join(format!("{name}.json")), &serde_json::to_vec(&json!({"session_id":"fixture-session", "hook_event_name":event, "turn_id":"initial", "prompt":"initial", "last_assistant_message":"done", "transcript_path":null})).unwrap());
    }
    // Disable shell job control so hook child processes keep the registered
    // actor's foreground group, as real provider hook children do.
    let hook = format!(
        "{} hook run --registration {}",
        quote(binary),
        quote(path.to_str().unwrap())
    );
    native_terminal::input(
        &session.id,
        &session.generation,
        format!(
            "set +m; {hook} < {}; {hook} < {}\r",
            quote(root.join("begin.json").to_str().unwrap()),
            quote(root.join("end.json").to_str().unwrap())
        )
        .as_bytes(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if hook_runtime::ready_snapshot(&path, Path::new("/proc"))
            .is_ok_and(|state| state.state == Readiness::Idle)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Hooks did not produce verified Idle: {}",
            native_terminal::capture(&session.id, &session.generation)
                .unwrap()
                .text
        );
        thread::sleep(Duration::from_millis(30));
    }
    let mut snapshot = json!({"panes":[native::pane(&session)]});
    assert_eq!(
        hook_runtime::enrich_native_sessions(&directory, &mut snapshot, &Terminals::default())
            .unwrap(),
        1
    );
    assert_eq!(
        snapshot["panes"][0]["agent_session"]["value"],
        "fixture-session"
    );
    assert!(matches!(
        feeder.submit(&frame).unwrap(),
        Submission::Accepted(_)
    ));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let capture = native_terminal::capture(&session.id, &session.generation).unwrap();
        if capture
            .text
            .replace('\n', "")
            .contains("native_fixture_payload")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Submitted frame did not reach fixture PTY"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(feeder.submit(&frame).unwrap(), Submission::Busy);
    frame.terminal.generation = "0".repeat(64);
    assert_eq!(feeder.submit(&frame).unwrap(), Submission::Unavailable);
    native_terminal::close(&session.id, &session.generation).unwrap();
}
