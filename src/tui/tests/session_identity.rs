//! Real isolated daemon/PTY identity transport. The provider is an inert local
//! executable: no accounts, hooks, terminal history or model calls are used.
use daedalus_tui::{
    adapters::{native, terminals},
    app::native_terminal::SessionInfo,
};
use std::{fs, path::PathBuf, process::Command};

struct Fixture {
    root: PathBuf,
    sessions: Vec<SessionInfo>,
}
impl Fixture {
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_daedalus-tui"));
        command
            .env_clear()
            .env("HOME", &self.root)
            .env("LOCALAPPDATA", &self.root)
            .env("USERPROFILE", &self.root)
            .env("TERM", "xterm-256color")
            .current_dir(&self.root);
        #[cfg(unix)]
        command.env("PATH", "/usr/bin:/bin");
        #[cfg(windows)]
        if let Some(system) = std::env::var_os("SystemRoot") {
            command
                .env("PATH", PathBuf::from(&system).join("System32"))
                .env("SystemRoot", system);
        }
        command
    }
    fn start(&mut self, args: &[&str]) -> SessionInfo {
        self.start_harness("codex", args)
    }
    fn start_harness(&mut self, harness: &str, args: &[&str]) -> SessionInfo {
        let mut command = self.command();
        command
            .args([
                "terminal-native",
                "start",
                "--name",
                "Fixture",
                "--workspace",
                "Private",
                "--",
            ])
            .arg(self.root.join(if cfg!(windows) {
                format!("{harness}.exe")
            } else {
                harness.into()
            }))
            .args(args);
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let info: SessionInfo = serde_json::from_slice(&output.stdout).unwrap();
        self.sessions.push(info.clone());
        info
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for session in &self.sessions {
            let _ = self
                .command()
                .args(["terminal-native", "close", &session.id, &session.generation])
                .output();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[test]
fn native_daemon_reports_only_explicit_conversation_identity_on_linux_and_windows() {
    let mut nonce = [0; 16];
    daedalus_tui::platform::random_bytes(&mut nonce).unwrap();
    let hex: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let root = std::env::temp_dir().join(format!("daedalus-identity-{hex}"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    }
    #[cfg(windows)]
    daedalus_tui::platform::ensure_private_dir(&root).unwrap();
    let mut fixture = Fixture {
        root,
        sessions: Vec::new(),
    };
    let source = fixture.root.join("provider.rs");
    fs::write(
        &source,
        "fn main() { loop { std::thread::sleep(std::time::Duration::from_secs(1)); } }",
    )
    .unwrap();
    let provider = fixture
        .root
        .join(if cfg!(windows) { "Codex.exe" } else { "codex" });
    let compile = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .args(["--edition=2024", "--crate-name", "identity_fixture"])
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
    const SID: &str = "019a043e-1234-7000-8000-000000000001";
    let info = fixture.start(&["resume", SID, "--model", "private-argument-not-metadata"]);
    assert_eq!(terminals::reported_session(&native::pane(&info)), Some(SID));
    let wire = serde_json::to_string(&info).unwrap();
    assert!(!wire.contains("private-argument-not-metadata"));
    assert!(!wire.contains("argv") && !wire.contains("environment"));
    let output = fixture
        .command()
        .args(["terminal-native", "list"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let refreshed: SessionInfo = serde_json::from_value(
        listed
            .as_array()
            .unwrap()
            .iter()
            .find(|session| session["id"] == info.id)
            .unwrap()
            .clone(),
    )
    .unwrap();
    assert_eq!(
        terminals::reported_session(&native::pane(&refreshed)),
        Some(SID)
    );
    let fork = fixture.start(&["fork", SID]);
    assert!(fork.launch_session.is_none());
    assert!(terminals::reported_session(&native::pane(&fork)).is_none());
    let fresh = fixture.start(&[]);
    assert!(fresh.launch_session.is_none());
    for (harness, flag, id) in [
        ("pi", "--session-id", "Owned.Custom-ID"),
        ("cursor-agent", "--resume", SID),
        ("hermes", "--resume", "20261104_010203_a1b2c3d4e5f6"),
    ] {
        let alias = fixture.root.join(if cfg!(windows) {
            format!("{harness}.exe")
        } else {
            harness.into()
        });
        fs::copy(&provider, &alias).unwrap();
        let session = fixture.start_harness(
            harness,
            &[flag, id, "--model", "private-argument-not-metadata"],
        );
        assert_eq!(
            terminals::reported_session(&native::pane(&session)),
            Some(id),
            "{harness}"
        );
        let wire = serde_json::to_string(&session).unwrap();
        assert!(!wire.contains("private-argument-not-metadata") && !wire.contains("argv"));
        let named = fixture.start_harness(harness, &["--resume", "prompt-or-picker-name"]);
        assert!(named.launch_session.is_none(), "{harness}");
        let value = fixture.start_harness(harness, &["--model", "--resume", id]);
        assert!(value.launch_session.is_none(), "{harness}");
        if harness == "pi" {
            let fork = fixture.start_harness(harness, &["--fork", SID]);
            assert!(fork.launch_session.is_none());
        }
    }
}
