//! Real local PTY coverage. All shells, homes and processes belong to this fixture.
#![cfg(unix)]
use base64::{Engine as _, engine::general_purpose::STANDARD};
use daedalus_tui::app::native_terminal::{Capture, SessionInfo};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
struct Fixture {
    root: PathBuf,
    sessions: Vec<SessionInfo>,
}
impl Fixture {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt;
        let mut random = [0; 16];
        daedalus_tui::platform::random_bytes(&mut random).unwrap();
        let name: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let root = std::env::temp_dir().join(format!("daedalus-native-cli-{name}"));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self {
            root,
            sessions: Vec::new(),
        }
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_daedalus-tui"));
        cmd.env_clear()
            .env("HOME", &self.root)
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "xterm-256color")
            .env("SHELL", "/bin/sh")
            .current_dir(&self.root);
        cmd
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if output.stdout.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&output.stdout).unwrap()
        }
    }
    fn start(&mut self) -> SessionInfo {
        let value = self.ok(&[
            "terminal-native",
            "start",
            "--name",
            "Fixture shell",
            "--workspace",
            "Alpha",
            "--",
            "/bin/sh",
        ]);
        assert!(value.get("token").is_none());
        let info: SessionInfo = serde_json::from_value(value).unwrap();
        self.sessions.push(info.clone());
        info
    }
    fn wait_text(&self, info: &SessionInfo, text: &str) -> Capture {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let capture: Capture = serde_json::from_value(self.ok(&[
                "terminal-native",
                "capture",
                &info.id,
                &info.generation,
            ]))
            .unwrap();
            if capture.text.contains(text) {
                return capture;
            }
            assert!(Instant::now() < deadline, "Missing shell output: {text}");
            thread::sleep(Duration::from_millis(30));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for info in &self.sessions {
            let _ = self.run(&["terminal-native", "close", &info.id, &info.generation]);
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}
struct Stream {
    child: Child,
    lines: mpsc::Receiver<Value>,
}
impl Stream {
    fn new(fixture: &Fixture, info: &SessionInfo, mode: &str) -> Self {
        let mut child = fixture
            .command()
            .args([
                "terminal-native",
                "stream",
                &info.id,
                &info.generation,
                "--mode",
                mode,
                "--cols",
                "80",
                "--rows",
                "24",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(value) = serde_json::from_str(&line)
                    && tx.send(value).is_err()
                {
                    break;
                }
            }
        });
        Self { child, lines: rx }
    }
    fn send(&mut self, value: Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        serde_json::to_writer(&mut *stdin, &value).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }
    fn frame(&self) -> Value {
        let value = self.lines.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(value["type"], "terminal.frame", "{value}");
        value
    }
    fn frame_with(&self, text: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let frame = self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("Native stream ended or did not provide the requested screen");
            assert_eq!(frame["type"], "terminal.frame", "{frame}");
            let mut parser = vt100::Parser::new(
                frame["height"].as_u64().unwrap() as u16,
                frame["width"].as_u64().unwrap() as u16,
                0,
            );
            parser.process(&STANDARD.decode(frame["bytes"].as_str().unwrap()).unwrap());
            let contents = parser.screen().contents();
            if contents.contains(text) {
                return contents;
            }
            assert!(Instant::now() < deadline, "No {text:?}: {contents}");
        }
    }
    fn stop(&mut self) {
        self.send(json!({"type":"terminal.release"}));
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "Stream did not detach");
            thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn native_scroll_requests_keep_control_and_observer_streams_alive_and_independent() {
    let mut fixture = Fixture::new();
    let info = fixture.start();
    let mut control = Stream::new(&fixture, &info, "control");
    control.frame();
    let mut observer = Stream::new(&fixture, &info, "observe");
    observer.frame();
    control.send(json!({"type":"terminal.input","bytes":STANDARD.encode(
        b"for n in $(seq 1 60); do printf 'history-row-%02d\\n' \"$n\"; done; printf 'SCROLL_%s\\n' LIVE\r"
    )}));
    control.frame_with("SCROLL_LIVE");
    observer.frame_with("SCROLL_LIVE");
    control.send(json!({"type":"terminal.scroll","direction":"up","lines":1000}));
    let history = control.frame_with("history-row-01");
    assert!(!history.contains("SCROLL_LIVE"));
    assert!(fixture.wait_text(&info, "SCROLL_LIVE").session.controlled);
    observer.send(json!({"type":"terminal.scroll","direction":"up","lines":7}));
    let frame = observer.frame();
    let mut parser = vt100::Parser::new(24, 80, 0);
    parser.process(&STANDARD.decode(frame["bytes"].as_str().unwrap()).unwrap());
    let other = parser.screen().contents();
    assert!(other.contains("history-row-"));
    assert!(!other.contains("history-row-01") && !other.contains("SCROLL_LIVE"));
    control.send(json!({"type":"terminal.scroll","direction":"down","lines":1000}));
    control.frame_with("SCROLL_LIVE");
    observer.send(json!({"type":"terminal.scroll","direction":"down","lines":7}));
    observer.frame_with("SCROLL_LIVE");
    control.send(json!({"type":"terminal.scroll","direction":"up","lines":1000}));
    control.frame_with("history-row-01");
    control.send(
        json!({"type":"terminal.input","bytes":STANDARD.encode(b"printf 'TYPE_%s\\n' LIVE\r")}),
    );
    control.frame_with("TYPE_LIVE");
    observer.stop();
    control.stop();
}

