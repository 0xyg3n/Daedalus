#![cfg(unix)]
//! The installed executable starts its own backend with Python absent from PATH.
//! PTYs and private state are synthetic; no live terminal server is contacted.
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            fs::{DirBuilderExt, OpenOptionsExt},
            process::CommandExt,
        },
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
fn private_dashboard_file(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path.parent().unwrap())?;
    fs::write(path, bytes)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}
/// Upper bound for the interface to show expected output. Generous because
/// hosted macOS runners start dozens of debug PTY sessions on few cores; a
/// passing test returns as soon as the text appears.
const UI_WAIT: Duration = Duration::from_secs(60);
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Ui {
    command: Command,
    child: Child,
    master: File,
    directory: PathBuf,
    screen: vt100::Parser,
    raw: Vec<u8>,
    stderr: File,
}
impl Ui {
    fn start(demo: bool) -> Self {
        Self::configured(demo, false)
    }
    fn configured(demo: bool, configured: bool) -> Self {
        Self::with_graphics(demo, configured, false)
    }
    fn with_graphics(demo: bool, configured: bool, graphics: bool) -> Self {
        Self::with_preferences(demo, configured, graphics, None)
    }
    fn with_preferences(
        demo: bool,
        configured: bool,
        graphics: bool,
        preferences: Option<&[u8]>,
    ) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "daedalus-ui-startup-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        if configured {
            let state = directory.join(".config/daedalus");
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&state)
                .unwrap();
            let mut config = daedalus_tui::app::config::ControlConfig::fresh(&directory).unwrap();
            config.discovery.automatic = false;
            fs::write(
                state.join("control.json"),
                serde_json::to_vec(&config).unwrap(),
            )
            .unwrap();
            private_dashboard_file(
                &state.join("dashboard.json"),
                preferences.unwrap_or(
                    br#"{"discovery":{"scan_processes":false,"scan_default_roots":false}}"#,
                ),
            )
            .unwrap();
        }
        let (mut master, mut slave) = (-1, -1);
        let size = libc::winsize {
            ws_row: 48,
            ws_col: 160,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                #[cfg(target_os = "macos")]
                {
                    libc::openpty(
                        &mut master,
                        &mut slave,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        &size as *const libc::winsize as *mut libc::winsize,
                    )
                }
                #[cfg(not(target_os = "macos"))]
                {
                    libc::openpty(
                        &mut master,
                        &mut slave,
                        std::ptr::null_mut(),
                        std::ptr::null(),
                        &size,
                    )
                }
            },
            0
        );
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        for file in [&master, &slave] {
            assert_eq!(
                unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) },
                0
            );
        }
        assert_eq!(
            unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
            0
        );
        // Keep diagnostics in an unlinked private file: failures stay inspectable
        // without adding state to the empty-home assertions or blocking on pipes.
        let stderr_path = directory.join("stderr.log");
        let stderr = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&stderr_path)
            .unwrap();
        fs::remove_file(stderr_path).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_daedalus-tui"));
        command
            .env_clear()
            .env("PATH", "/nonexistent")
            .env("HOME", &directory)
            .env("TERM", "xterm-256color")
            .env("DAEDALUS_SPLASH", "0")
            .env("DAEDALUS_UPDATE_CHECK", "0")
            .env("DAEDALUS_GRAPHICS", if graphics { "kitty" } else { "0" })
            .env("DAEDALUS_BACKDROP", if graphics { "1" } else { "0" })
            .env("DAEDALUS_REDUCED_MOTION", "1")
            .env("DAEDALUS_ICONS", if graphics { "unicode" } else { "ascii" })
            .env("COLORTERM", "truecolor")
            .env("DAEDALUS_SCREENSAVER", if graphics { "1" } else { "0" })
            .current_dir(&directory)
            .arg("tui")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave))
            .stderr(Stdio::from(stderr.try_clone().unwrap()));
        if demo {
            command.arg("--demo");
        }
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        Self {
            command,
            child,
            master,
            directory,
            screen: vt100::Parser::new(48, 160, 0),
            raw: Vec::new(),
            stderr,
        }
    }
    fn stderr_text(&mut self) -> String {
        self.stderr.seek(SeekFrom::Start(0)).unwrap();
        let mut text = String::new();
        (&mut self.stderr)
            .take(65536)
            .read_to_string(&mut text)
            .unwrap();
        text
    }
    fn wait_text(&mut self, text: &str) {
        self.wait_presence(text, true);
    }
    fn send(&mut self, bytes: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut offset = 0;
        let mut scratch = [0; 16384];
        while offset < bytes.len() {
            match self.master.write(&bytes[offset..]) {
                Ok(0) => panic!("PTY write returned zero"),
                Ok(count) => offset += count,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    // The child can block writing a large render while the test
                    // is still writing input. Drain output so both sides proceed.
                    loop {
                        match self.master.read(&mut scratch) {
                            Ok(0) => break,
                            Ok(count) => {
                                self.raw.extend_from_slice(&scratch[..count]);
                                self.screen.process(&scratch[..count]);
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => (),
                            Err(error) => panic!("PTY read failed: {error}"),
                        }
                    }
                    assert!(
                        Instant::now() < deadline,
                        "PTY write stalled after {offset} bytes: {}",
                        self.screen.screen().contents()
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("PTY write failed: {error}"),
            }
        }
    }
    fn wait_presence(&mut self, text: &str, present: bool) {
        let deadline = Instant::now() + UI_WAIT;
        let mut bytes = [0; 16384];
        loop {
            match self.master.read(&mut bytes) {
                Ok(count) => {
                    self.raw.extend_from_slice(&bytes[..count]);
                    self.screen.process(&bytes[..count]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => (),
                Err(error) => panic!(
                    "PTY read failed: {error}; {}",
                    self.screen.screen().contents()
                ),
            }
            if self.screen.screen().contents().contains(text) == present {
                return;
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "TUI exited: {}\n{}",
                self.screen.screen().contents(),
                self.stderr_text()
            );
            assert!(
                Instant::now() < deadline,
                "Missing {text:?}: {}",
                self.screen.screen().contents()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn quit(&mut self) {
        self.master.write_all(b"q").unwrap();
        let deadline = Instant::now() + UI_WAIT / 2;
        loop {
            let mut bytes = [0; 16384];
            if let Ok(n) = self.master.read(&mut bytes) {
                self.screen.process(&bytes[..n]);
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            assert!(Instant::now() < deadline, "TUI did not exit after q");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn restart(&mut self) {
        assert!(self.child.try_wait().unwrap().is_some());
        // The old process can leave its final screen/terminal restoration in
        // the PTY queue after exit. It must not satisfy the new process's waits.
        let mut bytes = [0; 16384];
        loop {
            match self.master.read(&mut bytes) {
                Ok(0) => break,
                Ok(_) => (),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.raw_os_error() == Some(libc::EIO) =>
                {
                    break;
                }
                Err(e) => panic!("Cannot drain old PTY output: {e}"),
            }
        }
        self.raw.clear();
        self.screen = vt100::Parser::new(48, 160, 0);
        self.child = self.command.spawn().unwrap();
    }
}
impl Drop for Ui {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.directory);
    }
}
#[test]
fn native_demo_launches_existing_renderer_without_python_or_private_state() {
    let mut ui = Ui::start(true);
    ui.wait_text("project-alpha");
    ui.quit();
    assert_eq!(fs::read_dir(&ui.directory).unwrap().count(), 0);
}

#[test]
fn demo_drag_back_to_anchor_copies_the_current_selection_through_the_real_ui() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let mut ui = Ui::start(true);
    ui.wait_text("project-alpha");
    ui.master.write_all(b"]").unwrap();
    let deadline = Instant::now() + UI_WAIT;
    let (column, row, expected) = loop {
        let needles = [
            "Document the new",
            "cargo test --locked",
            "Review the failing",
            "summarize the diff",
            "explain the retry",
            "migrate the settings",
            "add pagination",
            "summarise this week",
            "rename the config",
            "GET /health",
        ];
        let mut found = None;
        for needle in needles {
            for row in 0..48 {
                for column in 0..160 - needle.len() as u16 {
                    if needle.bytes().enumerate().all(|(offset, byte)| {
                        ui.screen
                            .screen()
                            .cell(row, column + offset as u16)
                            .is_some_and(|cell| cell.contents().as_bytes() == [byte])
                    }) {
                        found = Some((column, row, needle.as_bytes()[0]));
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        if let Some(found) = found {
            break found;
        }
        assert!(
            Instant::now() < deadline,
            "No demo preview: {}",
            ui.screen.screen().contents()
        );
        assert!(ui.child.try_wait().unwrap().is_none());
        ui.wait_presence("impossible-selection-fixture-marker", false);
        std::thread::sleep(Duration::from_millis(20));
    };
    let begin = ui.raw.len();
    // Shift selects locally; these bytes go only to this owned demo PTY.
    let (x, y) = (column + 1, row + 1);
    ui.master
        .write_all(
            format!(
                "\x1b[<4;{x};{y}M\x1b[<36;{};{y}M\x1b[<36;{x};{y}M\x1b[<4;{x};{y}m\x03",
                x + 6,
            )
            .as_bytes(),
        )
        .unwrap();
    ui.wait_text("Copy request sent to terminal");
    let output = &ui.raw[begin..];
    let prefix = b"\x1b]52;c;";
    let start = output
        .windows(prefix.len())
        .position(|part| part == prefix)
        .expect("No OSC 52 copy request")
        + prefix.len();
    let end = output[start..]
        .windows(2)
        .position(|part| part == b"\x1b\\")
        .expect("Incomplete OSC 52 copy request")
        + start;
    assert_eq!(STANDARD.decode(&output[start..end]).unwrap(), [expected]);
    ui.quit();
    assert_eq!(fs::read_dir(&ui.directory).unwrap().count(), 0);
}

#[test]
fn fresh_native_launch_opens_existing_setup_and_cancel_writes_nothing() {
    let mut ui = Ui::start(false);
    ui.wait_text("Set up Daedalus");
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_text("Setup cancelled");
    ui.quit();
    assert_eq!(fs::read_dir(&ui.directory).unwrap().count(), 0);
}

#[test]
fn existing_configuration_launches_native_workspace_without_setup() {
    let mut ui = Ui::configured(false, true);
    let config = ui.directory.join(".config/daedalus/control.json");
    let before = fs::read(&config).unwrap();
    ui.wait_text("LIVE TOPOLOGY");
    ui.wait_text("0 terminals");
    assert!(!ui.screen.screen().contents().contains("Set up Daedalus"));
    ui.quit();
    assert_eq!(fs::read(config).unwrap(), before);
}

#[test]
fn corrupt_dashboard_does_not_prevent_startup_or_overwrite_preferences() {
    let corrupt = b"{broken-DASHBOARD_PRIVATE_SENTINEL";
    let mut ui = Ui::with_preferences(false, true, false, Some(corrupt));
    ui.wait_text("LIVE TOPOLOGY");
    ui.wait_text("1 warnings");
    ui.wait_text("Dashboard preferences unavailable");
    let directory = ui.directory.join(".config/daedalus");
    let path = directory.join("dashboard.json");
    let control = fs::read(directory.join("control.json")).unwrap();
    assert_eq!(
        fs::read(ui.directory.join(".config/daedalus/dashboard.json")).unwrap(),
        corrupt
    );
    ui.master
        .write_all(b"\x10Dashboard and instance discovery\r\r")
        .unwrap();
    ui.wait_text("Recover dashboard preferences?");
    assert!(
        !ui.screen
            .screen()
            .contents()
            .contains("DASHBOARD_PRIVATE_SENTINEL")
    );
    assert_eq!(fs::read(&path).unwrap(), corrupt);
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Dashboard preferences unchanged");
    ui.wait_presence("UPDATING", false);
    assert_eq!(fs::read(&path).unwrap(), corrupt);
    ui.master
        .write_all(b"\x10Dashboard and instance discovery\r\r")
        .unwrap();
    ui.wait_text("Recover dashboard preferences?");
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Dashboard preferences recovered");
    ui.wait_text("0 warnings");
    let state = daedalus_tui::app::dashboard::State::load(&directory).unwrap();
    assert!(!state.settings.scan_processes);
    assert!(!state.settings.scan_default_roots);
    let backups = fs::read_dir(&directory)
        .unwrap()
        .filter_map(|entry| {
            let p = entry.unwrap().path();
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("dashboard.backup-")
                .then_some(p)
        })
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(&backups[0]).unwrap(), corrupt);
    assert_eq!(fs::read(directory.join("control.json")).unwrap(), control);
    assert!(!directory.join("delivery.sqlite3").exists());
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    ui.master
        .write_all(b"\x10Dashboard and instance discovery\r\r")
        .unwrap();
    ui.wait_text("Global preferences:");
    assert!(
        !ui.screen
            .screen()
            .contents()
            .contains("Recover dashboard preferences?")
    );
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_presence("Global preferences:", false);
    ui.quit();
}

#[test]
fn control_history_settings_pages_scope_and_archive_use_the_real_private_ui() {
    use daedalus_tui::app::history::State;
    let mut ui = Ui::configured(false, true);
    ui.wait_text("LIVE TOPOLOGY");
    ui.quit();
    let global = ui.directory.join(".config/daedalus");
    let active = ui.directory.join("managed-instance");
    let config = fs::read(global.join("control.json")).unwrap();
    private_dashboard_file(&active.join("control.json"), &config).unwrap();
    let receipts = serde_json::to_vec(&serde_json::json!([
        {"version":1,"operation":"a","action":"account","state":"unconfirmed","time":1.0},
        {"version":1,"operation":"b","action":"terminal","state":"submitted","time":2.0},
        {"version":1,"operation":"c","action":"configure","state":"completed","time":3.0}
    ]))
    .unwrap();
    private_dashboard_file(&active.join("control-audit.json"), &receipts).unwrap();
    ui.command.arg("--config-dir").arg(&active);
    ui.restart();
    ui.wait_text("0 terminals");
    ui.master
        .write_all(b"\x10Control action history\r\r")
        .unwrap();
    ui.wait_text("Current instance history:");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Saved value: 1000");
    ui.master.write_all(b"\x151\r").unwrap();
    ui.wait_text("must be between 2 and 8192");
    ui.master.write_all(b"\x158\r").unwrap();
    ui.wait_text("Save history settings?");
    assert!(!active.join("control-history.json").exists());
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Control history unchanged");
    ui.wait_presence("UPDATING", false);
    assert!(!active.join("control-history.json").exists());
    ui.master
        .write_all(b"\x10Control action history\r\r")
        .unwrap();
    ui.wait_text("Current instance history:");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Saved value: 1000");
    ui.master.write_all(b"\x158\r").unwrap();
    ui.wait_text("Save history settings?");
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("History settings saved");
    assert_eq!(State::load(&active).unwrap().settings.entry_limit, 8);
    assert!(!global.join("control-history.json").exists());
    ui.master
        .write_all(b"\x10Control action history\r\r")
        .unwrap();
    ui.wait_text("Current instance history:");
    ui.master.write_all(b"\x1b[B\x1b[B\r").unwrap();
    ui.wait_text("Saved value: 50");
    ui.master.write_all(b"\x151\r").unwrap();
    ui.wait_text("Save history settings?");
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("History settings saved");
    ui.master
        .write_all(b"\x10Control action history\r\r")
        .unwrap();
    ui.wait_text("Current instance history:");
    ui.master.write_all(b"\x1b[F\r").unwrap();
    ui.wait_text("Global dashboard history:");
    assert!(!global.join("control-history.json").exists());
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_text("Control history unchanged");
    ui.wait_presence("UPDATING", false);
    ui.master
        .write_all(b"\x10Control action history\r\r")
        .unwrap();
    ui.wait_text("Current instance history:");
    for _ in 0..5 {
        ui.master.write_all(b"\x1b[B").unwrap();
    }
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Unresolved control outcomes");
    ui.wait_text("submitted");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Operation: b");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Unresolved control outcomes");
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("unconfirmed");
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_text("Control history unchanged");
    ui.wait_presence("UPDATING", false);
    for apply in [false, true] {
        ui.master
            .write_all(b"\x10Control action history\r\r")
            .unwrap();
        ui.wait_text("Current instance history:");
        for _ in 0..6 {
            ui.master.write_all(b"\x1b[B").unwrap();
        }
        ui.master.write_all(b"\r").unwrap();
        ui.wait_text("Archive control receipts?");
        assert_eq!(
            fs::read(active.join("control-audit.json")).unwrap(),
            receipts
        );
        if apply {
            ui.master.write_all(b"\x1b[B\r").unwrap();
            ui.wait_text("Control history archived");
        } else {
            ui.master.write_all(b"\r").unwrap();
            ui.wait_text("Control history unchanged");
            ui.wait_presence("UPDATING", false);
        }
    }
    assert_eq!(fs::read(active.join("control-audit.json")).unwrap(), b"[]");
    let archives = fs::read_dir(&active)
        .unwrap()
        .filter_map(|entry| {
            let p = entry.unwrap().path();
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("control-audit.archive-")
                .then_some(p)
        })
        .collect::<Vec<_>>();
    assert_eq!(archives.len(), 1);
    assert_eq!(fs::read(&archives[0]).unwrap(), receipts);
    assert_eq!(fs::read(active.join("control.json")).unwrap(), config);
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    ui.master
        .write_all(b"\x10Control action history\r\r")
        .unwrap();
    ui.wait_text("Current instance history:");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Saved value: 8");
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_text("Control history unchanged");
    ui.quit();
}

#[test]
fn history_policy_recovery_and_saved_archive_review_are_private_and_explicit_in_ui() {
    use daedalus_tui::app::history::State;
    let mut ui = Ui::configured(false, true);
    ui.wait_text("LIVE TOPOLOGY");
    let directory = ui.directory.join(".config/daedalus");
    let original = b"{broken-HISTORY_PRIVATE_SENTINEL";
    private_dashboard_file(&directory.join("control-history.json"), original).unwrap();
    let receipts = serde_json::to_vec(&serde_json::json!([
        {"version":1,"operation":"a","action":"account","state":"unconfirmed","time":1.0},
        {"version":1,"operation":"b","action":"terminal","state":"submitted","time":2.0}
    ]))
    .unwrap();
    private_dashboard_file(&directory.join("control-audit.json"), &receipts).unwrap();
    let control = fs::read(directory.join("control.json")).unwrap();
    for apply in [false, true] {
        ui.master
            .write_all(b"\x10Control action history\r\r")
            .unwrap();
        ui.wait_text("unsaved recovery draft");
        assert!(
            !ui.screen
                .screen()
                .contents()
                .contains("HISTORY_PRIVATE_SENTINEL")
        );
        ui.master.write_all(b"\r").unwrap();
        ui.wait_text("Recovery draft value: 1000");
        ui.master.write_all(b"\x158\r").unwrap();
        ui.wait_text("unsaved recovery draft");
        assert_eq!(
            fs::read(directory.join("control-history.json")).unwrap(),
            original
        );
        for _ in 0..4 {
            ui.master.write_all(b"\x1b[B").unwrap();
        }
        ui.master.write_all(b"\r").unwrap();
        ui.wait_text("Recover history policy?");
        assert_eq!(
            fs::read(directory.join("control-history.json")).unwrap(),
            original
        );
        if apply {
            ui.master.write_all(b"\x1b[B\r").unwrap();
            ui.wait_text("History policy recovered");
        } else {
            ui.master.write_all(b"\r").unwrap();
            ui.wait_text("Control history unchanged");
            ui.wait_presence("UPDATING", false);
            assert_eq!(
                fs::read(directory.join("control-history.json")).unwrap(),
                original
            );
        }
    }
    assert_eq!(State::load(&directory).unwrap().settings.entry_limit, 8);
    assert_eq!(
        fs::read(directory.join("control-audit.json")).unwrap(),
        receipts
    );
    let backups = fs::read_dir(&directory)
        .unwrap()
        .filter_map(|entry| {
            let p = entry.unwrap().path();
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("control-history.backup-")
                .then_some(p)
        })
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(&backups[0]).unwrap(), original);
    ui.master
        .write_all(b"\x10Control action history\r\r")
        .unwrap();
    ui.wait_text("Current instance history:");
    for _ in 0..6 {
        ui.master.write_all(b"\x1b[B").unwrap();
    }
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Archive control receipts?");
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Control history archived");
    let archived = fs::read_dir(&directory)
        .unwrap()
        .filter_map(|entry| {
            let p = entry.unwrap().path();
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("control-audit.archive-")
                .then_some(p)
        })
        .collect::<Vec<_>>();
    assert_eq!(archived.len(), 1);
    assert_eq!(fs::read(&archived[0]).unwrap(), receipts);
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    ui.master
        .write_all(b"\x10Control action history\r\r")
        .unwrap();
    ui.wait_text("Current instance history:");
    for _ in 0..7 {
        ui.master.write_all(b"\x1b[B").unwrap();
    }
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Saved receipt archives");
    ui.wait_text("Select an archive to inspect");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Archived control receipts");
    ui.wait_text("submitted");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Source: archive");
    ui.wait_text("Operation: b");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Archived control receipts");
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_text("Control history unchanged");
    assert_eq!(fs::read(directory.join("control.json")).unwrap(), control);
    assert_eq!(fs::read(&archived[0]).unwrap(), receipts);
    assert_eq!(
        fs::read(directory.join("control-audit.json")).unwrap(),
        b"[]"
    );
    ui.quit();
}

#[test]
fn interface_settings_are_reviewed_saved_and_survive_reopen() {
    let mut ui = Ui::configured(false, true);
    let path = ui.directory.join(".config/daedalus/control.json");
    let before = fs::read(&path).unwrap();
    ui.wait_text("LIVE TOPOLOGY");
    ui.master.write_all(b"\x10Interface settings").unwrap();
    ui.wait_text("Interface settings");
    ui.master.write_all(b"\r\r").unwrap();
    ui.wait_text("Saved in this configuration");
    // First field: colors. Choose Off, then review before confirming.
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Saved value: On");
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Save colors:");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Configuration saved privately");
    let saved = fs::read(&path).unwrap();
    let config = daedalus_tui::app::config::ControlConfig::parse(&saved).unwrap();
    assert!(!config.ui.colors);
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    assert_eq!(fs::read(&path).unwrap(), saved);
    ui.quit();
}

#[test]
fn agent_session_ids_reach_real_views_and_visibility_is_reviewed_saved_and_reopened() {
    use daedalus_tui::app::{config::ControlConfig, interface_settings::SETTINGS};
    use std::os::unix::fs::PermissionsExt;
    fn toggle(ui: &mut Ui, saved: &str, value: bool) {
        let path = ui.directory.join(".config/daedalus/control.json");
        let before = fs::read(&path).unwrap();
        ui.master.write_all(b"\x10Interface settings").unwrap();
        ui.wait_text("Interface settings_");
        ui.wait_presence("UPDATING", false);
        ui.master.write_all(b"\r").unwrap();
        ui.wait_presence("Interface settings_", false);
        ui.master.write_all(b"\r").unwrap();
        ui.wait_text("Saved in this configuration");
        let index = SETTINGS
            .iter()
            .position(|s| s.key == "agent_session_ids")
            .unwrap();
        for _ in 0..index {
            ui.master.write_all(b"\x1b[B").unwrap();
        }
        ui.master.write_all(b"\r").unwrap();
        ui.wait_text(&format!("Saved value: {saved}"));
        ui.master.write_all(b"\x1b[H").unwrap();
        if !value {
            ui.master.write_all(b"\x1b[B").unwrap();
        }
        ui.master.write_all(b"\r").unwrap();
        ui.wait_text("Save agent_session_ids:");
        assert_eq!(fs::read(&path).unwrap(), before);
        ui.master.write_all(b"\x1b[B\r").unwrap();
        ui.wait_text("Configuration saved privately");
        assert_eq!(
            ControlConfig::parse(&fs::read(&path).unwrap())
                .unwrap()
                .ui
                .agent_session_ids,
            value
        );
    }
    let mut ui = Ui::configured(false, true);
    ui.wait_text("LIVE TOPOLOGY");
    ui.quit();
    let path = ui.directory.join(".config/daedalus/control.json");
    let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    config.discovery.herdr_sessions = vec!["fixture".into()];
    fs::write(&path, config.encode().unwrap()).unwrap();
    let payload = ui.directory.join("session-payload.json");
    let bytes = serde_json::to_vec(&serde_json::json!({"result":{"panes":[
        {"pane_id":"a", "terminal_id":"terminal-a-generation", "workspace_id":"work", "tab_id":"tab", "title":"Fixture agent A", "cwd":ui.directory, "agent":"codex", "agent_session":{"agent":"codex", "kind":"id", "value":"agent-session-alpha-0123456789"}},
        {"pane_id":"b", "terminal_id":"terminal-b-generation", "workspace_id":"work", "tab_id":"tab", "title":"Fixture agent B", "cwd":ui.directory, "agent":"grok"}
    ]}})).unwrap();
    fs::write(&payload, &bytes).unwrap();
    let script = ui.directory.join("inert-herdr");
    fs::write(
        &script,
        "#!/bin/sh\nexec /bin/cat \"$SESSION_VIEW_FIXTURE_PAYLOAD\"\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    ui.command
        .env("DAEDALUS_HERDR_BIN", &script)
        .env("SESSION_VIEW_FIXTURE_PAYLOAD", &payload);
    ui.restart();
    ui.wait_text("2 terminals");
    ui.master.write_all(b"/Fixture agent A\r").unwrap();
    ui.wait_text("SID: agent-session-alpha-0123456789");
    for view in ["Tiles", "Wall", "Cube"] {
        ui.master.write_all(b"]").unwrap();
        ui.wait_text(&format!("Workspaces > {view}"));
        ui.wait_text("SID: agent-session-alpha-0123456789");
    }
    toggle(&mut ui, "On", false);
    ui.master.write_all(b"2").unwrap();
    ui.wait_text("Workspaces > Cube");
    ui.wait_presence("SID:", false);
    assert_eq!(fs::read(&payload).unwrap(), bytes);
    ui.quit();
    ui.restart();
    ui.wait_text("2 terminals");
    ui.wait_presence("SID:", false);
    assert!(
        !ControlConfig::parse(&fs::read(&path).unwrap())
            .unwrap()
            .ui
            .agent_session_ids
    );
    toggle(&mut ui, "Off", true);
    ui.master.write_all(b"2").unwrap();
    ui.wait_text("Workspaces > Cube");
    ui.master.write_all(b"/Fixture agent A\r").unwrap();
    ui.wait_text("SID: agent-session-alpha-0123456789");
    ui.master.write_all(b"]").unwrap();
    ui.wait_text("Not connected.");
    ui.wait_text("SID: agent-session-alpha-0123456789");
    ui.wait_presence("UPDATING", false);
    assert_eq!(fs::read(&payload).unwrap(), bytes);
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_text("Workspaces > Cube");
    ui.quit();
}

#[test]
fn real_ui_reports_agent_work_wait_idle_and_reviewed_feedback_visibility() {
    use daedalus_tui::app::{config::ControlConfig, interface_settings::SETTINGS};
    use std::os::unix::fs::PermissionsExt;
    let mut ui = Ui::configured(false, true);
    ui.wait_text("LIVE TOPOLOGY");
    ui.quit();
    let path = ui.directory.join(".config/daedalus/control.json");
    let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    config.discovery.herdr_sessions = vec!["fixture".into()];
    private_dashboard_file(&path, &config.encode().unwrap()).unwrap();
    let payload = ui.directory.join("activity-payload.json");
    let mut data = serde_json::json!({"result":{"panes":[
        {"pane_id":"a","terminal_id":"generation-a","workspace_id":"work","tab_id":"tab","title":"Fixture worker","cwd":ui.directory,"agent":"codex","agent_status":"working","agent_session":{"agent":"codex","kind":"id","value":"12345678-abcd-4000-8000-123456789abc"}},
        {"pane_id":"b","terminal_id":"generation-b","workspace_id":"work","tab_id":"tab","title":"Fixture needs input","cwd":ui.directory,"agent":"claude","agent_status":"blocked","agent_session":{"agent":"claude","kind":"id","value":"22345678-abcd-4000-8000-123456789abc"}},
        {"pane_id":"c","terminal_id":"generation-c","workspace_id":"work","tab_id":"tab","title":"Fixture idle","cwd":ui.directory,"agent":"grok","agent_status":"idle","agent_session":{"agent":"grok","kind":"id","value":"32345678-abcd-4000-8000-123456789abc"}}
    ]}});
    fs::write(&payload, serde_json::to_vec(&data).unwrap()).unwrap();
    let script = ui.directory.join("inert-activity-herdr");
    fs::write(
        &script,
        "#!/bin/sh\nexec /bin/cat \"$AGENT_ACTIVITY_FIXTURE_PAYLOAD\"\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    ui.command
        .env("DAEDALUS_HERDR_BIN", &script)
        .env("AGENT_ACTIVITY_FIXTURE_PAYLOAD", &payload);
    ui.restart();
    ui.wait_text("3 terminals");
    ui.wait_text("1 working");
    ui.wait_text("1 waiting");
    ui.wait_text("1 idle");
    ui.master.write_all(b"1").unwrap();
    ui.wait_text("Fixture needs input");
    ui.wait_text("waiting");
    data["result"]["panes"][0]["agent_status"] = serde_json::json!("idle");
    fs::write(&payload, serde_json::to_vec(&data).unwrap()).unwrap();
    ui.master.write_all(b"r").unwrap();
    ui.wait_text("2 idle");
    ui.wait_presence("1 working", false);
    ui.master.write_all(b"\x10Interface settings").unwrap();
    ui.wait_text("Interface settings_");
    ui.wait_presence("UPDATING", false);
    ui.master.write_all(b"\r\r").unwrap();
    ui.wait_text("Saved in this configuration");
    let index = SETTINGS
        .iter()
        .position(|setting| setting.key == "agent_activity")
        .unwrap();
    for _ in 0..index {
        ui.master.write_all(b"\x1b[B").unwrap();
    }
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Saved value: On");
    ui.master.write_all(b"\x1b[H\x1b[B\r").unwrap();
    ui.wait_text("Save agent_activity:");
    assert!(
        ControlConfig::parse(&fs::read(&path).unwrap())
            .unwrap()
            .ui
            .agent_activity
    );
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Configuration saved privately");
    assert!(
        !ControlConfig::parse(&fs::read(&path).unwrap())
            .unwrap()
            .ui
            .agent_activity
    );
    ui.quit();
    ui.restart();
    ui.wait_text("3 terminals");
    ui.wait_presence("2 idle", false);
    ui.quit();
}

#[test]
fn codex_fork_license_picker_reaches_review_in_real_ui_without_launching() {
    use daedalus_tui::app::config::ControlConfig;
    use std::os::unix::fs::PermissionsExt;
    let mut ui = Ui::configured(false, true);
    ui.wait_text("LIVE TOPOLOGY");
    ui.quit();
    let directory = ui.directory.join(".config/daedalus");
    let path = directory.join("control.json");
    let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    config.discovery.herdr_sessions = vec!["fixture".into()];
    private_dashboard_file(&path, &config.encode().unwrap()).unwrap();
    let original = ui.directory.join("source-profile");
    let selected = ui.directory.join("selected-profile");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&selected).unwrap();
    let checkpoints=serde_json::to_vec(&serde_json::json!({"checkpoints":[{"name":"Fixture conversation","harness":"codex","sid":"12345678-abcd-4000-8000-123456789abc","cwd":ui.directory,"profile":original}]})).unwrap();
    private_dashboard_file(&directory.join("dashboard-checkpoints.json"), &checkpoints).unwrap();
    private_dashboard_file(
        &directory.join("codex-licenses.json"),
        &serde_json::to_vec(&serde_json::json!({"licenses":{"Fixture account":{"home":selected}}}))
            .unwrap(),
    )
    .unwrap();
    let bin = ui.directory.join(".local/bin");
    fs::create_dir_all(&bin).unwrap();
    let codex = bin.join("codex");
    fs::write(
        &codex,
        "#!/bin/sh\nif [ \"$1\" = --help ]; then printf 'resume fork\\n'; exit 0; fi\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
    let payload = ui.directory.join("fork-destination.json");
    fs::write(&payload,serde_json::to_vec(&serde_json::json!({"result":{"panes":[{"pane_id":"destination","terminal_id":"generation","workspace_id":"work","tab_id":"tab","title":"Fixture destination","cwd":ui.directory,"agent":"bash"}]}})).unwrap()).unwrap();
    let script = ui.directory.join("inert-fork-herdr");
    fs::write(
        &script,
        "#!/bin/sh\nexec /bin/cat \"$FORK_LICENSE_UI_PAYLOAD\"\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    ui.command
        .env("PATH", &bin)
        .env("DAEDALUS_HERDR_BIN", &script)
        .env("FORK_LICENSE_UI_PAYLOAD", &payload);
    ui.restart();
    ui.wait_text("1 checkpoints");
    ui.master.write_all(b"3").unwrap();
    ui.wait_text("Fixture conversation");
    ui.master.write_all(b"/Fixture conversation\r\r").unwrap();
    ui.wait_text("Fork in the same harness");
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Fork with which license?");
    ui.wait_text("Fixture account");
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Open a new pane below which terminal?");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("License: Fixture account");
    ui.wait_text("Native profile:");
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_text("Cancelled");
    assert_eq!(
        fs::read(directory.join("dashboard-checkpoints.json")).unwrap(),
        checkpoints
    );
    assert!(!selected.join("sessions").exists());
    ui.quit();
}

#[test]
fn checkpoint_policy_is_saved_in_real_ui_and_oversized_observation_recovers_without_data_loss() {
    use daedalus_tui::app::{config::ControlConfig, runtime_settings::SETTINGS};
    fn edit(ui: &mut Ui, key: &str, value: &str) {
        let path = ui.directory.join(".config/daedalus/control.json");
        let before = fs::read(&path).unwrap();
        let index = SETTINGS.iter().position(|s| s.key == key).unwrap();
        ui.master.write_all(b"\x10Terminals and discovery").unwrap();
        ui.wait_text("Terminals and discovery_");
        ui.wait_presence("UPDATING", false);
        ui.master.write_all(b"\r").unwrap();
        ui.wait_presence("Terminals and discovery_", false);
        ui.master.write_all(b"\r").unwrap();
        ui.wait_text("Saved in this configuration");
        for _ in 0..index {
            ui.master.write_all(b"\x1b[B").unwrap();
        }
        ui.master.write_all(b"\r").unwrap();
        ui.wait_text("Enter a whole number from");
        ui.master
            .write_all(format!("\x15{value}\r").as_bytes())
            .unwrap();
        ui.wait_text(&format!("Save {key}:"));
        assert_eq!(fs::read(&path).unwrap(), before);
        ui.master.write_all(b"\x1b[B\r").unwrap();
        ui.wait_text("Configuration saved privately");
        let config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(&config.runtime).unwrap()[key],
            value.parse::<u64>().unwrap()
        );
    }
    let mut ui = Ui::configured(false, true);
    ui.wait_text("LIVE TOPOLOGY");
    for (key, value) in [
        ("checkpoint_file_limit_bytes", "4096"),
        ("checkpoint_work_limit_bytes", "65536"),
        ("checkpoint_entry_limit", "17"),
        ("checkpoint_value_limit", "512"),
        ("checkpoint_timeout_ms", "1234"),
    ] {
        edit(&mut ui, key, value);
    }
    ui.quit();
    let path = ui
        .directory
        .join(".config/daedalus/dashboard-checkpoints.json");
    let raw = serde_json::to_vec(&serde_json::json!({"checkpoints":[{"name":"Synthetic checkpoint", "harness":"codex", "sid":"fixture-id", "cwd":ui.directory, "origin":"x".repeat(5000)}]})).unwrap();
    private_dashboard_file(&path, &raw).unwrap();
    ui.restart();
    ui.wait_text("0 terminals");
    ui.master.write_all(b"\x10Discovery warnings\r\r").unwrap();
    ui.wait_text("checkpoint_file_limit_bytes");
    assert_eq!(fs::read(&path).unwrap(), raw);
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_presence("checkpoint_file_limit_bytes", false);
    edit(&mut ui, "checkpoint_file_limit_bytes", "8192");
    ui.master.write_all(b"3").unwrap();
    ui.wait_text("Synthetic checkpoint");
    assert_eq!(fs::read(&path).unwrap(), raw);
    ui.quit();
    ui.restart();
    ui.wait_text("1 checkpoints");
    ui.master.write_all(b"3").unwrap();
    ui.wait_text("Synthetic checkpoint");
    assert_eq!(fs::read(&path).unwrap(), raw);
    ui.quit();
}

#[test]
fn saved_local_navigation_and_search_limits_reach_real_ui_and_survive_restart() {
    use daedalus_tui::app::{config::ControlConfig, interface_settings::SETTINGS};
    let mut ui = Ui::configured(false, true);
    let path = ui.directory.join(".config/daedalus/control.json");
    ui.wait_text("LIVE TOPOLOGY");
    for (key, value) in [
        ("list_page_rows", "4"),
        ("details_page_rows", "6"),
        ("options_page_items", "2"),
        ("mouse_scroll_rows", "2"),
        ("choice_wheel_items", "3"),
        ("shortcut_query_limit_chars", "5"),
        // Keep the full command name available until all fields are saved.
        ("search_query_limit_chars", "7"),
    ] {
        let before = fs::read(&path).unwrap();
        let index = SETTINGS.iter().position(|s| s.key == key).unwrap();
        ui.master.write_all(b"\x10Interface settings\r\r").unwrap();
        ui.wait_text("Saved in this configuration");
        for _ in 0..index {
            ui.master.write_all(b"\x1b[B").unwrap();
        }
        ui.master.write_all(b"\r").unwrap();
        ui.wait_text("Enter a whole number from 1 to");
        ui.master.write_all(b"\x150\r").unwrap();
        ui.wait_text("Enter a number from 1 to");
        assert_eq!(fs::read(&path).unwrap(), before);
        ui.master
            .write_all(format!("\x15{value}\r").as_bytes())
            .unwrap();
        ui.wait_text(&format!("Save {key}:"));
        assert_eq!(fs::read(&path).unwrap(), before);
        ui.master.write_all(b"\x1b[B\r").unwrap();
        ui.wait_text("Configuration saved privately");
        let saved = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.ui.value(key).unwrap(), value.parse::<u64>().unwrap());
    }
    let saved = fs::read(&path).unwrap();
    // These inputs are handled by the running UI after its settings response.
    ui.master.write_all(b"\x10abcdefghijk").unwrap();
    ui.wait_text("abcdefg_");
    ui.master.write_all(b"\x1bOP").unwrap();
    ui.wait_text("Keyboard shortcuts");
    ui.master.write_all(b"123456789").unwrap();
    ui.wait_text("Search  12345");
    ui.wait_presence("Search  123456", false);
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_presence("Keyboard shortcuts", false);
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_presence("abcdefg_", false);
    ui.master.write_all(b",").unwrap();
    ui.wait_text("1/16");
    ui.master.write_all(b"\x1b[6~").unwrap();
    ui.wait_text("3/16");
    ui.master.write_all(b"\x1b[<65;80;20M").unwrap();
    ui.wait_text("6/16");
    ui.master.write_all(b"\x1b[<64;80;20M").unwrap();
    ui.wait_text("3/16");
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_presence("3/16", false);
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    ui.master
        .write_all("\x10\x1b[200~界é🙂abcdef\x1b[201~".as_bytes())
        .unwrap();
    // This fixture requests ASCII rendering. Wide Unicode cells use fallback
    // glyphs, but the accepted query still ends at its seventh scalar, `d`.
    ui.wait_text("? ?? abcd_");
    assert_eq!(fs::read(&path).unwrap(), saved);
    assert!(
        !path
            .parent()
            .unwrap()
            .join("telegram-service.json")
            .exists()
    );
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_presence("? ?? abcd_", false);
    ui.quit();
}

#[test]
fn saved_native_history_and_wheel_policy_reach_real_read_only_ui_scrolling() {
    use daedalus_tui::app::{config::ControlConfig, interface_settings, runtime_settings};
    fn native(home: &std::path::Path, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_daedalus-tui"))
            .env_clear()
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin")
            .env("SHELL", "/bin/sh")
            .env("TERM", "xterm-256color")
            .current_dir(home)
            .args(args)
            .output()
            .unwrap()
    }
    struct OwnedNative {
        home: PathBuf,
        id: String,
        generation: String,
    }
    impl Drop for OwnedNative {
        fn drop(&mut self) {
            let _ = native(
                &self.home,
                &["terminal-native", "close", &self.id, &self.generation],
            );
        }
    }
    fn edit(ui: &mut Ui, group: &str, index: usize, key: &str, saved: &str, value: &str) {
        ui.master
            .write_all(format!("\x10{group}\r\r").as_bytes())
            .unwrap();
        ui.wait_text("Saved in this configuration");
        for _ in 0..index {
            ui.master.write_all(b"\x1b[B").unwrap();
        }
        ui.master.write_all(b"\r").unwrap();
        ui.wait_text(&format!("Saved value: {saved}"));
        if key == "terminal_scroll_rows" {
            ui.master.write_all(b"0\r").unwrap();
            ui.wait_text("Enter a number from 1 to 1000");
        }
        ui.master
            .write_all(format!("\x15{value}\r").as_bytes())
            .unwrap();
        ui.wait_text(&format!("Save {key}:"));
        ui.master.write_all(b"\x1b[B\r").unwrap();
        ui.wait_text("Configuration saved privately");
    }
    let mut ui = Ui::configured(false, true);
    ui.wait_text("LIVE TOPOLOGY");
    let wheel = interface_settings::SETTINGS
        .iter()
        .position(|s| s.key == "terminal_scroll_rows")
        .unwrap();
    let history = runtime_settings::SETTINGS
        .iter()
        .position(|s| s.key == "native_scrollback_rows")
        .unwrap();
    edit(
        &mut ui,
        "Interface settings",
        wheel,
        "terminal_scroll_rows",
        "3",
        "7",
    );
    edit(
        &mut ui,
        "Terminals and discovery",
        history,
        "native_scrollback_rows",
        "1000",
        "8",
    );
    let directory = ui.directory.join(".config/daedalus");
    let path = directory.join("control.json");
    let saved = fs::read(&path).unwrap();
    let config = ControlConfig::parse(&saved).unwrap();
    assert_eq!(config.ui.terminal_scroll_rows, 7);
    assert_eq!(config.runtime.native_scrollback_rows, 8);
    ui.master.write_all(b"\x10Instance settings\r\r").unwrap();
    ui.wait_text("Changes require an explicit review");
    let field = daedalus_tui::app::control_settings::FIELDS
        .iter()
        .position(|field| field.key == "discovery.native")
        .unwrap();
    for _ in 0..field {
        ui.master.write_all(b"\x1b[B").unwrap();
    }
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Native terminal discovery override");
    ui.wait_text("Ctrl+U clears the field.");
    ui.master.write_all(b"\x15true\r").unwrap();
    ui.wait_text("Save instance settings?");
    assert_eq!(fs::read(&path).unwrap(), saved);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Configuration saved privately");
    ui.quit();
    let saved = fs::read(&path).unwrap();
    let config = ControlConfig::parse(&saved).unwrap();
    assert!(!config.discovery.automatic);
    assert_eq!(config.discovery.native, Some(true));
    let output = native(
        &ui.directory,
        &[
            "terminal-native",
            "start",
            "--config-dir",
            directory.to_str().unwrap(),
            "--name",
            "wheel-fixture",
            "--workspace",
            "Wheel",
            "--cols",
            "80",
            "--rows",
            "24",
            "--",
            "/bin/sh",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let session: daedalus_tui::app::native_terminal::SessionInfo =
        serde_json::from_slice(&output.stdout).unwrap();
    let owned = OwnedNative {
        home: ui.directory.clone(),
        id: session.id,
        generation: session.generation,
    };
    ui.restart();
    ui.master.write_all(b"2").unwrap();
    ui.wait_text("wheel-fixture");
    ui.master.write_all(b"\x0bwheel-fixture\r").unwrap();
    ui.wait_text("Watching:");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let result = native(
            &ui.directory,
            &["terminal-native", "capture", &owned.id, &owned.generation],
        );
        let capture: daedalus_tui::app::native_terminal::Capture =
            serde_json::from_slice(&result.stdout).unwrap();
        if (capture.session.cols, capture.session.rows) != (80, 24) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Owned viewer did not settle its size"
        );
        ui.wait_presence("impossible-native-wheel-marker", false);
        std::thread::sleep(Duration::from_millis(20));
    }
    let result = native(
        &ui.directory,
        &[
            "terminal-native",
            "input",
            &owned.id,
            &owned.generation,
            "--text",
            "for n in $(seq 1 100); do printf 'wheel-history-%03d\\n' \"$n\"; done; printf 'NATIVE_%s\\n' SCROLL_READY\r",
        ],
    );
    assert!(result.status.success());
    ui.wait_text("NATIVE_SCROLL_READY");
    assert!(!ui.screen.screen().contents().contains("External terminals"));
    let needle = b"NATIVE_SCROLL_READY";
    let (x, y) = (0..48)
        .find_map(|row| {
            (0..160 - needle.len() as u16).find_map(|col| {
                needle
                    .iter()
                    .enumerate()
                    .all(|(n, byte)| {
                        ui.screen
                            .screen()
                            .cell(row, col + n as u16)
                            .is_some_and(|cell| cell.contents().as_bytes() == [*byte])
                    })
                    .then_some((col + 1, row + 1))
            })
        })
        .unwrap();
    ui.master
        .write_all(format!("\x1b[<64;{x};{y}M").as_bytes())
        .unwrap();
    ui.wait_text("History: 7 rows up");
    ui.master
        .write_all(format!("\x1b[<64;{x};{y}M").as_bytes())
        .unwrap();
    ui.wait_text("History: 8 rows up");
    let live = native(
        &ui.directory,
        &["terminal-native", "capture", &owned.id, &owned.generation],
    );
    let live: daedalus_tui::app::native_terminal::Capture =
        serde_json::from_slice(&live.stdout).unwrap();
    assert!(live.text.contains("NATIVE_SCROLL_READY"));
    assert!(!live.session.controlled);
    ui.master
        .write_all(format!("\x1b[<65;{x};{y}M").as_bytes())
        .unwrap();
    ui.wait_text("History: 1 row up");
    ui.master
        .write_all(format!("\x1b[<65;{x};{y}M").as_bytes())
        .unwrap();
    ui.wait_text("Watching:");
    ui.wait_text("NATIVE_SCROLL_READY");
    assert_eq!(fs::read(&path).unwrap(), saved);
    ui.quit();
    ui.restart();
    ui.wait_text("wheel-fixture");
    assert_eq!(fs::read(&path).unwrap(), saved);
    ui.quit();
}

#[test]
fn checkpoint_tree_policy_is_reviewed_saved_and_reopened_in_the_real_ui() {
    use daedalus_tui::app::{
        config::ControlConfig,
        interface_settings::{CheckpointMatches, SETTINGS},
    };
    let mut ui = Ui::configured(false, true);
    let path = ui.directory.join(".config/daedalus/control.json");
    let before = fs::read(&path).unwrap();
    let position = SETTINGS
        .iter()
        .position(|setting| setting.key == "checkpoint_matches")
        .unwrap();
    let open = |ui: &mut Ui| {
        ui.master.write_all(b"\x10Interface settings\r\r").unwrap();
        ui.wait_text("Saved in this configuration");
        for _ in 0..position {
            ui.master.write_all(b"\x1b[B").unwrap();
        }
        ui.master.write_all(b"\r").unwrap();
    };
    ui.wait_text("LIVE TOPOLOGY");
    open(&mut ui);
    ui.wait_text("Saved value: all");
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Save checkpoint_matches:");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Configuration saved privately");
    let saved = fs::read(&path).unwrap();
    assert_eq!(
        ControlConfig::parse(&saved).unwrap().ui.checkpoint_matches,
        CheckpointMatches::Source
    );
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    open(&mut ui);
    ui.wait_text("Saved value: source");
    assert_eq!(fs::read(&path).unwrap(), saved);
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_text("Cancelled");
    ui.quit();
}
#[test]
fn conflicting_observations_are_explained_and_recover_in_the_real_private_ui() {
    use daedalus_tui::app::config::ControlConfig;
    use std::os::unix::fs::PermissionsExt;
    let mut ui = Ui::configured(false, true);
    ui.wait_text("LIVE TOPOLOGY");
    ui.quit();
    let path = ui.directory.join(".config/daedalus/control.json");
    let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    config.discovery.herdr_sessions = vec!["fixture".into()];
    private_dashboard_file(&path, &config.encode().unwrap()).unwrap();
    let before = fs::read(&path).unwrap();
    let first = serde_json::json!({"pane_id":"pane","terminal_id":"generation","workspace_id":"work","tab_id":"tab","title":"Visible fixture"});
    let mut conflict = first.clone();
    conflict["title"] = serde_json::json!("Conflicting fixture");
    let payload = ui.directory.join("payload");
    fs::write(
        &payload,
        serde_json::to_vec(&serde_json::json!({"result":{"panes":[first.clone(),conflict]}}))
            .unwrap(),
    )
    .unwrap();
    let executable = ui.directory.join("inert-herdr");
    fs::write(
        &executable,
        "#!/bin/sh\nexec /bin/cat \"$FIXTURE_PAYLOAD\"\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    ui.command
        .env("DAEDALUS_HERDR_BIN", executable)
        .env("FIXTURE_PAYLOAD", &payload);
    ui.restart();
    ui.wait_text("PARTIAL");
    ui.wait_text("1 terminals");
    ui.master.write_all(b"\x10Discovery warnings\r\r").unwrap();
    ui.wait_text("Backend: discovery");
    ui.wait_text("Error: Conflicting terminal observations");
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_presence("Backend: discovery", false);
    ui.wait_text("Recent actions, deliveries, tasks and health.");
    ui.quit();
    fs::write(
        &payload,
        serde_json::to_vec(&serde_json::json!({"result":{"panes":[first.clone(),first]}})).unwrap(),
    )
    .unwrap();
    ui.restart();
    ui.wait_text("* LIVE");
    ui.wait_text("1 terminals");
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(
        !path
            .parent()
            .unwrap()
            .join("telegram-service.json")
            .exists()
    );
    ui.quit();
}
#[test]
fn instance_socket_editor_preserves_commas_and_uses_reviewed_private_saves_in_the_real_ui() {
    use daedalus_tui::app::{config::ControlConfig, control_settings::FIELDS};
    let mut ui = Ui::configured(false, true);
    let path = ui.directory.join(".config/daedalus/control.json");
    let before = fs::read(&path).unwrap();
    let socket = ui
        .directory
        .join("tmux, socket")
        .to_str()
        .unwrap()
        .to_owned();
    ui.wait_text("LIVE TOPOLOGY");
    ui.master.write_all(b"\x10Instance settings\r\r").unwrap();
    ui.wait_text("Lists and connections are edited as exact entries");
    let field = FIELDS.iter().position(|f| f.key == "tmux_sockets").unwrap();
    for _ in 0..field {
        ui.master.write_all(b"\x1b[B").unwrap();
    }
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("0 entries; edits are an unsaved draft");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Enter one exact value");
    ui.master.write_all(socket.as_bytes()).unwrap();
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("1 entry; edits are an unsaved draft");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\x1b[B\r").unwrap();
    ui.wait_text("Save instance settings?");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Configuration saved privately");
    let saved = fs::read(&path).unwrap();
    assert_eq!(
        ControlConfig::parse(&saved).unwrap().tmux_sockets,
        vec![socket.clone()]
    );
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    ui.master.write_all(b"\x10Instance settings\r\r").unwrap();
    ui.wait_text("Lists and connections are edited as exact entries");
    for _ in 0..field {
        ui.master.write_all(b"\x1b[B").unwrap();
    }
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("1 entry; edits are an unsaved draft");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Saved connection entry");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Enter one exact value");
    ui.wait_text(&socket);
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_text("Instance settings unchanged");
    assert_eq!(fs::read(&path).unwrap(), saved);
    ui.quit();
}
#[test]
#[cfg(target_os = "linux")]
fn a_delayed_output_close_keeps_new_command_search_in_the_real_ui() {
    fn identity(pid: u32) -> Option<(u32, u64)> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let fields: Vec<_> = stat.rsplit_once(')')?.1.split_whitespace().collect();
        Some((fields.get(1)?.parse().ok()?, fields.get(19)?.parse().ok()?))
    }
    struct PausedBackend {
        pid: u32,
        identity: (u32, u64),
    }
    impl Drop for PausedBackend {
        fn drop(&mut self) {
            if identity(self.pid) == Some(self.identity) {
                unsafe {
                    libc::kill(self.pid as i32, libc::SIGCONT);
                }
            }
        }
    }
    let mut ui = Ui::configured(false, true);
    ui.wait_text("* LIVE");
    let path = ui.directory.join(".config/daedalus/control.json");
    let before = fs::read(&path).unwrap();
    ui.master.write_all(b"\x10Discovery warnings\r\r").unwrap();
    ui.wait_text("No discovery warnings in the latest snapshot.");
    let parent = ui.child.id();
    let children = fs::read_to_string(format!("/proc/{parent}/task/{parent}/children")).unwrap();
    let candidates: Vec<u32> = children
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .filter(|pid| {
            fs::read(format!("/proc/{pid}/cmdline"))
                .unwrap()
                .split(|b| *b == 0)
                .any(|arg| arg == b"backend")
        })
        .collect();
    assert_eq!(candidates.len(), 1);
    let pid = candidates[0];
    let expected = identity(pid).unwrap();
    assert_eq!(expected.0, parent);
    let paused = PausedBackend {
        pid,
        identity: expected,
    };
    assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGSTOP) }, 0);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert_eq!(identity(pid), Some(expected));
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        if matches!(
            stat.rsplit_once(')').unwrap().1.split_whitespace().next(),
            Some("T" | "t")
        ) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "owned fixture backend did not stop"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_presence("No discovery warnings in the latest snapshot.", false);
    ui.master.write_all(b"\x10Terminals and discovery").unwrap();
    ui.wait_text("Terminals and discovery");
    ui.wait_text("UPDATING");
    ui.master.write_all(b"\r\r").unwrap();
    ui.wait_text("Still working on the previous action");
    assert!(
        !ui.screen
            .screen()
            .contents()
            .contains("Saved in this configuration")
    );
    drop(paused);
    ui.wait_text("* LIVE");
    let shown = ui.screen.screen().contents();
    assert!(
        shown.contains("Find >") && shown.contains("Terminals and discovery"),
        "old reply replaced the new command search: {shown}"
    );
    assert!(
        !shown.contains("Saved in this configuration"),
        "blocked Enter was replayed after completion"
    );
    ui.master.write_all(b"\r\r").unwrap();
    ui.wait_text("Saved in this configuration");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Saved value: 120");
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_text("Cancelled");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.quit();
}
#[test]
fn discovery_limits_show_warnings_and_can_be_reviewed_saved_and_reopened_in_the_real_ui() {
    use daedalus_tui::app::{config::ControlConfig, runtime_settings::SETTINGS};
    use std::os::unix::fs::PermissionsExt;
    let mut ui = Ui::configured(false, true);
    ui.wait_text("LIVE TOPOLOGY");
    ui.quit();
    let path = ui.directory.join(".config/daedalus/control.json");
    let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    config.discovery.automatic = false;
    config.discovery.herdr_sessions = vec!["a".into(), "b".into()];
    config.runtime.discovery_endpoint_limit = 1;
    private_dashboard_file(&path, &config.encode().unwrap()).unwrap();
    let executable = ui.directory.join("inert-herdr");
    fs::write(
        &executable,
        "#!/bin/sh\nprintf '%s\\n' '{\"result\":{\"panes\":[]}}'\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    ui.command.env("DAEDALUS_HERDR_BIN", executable);
    ui.restart();
    ui.wait_text("PARTIAL");
    ui.master.write_all(b"\x10Discovery warnings\r\r").unwrap();
    ui.wait_text("Terminal endpoint limit reached");
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_presence("Terminal endpoint limit reached", false);
    // Closing output has no status message. Its backend acknowledgement
    // navigates from command search to the action's Activity page.
    ui.wait_text("Recent actions, deliveries, tasks and health.");
    let field = SETTINGS
        .iter()
        .position(|s| s.key == "discovery_endpoint_limit")
        .unwrap();
    let open = |ui: &mut Ui| {
        ui.master
            .write_all(b"\x10Terminals and discovery\r\r")
            .unwrap();
        ui.wait_text("Saved in this configuration");
        for _ in 0..field {
            ui.master.write_all(b"\x1b[B").unwrap();
        }
        ui.master.write_all(b"\r").unwrap();
    };
    let before = fs::read(&path).unwrap();
    open(&mut ui);
    ui.wait_text("Saved value: 1");
    ui.master.write_all(b"\x150\r").unwrap();
    ui.wait_text("must be between 1 and 8192");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x152\r").unwrap();
    ui.wait_text("Save discovery_endpoint_limit:");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Configuration saved privately");
    let saved = fs::read(&path).unwrap();
    assert_eq!(
        ControlConfig::parse(&saved)
            .unwrap()
            .runtime
            .discovery_endpoint_limit,
        2
    );
    ui.wait_text("* LIVE");
    ui.master.write_all(b"\x10Discovery warnings\r\r").unwrap();
    ui.wait_text("No discovery warnings in the latest snapshot.");
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_presence("No discovery warnings in the latest snapshot.", false);
    ui.wait_text("Recent actions, deliveries, tasks and health.");
    ui.quit();
    ui.restart();
    ui.wait_text("* LIVE");
    open(&mut ui);
    ui.wait_text("Saved value: 2");
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_text("Cancelled");
    assert_eq!(fs::read(&path).unwrap(), saved);
    ui.quit();
}
#[test]
fn shared_metadata_budget_is_explained_and_can_be_saved_and_reopened_in_the_real_ui() {
    use daedalus_tui::app::{config::ControlConfig, runtime_settings::SETTINGS};
    use std::os::unix::fs::PermissionsExt;
    let mut ui = Ui::configured(false, true);
    ui.wait_text("LIVE TOPOLOGY");
    ui.quit();
    let path = ui.directory.join(".config/daedalus/control.json");
    let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    config.discovery.automatic = false;
    config.discovery.herdr_sessions = vec!["fixture".into()];
    config.runtime.discovery_metadata_value_limit = 256;
    private_dashboard_file(&path, &config.encode().unwrap()).unwrap();
    let payload = ui.directory.join("payload");
    let panes: Vec<_> = (0..40)
        .map(|n| serde_json::json!({"pane_id":format!("p{n}"),"terminal_id":"generation"}))
        .collect();
    fs::write(
        &payload,
        serde_json::to_vec(&serde_json::json!({"result":{"panes":panes}})).unwrap(),
    )
    .unwrap();
    let executable = ui.directory.join("inert-herdr");
    fs::write(
        &executable,
        "#!/bin/sh\nexec /bin/cat \"$FIXTURE_PAYLOAD\"\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    ui.command
        .env("DAEDALUS_HERDR_BIN", executable)
        .env("FIXTURE_PAYLOAD", payload);
    ui.restart();
    ui.wait_text("PARTIAL");
    ui.master.write_all(b"\x10Discovery warnings\r\r").unwrap();
    ui.wait_text("Discovery metadata limit reached");
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_presence("Discovery metadata limit reached", false);
    ui.wait_text("Recent actions, deliveries, tasks and health.");
    let field = SETTINGS
        .iter()
        .position(|s| s.key == "discovery_metadata_value_limit")
        .unwrap();
    let open = |ui: &mut Ui| {
        ui.master
            .write_all(b"\x10Terminals and discovery\r\r")
            .unwrap();
        ui.wait_text("Saved in this configuration");
        for _ in 0..field {
            ui.master.write_all(b"\x1b[B").unwrap();
        }
        ui.master.write_all(b"\r").unwrap();
    };
    let before = fs::read(&path).unwrap();
    open(&mut ui);
    ui.wait_text("Saved value: 256");
    ui.master.write_all(b"\x15255\r").unwrap();
    ui.wait_text("must be between 256 and 1000000");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x152048\r").unwrap();
    ui.wait_text("Save discovery_metadata_value_limit:");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Configuration saved privately");
    ui.wait_text("* LIVE");
    let saved = fs::read(&path).unwrap();
    assert_eq!(
        ControlConfig::parse(&saved)
            .unwrap()
            .runtime
            .discovery_metadata_value_limit,
        2048
    );
    ui.master.write_all(b"\x10Discovery warnings\r\r").unwrap();
    ui.wait_text("No discovery warnings in the latest snapshot.");
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_presence("No discovery warnings in the latest snapshot.", false);
    ui.wait_text("Recent actions, deliveries, tasks and health.");
    ui.quit();
    ui.restart();
    ui.wait_text("* LIVE");
    open(&mut ui);
    ui.wait_text("Saved value: 2048");
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_text("Cancelled");
    assert_eq!(fs::read(&path).unwrap(), saved);
    assert!(!path.parent().unwrap().join("delivery.sqlite3").exists());
    ui.quit();
}
#[test]
fn unsupported_layout_geometry_keeps_the_real_ui_readable_and_recovers() {
    use daedalus_tui::app::config::ControlConfig;
    use std::os::unix::fs::PermissionsExt;
    let mut ui = Ui::configured(false, true);
    ui.wait_text("LIVE TOPOLOGY");
    ui.quit();
    let path = ui.directory.join(".config/daedalus/control.json");
    let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    config.discovery.automatic = false;
    config.discovery.herdr_sessions = vec!["fixture".into()];
    private_dashboard_file(&path, &config.encode().unwrap()).unwrap();
    let before = fs::read(&path).unwrap();
    let payload = ui.directory.join("payload");
    let data = |width| serde_json::json!({"result":{"panes":[{"pane_id":"pane","terminal_id":"generation","workspace_id":"work","tab_id":"tab","title":"Visible terminal"}],"layouts":[{"workspace_id":"work","tab_id":"tab","area":{"width":width,"height":24},"panes":[{"pane_id":"pane","rect":{"x":0,"y":0,"width":width,"height":24}}]}]}});
    fs::write(&payload, serde_json::to_vec(&data(70000)).unwrap()).unwrap();
    let executable = ui.directory.join("inert-herdr");
    fs::write(
        &executable,
        "#!/bin/sh\nexec /bin/cat \"$FIXTURE_PAYLOAD\"\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    ui.command
        .env("DAEDALUS_HERDR_BIN", executable)
        .env("FIXTURE_PAYLOAD", &payload);
    ui.restart();
    ui.wait_text("PARTIAL");
    ui.wait_text("1 terminals");
    ui.master.write_all(b"\x10Discovery warnings\r\r").unwrap();
    ui.wait_text("1. Layout geometry is incomplete");
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_presence("1. Layout geometry is incomplete", false);
    ui.wait_text("Recent actions, deliveries, tasks and health.");
    ui.quit();
    fs::write(&payload, serde_json::to_vec(&data(120)).unwrap()).unwrap();
    ui.restart();
    ui.wait_text("* LIVE");
    ui.wait_text("1 terminals");
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(!path.parent().unwrap().join("delivery.sqlite3").exists());
    ui.quit();
}
#[test]
fn relationship_limit_is_explained_and_can_be_saved_and_reopened_in_the_real_ui() {
    use daedalus_tui::app::{config::ControlConfig, runtime_settings::SETTINGS};
    use std::os::unix::fs::PermissionsExt;
    let mut ui = Ui::configured(false, true);
    ui.wait_text("LIVE TOPOLOGY");
    ui.quit();
    let path = ui.directory.join(".config/daedalus/control.json");
    let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    config.discovery.automatic = false;
    config.discovery.herdr_sessions = vec!["fixture".into()];
    config.runtime.discovery_relationship_limit = 1;
    private_dashboard_file(&path, &config.encode().unwrap()).unwrap();
    let payload = ui.directory.join("payload");
    let panes: Vec<_> = (0..40)
        .map(|n| serde_json::json!({"pane_id":format!("p{n}"),"terminal_id":"generation"}))
        .collect();
    fs::write(
        &payload,
        serde_json::to_vec(&serde_json::json!({"result":{"panes":panes}})).unwrap(),
    )
    .unwrap();
    let executable = ui.directory.join("inert-herdr");
    fs::write(
        &executable,
        "#!/bin/sh\nexec /bin/cat \"$FIXTURE_PAYLOAD\"\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    ui.command
        .env("DAEDALUS_HERDR_BIN", executable)
        .env("FIXTURE_PAYLOAD", payload);
    ui.restart();
    ui.wait_text("PARTIAL");
    ui.master.write_all(b"\x10Discovery warnings\r\r").unwrap();
    ui.wait_text("1. Relationship display is incomplete");
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_presence("1. Relationship display is incomplete", false);
    ui.wait_text("Recent actions, deliveries, tasks and health.");
    let field = SETTINGS
        .iter()
        .position(|s| s.key == "discovery_relationship_limit")
        .unwrap();
    let open = |ui: &mut Ui| {
        ui.master
            .write_all(b"\x10Terminals and discovery\r\r")
            .unwrap();
        ui.wait_text("Saved in this configuration");
        for _ in 0..field {
            ui.master.write_all(b"\x1b[B").unwrap();
        }
        ui.master.write_all(b"\r").unwrap();
    };
    let before = fs::read(&path).unwrap();
    open(&mut ui);
    ui.wait_text("Saved value: 1");
    ui.master.write_all(b"\x150\r").unwrap();
    ui.wait_text("must be between 1 and 262144");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x154096\r").unwrap();
    ui.wait_text("Save discovery_relationship_limit:");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Configuration saved privately");
    ui.wait_text("* LIVE");
    let saved = fs::read(&path).unwrap();
    assert_eq!(
        ControlConfig::parse(&saved)
            .unwrap()
            .runtime
            .discovery_relationship_limit,
        4096
    );
    ui.master.write_all(b"\x10Discovery warnings\r\r").unwrap();
    ui.wait_text("No discovery warnings in the latest snapshot.");
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_presence("No discovery warnings in the latest snapshot.", false);
    ui.wait_text("Recent actions, deliveries, tasks and health.");
    ui.quit();
    ui.restart();
    ui.wait_text("* LIVE");
    open(&mut ui);
    ui.wait_text("Saved value: 4096");
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_text("Cancelled");
    assert_eq!(fs::read(&path).unwrap(), saved);
    assert!(!path.parent().unwrap().join("delivery.sqlite3").exists());
    ui.quit();
}
#[test]
fn database_snapshot_limit_is_scrollable_reviewed_saved_and_reopened_in_the_real_ui() {
    use daedalus_tui::app::{config::ControlConfig, runtime_settings::SETTINGS};
    let mut ui = Ui::configured(false, true);
    let path = ui.directory.join(".config/daedalus/control.json");
    let before = fs::read(&path).unwrap();
    let field = SETTINGS
        .iter()
        .position(|s| s.key == "database_snapshot_limit_bytes")
        .unwrap();
    let open = |ui: &mut Ui| {
        ui.master
            .write_all(b"\x10Terminals and discovery\r\r")
            .unwrap();
        ui.wait_text("Saved in this configuration");
        for _ in 0..field {
            ui.master.write_all(b"\x1b[B").unwrap();
        }
        ui.master.write_all(b"\r").unwrap();
    };
    ui.wait_text("LIVE TOPOLOGY");
    open(&mut ui);
    ui.wait_text("Saved value: 268435456");
    ui.master.write_all(b"\x150\r").unwrap();
    ui.wait_text("must be between 4096 and 268435456");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x15131072\r").unwrap();
    ui.wait_text("Save database_snapshot_limit_bytes:");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Configuration saved privately");
    let saved = fs::read(&path).unwrap();
    let config = ControlConfig::parse(&saved).unwrap();
    assert_eq!(config.runtime.database_snapshot_limit_bytes, 131072);
    assert_eq!(config.runtime.database_snapshot_timeout_ms, 10000);
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    open(&mut ui);
    ui.wait_text("Saved value: 131072");
    assert_eq!(fs::read(&path).unwrap(), saved);
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_text("Cancelled");
    ui.quit();
}
#[test]
fn resize_movement_limit_is_reviewed_saved_and_reopened_in_the_real_ui() {
    use daedalus_tui::app::{config::ControlConfig, runtime_settings::SETTINGS};
    let mut ui = Ui::configured(false, true);
    let path = ui.directory.join(".config/daedalus/control.json");
    let before = fs::read(&path).unwrap();
    let field = SETTINGS
        .iter()
        .position(|s| s.key == "terminal_resize_max_cells")
        .unwrap();
    let open = |ui: &mut Ui| {
        ui.master
            .write_all(b"\x10Terminals and discovery\r\r")
            .unwrap();
        ui.wait_text("Saved in this configuration");
        for _ in 0..field {
            ui.master.write_all(b"\x1b[B").unwrap();
        }
        ui.master.write_all(b"\r").unwrap();
    };
    ui.wait_text("LIVE TOPOLOGY");
    open(&mut ui);
    ui.wait_text("Saved value: 4096");
    ui.master.write_all(b"\x150\r").unwrap();
    ui.wait_text("must be between 1 and 65535");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1517\r").unwrap();
    ui.wait_text("Save terminal_resize_max_cells:");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Configuration saved privately");
    let saved = fs::read(&path).unwrap();
    let config = ControlConfig::parse(&saved).unwrap();
    assert_eq!(config.runtime.terminal_resize_max_cells, 17);
    assert_eq!(config.runtime.terminal_resize_pane_limit, 128);
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    open(&mut ui);
    ui.wait_text("Saved value: 17");
    assert_eq!(fs::read(&path).unwrap(), saved);
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_text("Cancelled");
    ui.quit();
}
#[test]
fn viewer_history_limit_is_reviewed_saved_and_reopened_in_the_real_ui() {
    use daedalus_tui::app::{config::ControlConfig, runtime_settings::SETTINGS};
    let mut ui = Ui::configured(false, true);
    let path = ui.directory.join(".config/daedalus/control.json");
    let before = fs::read(&path).unwrap();
    let field = SETTINGS
        .iter()
        .position(|s| s.key == "terminal_view_history_limit")
        .unwrap();
    let open = |ui: &mut Ui| {
        ui.master
            .write_all(b"\x10Terminals and discovery\r\r")
            .unwrap();
        ui.wait_text("Saved in this configuration");
        for _ in 0..field {
            ui.master.write_all(b"\x1b[B").unwrap();
        }
        ui.master.write_all(b"\r").unwrap();
    };
    ui.wait_text("LIVE TOPOLOGY");
    open(&mut ui);
    ui.wait_text("Saved value: 200");
    ui.master.write_all(b"\x150\r").unwrap();
    ui.wait_text("must be between 1 and 65536");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1517\r").unwrap();
    ui.wait_text("Save terminal_view_history_limit:");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Configuration saved privately");
    let saved = fs::read(&path).unwrap();
    let config = ControlConfig::parse(&saved).unwrap();
    assert_eq!(config.runtime.terminal_view_history_limit, 17);
    assert_eq!(config.runtime.terminal_resize_pane_limit, 128);
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    open(&mut ui);
    ui.wait_text("Saved value: 17");
    assert_eq!(fs::read(&path).unwrap(), saved);
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_text("Cancelled");
    ui.quit();
}
#[test]
fn viewer_history_value_budget_is_reviewed_saved_and_reopened_in_the_real_ui() {
    use daedalus_tui::app::{config::ControlConfig, runtime_settings::SETTINGS};
    let mut ui = Ui::configured(false, true);
    let path = ui.directory.join(".config/daedalus/control.json");
    let before = fs::read(&path).unwrap();
    let field = SETTINGS
        .iter()
        .position(|s| s.key == "terminal_view_history_value_limit")
        .unwrap();
    let open = |ui: &mut Ui| {
        ui.master
            .write_all(b"\x10Terminals and discovery\r\r")
            .unwrap();
        ui.wait_text("Saved in this configuration");
        for _ in 0..field {
            ui.master.write_all(b"\x1b[B").unwrap();
        }
        ui.master.write_all(b"\r").unwrap();
    };
    ui.wait_text("LIVE TOPOLOGY");
    open(&mut ui);
    ui.wait_text("Saved value: 100000");
    ui.master.write_all(b"\x150\r").unwrap();
    ui.wait_text("must be between 128 and 8388608");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x151024\r").unwrap();
    ui.wait_text("Save terminal_view_history_value_limit:");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Configuration saved privately");
    let saved = fs::read(&path).unwrap();
    let config = ControlConfig::parse(&saved).unwrap();
    assert_eq!(config.runtime.terminal_view_history_value_limit, 1024);
    assert_eq!(config.runtime.terminal_resize_pane_limit, 128);
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    open(&mut ui);
    ui.wait_text("Saved value: 1024");
    assert_eq!(fs::read(&path).unwrap(), saved);
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_text("Cancelled");
    ui.quit();
}
#[test]
fn instance_import_entry_budget_is_scrollable_reviewed_saved_and_reopened_in_the_real_ui() {
    use daedalus_tui::app::{config::ControlConfig, runtime_settings::SETTINGS};
    let mut ui = Ui::configured(false, true);
    let path = ui.directory.join(".config/daedalus/control.json");
    let before = fs::read(&path).unwrap();
    let field = SETTINGS
        .iter()
        .position(|s| s.key == "instance_import_entry_limit")
        .unwrap();
    let open = |ui: &mut Ui| {
        ui.master
            .write_all(b"\x10Terminals and discovery\r\r")
            .unwrap();
        ui.wait_text("Saved in this configuration");
        for _ in 0..field {
            ui.master.write_all(b"\x1b[B").unwrap();
        }
        ui.master.write_all(b"\r").unwrap();
    };
    ui.wait_text("LIVE TOPOLOGY");
    open(&mut ui);
    ui.wait_text("Saved value: 4096");
    ui.master.write_all(b"\x150\r").unwrap();
    ui.wait_text("must be between 1 and 65536");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x15128\r").unwrap();
    ui.wait_text("Save instance_import_entry_limit:");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Configuration saved privately");
    let saved = fs::read(&path).unwrap();
    let config = ControlConfig::parse(&saved).unwrap();
    assert_eq!(config.runtime.instance_import_entry_limit, 128);
    assert_eq!(config.runtime.terminal_resize_pane_limit, 128);
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    open(&mut ui);
    ui.wait_text("Saved value: 128");
    assert_eq!(fs::read(&path).unwrap(), saved);
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_text("Cancelled");
    ui.quit();
}
#[test]
fn queued_frame_budget_is_scrollable_reviewed_saved_and_reopened_in_the_real_ui() {
    use daedalus_tui::app::{config::ControlConfig, runtime_settings::SETTINGS};
    let mut ui = Ui::configured(false, true);
    let path = ui.directory.join(".config/daedalus/control.json");
    let before = fs::read(&path).unwrap();
    let field = SETTINGS
        .iter()
        .position(|s| s.key == "instance_import_frame_limit_bytes")
        .unwrap();
    let open = |ui: &mut Ui| {
        ui.master
            .write_all(b"\x10Terminals and discovery\r\r")
            .unwrap();
        ui.wait_text("Saved in this configuration");
        for _ in 0..field {
            ui.master.write_all(b"\x1b[B").unwrap();
        }
        ui.master.write_all(b"\r").unwrap();
    };
    ui.wait_text("LIVE TOPOLOGY");
    open(&mut ui);
    ui.wait_text("Saved value: 262144");
    ui.master.write_all(b"\x150\r").unwrap();
    ui.wait_text("must be between 1024 and 8388608");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x152048\r").unwrap();
    ui.wait_text("Save instance_import_frame_limit_bytes:");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Configuration saved privately");
    let saved = fs::read(&path).unwrap();
    let config = ControlConfig::parse(&saved).unwrap();
    assert_eq!(config.runtime.instance_import_frame_limit_bytes, 2048);
    assert_eq!(config.runtime.terminal_resize_pane_limit, 128);
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    open(&mut ui);
    ui.wait_text("Saved value: 2048");
    assert_eq!(fs::read(&path).unwrap(), saved);
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_text("Cancelled");
    ui.quit();
}
#[test]
fn native_metadata_policy_is_reviewed_saved_and_reopened_in_the_real_ui() {
    use daedalus_tui::app::{config::ControlConfig, runtime_settings::SETTINGS};
    let mut ui = Ui::configured(false, true);
    let path = ui.directory.join(".config/daedalus/control.json");
    let before = fs::read(&path).unwrap();
    let field = SETTINGS
        .iter()
        .position(|s| s.key == "native_metadata_entry_limit")
        .unwrap();
    ui.wait_text("LIVE TOPOLOGY");
    ui.master
        .write_all(b"\x10Terminals and discovery\r\r")
        .unwrap();
    ui.wait_text("Saved in this configuration");
    for _ in 0..field {
        ui.master.write_all(b"\x1b[B").unwrap();
    }
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Saved value: 16384");
    ui.master.write_all(b"0\r").unwrap();
    ui.wait_text("must be between 1 and 1048576");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"1234\r").unwrap();
    ui.wait_text("Save native_metadata_entry_limit:");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Configuration saved privately");
    assert_eq!(
        ControlConfig::parse(&fs::read(&path).unwrap())
            .unwrap()
            .runtime
            .native_metadata_entry_limit,
        1234
    );
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    assert_eq!(
        ControlConfig::parse(&fs::read(&path).unwrap())
            .unwrap()
            .runtime
            .native_metadata_entry_limit,
        1234
    );
    ui.quit();
}
#[test]
fn native_command_model_limit_is_reviewed_saved_and_reopened_in_the_real_ui() {
    use daedalus_tui::app::{config::ControlConfig, runtime_settings::SETTINGS};
    let mut ui = Ui::configured(false, true);
    let path = ui.directory.join(".config/daedalus/control.json");
    let before = fs::read(&path).unwrap();
    let field = SETTINGS
        .iter()
        .position(|s| s.key == "native_command_model_limit_bytes")
        .unwrap();
    ui.wait_text("LIVE TOPOLOGY");
    ui.master
        .write_all(b"\x10Terminals and discovery\r\r")
        .unwrap();
    ui.wait_text("Saved in this configuration");
    for _ in 0..field {
        ui.master.write_all(b"\x1b[B").unwrap();
    }
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Saved value: 128");
    ui.master.write_all(b"0\r").unwrap();
    ui.wait_text("must be between 1 and 4096");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"256\r").unwrap();
    ui.wait_text("Save native_command_model_limit_bytes:");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Configuration saved privately");
    assert_eq!(
        ControlConfig::parse(&fs::read(&path).unwrap())
            .unwrap()
            .runtime
            .native_command_model_limit_bytes,
        256
    );
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    assert_eq!(
        ControlConfig::parse(&fs::read(&path).unwrap())
            .unwrap()
            .runtime
            .native_command_model_limit_bytes,
        256
    );
    ui.quit();
}
#[test]
fn terminal_runtime_settings_are_validated_reviewed_saved_and_reopened() {
    let mut ui = Ui::configured(false, true);
    let path = ui.directory.join(".config/daedalus/control.json");
    let before = fs::read(&path).unwrap();
    ui.wait_text("LIVE TOPOLOGY");
    ui.master
        .write_all(b"\x10Terminals and discovery\r\r")
        .unwrap();
    ui.wait_text("Saved in this configuration");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Saved value: 120");
    ui.master.write_all(b"0\r").unwrap();
    ui.wait_text("must be between 2 and 512");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"93\r").unwrap();
    ui.wait_text("Save native_launch_cols:");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Configuration saved privately");
    let saved = fs::read(&path).unwrap();
    let config = daedalus_tui::app::config::ControlConfig::parse(&saved).unwrap();
    assert_eq!(config.runtime.native_launch_cols, 93);
    assert_eq!(config.runtime.shell_cols, 120);
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    assert_eq!(fs::read(&path).unwrap(), saved);
    ui.master
        .write_all(b"\x10Terminals and discovery\r\r")
        .unwrap();
    ui.wait_text("New terminal columns");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Saved value: 93");
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_text("Cancelled");
    ui.quit();
}
#[test]
fn clipboard_copy_limit_can_be_scrolled_to_reviewed_and_saved_in_the_real_ui() {
    use daedalus_tui::app::{config::ControlConfig, interface_settings::SETTINGS};
    let mut ui = Ui::configured(false, true);
    let path = ui.directory.join(".config/daedalus/control.json");
    let before = fs::read(&path).unwrap();
    let field = SETTINGS
        .iter()
        .position(|s| s.key == "clipboard_write_limit_bytes")
        .unwrap();
    ui.wait_text("LIVE TOPOLOGY");
    ui.master.write_all(b"\x10Interface settings\r\r").unwrap();
    ui.wait_text("Saved in this configuration");
    for _ in 0..field {
        ui.master.write_all(b"\x1b[B").unwrap();
    }
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Clipboard copy limit (bytes)");
    ui.wait_text("Saved value: 524290");
    ui.master.write_all(b"64\r").unwrap();
    ui.wait_text("Save clipboard_write_limit_bytes:");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Configuration saved privately");
    assert_eq!(
        ControlConfig::parse(&fs::read(&path).unwrap())
            .unwrap()
            .ui
            .clipboard_write_limit_bytes,
        64
    );
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    assert_eq!(
        ControlConfig::parse(&fs::read(&path).unwrap())
            .unwrap()
            .ui
            .clipboard_write_limit_bytes,
        64
    );
    ui.quit();
}
#[test]
fn dashboard_preferences_are_reviewed_saved_and_reopened_in_the_global_directory() {
    use daedalus_tui::app::{config::ControlConfig, dashboard::State};
    let mut ui = Ui::configured(false, true);
    ui.wait_text("LIVE TOPOLOGY");
    let root = ui.directory.join(".config/daedalus");
    let path = root.join("dashboard.json");
    let candidate = ui.directory.join("candidate");
    let mut config = ControlConfig::fresh(&ui.directory).unwrap();
    config.agent_name = "candidate".into();
    private_dashboard_file(&candidate.join("control.json"), &config.encode().unwrap()).unwrap();
    let explicit_root = ui.directory.to_string_lossy().into_owned();
    for (field, value) in [
        ("instance_limit", "23"),
        ("add-root", explicit_root.as_str()),
        ("add-instance", candidate.to_str().unwrap()),
    ] {
        let before = fs::read(&path).unwrap();
        ui.master
            .write_all(b"\x10Dashboard and instance discovery\r\r")
            .unwrap();
        ui.wait_text("Global preferences:");
        let index = match field {
            "add-root" => daedalus_tui::app::dashboard::FIELDS.len(),
            "add-instance" => daedalus_tui::app::dashboard::FIELDS.len() + 2,
            _ => daedalus_tui::app::dashboard::FIELDS
                .iter()
                .position(|f| f.key == field)
                .unwrap(),
        };
        for _ in 0..index {
            ui.master.write_all(b"\x1b[B").unwrap();
        }
        ui.master.write_all(b"\r").unwrap();
        if field == "instance_limit" {
            ui.wait_text("Saved value: 128.");
        } else {
            ui.wait_text("Enter an absolute local directory.");
        }
        ui.master
            .write_all(format!("\x15{value}\r").as_bytes())
            .unwrap();
        ui.wait_text("Save dashboard preferences?");
        assert_eq!(fs::read(&path).unwrap(), before);
        ui.master.write_all(b"\x1b[B\r").unwrap();
        ui.wait_text("Dashboard preferences saved");
    }
    let saved = State::load(&root).unwrap();
    assert_eq!(saved.settings.instance_limit, 23);
    assert_eq!(saved.settings.search_roots, vec![ui.directory.clone()]);
    assert_eq!(saved.saved_instances(), vec![candidate]);
    assert!(saved.show()["last_instance"].is_null());
    assert!(!root.join("telegram-runtime.json").exists());
    ui.quit();
    ui.restart();
    ui.wait_text("LIVE TOPOLOGY");
    let saved = State::load(&root).unwrap();
    assert_eq!(saved.settings.instance_limit, 23);
    assert!(!saved.settings.scan_processes);
    ui.quit();
}
#[test]
fn telegram_runtime_settings_are_reviewed_and_saved_without_starting_the_service() {
    use daedalus_tui::app::telegram_config::{ServiceConfig, ServicePlan};
    let mut ui = Ui::configured(false, true);
    ui.wait_text("LIVE TOPOLOGY");
    let directory = ui.directory.join(".config/daedalus");
    let path = directory.join("telegram-service.json");
    let config:ServiceConfig=serde_json::from_value(serde_json::json!({"version":1,"instance":"fixture","bot_id":123,"token_file":directory.join("token")})).unwrap();
    let plan = ServicePlan::fresh(&path, config).unwrap();
    plan.apply(&plan.approval_token()).unwrap();
    let before = fs::read(&path).unwrap();
    ui.master
        .write_all(b"\x10Telegram configuration\r\r")
        .unwrap();
    ui.wait_text("Edit this connection without repeating onboarding");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Saved settings for this connection");
    let index = daedalus_tui::app::telegram_runtime::FIELDS
        .iter()
        .position(|field| field.key == "loop_interval_ms")
        .unwrap();
    for _ in 0..index {
        ui.master.write_all(b"\x1b[B").unwrap();
    }
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Service cycle interval (ms)");
    ui.wait_text("Saved value: 100.");
    ui.master.write_all(b"\x15250\r").unwrap();
    ui.wait_text("Save loop_interval_ms:");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Telegram settings saved");
    assert_eq!(
        ServiceConfig::load(&path).unwrap().runtime.loop_interval_ms,
        250
    );
    assert!(!directory.join("telegram-runtime.json").exists());
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_presence("Telegram result", false);
    for (key, label, value) in [
        ("feedback_ack_enabled", "Queue reaction", "false"),
        ("callback_ttl_ms", "Confirmation lifetime (ms)", "2500"),
        (
            "progress_interval_ms",
            "Progress refresh interval (ms)",
            "500",
        ),
        (
            "stream_chunk_utf16_limit",
            "Stream chunk size (UTF-16 units)",
            "8",
        ),
    ] {
        let before = fs::read(&path).unwrap();
        ui.master
            .write_all(b"\x10Telegram configuration\r\r")
            .unwrap();
        ui.wait_text("Edit this connection without repeating onboarding");
        ui.master.write_all(b"\r").unwrap();
        ui.wait_text("Saved settings for this connection");
        let index = daedalus_tui::app::telegram_runtime::FIELDS
            .iter()
            .position(|field| field.key == key)
            .unwrap();
        for _ in 0..index {
            ui.master.write_all(b"\x1b[B").unwrap();
        }
        ui.master.write_all(b"\r").unwrap();
        ui.wait_text(label);
        ui.wait_text(&format!(
            "Saved value: {}.",
            ServiceConfig::load(&path)
                .unwrap()
                .runtime
                .display(key)
                .unwrap()
        ));
        ui.master
            .write_all(format!("\x15{value}\r").as_bytes())
            .unwrap();
        ui.wait_text(&format!("Save {key}:"));
        assert_eq!(fs::read(&path).unwrap(), before);
        ui.master.write_all(b"\x1b[B\r").unwrap();
        ui.wait_text("Telegram settings saved");
        assert_eq!(
            ServiceConfig::load(&path)
                .unwrap()
                .runtime
                .display(key)
                .unwrap(),
            value
        );
        ui.master.write_all(b"\x1b").unwrap();
        ui.wait_presence("Telegram result", false);
    }
    ui.quit();
    ui.restart();
    ui.wait_text("LIVE TOPOLOGY");
    let saved = ServiceConfig::load(&path).unwrap();
    assert!(!saved.runtime.feedback_ack_enabled);
    assert_eq!(saved.runtime.callback_ttl_ms, 2500);
    assert_eq!(saved.runtime.progress_interval_ms, 500);
    assert_eq!(saved.runtime.stream_chunk_utf16_limit, 8);
    assert!(!directory.join("telegram-runtime.json").exists());
    ui.quit();
}
#[test]
fn hindsight_settings_edit_full_query_and_exact_bank_with_review_and_reopen() {
    use daedalus_tui::app::memory::{self, Binding, Config};
    let mut ui = Ui::configured(false, true);
    ui.wait_text("LIVE TOPOLOGY");
    let directory = ui.directory.join(".config/daedalus");
    let path = directory.join("hindsight.json");
    let registration:daedalus_tui::app::hook_runtime::Registration=serde_json::from_value(serde_json::json!({
        "version":1,"id":"fixture-hook","directory":directory,
        "scope":{"instance":"fixture","terminal":{"backend":"herdr","endpoint":"fixture","pane":"pane","generation":"generation"},"conversation":"session","harness":"claude"},
        "goal_id":null,"harness_process":null,"transcript":null,"events":["Stop"],
        "options":{"telegram_delivery":false,"telegram_progress":false,"notifications":false,"hindsight":true},
        "activity_lease_ms":30000,"pending_timeout_ms":30000,"hook_timeout_ms":12000,
    })).unwrap();
    let write_private = |path: &PathBuf, bytes: &[u8]| {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path.parent().unwrap())
            .unwrap();
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    };
    write_private(
        &registration.path(),
        &serde_json::to_vec(&registration).unwrap(),
    );
    let mut config = Config::fresh()
        .set("endpoint", "http://127.0.0.1:8888")
        .unwrap();
    config.bindings.push(Binding {
        registration_id: registration.id.clone(),
        scope: registration.scope.clone(),
        bank: "initial-bank".into(),
    });
    let mut other = config.bindings[0].clone();
    other.registration_id = "inactive-other-hook".into();
    other.bank = "keep-this-bank".into();
    config.bindings.push(other.clone());
    write_private(&path, &serde_json::to_vec(&config).unwrap());
    let before = fs::read(&path).unwrap();
    ui.master.write_all(b"\x10Hindsight settings\r\r").unwrap();
    ui.wait_text("Changes remain in this draft");
    let select = |ui: &mut Ui, index: usize| {
        for _ in 0..index {
            ui.master.write_all(b"\x1b[B").unwrap();
        }
        ui.master.write_all(b"\r").unwrap();
    };
    select(&mut ui, 1);
    ui.wait_text("Ctrl+U clears the field.");
    ui.master.write_all(b"\x15http://localhost:8999\r").unwrap();
    ui.wait_text("Changes remain in this draft");
    select(&mut ui, 3);
    ui.wait_text("bytes of decoded text");
    let query = "x".repeat(8192);
    ui.send(format!("\x15\x1b[200~{query}\x1b[201~\r").as_bytes());
    ui.wait_text("Changes remain in this draft");
    select(&mut ui, 4);
    ui.wait_text("Ctrl+U clears the field.");
    ui.master.write_all(b"\x1599\r").unwrap();
    ui.wait_text("must be from 100 to 8000");
    ui.master.write_all(b"\x152500\r").unwrap();
    ui.wait_text("Changes remain in this draft");
    select(&mut ui, 0);
    ui.wait_text("Keep memory effects off");
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Changes remain in this draft");
    select(&mut ui, 5);
    ui.wait_text("Hindsight registration banks");
    select(&mut ui, 0);
    ui.wait_text("Edit registration bank");
    select(&mut ui, 0);
    ui.wait_text("Bank for the selected registration");
    ui.master.write_all(b"\x15replacement-bank\r").unwrap();
    ui.wait_text("Hindsight registration banks");
    ui.master.write_all(b"\x1b[F\r").unwrap();
    ui.wait_text("Changes remain in this draft");
    select(&mut ui, 6);
    ui.wait_text("Review Hindsight configuration");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Confirm Hindsight configuration");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Hindsight configuration saved");
    let config = memory::load(&directory).unwrap().unwrap();
    assert!(config.enabled);
    assert_eq!(config.endpoint, "http://localhost:8999");
    assert_eq!(config.timeout_ms, 2500);
    assert_eq!(config.recall_query, query);
    assert_eq!(config.bindings[0].bank, "replacement-bank");
    assert_eq!(config.bindings[1], other);
    assert!(!directory.join("delivery.sqlite3").exists());
    let saved = fs::read(&path).unwrap();
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    ui.master.write_all(b"\x10Hindsight settings\r\r").unwrap();
    ui.wait_text("Changes remain in this draft");
    select(&mut ui, 3);
    ui.wait_text("Recall query");
    ui.wait_text(&"x".repeat(64));
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_text("Memory configuration unchanged");
    assert_eq!(fs::read(&path).unwrap(), saved);
    ui.quit();
}
#[test]
fn focus_in_repaints_a_screen_the_host_blanked() {
    let mut ui = Ui::configured(false, true);
    ui.wait_text("LIVE TOPOLOGY");
    assert!(
        String::from_utf8_lossy(&ui.raw).contains("\x1b[?1004h"),
        "The interface asks to be told when its pane regains focus"
    );
    // A multiplexer blanks a pane it hid; the program is never told what the
    // screen now holds, so a diff against its own buffer repaints nothing.
    ui.screen.process(b"\x1b[2J\x1b[H");
    assert!(!ui.screen.screen().contents().contains("LIVE TOPOLOGY"));
    ui.master.write_all(b"\x1b[I").unwrap();
    ui.wait_text("LIVE TOPOLOGY");
    ui.quit();
}
#[test]
fn native_profile_editor_prefills_reviews_cancels_and_clears_saved_override() {
    use daedalus_tui::app::config::ControlConfig;
    let mut ui = Ui::configured(false, true);
    ui.wait_text("LIVE TOPOLOGY");
    ui.quit();
    let directory = ui.directory.join(".config/daedalus");
    let path = directory.join("control.json");
    let old = ui.directory.join("original-profile");
    let next = ui.directory.join("selected-profile");
    let sibling = ui.directory.join("sibling-profile");
    for profile in [&old, &next, &sibling] {
        fs::create_dir(profile).unwrap();
        fs::write(profile.join("native-account"), b"unchanged-account-fixture").unwrap();
    }
    let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    config
        .profiles
        .insert("claude".into(), old.to_str().unwrap().into());
    config
        .profiles
        .insert("codex".into(), sibling.to_str().unwrap().into());
    private_dashboard_file(&path, &config.encode().unwrap()).unwrap();
    let before = fs::read(&path).unwrap();
    ui.restart();
    ui.wait_text("0 terminals");
    ui.master
        .write_all(b"\x10Use an existing account profile")
        .unwrap();
    ui.wait_text("Guided control");
    ui.master.write_all(b"\r\r").unwrap();
    ui.wait_text("Each tool owns its account");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Profile root for Claude Code");
    ui.wait_text(old.to_str().unwrap());
    ui.master.write_all(b"\x15relative\r").unwrap();
    ui.wait_text("Profile directory must be absolute");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x15").unwrap();
    ui.master
        .write_all(next.to_str().unwrap().as_bytes())
        .unwrap();
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Save native account profile?");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\r").unwrap();
    ui.wait_presence("Save native account profile?", false);
    ui.wait_presence("UPDATING", false);
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master
        .write_all(b"\x10Use an existing account profile")
        .unwrap();
    ui.wait_text("Guided control");
    ui.master.write_all(b"\r\r").unwrap();
    ui.wait_text("Each tool owns its account");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Profile root for Claude Code");
    ui.wait_text(old.to_str().unwrap());
    ui.master.write_all(b"\x15").unwrap();
    ui.master
        .write_all(next.to_str().unwrap().as_bytes())
        .unwrap();
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Save native account profile?");
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Native profile selection saved");
    let saved = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved.profiles["claude"], next.to_str().unwrap());
    assert_eq!(saved.profiles["codex"], sibling.to_str().unwrap());
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    ui.master
        .write_all(b"\x10Use an existing account profile")
        .unwrap();
    ui.wait_text("Guided control");
    ui.master.write_all(b"\r\r").unwrap();
    ui.wait_text("Each tool owns its account");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Profile root for Claude Code");
    ui.wait_text(next.to_str().unwrap());
    ui.master.write_all(b"\x15inherit\r").unwrap();
    ui.wait_text("Save native account profile?");
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Native profile selection saved");
    let saved = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
    assert!(!saved.profiles.contains_key("claude"));
    assert_eq!(saved.profiles["codex"], sibling.to_str().unwrap());
    for profile in [&old, &next, &sibling] {
        assert_eq!(
            fs::read(profile.join("native-account")).unwrap(),
            b"unchanged-account-fixture"
        );
    }
    assert!(!directory.join("delivery.sqlite3").exists());
    ui.quit();
}

