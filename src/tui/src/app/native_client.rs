//! A native viewer owns a child PTY, never Daedalus's keyboard escape route.
//! Detaching terminates only that viewer client, not the source terminal.
use portable_pty::{Child, CommandBuilder, PtySize};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::fd::AsRawFd,
    process::Command,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, String>;

#[cfg(test)]
const F11: &[u8] = b"\x1b[23~";
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Default)]
struct Input {
    pending: Vec<u8>,
    prefix: Vec<u8>,
    paste: bool,
}
// CSI-u uses codepoint;1+modifiers:event-type, and modifyOtherKeys uses
// CSI 27;1+modifiers;codepoint~. Decode reserved keys only; preserve every
// other byte. https://sw.kovidgoyal.net/kitty/keyboard-protocol/
fn keyboard(bytes: &[u8]) -> Option<(u32, u32, u32)> {
    if bytes == b"\x1d" {
        return Some((93, 4, 1));
    }
    if bytes.len() == 1 {
        return Some((u32::from(bytes[0]), 0, 1));
    }
    let body = std::str::from_utf8(bytes.strip_prefix(b"\x1b[")?).ok()?;
    let end = body.as_bytes().last()?;
    let fields = body[..body.len() - 1].split(';').collect::<Vec<_>>();
    let number = |text: &str| text.split(':').next()?.parse::<u32>().ok();
    let first = number(fields.first()?)?;
    let flags = fields.get(1).copied().unwrap_or("1");
    let modifiers = number(flags)?.checked_sub(1)? & 63;
    let event = flags
        .split(':')
        .nth(1)
        .map_or(Some(1), |s| s.parse::<u32>().ok())?;
    if *end == b'u' {
        Some((first, modifiers, event))
    } else if *end == b'~' && first == 27 {
        Some((number(fields.get(2)?)?, modifiers, event))
    } else if *end == b'~' && first == 23 {
        Some((57374, modifiers, event))
    } else {
        None
    }
}
impl Input {
    fn token(&mut self, token: Vec<u8>, output: &mut Vec<u8>) -> bool {
        if token == PASTE_START {
            output.append(&mut self.prefix);
            output.extend(token);
            self.paste = true;
            return false;
        }
        if let Some((code, modifiers, event)) = keyboard(&token) {
            if code == 57374 && event != 3 {
                return true;
            }
            if !self.prefix.is_empty()
                && (event == 3 || (57441..=57454).contains(&code))
                && self.prefix.len() < 4096
            {
                self.prefix.extend(token);
                return false;
            }
            if (matches!(code, 93 | 53) && modifiers & 4 != 0 || code == 29)
                && modifiers & !5 == 0
                && event != 3
            {
                if self.prefix.is_empty() {
                    self.prefix = token;
                } else {
                    output.append(&mut self.prefix);
                    output.extend(token);
                }
                return false;
            }
            if matches!(code, 100 | 68)
                && modifiers & !1 == 0
                && event != 3
                && !self.prefix.is_empty()
            {
                self.prefix.clear();
                return true;
            }
        }
        output.append(&mut self.prefix);
        output.extend(token);
        false
    }
    fn push(&mut self, bytes: &[u8]) -> (Vec<u8>, bool) {
        let mut output = Vec::new();
        for byte in bytes {
            if self.paste {
                self.pending.push(*byte);
                while !self.pending.is_empty() {
                    if self.pending == PASTE_END {
                        output.append(&mut self.pending);
                        self.paste = false;
                        break;
                    }
                    if PASTE_END.starts_with(&self.pending) {
                        break;
                    }
                    output.push(self.pending.remove(0));
                }
                continue;
            }
            if self.pending.is_empty() && *byte != 0x1b {
                if self.token(vec![*byte], &mut output) {
                    return (output, true);
                }
                continue;
            }
            self.pending.push(*byte);
            let complete = if self.pending.starts_with(b"\x1b[") {
                self.pending.len() > 2 && (0x40..=0x7e).contains(byte) || self.pending.len() > 128
            } else {
                self.pending.len() > 1
            };
            if complete {
                let token = std::mem::take(&mut self.pending);
                if self.token(token, &mut output) {
                    return (output, true);
                }
            }
        }
        (output, false)
    }
    fn flush_escape(&mut self) -> Vec<u8> {
        if self.pending.first() == Some(&0x1b) && !self.paste {
            let mut output = std::mem::take(&mut self.prefix);
            output.append(&mut self.pending);
            output
        } else {
            Vec::new()
        }
    }
}

