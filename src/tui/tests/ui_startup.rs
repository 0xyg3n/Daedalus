#![cfg(unix)]
//! The installed executable starts its own backend with Python absent from PATH.
//! PTYs and private state are synthetic; no live terminal server is contacted.
use std::{
    fs::{self, File},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::DirBuilderExt, process::CommandExt},
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Ui {
    command: Command,
    child: Child,
    master: File,
    directory: PathBuf,
    screen: vt100::Parser,
    raw: Vec<u8>,
}
impl Ui {
    fn start(demo: bool) -> Self {
        Self::configured(demo, false)
    }
    fn configured(demo: bool, configured: bool) -> Self {
        Self::with_graphics(demo, configured, false)
    }
    fn with_graphics(demo: bool, configured: bool, graphics: bool) -> Self {
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
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    &size,
                )
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
            .stderr(Stdio::null());
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
        }
    }
    fn wait_text(&mut self, text: &str) {
        self.wait_presence(text, true);
    }
    fn wait_presence(&mut self, text: &str, present: bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
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
                "TUI exited: {}",
                self.screen.screen().contents()
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
        let deadline = Instant::now() + Duration::from_secs(5);
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
fn screensaver_is_glitch_art_and_sends_no_pictures_without_the_splash() {
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
        !raw.contains("\x1b_G"),
        "No graphics query, upload or placement without the splash"
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
    for _ in 0..4 {
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
    ui.screen = vt100::Parser::new(48, 160, 0);
    ui.child = ui.command.spawn().unwrap();
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