#[test]
fn gateway_editor_prefills_validates_and_preserves_disabled_state_after_reopen() {
    use daedalus_tui::app::gateway::{self, Config, Format};
    let mut ui = Ui::configured(false, true);
    ui.wait_text("LIVE TOPOLOGY");
    let directory = ui.directory.join(".config/daedalus");
    let path = directory.join("gateway.json");
    let config = Config {
        version: 1,
        enabled: false,
        format: Format::AnthropicMessages,
        endpoint: "https://gateway.example.invalid".into(),
        model: "initial-model".into(),
        credential_file: directory.join("unprovisioned-key"),
        harness: "claude".into(),
    };
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap()
        .write_all(&serde_json::to_vec(&config).unwrap())
        .unwrap();
    let before = fs::read(&path).unwrap();
    ui.master
        .write_all(b"\x10Connect an LLM proxy gateway\r\r")
        .unwrap();
    ui.wait_text("Editing settings preserves enable state");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Ctrl+U clears the field");
    ui.master
        .write_all(b"\x15http://external.example.invalid\r")
        .unwrap();
    ui.wait_text("Gateway requires HTTPS");
    ui.master
        .write_all(b"\x15http://localhost:8999/v1\r")
        .unwrap();
    ui.wait_text("Gateway model ID");
    ui.master.write_all(b"\x15updated-model\r").unwrap();
    ui.wait_text("Existing private credential file");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Gateway enable state");
    ui.wait_text("Keep disabled");
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Confirm gateway configuration");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Gateway configuration saved privately");
    let saved = gateway::load(&directory).unwrap().unwrap();
    assert!(!saved.enabled);
    assert_eq!(saved.model, "updated-model");
    assert_eq!(saved.endpoint, "http://localhost:8999/v1");
    assert_eq!(saved.credential_file, config.credential_file);
    assert!(!saved.credential_file.exists());
    assert!(fs::read_dir(&directory).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("gateway.backup-")
    }));
    assert!(!directory.join("delivery.sqlite3").exists());
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    assert_eq!(gateway::load(&directory).unwrap(), Some(saved));
    ui.quit();
}