#[test]
fn native_history_capacity_uses_saved_policy_and_explicit_zero_override() {
    let mut fixture = Fixture::new();
    let state = fixture.root.join("config");
    let directory = state.to_str().unwrap();
    fixture.ok(&[
        "config",
        "init",
        "--config-dir",
        directory,
        "--workspace",
        fixture.root.to_str().unwrap(),
        "--apply",
    ]);
    let path = state.join("control.json");
    let before = fs::read(&path).unwrap();
    let args = [
        "config",
        "set",
        "--config-dir",
        directory,
        "--key",
        "runtime.native_scrollback_rows",
        "--value",
        "8",
    ];
    fixture.ok(&args);
    assert_eq!(fs::read(&path).unwrap(), before);
    let receipt = fixture.ok(&[args.as_slice(), &["--apply"]].concat());
    assert_eq!(
        fs::read(receipt["result"]["backup"].as_str().unwrap()).unwrap(),
        before
    );
    let saved = fs::read(&path).unwrap();
    let bad = fixture.run(&[
        "terminal-native",
        "start",
        "--scrollback-rows",
        "65537",
        "--",
        "/bin/sh",
    ]);
    assert!(!bad.status.success());
    assert!(
        fixture
            .ok(&["terminal-native", "list"])
            .as_array()
            .unwrap()
            .is_empty()
    );
    for (override_rows, expected) in [(false, 8), (true, 0)] {
        let mut args = vec!["terminal-native", "start", "--config-dir", directory];
        if override_rows {
            args.extend(["--scrollback-rows", "0"]);
        }
        args.extend(["--", "/bin/sh"]);
        let info: SessionInfo = serde_json::from_value(fixture.ok(&args)).unwrap();
        fixture.sessions.push(info.clone());
        fixture.ok(&["terminal-native", "input", &info.id, &info.generation, "--text",
            "for n in $(seq 1 60); do printf 'bounded-row-%02d\\n' \"$n\"; done; printf 'BOUND_%s\\n' LIVE\r"]);
        fixture.wait_text(&info, "BOUND_LIVE");
        let history: Capture = serde_json::from_value(fixture.ok(&[
            "terminal-native",
            "capture",
            &info.id,
            &info.generation,
            "--scrollback-rows",
            "65536",
        ]))
        .unwrap();
        assert_eq!(history.scrollback_rows, Some(expected));
        assert!(!history.text.contains("bounded-row-01"));
        let live = fixture.wait_text(&info, "BOUND_LIVE");
        assert_eq!(live.scrollback_rows, Some(0));
    }
    assert_eq!(fs::read(&path).unwrap(), saved);
}

