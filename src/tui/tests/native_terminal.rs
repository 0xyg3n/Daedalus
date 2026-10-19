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