#[test]
fn workspace_native_logos_negotiate_without_splash_and_hide_behind_options() {
    fn wait_raw(ui: &mut Ui, start: usize, needle: &str) {
        let deadline = Instant::now() + UI_WAIT;
        while !String::from_utf8_lossy(&ui.raw[start..]).contains(needle) {
            ui.wait_presence("impossible-logo-fixture-marker", false);
            assert!(
                Instant::now() < deadline,
                "Missing graphics command {needle:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let mut ui = Ui::with_graphics(true, false, true);
    wait_raw(&mut ui, 0, "\x1b_Gi=31");
    ui.master
        .write_all(b"\x1b_Gi=31;OK\x1b\\\x1b[6;16;8t\x1b[?62;22c")
        .unwrap();
    ui.wait_text("project-alpha");
    wait_raw(&mut ui, 0, "a=p,i=72");
    assert!(
        !String::from_utf8_lossy(&ui.raw).contains("a=t,f=100,i=7183"),
        "splash remains disabled"
    );
    let opening = ui.raw.len();
    ui.master.write_all(b",").unwrap();
    wait_raw(&mut ui, opening, "a=d,d=i,i=72");
    let hidden = ui.raw.len();
    ui.master.write_all(b"\x1b").unwrap();
    wait_raw(&mut ui, hidden, "a=p,i=72");
    assert!(
        !String::from_utf8_lossy(&ui.raw[hidden..]).contains("a=t,f=100"),
        "returning reuses cached pixels"
    );
    ui.quit();
    assert_eq!(fs::read_dir(&ui.directory).unwrap().count(), 0);
}

#[test]
fn harness_logo_mode_has_a_review_and_survives_restart() {
    use daedalus_tui::app::{config::ControlConfig, interface_settings::SETTINGS};
    let mut ui = Ui::configured(false, true);
    ui.wait_text("LIVE TOPOLOGY");
    let path = ui.directory.join(".config/daedalus/control.json");
    let before = fs::read(&path).unwrap();
    ui.master.write_all(b"\x10Interface settings").unwrap();
    ui.wait_text("Interface settings_");
    ui.wait_presence("UPDATING", false);
    ui.master.write_all(b"\r\r").unwrap();
    ui.wait_text("Saved in this configuration");
    let index = SETTINGS
        .iter()
        .position(|setting| setting.key == "harness_logos")
        .unwrap();
    for _ in 0..index {
        ui.master.write_all(b"\x1b[B").unwrap();
    }
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Saved value: auto");
    ui.master.write_all(b"\x1b[H\x1b[B\r").unwrap();
    ui.wait_text("Save harness_logos:");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Configuration saved privately");
    assert_eq!(
        ControlConfig::parse(&fs::read(&path).unwrap())
            .unwrap()
            .ui
            .harness_logos,
        "glyphs"
    );
    ui.quit();
    ui.restart();
    ui.wait_text("LIVE TOPOLOGY");
    assert_eq!(
        ControlConfig::parse(&fs::read(&path).unwrap())
            .unwrap()
            .ui
            .harness_logos,
        "glyphs"
    );
    ui.quit();
}

#[test]
fn screensaver_is_glitch_art_and_sends_no_images_when_terminal_does_not_reply() {
    let mut ui = Ui::with_graphics(true, false, true);
    // Only the screensaver's hint offers "any key".
    ui.wait_text("any key");
    let contents = ui.screen.screen().contents();
    let blocks = contents
        .chars()
        .filter(|c| ('\u{2580}'..='\u{259f}').contains(c))
        .count();
    assert!(
        blocks > 500,
        "The screensaver draws the artwork in block characters: {blocks}\n{contents}"
    );
    let raw = String::from_utf8_lossy(&ui.raw);
    assert!(
        !raw.contains("a=t,f=100") && !raw.contains("a=p,i="),
        "Unsupported graphics must retain text without image uploads or placements"
    );
    ui.master.write_all(b"x").unwrap();
    ui.wait_text("project-alpha");
    ui.quit();
    assert_eq!(fs::read_dir(&ui.directory).unwrap().count(), 0);
}

#[test]
fn finish_setup_persists_local_configuration() {
    let mut ui = Ui::start(false);
    ui.wait_text("Set up Daedalus");
    // Select the advertised Finish and explore terminals action in the real UI.
    for _ in 0..5 {
        ui.master.write_all(b"\x1b[B").unwrap();
    }
    ui.master.write_all(b"\r").unwrap();
    let config = ui.directory.join(".config/daedalus/control.json");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !config.exists() && Instant::now() < deadline {
        let mut bytes = [0; 16384];
        if let Ok(n) = ui.master.read(&mut bytes) {
            ui.screen.process(&bytes[..n]);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        config.exists(),
        "Finish returned to the dashboard without saving configuration; Attach will reject this instance"
    );
    daedalus_tui::app::config::ControlConfig::parse(&fs::read(&config).unwrap()).unwrap();
    let before = fs::read(&config).unwrap();
    ui.wait_presence("Set up Daedalus", false);
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    assert!(!ui.screen.screen().contents().contains("Set up Daedalus"));
    assert_eq!(fs::read(config).unwrap(), before);
    ui.quit();
}

#[test]
fn rapid_navigation_and_search_preserve_input_order() {
    let mut ui = Ui::start(true);
    ui.wait_text("project-alpha");
    ui.master
        .write_all(b"]]/burst-preserves-every-keystroke")
        .unwrap();
    ui.wait_text("burst-preserves-every-keystroke");
    ui.master.write_all(b"\x1b").unwrap();
    ui.wait_presence("burst-preserves-every-keystroke", false);
    ui.quit();
    assert_eq!(fs::read_dir(&ui.directory).unwrap().count(), 0);
}

#[test]
fn workspace_usage_recording_switch_is_reviewed_saved_and_reopened() {
    use daedalus_tui::app::{config::ControlConfig, interface_settings::SETTINGS};
    let mut ui = Ui::configured(false, true);
    let path = ui.directory.join(".config/daedalus/control.json");
    let before = fs::read(&path).unwrap();
    ui.wait_text("LIVE TOPOLOGY");
    ui.master.write_all(b"\x10Interface settings").unwrap();
    ui.wait_text("Interface settings_");
    ui.wait_presence("UPDATING", false);
    ui.master.write_all(b"\r\r").unwrap();
    ui.wait_text("Saved in this configuration");
    let index = SETTINGS
        .iter()
        .position(|s| s.key == "workspace_usage_recording")
        .unwrap();
    for _ in 0..index {
        ui.master.write_all(b"\x1b[B").unwrap();
    }
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Saved value: On");
    ui.master.write_all(b"\x1b[H\x1b[B\r").unwrap();
    ui.wait_text("Save workspace_usage_recording:");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Configuration saved privately");
    assert!(
        !ControlConfig::parse(&fs::read(&path).unwrap())
            .unwrap()
            .ui
            .workspace_usage_recording
    );
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    assert!(
        !ControlConfig::parse(&fs::read(&path).unwrap())
            .unwrap()
            .ui
            .workspace_usage_recording
    );
    ui.quit();
}
#[test]
fn workspace_usage_gap_is_validated_reviewed_saved_and_reopened() {
    use daedalus_tui::app::{config::ControlConfig, runtime_settings::SETTINGS};
    let mut ui = Ui::configured(false, true);
    let path = ui.directory.join(".config/daedalus/control.json");
    let before = fs::read(&path).unwrap();
    let index = SETTINGS
        .iter()
        .position(|s| s.key == "workspace_usage_max_gap_ms")
        .unwrap();
    ui.wait_text("LIVE TOPOLOGY");
    ui.master
        .write_all(b"\x10Terminals and discovery\r\r")
        .unwrap();
    ui.wait_text("Saved in this configuration");
    for _ in 0..index {
        ui.master.write_all(b"\x1b[B").unwrap();
    }
    ui.master.write_all(b"\r").unwrap();
    ui.wait_text("Saved value: 30000");
    ui.master.write_all(b"0\r").unwrap();
    ui.wait_text("must be between 1 and 86400000");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"1234\r").unwrap();
    ui.wait_text("Save workspace_usage_max_gap_ms:");
    assert_eq!(fs::read(&path).unwrap(), before);
    ui.master.write_all(b"\x1b[B\r").unwrap();
    ui.wait_text("Configuration saved privately");
    assert_eq!(
        ControlConfig::parse(&fs::read(&path).unwrap())
            .unwrap()
            .runtime
            .workspace_usage_max_gap_ms,
        1234
    );
    ui.quit();
    ui.restart();
    ui.wait_text("0 terminals");
    assert_eq!(
        ControlConfig::parse(&fs::read(&path).unwrap())
            .unwrap()
            .runtime
            .workspace_usage_max_gap_ms,
        1234
    );
    ui.quit();
}