#[test]
fn native_resize_only_frame_is_emitted_without_new_terminal_output() {
    let mut fixture = Fixture::new();
    let info: SessionInfo = serde_json::from_value(fixture.ok(&[
        "terminal-native",
        "start",
        "--",
        "/bin/sh",
        "-c",
        "printf FRAME_STATIC; read _",
    ]))
    .unwrap();
    fixture.sessions.push(info.clone());
    fixture.wait_text(&info, "FRAME_STATIC");
    let mut stream = Stream::new(&fixture, &info, "control");
    let initial = stream.frame();
    assert_eq!(initial["width"], 80);
    stream.send(json!({"type":"terminal.resize","cols":81,"rows":24}));
    let resized = stream.frame();
    assert_eq!(resized["width"], 81);
    assert_eq!(resized["height"], 24);
    assert_eq!(resized["bytes"], initial["bytes"]);
    stream.stop();
}

#[test]
#[cfg(target_os = "linux")]
fn guided_native_launch_publishes_child_identity_and_registers_before_provider_exec() {
    use daedalus_tui::app::{config::ControlConfig, hook_runtime, setup::SetupPlan};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let mut fixture = Fixture::new();
    let parent = fixture.start();
    let bin = fixture.root.join("bin");
    fs::DirBuilder::new().mode(0o700).create(&bin).unwrap();
    let source = fixture.root.join("provider.rs");
    fs::write(&source, include_str!("fixtures/launch_provider.rs")).unwrap();
    let provider = bin.join("claude");
    let compile = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .args(["--edition=2024", "--crate-name", "launch_provider_fixture"])
        .arg(&source)
        .arg("-o")
        .arg(&provider)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o700)).unwrap();
    // First-launch pins an owned non-writable executable. Cargo's shared build
    // directory can use group-writable modes, so stage a private packaged copy.
    let binary = bin.join("daedalus");
    fs::copy(env!("CARGO_BIN_EXE_daedalus-tui"), &binary).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let directory = fixture.root.join("instance");
    SetupPlan::fresh(&directory, ControlConfig::fresh(&fixture.root).unwrap())
        .unwrap()
        .apply()
        .unwrap();
    let mut backend = Command::new(binary)
        .env_clear()
        .env("HOME", &fixture.root)
        .env("TERM", "xterm-256color")
        .env("SHELL", "/bin/sh")
        .env("DAEDALUS_TMUX_BIN", "/bin/true")
        .env("DAEDALUS_HERDR_BIN", "/bin/true")
        .current_dir(&fixture.root)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .args([
            "backend",
            "--stdio",
            "--config-dir",
            directory.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = backend.stdin.take().unwrap();
    for request in [
        json!({"version":2,"op":"refresh"}),
        json!({"version":2,"op":"action","page":4,"action":"tools.open"}),
        json!({"version":2,"op":"reply","id":1,"value":"claude"}),
        json!({"version":2,"op":"reply","id":2,"value":"0"}),
        json!({"version":2,"op":"reply","id":3,"value":"launch"}),
        json!({"version":2,"op":"quit"}),
    ] {
        writeln!(input, "{request}").unwrap();
    }
    drop(input);
    let output = backend.wait_with_output().unwrap();
    let listed: Vec<SessionInfo> =
        serde_json::from_value(fixture.ok(&["terminal-native", "list"])).unwrap();
    let children: Vec<_> = listed.into_iter().filter(|s| s.id != parent.id).collect();
    fixture.sessions.extend(children.iter().cloned());
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
    let errors: Vec<_> = frames.iter().filter(|f| f["kind"] == "error").collect();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(children.len(), 1);
    let child = &children[0];
    let ticket = fs::read_dir(&directory)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("launch-ticket-")
        })
        .unwrap();
    assert!(
        ticket.join("ready.json").is_file(),
        "Native pane was created without publishing its identity"
    );
    fixture.wait_text(child, "FIXTURE_PROVIDER_REGISTERED");
    let bound_path: PathBuf =
        serde_json::from_slice(&fs::read(ticket.join("ready.json")).unwrap()).unwrap();
    let bound: daedalus_tui::app::hook_launch::Plan =
        serde_json::from_slice(&fs::read(bound_path).unwrap()).unwrap();
    let registered = hook_runtime::inspect(&bound.spec.registration.path()).unwrap();
    assert_eq!(registered.scope.terminal.endpoint, child.id);
    assert_eq!(registered.scope.terminal.generation, child.generation);
    assert_ne!(registered.scope.terminal.endpoint, parent.id);
    let provider_pid: u32 = fs::read_to_string(directory.join("provider-started"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(registered.harness_process.unwrap().pid, provider_pid);
    assert_eq!(child.pid, provider_pid);
    assert!(
        frames
            .iter()
            .any(|f| f.to_string().contains("hook bootstrap submitted"))
    );
}
#[test]
fn saved_launch_dimensions_reach_real_native_terminals_and_explicit_flags_win() {
    use daedalus_tui::app::{config::ControlConfig, setup::SetupPlan};
    let mut fixture = Fixture::new();
    let directory = fixture.root.join("instance");
    let mut config = ControlConfig::fresh(&fixture.root).unwrap();
    config.runtime.native_launch_cols = 93;
    config.runtime.native_launch_rows = 31;
    config.runtime.shell_cols = 87;
    config.runtime.shell_rows = 29;
    SetupPlan::fresh(&directory, config)
        .unwrap()
        .apply()
        .unwrap();
    let mut start = vec![
        "terminal-native",
        "start",
        "--config-dir",
        directory.to_str().unwrap(),
        "--",
        "/bin/sh",
    ];
    let value = fixture.ok(&start);
    let parent: SessionInfo = serde_json::from_value(value).unwrap();
    fixture.sessions.push(parent.clone());
    assert_eq!((parent.cols, parent.rows), (87, 29));
    // Verify the kernel PTY dimensions as seen by its child, too.
    fixture.ok(&[
        "terminal-native",
        "input",
        &parent.id,
        &parent.generation,
        "--text",
        "printf 'SIZE='; stty size\n",
    ]);
    fixture.wait_text(&parent, "SIZE=29 87");
    let target = directory.join("target.json");
    {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&target)
            .unwrap()
            .write_all(&serde_json::to_vec(&daedalus_tui::adapters::native::pane(&parent)).unwrap())
            .unwrap();
    }
    let receipt = fixture.ok(&[
        "terminal",
        "launch",
        "--config-dir",
        directory.to_str().unwrap(),
        "--target",
        target.to_str().unwrap(),
        "--harness",
        "shell",
        "--apply",
    ]);
    let id = receipt["terminal"]["session"].as_str().unwrap();
    let generation = receipt["terminal"]["terminal_id"].as_str().unwrap();
    let capture: Capture =
        serde_json::from_value(fixture.ok(&["terminal-native", "capture", id, generation]))
            .unwrap();
    fixture.sessions.push(capture.session.clone());
    assert_eq!((capture.session.cols, capture.session.rows), (93, 31));
    start.splice(4..4, ["--cols", "99", "--rows", "28"]);
    let explicit: SessionInfo = serde_json::from_value(fixture.ok(&start)).unwrap();
    fixture.sessions.push(explicit.clone());
    assert_eq!((explicit.cols, explicit.rows), (99, 28));
    let missing = fixture.root.join("missing");
    let refused = fixture.run(&[
        "terminal-native",
        "start",
        "--config-dir",
        missing.to_str().unwrap(),
        "--",
        "/bin/sh",
    ]);
    assert!(!refused.status.success());
    let listed = fixture.ok(&["terminal-native", "list"]);
    assert_eq!(listed.as_array().unwrap().len(), 3);
}
#[test]
fn detached_shell_is_global_and_stream_control_has_exact_identity() {
    let mut fixture = Fixture::new();
    let info = fixture.start();
    fs::create_dir(fixture.root.join("another-project")).unwrap();
    let listed = fixture
        .command()
        .current_dir(fixture.root.join("another-project"))
        .args(["terminal-native", "list"])
        .output()
        .unwrap();
    assert!(listed.status.success());
    let listed: Vec<SessionInfo> = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed[0].id, info.id);
    let mut control = Stream::new(&fixture, &info, "control");
    control.frame();
    let mut observer = Stream::new(&fixture, &info, "observe");
    observer.frame();
    let blocked = fixture.run(&[
        "terminal-native",
        "input",
        &info.id,
        &info.generation,
        "--text",
        "do not send",
    ]);
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("attached client"));
    control.send(json!({"type":"terminal.resize","cols":94,"rows":32}));
    control.send(json!({"type":"terminal.input","bytes":STANDARD.encode(b"printf '\\033[38;2;40;80;120mSTREAM_%s\\033[0m\\n' 'READY'; stty size\r")}));
    let capture = fixture.wait_text(&info, "32 94");
    assert!(capture.text.contains("STREAM_READY"));
    assert!(capture.ansi.contains("38;2;40;80;120"));
    assert!(
        !fixture
            .run(&["terminal-native", "close", &info.id, &"0".repeat(64)])
            .status
            .success()
    );
    observer.stop();
    control.stop();
    fixture.ok(&[
        "terminal-native",
        "input",
        &info.id,
        &info.generation,
        "--text",
        "printf 'AFTER_%s\\n' DETACH\r",
    ]);
    fixture.wait_text(&info, "AFTER_DETACH");
    fixture.ok(&["terminal-native", "close", &info.id, &info.generation]);
    let deadline = Instant::now() + Duration::from_secs(3);
    while !fixture
        .ok(&["terminal-native", "list"])
        .as_array()
        .unwrap()
        .is_empty()
    {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
}
#[test]
fn shell_wrapper_detaches_without_killing_its_shell() {
    let mut fixture = Fixture::new();
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = portable_pty::CommandBuilder::new(env!("CARGO_BIN_EXE_daedalus-tui"));
    command.args([
        "shell",
        "--name",
        "Wrapper fixture",
        "--workspace",
        "Beta",
        "--",
        "/bin/sh",
    ]);
    command.env_clear();
    command.env("HOME", &fixture.root);
    command.env("PATH", "/usr/bin:/bin");
    command.env("SHELL", "/bin/sh");
    command.env("TERM", "xterm-256color");
    command.cwd(&fixture.root);
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    thread::spawn(move || {
        let mut bytes = [0; 4096];
        while let Ok(n) = reader.read(&mut bytes) {
            if n == 0 {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(6);
    let info = loop {
        let listed = fixture.ok(&["terminal-native", "list"]);
        if let Some(info) = listed.as_array().unwrap().first() {
            break serde_json::from_value::<SessionInfo>(info.clone()).unwrap();
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(30));
    };
    fixture.sessions.push(info.clone());
    // Wait until attach has acquired input ownership and entered its alternate screen.
    while !fixture.ok(&["terminal-native","capture",&info.id,&info.generation])["session"]["controlled"].as_bool().unwrap(){assert!(Instant::now()<deadline);thread::sleep(Duration::from_millis(30));}
    thread::sleep(Duration::from_millis(100));
    writer.write_all(b"\x1dd").unwrap();
    writer.flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("Wrapper did not detach");
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        fixture
            .ok(&["terminal-native", "list"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fixture.ok(&[
        "terminal-native",
        "input",
        &info.id,
        &info.generation,
        "--text",
        "printf 'WRAPPER_%s\\n' ALIVE\r",
    ]);
    fixture.wait_text(&info, "WRAPPER_ALIVE");
}

#[test]
fn noninteractive_shell_wrapper_does_not_leave_an_unwanted_session() {
    let fixture = Fixture::new();
    let output = fixture.run(&["shell"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("interactive terminal"));
    assert!(
        fixture
            .ok(&["terminal-native", "list"])
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn help_and_invalid_options_never_start_a_terminal() {
    let fixture = Fixture::new();
    for args in [
        vec!["shell", "--help"],
        vec!["shell", "-h"],
        vec!["terminal-native", "start", "--help"],
        vec!["terminal-native", "attach", "--help"],
    ] {
        let output = fixture.run(&args);
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("Ctrl+]"));
    }
    for args in [
        vec!["terminal-native", "start", "--unknown"],
        vec!["terminal-native", "start", "--name"],
        vec!["terminal-native", "start", "--cwd", "--name", "Shell"],
        vec!["terminal-native", "start", "--name", "One", "--name", "Two"],
        vec!["terminal-native", "nonsense"],
    ] {
        assert!(!fixture.run(&args).status.success());
    }
    assert!(
        fixture
            .ok(&["terminal-native", "list"])
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn wrapper_reports_normal_exit_and_nonzero_exit_without_hiding_daemon_failure() {
    for outcome in ["zero", "nonzero", "daemon-failure"] {
        wrapper_exit(outcome);
    }
}
fn wrapper_exit(outcome: &str) {
    let mut fixture = Fixture::new();
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = portable_pty::CommandBuilder::new(env!("CARGO_BIN_EXE_daedalus-tui"));
    command.args([
        "shell",
        "--name",
        "Exit fixture",
        "--workspace",
        "Exit tests",
        "--",
        "/bin/sh",
    ]);
    command.env_clear();
    for (key, value) in [
        ("PATH", "/usr/bin:/bin"),
        ("SHELL", "/bin/sh"),
        ("TERM", "xterm-256color"),
    ] {
        command.env(key, value);
    }
    command.env("HOME", &fixture.root);
    command.cwd(&fixture.root);
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = [0; 4096];
        while let Ok(n) = reader.read(&mut bytes) {
            if n == 0 {
                break;
            }
            let _ = tx.send(bytes[..n].to_vec());
        }
    });
    let deadline = Instant::now() + Duration::from_secs(6);
    let info = loop {
        let listed = fixture.ok(&["terminal-native", "list"]);
        if let Some(info) = listed.as_array().unwrap().first() {
            let info: SessionInfo = serde_json::from_value(info.clone()).unwrap();
            if info.controlled {
                break info;
            }
        }
        assert!(Instant::now() < deadline, "Wrapper failed to attach");
        thread::sleep(Duration::from_millis(25));
    };
    fixture.sessions.push(info.clone());
    if outcome == "daemon-failure" {
        #[cfg(target_os = "linux")]
        {
            // Only the daemon parent of this fixture's exact shell is signalled.
            let stat = fs::read_to_string(format!("/proc/{}/stat", info.pid)).unwrap();
            let parent = stat
                .rsplit_once(')')
                .unwrap()
                .1
                .split_whitespace()
                .nth(1)
                .unwrap()
                .parse::<i32>()
                .unwrap();
            assert!(parent > 1);
            assert_eq!(unsafe { libc::kill(parent, libc::SIGKILL) }, 0);
        }
        #[cfg(not(target_os = "linux"))]
        {
            writer.write_all(b"exit 7\r").unwrap();
            writer.flush().unwrap();
        }
    } else {
        writer
            .write_all(if outcome == "zero" {
                b"exit 0\r"
            } else {
                b"exit 7\r"
            })
            .unwrap();
        writer.flush().unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(6);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("Wrapper failed to finish: {outcome}");
        }
        thread::sleep(Duration::from_millis(20));
    };
    let mut output = Vec::new();
    while let Ok(bytes) = rx.recv_timeout(Duration::from_millis(100)) {
        output.extend(bytes);
    }
    let output = String::from_utf8_lossy(&output);
    if outcome == "zero" {
        assert!(status.success(), "{output}");
        assert!(!output.contains("unavailable"), "{output}");
    } else {
        assert!(
            !status.success(),
            "{outcome} must not be reported as a successful exit"
        );
        if outcome == "nonzero" {
            assert!(output.contains("exited with status 7"), "{output}");
        }
    }
    assert!(
        fixture
            .ok(&["terminal-native", "list"])
            .as_array()
            .unwrap()
            .is_empty(),
        "Exited terminals must disappear even while completion is draining"
    );
}

#[test]
fn discovery_and_failed_reads_do_not_create_user_state() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .ok(&["terminal-native", "list"])
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !fixture
            .run(&[
                "terminal-native",
                "capture",
                &"0".repeat(64),
                &"1".repeat(64)
            ])
            .status
            .success()
    );
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
}