struct Terminal {
    tty: File,
    original: libc::termios,
}
impl Terminal {
    fn open() -> Result<Self> {
        let tty = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .map_err(|_| "Native client needs a controlling terminal")?;
        let mut original = std::mem::MaybeUninit::uninit();
        if unsafe { libc::tcgetattr(tty.as_raw_fd(), original.as_mut_ptr()) } != 0 {
            return Err("Cannot read native terminal mode".into());
        }
        let original = unsafe { original.assume_init() };
        let mut raw = original;
        unsafe {
            libc::cfmakeraw(&mut raw);
        }
        if unsafe { libc::tcsetattr(tty.as_raw_fd(), libc::TCSANOW, &raw) } != 0 {
            return Err("Cannot enable native terminal input".into());
        }
        Ok(Self { tty, original })
    }
    fn size(&self) -> Result<PtySize> {
        let mut size = std::mem::MaybeUninit::<libc::winsize>::zeroed();
        if unsafe { libc::ioctl(self.tty.as_raw_fd(), libc::TIOCGWINSZ, size.as_mut_ptr()) } != 0 {
            return Err("Cannot read native viewer size".into());
        }
        let size = unsafe { size.assume_init() };
        Ok(PtySize {
            rows: size.ws_row.max(2),
            cols: size.ws_col.max(2),
            pixel_width: size.ws_xpixel,
            pixel_height: size.ws_ypixel,
        })
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.tty.write_all(
            b"\x1b[=0u\x1b[>4;0m\x1b[?1l\x1b>\x1b[0m\x1b[?25h\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1049l",
        );
        let _ = self.tty.flush();
        unsafe {
            libc::tcsetattr(self.tty.as_raw_fd(), libc::TCSANOW, &self.original);
        }
    }
}
struct Viewer(Box<dyn Child + Send + Sync>);
impl Drop for Viewer {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        let _ = self.0.kill();
        let deadline = Instant::now() + Duration::from_millis(150);
        while Instant::now() < deadline {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // A viewer that ignores SIGHUP must not hold the dashboard hostage.
        // Signal its exact child PID, never its process group or source pane.
        if let Some(pid) = self.0.process_id() {
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
        let deadline = Instant::now() + Duration::from_millis(150);
        while Instant::now() < deadline {
            if self.0.try_wait().ok().flatten().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

pub fn run(command: Command) -> Result<String> {
    let mut terminal = Terminal::open()?;
    let mut size = terminal.size()?;
    let pair = portable_pty::native_pty_system()
        .openpty(size)
        .map_err(|_| "Cannot open native viewer PTY")?;
    let mut builder = CommandBuilder::new(command.get_program());
    builder.args(command.get_args());
    builder.env_clear();
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            builder.env(key, value);
        }
    }
    if let Some(directory) = command.get_current_dir() {
        builder.cwd(directory);
    }
    let mut viewer = Viewer(
        pair.slave
            .spawn_command(builder)
            .map_err(|_| "Cannot start native viewer client")?,
    );
    drop(pair.slave);
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|_| "Cannot read native viewer")?;
    let mut writer = pair
        .master
        .take_writer()
        .map_err(|_| "Cannot write native viewer")?;
    let master = pair
        .master
        .as_raw_fd()
        .ok_or("Native viewer PTY cannot be polled")?;
    terminal.tty.write_all(b"\r\nDaedalus native viewer: F11 or Ctrl+] then D returns. The source terminal keeps running.\r\n")
        .and_then(|_|terminal.tty.flush()).map_err(|_| "Cannot display native viewer return keys")?;
    let mut input = Input::default();
    let mut last_input = Instant::now();
    let mut buffer = [0u8; 16384];
    loop {
        if let Some(status) = viewer
            .0
            .try_wait()
            .map_err(|_| "Cannot inspect native viewer")?
        {
            return if status.success() {
                Ok("Native client detached; original terminal remains running".into())
            } else {
                Err("Native client exited unsuccessfully; original terminal remains running".into())
            };
        }
        let next = terminal.size()?;
        if next != size {
            pair.master
                .resize(next)
                .map_err(|_| "Cannot resize native viewer")?;
            size = next;
        }
        let mut fds = [
            libc::pollfd {
                fd: terminal.tty.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: master,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 40) } < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err("Cannot poll native viewer".into());
        }
        if fds[0].revents & libc::POLLIN != 0 {
            let count = terminal
                .tty
                .read(&mut buffer)
                .map_err(|_| "Cannot read native viewer input")?;
            if count == 0 {
                return Ok("Native viewer input closed; original terminal remains running".into());
            }
            let (bytes, detach) = input.push(&buffer[..count]);
            writer
                .write_all(&bytes)
                .and_then(|_| writer.flush())
                .map_err(|_| "Native viewer input ended")?;
            if detach {
                return Ok("Returned to Daedalus; original terminal remains running".into());
            }
            last_input = Instant::now();
        } else if last_input.elapsed() >= Duration::from_millis(100) {
            let bytes = input.flush_escape();
            if !bytes.is_empty() {
                writer
                    .write_all(&bytes)
                    .and_then(|_| writer.flush())
                    .map_err(|_| "Native viewer input ended")?;
            }
        }
        if fds[1].revents & libc::POLLIN != 0 {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    return Ok("Native viewer closed; original terminal remains running".into());
                }
                Ok(count) => terminal
                    .tty
                    .write_all(&buffer[..count])
                    .and_then(|_| terminal.tty.flush())
                    .map_err(|_| "Cannot render native viewer")?,
                Err(error) if error.raw_os_error() == Some(libc::EIO) => {
                    return Ok("Native viewer closed; original terminal remains running".into());
                }
                Err(_) => return Err("Cannot read native viewer output".into()),
            }
        }
        if fds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            return Ok("Native viewer input closed; original terminal remains running".into());
        }
        if fds[1].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            return Ok("Native viewer closed; original terminal remains running".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enhanced_keyboard_prefixes_and_release_events_keep_the_same_return_shortcut() {
        for bytes in [
            b"\x1b[93;5u\x1b[100u".as_slice(),
            b"\x1b[93;5:1u\x1b[57442;1:3u\x1b[93;1:3u\x1b[100u",
            b"\x1b[29;5u\x1b[100u",
            b"\x1b[93;5:1u\x1b[93;5:3u\x1b[100;1:1u",
            b"\x1b[27;5;93~d",
            b"\x1dd",
            b"\x1d\x1b[100u",
            b"\x1b[53;5uD",
            b"\x1b[23;1:1~",
        ] {
            let mut input = Input::default();
            let mut forwarded = Vec::new();
            let mut detached = false;
            for byte in bytes {
                let (output, done) = input.push(&[*byte]);
                forwarded.extend(output);
                detached |= done;
            }
            assert!(detached, "return not recognized: {bytes:?}");
            assert!(
                forwarded.is_empty(),
                "shortcut was forwarded: {forwarded:?}"
            );
        }
        let mut input = Input::default();
        let bytes = b"\x1b[93;5u\x1b[93;5:3u\x1b[120u";
        assert_eq!(input.push(bytes), (bytes.to_vec(), false));
    }
    #[test]
    #[ignore = "isolated PTY child fixture, executed by the native return regression"]
    fn native_return_child_fixture() {
        use std::process::Stdio;
        let mut source = Command::new("/bin/sleep")
            .arg("10")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let tty = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .unwrap();
        let mut before = std::mem::MaybeUninit::uninit();
        assert_eq!(
            unsafe { libc::tcgetattr(tty.as_raw_fd(), before.as_mut_ptr()) },
            0
        );
        let before = unsafe { before.assume_init() };
        let mut viewer = Command::new("/bin/sh");
        viewer.args([
            "-c",
            r#"trap '' HUP; test "$(/bin/stty size)" = "30 100" || exit 1; /bin/stty raw -echo; printf NATIVE_READY; exec /bin/cat"#,
        ]);
        viewer.env("TERM", "xterm-256color");
        let result = run(viewer);
        let alive = source.try_wait().unwrap().is_none();
        let _ = source.kill();
        let _ = source.wait();
        assert!(result.unwrap().contains("Returned to Daedalus"));
        assert!(alive, "source process was terminated");
        let mut after = std::mem::MaybeUninit::uninit();
        assert_eq!(
            unsafe { libc::tcgetattr(tty.as_raw_fd(), after.as_mut_ptr()) },
            0
        );
        let after = unsafe { after.assume_init() };
        assert_eq!(before.c_lflag, after.c_lflag);
        assert_eq!(before.c_iflag, after.c_iflag);
        println!("NATIVE_RETURNED_SOURCE_ALIVE_MODES_RESTORED");
    }
    #[test]
    fn native_viewer_f11_and_prefix_return_from_a_real_pty_without_stopping_the_source() {
        for shortcut in [F11, b"\x1dd".as_slice(), b"\x1b[93;5u\x1b[100u"] {
            let pair = portable_pty::native_pty_system()
                .openpty(PtySize {
                    rows: 30,
                    cols: 100,
                    ..PtySize::default()
                })
                .unwrap();
            let mut command = CommandBuilder::new(std::env::current_exe().unwrap());
            command.args([
                "--ignored",
                "--exact",
                "app::native_client::tests::native_return_child_fixture",
                "--nocapture",
            ]);
            let mut child = Viewer(pair.slave.spawn_command(command).unwrap());
            drop(pair.slave);
            let fd = pair.master.as_raw_fd().unwrap();
            let mut reader = pair.master.try_clone_reader().unwrap();
            let mut writer = pair.master.take_writer().unwrap();
            let mut output = Vec::new();
            let mut sent = false;
            let mut escaped = false;
            let deadline = Instant::now() + Duration::from_secs(6);
            loop {
                let mut fd = [libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                }];
                assert!(unsafe { libc::poll(fd.as_mut_ptr(), 1, 40) } >= 0);
                if fd[0].revents & libc::POLLIN != 0 {
                    let mut bytes = [0u8; 8192];
                    if let Ok(count) = reader.read(&mut bytes) {
                        output.extend_from_slice(&bytes[..count]);
                    }
                }
                let shown = String::from_utf8_lossy(&output);
                if !sent && shown.contains("NATIVE_READY") {
                    writer.write_all(b"SAFE_INPUT").unwrap();
                    writer.flush().unwrap();
                    sent = true;
                }
                if sent && !escaped && shown.contains("SAFE_INPUT") {
                    writer.write_all(&shortcut[..1]).unwrap();
                    writer.flush().unwrap();
                    std::thread::sleep(Duration::from_millis(20));
                    writer.write_all(&shortcut[1..]).unwrap();
                    writer.flush().unwrap();
                    escaped = true;
                }
                if let Some(status) = child.0.try_wait().unwrap() {
                    assert!(status.success(), "{shown}");
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "native viewer failed to return: {shown}"
                );
            }
            assert!(
                String::from_utf8_lossy(&output)
                    .contains("NATIVE_RETURNED_SOURCE_ALIVE_MODES_RESTORED")
            );
            assert!(
                !output
                    .windows(shortcut.len())
                    .any(|bytes| bytes == shortcut),
                "detach shortcut leaked into viewer input"
            );
        }
    }
    #[test]
    fn return_shortcuts_are_fragment_safe_and_never_reach_the_client() {
        for shortcut in [b"\x1dd".as_slice(), b"\x1dD", F11] {
            for split in 0..shortcut.len() {
                let mut input = Input::default();
                let (first, detach) = input.push(&shortcut[..split]);
                assert!(!detach && first.is_empty());
                let (rest, detach) = input.push(&shortcut[split..]);
                assert!(detach && rest.is_empty());
            }
        }
        let mut input = Input::default();
        assert_eq!(
            input.push(b"normal\x1d\x1d\x1dx"),
            (b"normal\x1d\x1d\x1dx".to_vec(), false)
        );
        assert_eq!(input.push(b"\x1b"), (vec![], false));
        assert_eq!(input.flush_escape(), b"\x1b");
    }
    #[test]
    fn bracketed_paste_is_forwarded_verbatim_even_when_it_contains_return_sequences() {
        let bytes = b"\x1b[200~text\x1dd\x1b[23~\x1b[201~";
        let mut input = Input::default();
        let mut output = Vec::new();
        for byte in bytes {
            let (forward, detach) = input.push(&[*byte]);
            assert!(!detach);
            output.extend(forward);
        }
        assert_eq!(output, bytes);
        assert!(input.push(F11).1);
    }
}
