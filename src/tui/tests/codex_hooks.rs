#![cfg(target_os = "linux")]
//! Real owned native processes and a private PTY; the Herdr API is a local fixture.
//! No live harness, terminal server, account, model or external message is used.
use daedalus_tui::{
    adapters::terminals::process_start,
    app::hook_runtime::{self, Registration},
};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            fs::{DirBuilderExt, PermissionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    children: Vec<Child>,
    _pty: File,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn quote(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}
fn wait_file(path: &Path, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !path.exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "fixture exited before {}",
            path.display()
        );
        assert!(
            Instant::now() < deadline,
            "fixture timed out: {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn native_codex_daemon_and_terminal_hooks_refresh_only_their_proved_binding() {
    let root = std::env::temp_dir().join(format!(
        "daedalus-native-codex-hook-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let (mut master, mut slave) = (-1, -1);
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        },
        0
    );
    let master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    for fd in [&master, &slave] {
        assert_eq!(
            unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) },
            0
        );
    }
    let mut fixture = Fixture {
        root: root.clone(),
        children: Vec::new(),
        _pty: master,
    };
    let source = root.join("fixture.rs");
    let codex = root.join("codex");
    fs::write(&source, include_str!("fixtures/codex_hook.rs")).unwrap();
    let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let compile = Command::new(compiler)
        .args(["--edition=2024", "--crate-name", "codex_hook_fixture"])
        .arg(&source)
        .arg("-o")
        .arg(&codex)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "fixture compiler: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
    let directory = root.join("instance");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let registration:Registration=serde_json::from_value(json!({
        "version":1,"id":"codex","directory":directory,"harness_process":{"pid":99,"start_ticks":1},
        "server_emitter":null,"transcript":null,"goal_id":null,
        "scope":{"instance":"fixture","harness":"codex","conversation":"019a043e-1234-7000-8000-000000000001",
          "terminal":{"backend":"herdr","endpoint":"fixture","pane":"pane","generation":"old-generation"}},
        "events":["PostToolUse"],"options":{"telegram_delivery":false,"telegram_progress":false,"notifications":false,"hindsight":false},
        "activity_lease_ms":30000,"pending_timeout_ms":30000,"hook_timeout_ms":12000
    })).unwrap();
    let registration_path = hook_runtime::install(&registration).unwrap();
    let herdr = root.join("herdr");
    fs::write(&herdr,format!("#!/bin/sh\n[ \"$1\" = --session ] && [ \"$2\" = fixture ] && [ \"$3\" = pane ] || exit 1\ncase \"$4\" in\nget) /bin/cat {};;\nprocess-info) /bin/cat {};;\n*) exit 1;;\nesac\n",quote(&root.join("pane.json")),quote(&root.join("info.json")))).unwrap();
    fs::set_permissions(&herdr, fs::Permissions::from_mode(0o700)).unwrap();
    let payload = root.join("payload.json");
    fs::write(&payload,json!({"hook_event_name":"PostToolUse","session_id":"019a043e-1234-7000-8000-000000000001","tool_use_id":"daemon-tool"}).to_string()).unwrap();
    let fields = |name: &str| {
        vec![
            root.join(format!("{name}.run")),
            PathBuf::from(env!("CARGO_BIN_EXE_daedalus-tui")),
            registration_path.clone(),
            payload.clone(),
            root.join(format!("{name}.receipt")),
            root.join(format!("{name}.error")),
            root.join(format!("{name}.quit")),
        ]
    };
    let mut client = Command::new(&codex);
    client
        .args([
            "resume",
            "019a043e-1234-7000-8000-000000000001",
            "--fixture-hook",
        ])
        .args(fields("client"))
        .env_clear()
        .env("HOME", &root)
        .env("PATH", "/nonexistent")
        .env("DAEDALUS_HERDR_BIN", &herdr)
        .env("HERDR_SESSION", "fixture")
        .env("HERDR_PANE_ID", "pane")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave));
    unsafe {
        client.pre_exec(|| {
            if libc::setsid() < 0
                || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0
                || libc::tcsetpgrp(0, libc::getpid()) < 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    fixture.children.push(client.spawn().unwrap());
    let client_pid = fixture.children[0].id();
    let pane = json!({"pane_id":"pane","terminal_id":"new-generation","agent_session":{"kind":"id","value":"019a043e-1234-7000-8000-000000000001"}});
    fs::write(root.join("pane.json"), json!({"pane":pane}).to_string()).unwrap();
    fs::write(root.join("info.json"),json!({"process_info":{"pane_id":"pane","shell_pid":client_pid,"foreground_processes":[{"pid":client_pid}]}}).to_string()).unwrap();
    let mut daemon = Command::new(&codex);
    daemon
        .args(["app-server", "--managed-daemon", "--fixture-hook"])
        .args(fields("daemon"))
        .env_clear()
        .env("HOME", &root)
        .env("PATH", "/nonexistent")
        .env("DAEDALUS_HERDR_BIN", &herdr)
        .env("HERDR_SESSION", "stale-server")
        .env("HERDR_PANE_ID", "stale-pane")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        daemon.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    fixture.children.push(daemon.spawn().unwrap());
    let daemon_pid = fixture.children[1].id();
    fs::write(root.join("daemon.run"), b"run").unwrap();
    wait_file(&root.join("daemon.receipt"), &mut fixture.children[1]);
    let receipt = fs::read_to_string(root.join("daemon.receipt")).unwrap();
    assert_eq!(
        receipt,
        "0\n{}\n",
        "native daemon hook failed: {}",
        fs::read_to_string(root.join("daemon.error")).unwrap()
    );
    let bound = hook_runtime::inspect(&registration_path).unwrap();
    assert_eq!(bound.harness_process.as_ref().unwrap().pid, client_pid);
    assert_eq!(
        bound.server_emitter.as_ref().unwrap().process.pid,
        daemon_pid
    );
    assert_eq!(bound.scope.terminal.generation, "new-generation");
    assert_eq!(bound.scope.conversation, registration.scope.conversation);
    assert_eq!(
        bound
            .harness_process
            .as_ref()
            .unwrap()
            .start_ticks
            .to_string(),
        process_start(Path::new("/proc"), &client_pid.to_string()).unwrap()
    );
    // Switch back to a hook emitted directly by the same real PTY client.
    fs::write(&payload,json!({"hook_event_name":"PostToolUse","session_id":"019a043e-1234-7000-8000-000000000001","tool_use_id":"client-tool"}).to_string()).unwrap();
    fs::write(root.join("client.run"), b"run").unwrap();
    wait_file(&root.join("client.receipt"), &mut fixture.children[0]);
    assert_eq!(
        fs::read_to_string(root.join("client.receipt")).unwrap(),
        "0\n{}\n",
        "native terminal hook failed: {}",
        fs::read_to_string(root.join("client.error")).unwrap()
    );
    let bound = hook_runtime::inspect(&registration_path).unwrap();
    assert_eq!(bound.harness_process.as_ref().unwrap().pid, client_pid);
    assert!(bound.server_emitter.is_none());
    let status: Value = serde_json::from_slice(
        &fs::read(directory.join("hook-status/codex-PostToolUse.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(status["state"], "ok");
    let db = rusqlite::Connection::open(directory.join("hook-observations.sqlite3")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM hook_observations WHERE event_id NOT LIKE 'codex-rebind:%'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
}
