//! Embedded interactive terminal views.
//!
//! The backend verifies the target and hands over the exact argv of a stream
//! client (`Plan`). This module runs that client, keeps a VT screen with the
//! `vt100` parser, renders it, and encodes keys, paste and mouse input as the
//! raw bytes a terminal would send. Detaching ends only the stream client;
//! the terminal and its processes keep running.
use crate::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};
use serde::Deserialize;
use std::{
    collections::{HashSet, VecDeque},
    io::{self, BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

/// Bytes per `send-keys -H` command; keeps control-mode lines short.
const CHUNK: usize = 96;
/// Upper bound of stream data handled per frame, so input stays responsive.
const BUDGET: usize = 256 * 1024;
const PUMP_SLICE: Duration = Duration::from_millis(4);
const CHECK_EVERY: Duration = Duration::from_secs(2);
/// Herdr refuses an attach while a pane read is running; it asks to retry.
/// Retries wait a little longer each time.
const RETRIES: u8 = 5;
/// With no screen from the stream client by then, it is not coming.
const NO_SCREEN: Duration = Duration::from_secs(8);

#[derive(Clone, Default, Deserialize, Debug)]
#[serde(default)]
pub struct Plan {
    pub adapter: String,
    pub argv: Vec<String>,
    /// Herdr: stream variants; the frontend appends --cols/--rows after the target.
    pub observe_argv: Vec<String>,
    pub control_argv: Vec<String>,
    pub takeover_argv: Vec<String>,
    pub mode: String,
    pub title: String,
    pub backend: String,
    pub server: String,
    pub terminal: String,
    pub pid: String,
    pub views: usize,
    pub input: String,
    pub takeover: bool,
    pub node: String,
    /// Local clients that already hold the terminal's input (Herdr).
    pub holders: Vec<Holder>,
}

#[derive(Clone, Default, Deserialize, Debug, PartialEq)]
#[serde(default)]
pub struct Holder {
    pub pid: u32,
    pub how: String,
    /// The program that started it, by name only.
    pub by: String,
}

/// "herdr agent attach (process 42, from project_terminal.py)".
pub fn describe_holders(holders: &[Holder]) -> String {
    holders
        .iter()
        .map(|h| {
            let from = if h.by.is_empty() {
                String::new()
            } else {
                format!(", from {}", h.by)
            };
            format!("{} (process {}{from})", h.how, h.pid)
        })
        .collect::<Vec<_>>()
        .join(" and ")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Output only. Daedalus sends nothing to the terminal.
    Observe,
    /// Keys, paste and mouse go to the terminal.
    Control,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Phase {
    Connecting,
    Live,
    Ended(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Purpose {
    Info,
    Primary,
    Screen,
    Check,
    Input,
    Flags,
}

/// Pane state reported by tmux, enough to rebuild the VT state on attach.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneInfo {
    pub pid: String,
    pub cols: u16,
    pub rows: u16,
    pub cursor: (u16, u16),
    pub alternate: bool,
    pub region: (u16, u16),
    pub cursor_visible: bool,
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub mouse: [bool; 5],
    pub insert: bool,
    pub origin: bool,
    pub wrap: bool,
}

pub const INFO_FORMAT: &str = "#{pane_pid} #{pane_width} #{pane_height} #{cursor_x} #{cursor_y} #{alternate_on} #{scroll_region_upper} #{scroll_region_lower} #{cursor_flag} #{keypad_cursor_flag} #{keypad_flag} #{mouse_standard_flag} #{mouse_button_flag} #{mouse_any_flag} #{mouse_sgr_flag} #{mouse_utf8_flag} #{insert_flag} #{origin_flag} #{wrap_flag}";

impl PaneInfo {
    pub fn parse(line: &str) -> Option<Self> {
        let f: Vec<&str> = line.split(' ').collect();
        if f.len() != 19 {
            return None;
        }
        let n = |i: usize| f[i].parse::<u16>().ok();
        let b = |i: usize| f[i] == "1";
        Some(Self {
            pid: f[0].to_owned(),
            cols: n(1)?.max(1),
            rows: n(2)?.max(1),
            cursor: (n(3)?, n(4)?),
            alternate: b(5),
            region: (n(6).unwrap_or(0), n(7).unwrap_or(0)),
            cursor_visible: b(8),
            app_cursor: b(9),
            app_keypad: b(10),
            mouse: [b(11), b(12), b(13), b(14), b(15)],
            insert: b(16),
            origin: b(17),
            wrap: b(18),
        })
    }
    /// Escape sequences that put a fresh parser into this pane's state.
    pub fn restore(&self, primary: Option<&[u8]>, screen: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(primary) = primary {
            draw(&mut out, primary);
            out.extend_from_slice(b"\x1b[?1049h");
        }
        draw(&mut out, screen);
        let (upper, lower) = self.region;
        if lower > upper && (upper > 0 || lower + 1 < self.rows) {
            out.extend_from_slice(format!("\x1b[{};{}r", upper + 1, lower + 1).as_bytes());
        }
        if self.origin {
            out.extend_from_slice(b"\x1b[?6h");
        }
        let row = if self.origin {
            self.cursor.1.saturating_sub(upper)
        } else {
            self.cursor.1
        };
        out.extend_from_slice(format!("\x1b[{};{}H", row + 1, self.cursor.0 + 1).as_bytes());
        let modes: [(bool, &[u8]); 10] = [
            (!self.cursor_visible, b"\x1b[?25l"),
            (self.app_cursor, b"\x1b[?1h"),
            (self.app_keypad, b"\x1b="),
            (self.mouse[0], b"\x1b[?1000h"),
            (self.mouse[1], b"\x1b[?1002h"),
            (self.mouse[2], b"\x1b[?1003h"),
            (self.mouse[3], b"\x1b[?1006h"),
            (self.mouse[4], b"\x1b[?1005h"),
            (self.insert, b"\x1b[4h"),
            (!self.wrap, b"\x1b[?7l"),
        ];
        for (on, code) in modes {
            if on {
                out.extend_from_slice(code);
            }
        }
        out
    }
}

/// Draw captured lines at absolute rows, so no line can scroll the screen.
fn draw(out: &mut Vec<u8>, captured: &[u8]) {
    for (i, line) in captured.split(|&b| b == b'\n').enumerate() {
        out.extend_from_slice(format!("\x1b[{};1H\x1b[0m", i + 1).as_bytes());
        out.extend_from_slice(line.strip_suffix(b"\r").unwrap_or(line));
    }
    out.extend_from_slice(b"\x1b[0m");
}

/// tmux control mode escapes bytes below 32 and backslash as `\ooo`.
pub fn unescape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        let octal = |j: usize| data.get(j).is_some_and(|c| (b'0'..=b'7').contains(c));
        if data[i] == b'\\' && octal(i + 1) && octal(i + 2) && octal(i + 3) {
            let value = (u32::from(data[i + 1] - b'0') << 6)
                | (u32::from(data[i + 2] - b'0') << 3)
                | u32::from(data[i + 3] - b'0');
            out.push(value as u8);
            i += 4;
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}

/// A `send-keys -H` command for raw bytes; hex leaves no room for quoting bugs.
pub fn hex_keys(pane: &str, bytes: &[u8]) -> String {
    let mut line = format!("send-keys -t {pane} -H");
    for b in bytes {
        line.push_str(&format!(" {b:02x}"));
    }
    line
}

pub fn valid_tmux_pane(pane: &str) -> bool {
    pane.len() > 1 && pane.starts_with('%') && pane[1..].bytes().all(|b| b.is_ascii_digit())
}

/// Lines from the stream client: tmux control mode or Herdr JSON frames.
enum Stream {
    Line(Vec<u8>),
    /// The client exited; carries the last line it wrote to stderr, if any.
    Closed(String),
}

struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<Stream>,
}

fn launch(argv: &[String]) -> io::Result<Client> {
    let Some((program, args)) = argv.split_first() else {
        return Err(io::Error::other("Empty stream command"));
    };
    let mut child = Command::new(program)
        .args(args)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdin = child.stdin.take();
    let (etx, erx) = mpsc::channel();
    if let Some(mut stderr) = child.stderr.take() {
        // Drain it all so the client never blocks; keep only the tail.
        thread::spawn(move || {
            let (mut tail, mut chunk) = (Vec::new(), [0u8; 4096]);
            while let Ok(n @ 1..) = stderr.read(&mut chunk) {
                tail.extend_from_slice(&chunk[..n]);
                let excess = tail.len().saturating_sub(2048);
                tail.drain(..excess);
            }
            let _ = etx.send(last_line(&tail));
        });
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("No stream"))?;
    let (tx, rx) = mpsc::sync_channel(32);
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if line.last() == Some(&b'\n') {
                        line.pop();
                    }
                    if tx.send(Stream::Line(line.clone())).is_err() {
                        return;
                    }
                }
            }
        }
        let detail = erx
            .recv_timeout(Duration::from_millis(500))
            .unwrap_or_default();
        let _ = tx.send(Stream::Closed(detail));
    });
    Ok(Client { child, stdin, rx })
}

/// Last non-empty stderr line, printable and short enough for a notice.
fn last_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let line = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    line.chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Herdr's stream CLI reads the target first; the size options follow it.
pub fn sized(argv: &[String], rows: u16, cols: u16) -> Vec<String> {
    let mut argv = argv.to_vec();
    argv.extend([
        "--cols".into(),
        cols.max(2).to_string(),
        "--rows".into(),
        rows.max(2).to_string(),
    ]);
    argv
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn b64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        if c == b'=' {
            break;
        }
        let v = B64.iter().position(|&b| b == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

pub struct Embed {
    pub plan: Plan,
    pub mode: Mode,
    pub phase: Phase,
    pub parser: vt100::Parser,
    /// Ctrl+] was pressed; the next key is a Daedalus command.
    pub prefix: bool,
    /// tmux only: this client sizes the window (explicit, affects other viewers).
    pub fit: bool,
    pub notice: String,
    /// Display area of the last frame (rows, cols).
    pub viewport: (u16, u16),
    /// Where the terminal's top-left cell was drawn, for mouse mapping.
    pub origin: Option<(u16, u16, u16, u16)>,
    /// Cells hidden above/left when the terminal is larger than the view.
    pub scroll: (u16, u16),
    client: Option<Client>,
    block: Option<(bool, Vec<Vec<u8>>)>,
    pending: VecDeque<Purpose>,
    muted: HashSet<String>,
    synced: bool,
    info: Option<PaneInfo>,
    primary: Option<Vec<u8>>,
    last_check: Instant,
    /// Herdr observers cannot resize; restart at the new size instead.
    respawn: bool,
    /// Size requested from Herdr.
    requested: (u16, u16),
    /// The running client's command before sizing, for a transient retry.
    argv: Vec<String>,
    retries: u8,
    /// When a transient refusal is retried.
    retry_at: Option<Instant>,
    /// When the current client started; a screen is due soon after.
    since: Instant,
    /// Who holds the terminal's input, when another client does.
    pub held: Option<String>,
}

/// What to show while another client holds input.
fn held_notice(held: &str) -> String {
    format!("Input is held by {held}. Watching; Ctrl+] then T takes over.")
}

impl Embed {
    /// Start the stream client of a verified plan at the given view size.
    pub fn start(plan: Plan, rows: u16, cols: u16) -> io::Result<Self> {
        let mode = if plan.mode == "control" {
            Mode::Control
        } else {
            Mode::Observe
        };
        let tmux = plan.adapter == "tmux-control";
        if plan.argv.is_empty() || (tmux && !valid_tmux_pane(&plan.terminal)) {
            return Err(io::Error::other("Invalid attach plan"));
        }
        if !tmux && plan.adapter != "herdr-session" {
            return Err(io::Error::other("Unsupported terminal adapter"));
        }
        // Herdr refuses input while another client holds it: watch instead,
        // and say who holds it, rather than wait on a refusal.
        let held = (!tmux && mode == Mode::Control && !plan.holders.is_empty())
            .then(|| describe_holders(&plan.holders));
        let (mode, base_argv) = if held.is_some() && !plan.observe_argv.is_empty() {
            (Mode::Observe, plan.observe_argv.clone())
        } else {
            (mode, plan.argv.clone())
        };
        let argv = if tmux {
            base_argv.clone()
        } else {
            sized(&base_argv, rows, cols)
        };
        let client = launch(&argv)?;
        let mut embed = Self {
            argv: base_argv,
            retries: 0,
            retry_at: None,
            since: Instant::now(),
            notice: held.as_deref().map(held_notice).unwrap_or_default(),
            held,
            plan,
            mode,
            phase: Phase::Connecting,
            parser: vt100::Parser::new(rows.max(2), cols.max(2), 0),
            prefix: false,
            fit: false,
            viewport: (rows, cols),
            origin: None,
            scroll: (0, 0),
            client: Some(client),
            block: None,
            pending: VecDeque::new(),
            muted: HashSet::new(),
            synced: false,
            info: None,
            primary: None,
            last_check: Instant::now(),
            respawn: false,
            requested: (rows, cols),
        };
        if tmux {
            embed.query_info();
        }
        Ok(embed)
    }

    /// Replace the stream client (observe, control or Herdr takeover). A new
    /// tmux client re-reads the screen and re-checks the pane's process first.
    fn restart(&mut self, argv: &[String], mode: Mode) {
        self.stop_client();
        let unsized_argv = argv.to_vec();
        let (rows, cols) = self.viewport;
        let tmux = self.is_tmux();
        let argv = if tmux {
            argv.to_vec()
        } else {
            sized(argv, rows, cols)
        };
        match launch(&argv) {
            Ok(client) => {
                self.client = Some(client);
                self.argv = unsized_argv;
                self.mode = mode;
                self.phase = Phase::Connecting;
                self.since = Instant::now();
                self.retry_at = None;
                // Callers explain why they restarted; otherwise the first frame does.
                self.notice.clear();
                self.requested = (rows, cols);
                self.respawn = false;
                self.fit = false;
                if tmux {
                    self.block = None;
                    self.pending.clear();
                    self.muted.clear();
                    self.synced = false;
                    self.info = None;
                    self.primary = None;
                    self.query_info();
                }
            }
            Err(_) => self.end("Could not start the stream client"),
        }
    }

    pub fn is_tmux(&self) -> bool {
        self.plan.adapter == "tmux-control"
    }

    pub fn live(&self) -> bool {
        self.phase == Phase::Live
    }

    fn command(&mut self, purpose: Purpose, line: &str) {
        let Some(stdin) = self.client.as_mut().and_then(|c| c.stdin.as_mut()) else {
            return;
        };
        if writeln!(stdin, "{line}")
            .and_then(|_| stdin.flush())
            .is_err()
        {
            self.end("Stream client stopped");
            return;
        }
        self.pending.push_back(purpose);
    }

    fn query_info(&mut self) {
        let pane = self.plan.terminal.clone();
        self.command(
            Purpose::Info,
            &format!("display-message -p -t {pane} '{INFO_FORMAT}'"),
        );
    }

    pub fn end(&mut self, reason: &str) {
        if !matches!(self.phase, Phase::Ended(_)) {
            self.phase = Phase::Ended(reason.to_owned());
        }
        self.mode = Mode::Observe;
        self.stop_client();
    }

    /// Stop the stream client only. Never signals the terminal or its server.
    fn stop_client(&mut self) {
        let Some(mut client) = self.client.take() else {
            return;
        };
        if let Some(mut stdin) = client.stdin.take()
            && !self.is_tmux()
        {
            // Herdr releases the attach slot on request or on stdin EOF.
            let _ = writeln!(stdin, r#"{{"type":"terminal.release"}}"#);
        }
        // Release/EOF has already been sent. Reap only this stream client in
        // the background so pane switches and Observe/Attach never stall the UI.
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(300);
            while Instant::now() < deadline {
                if matches!(client.child.try_wait(), Ok(Some(_))) {
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
            // Ends the stream client process only; the terminal survives.
            let _ = client.child.kill();
            let _ = client.child.wait();
        });
    }

    pub fn detach(&mut self) {
        self.stop_client();
    }

    /// Consume stream output. Returns true when the screen may have changed.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        let mut used = 0;
        if self.retry_at.is_some_and(|at| Instant::now() >= at) {
            let (argv, mode) = (self.argv.clone(), self.mode);
            let notice = std::mem::take(&mut self.notice);
            self.restart(&argv, mode);
            self.notice = notice;
            changed = true;
        }
        if self.phase == Phase::Connecting
            && self.retry_at.is_none()
            && self.client.is_some()
            && self.since.elapsed() >= NO_SCREEN
        {
            self.end("No screen arrived in 8 seconds. Enter tries again; Esc returns.");
            return true;
        }
        let started = Instant::now();
        while used < BUDGET && (used == 0 || started.elapsed() < PUMP_SLICE) {
            let Some(client) = self.client.as_ref() else {
                break;
            };
            match client.rx.try_recv() {
                Ok(Stream::Line(line)) => {
                    used += line.len() + 1;
                    changed |= if self.is_tmux() {
                        self.on_line(&line)
                    } else {
                        self.on_frame(&line)
                    };
                }
                Ok(Stream::Closed(detail)) if !detail.is_empty() => {
                    self.end(&format!(
                        "Stream ended: {detail}. The terminal itself was not touched."
                    ));
                    changed = true;
                    break;
                }
                Ok(Stream::Closed(_)) | Err(TryRecvError::Disconnected) => {
                    self.end("Stream ended; the terminal itself was not touched");
                    changed = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        if self.respawn && !self.is_tmux() && self.live() && self.mode == Mode::Observe {
            let argv = self.plan.observe_argv.clone();
            self.restart(&argv, Mode::Observe);
        }
        if self.is_tmux() && self.live() && self.last_check.elapsed() > CHECK_EVERY {
            self.last_check = Instant::now();
            let pane = self.plan.terminal.clone();
            self.command(
                Purpose::Check,
                &format!("display-message -p -t {pane} '#{{pane_pid}}'"),
            );
        }
        changed
    }

    fn on_line(&mut self, line: &[u8]) -> bool {
        if let Some((ours, lines)) = self.block.as_mut() {
            let end = line.starts_with(b"%end ");
            let error = line.starts_with(b"%error ");
            if !(end || error) {
                lines.push(line.to_vec());
                return false;
            }
            let ours = *ours;
            let lines = self.block.take().map(|b| b.1).unwrap_or_default();
            if ours && let Some(purpose) = self.pending.pop_front() {
                return self.on_response(purpose, end, lines);
            }
            return false;
        }
        if line.starts_with(b"%begin ") {
            // The third field is 1 for commands this client sent.
            let ours = line.split(|&b| b == b' ').nth(3) == Some(b"1");
            self.block = Some((ours, Vec::new()));
            return false;
        }
        if let Some(rest) = line.strip_prefix(b"%output ") {
            let split = rest.iter().position(|&b| b == b' ').unwrap_or(rest.len());
            let pane = String::from_utf8_lossy(&rest[..split]).into_owned();
            if pane != self.plan.terminal {
                if self.muted.insert(pane.clone()) && valid_tmux_pane(&pane) {
                    // Other panes of the session: ask tmux to stop sending them.
                    self.command(Purpose::Flags, &format!("refresh-client -A '{pane}:off'"));
                }
                return false;
            }
            if !self.synced {
                // Already contained in the screen capture that follows.
                return false;
            }
            let data = unescape(rest.get(split + 1..).unwrap_or_default());
            self.parser.process(&data);
            return true;
        }
        if line.starts_with(b"%exit") {
            self.end("tmux ended this stream; the terminal itself was not touched");
            return true;
        }
        if line.starts_with(b"%layout-change") {
            self.query_info();
        } else if line.starts_with(b"%window-close")
            || line.starts_with(b"%unlinked-window-close")
            || line.starts_with(b"%sessions-changed")
        {
            self.last_check = Instant::now() - CHECK_EVERY;
        }
        false
    }

    fn on_response(&mut self, purpose: Purpose, ok: bool, lines: Vec<Vec<u8>>) -> bool {
        let first = lines
            .first()
            .map(|l| String::from_utf8_lossy(l).trim().to_owned())
            .unwrap_or_default();
        match purpose {
            Purpose::Info | Purpose::Check if !ok => {
                self.end("Terminal closed");
                true
            }
            Purpose::Check => {
                if !self.plan.pid.is_empty() && first != self.plan.pid {
                    self.end("Terminal process changed; input stopped. Select it again.");
                    return true;
                }
                false
            }
            Purpose::Info => {
                let Some(info) = PaneInfo::parse(&first) else {
                    self.end("Unexpected tmux response");
                    return true;
                };
                if !self.plan.pid.is_empty() && info.pid != self.plan.pid {
                    self.end("Terminal process changed; input stopped. Select it again.");
                    return true;
                }
                let same = self.synced
                    && self
                        .info
                        .as_ref()
                        .is_some_and(|old| (old.rows, old.cols) == (info.rows, info.cols));
                if same {
                    return false;
                }
                // New attach or new size: rebuild the screen from a fresh capture.
                self.synced = false;
                let pane = self.plan.terminal.clone();
                if info.alternate {
                    self.command(
                        Purpose::Primary,
                        &format!("capture-pane -p -e -a -t {pane}"),
                    );
                }
                self.command(Purpose::Screen, &format!("capture-pane -p -e -t {pane}"));
                self.info = Some(info);
                false
            }
            Purpose::Primary => {
                self.primary = Some(lines.join(&b'\n'));
                false
            }
            Purpose::Screen => {
                let Some(info) = self.info.clone() else {
                    return false;
                };
                if !ok {
                    self.end("Terminal closed");
                    return true;
                }
                let screen = lines.join(&b'\n');
                let primary = self.primary.take();
                self.parser = vt100::Parser::new(info.rows, info.cols, 0);
                self.parser
                    .process(&info.restore(primary.as_deref(), &screen));
                self.synced = true;
                if self.phase == Phase::Connecting {
                    self.phase = Phase::Live;
                    if self.mode == Mode::Control && self.notice.is_empty() {
                        self.notice = "Control: keys go to the terminal; input is shared".into();
                    }
                }
                true
            }
            Purpose::Input if !ok => {
                self.notice = "Input was not accepted; the terminal may have closed".into();
                self.last_check = Instant::now() - CHECK_EVERY;
                true
            }
            Purpose::Input | Purpose::Flags => false,
        }
    }

    /// One Herdr stream line: a rendered frame or the reason it closed.
    fn on_frame(&mut self, line: &[u8]) -> bool {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
            return false;
        };
        match value["type"].as_str() {
            Some("terminal.frame") => {
                let (Some(w), Some(h)) = (value["width"].as_u64(), value["height"].as_u64()) else {
                    return false;
                };
                let Some(bytes) = value["bytes"].as_str().and_then(b64_decode) else {
                    return false;
                };
                let (w, h) = (w.clamp(1, 1000) as u16, h.clamp(1, 1000) as u16);
                if value["full"].as_bool() == Some(true) && self.parser.screen().size() != (h, w) {
                    self.parser = vt100::Parser::new(h, w, 0);
                }
                self.parser.process(&bytes);
                if self.phase == Phase::Connecting {
                    self.phase = Phase::Live;
                    self.retries = 0;
                    if self.notice.is_empty() {
                        self.notice = if self.mode == Mode::Control {
                            "Control: keys go to the terminal; this view sets its size".into()
                        } else {
                            "Observing: nothing you type reaches the terminal".into()
                        };
                    }
                }
                true
            }
            Some("terminal.closed") => {
                let reason = value["reason"].as_str().unwrap_or("closed").to_owned();
                if reason.contains("already has an attached client") {
                    // Never take control implicitly: watch, and offer takeover.
                    let held = self
                        .held
                        .clone()
                        .unwrap_or_else(|| "another Herdr client".into());
                    let argv = self.plan.observe_argv.clone();
                    self.restart(&argv, Mode::Observe);
                    self.notice = held_notice(&held);
                    self.held = Some(held);
                } else if reason.contains("read in progress") && self.retries < RETRIES {
                    // Herdr is busy reading this terminal: try again shortly,
                    // a little later each time.
                    self.retries += 1;
                    self.stop_client();
                    self.phase = Phase::Connecting;
                    let wait = 0.25 * f32::from(1u8 << (self.retries - 1).min(3));
                    self.retry_at = Some(Instant::now() + Duration::from_secs_f32(wait));
                    self.notice = format!(
                        "Herdr is busy reading this terminal; trying again ({}/{RETRIES})…",
                        self.retries
                    );
                } else if reason.contains("taken over") {
                    let argv = self.plan.observe_argv.clone();
                    self.restart(&argv, Mode::Observe);
                    self.notice = "Another client took control. Watching only.".into();
                } else {
                    self.end(&format!(
                        "Herdr: {reason}. The terminal itself was not touched."
                    ));
                }
                true
            }
            _ => false,
        }
    }

    /// Raw bytes to the terminal. Only in control mode, only when live.
    pub fn send(&mut self, bytes: &[u8]) {
        if self.mode != Mode::Control || !self.live() || bytes.is_empty() {
            return;
        }
        if self.is_tmux() {
            let pane = self.plan.terminal.clone();
            for chunk in bytes.chunks(CHUNK) {
                self.command(Purpose::Input, &hex_keys(&pane, chunk));
            }
        } else {
            let line = format!(
                r#"{{"type":"terminal.input","bytes":"{}"}}"#,
                b64_encode(bytes)
            );
            self.herdr_line(&line);
        }
    }

    fn herdr_line(&mut self, line: &str) {
        let Some(stdin) = self.client.as_mut().and_then(|c| c.stdin.as_mut()) else {
            return;
        };
        if writeln!(stdin, "{line}")
            .and_then(|_| stdin.flush())
            .is_err()
        {
            self.end("Stream client stopped");
        }
    }

    pub fn key(&mut self, key: KeyEvent) {
        let screen = self.parser.screen();
        let bytes = encode_key(key, screen.application_cursor());
        self.send(&bytes);
    }

    pub fn paste(&mut self, text: &str) {
        let bracketed = self.parser.screen().bracketed_paste();
        self.send(&encode_paste(text, bracketed));
    }

    /// Forward a mouse event if the application asked for mouse reporting.
    /// Returns false when the event is outside the terminal or unused by it.
    pub fn mouse(&mut self, event: MouseEvent) -> bool {
        let Some((x, y, w, h)) = self.origin else {
            return false;
        };
        if event.column < x || event.row < y || event.column >= x + w || event.row >= y + h {
            return false;
        }
        if !self.is_tmux() {
            // Herdr does not forward mouse modes; the wheel scrolls its history.
            let direction = match event.kind {
                MouseEventKind::ScrollUp => "up",
                MouseEventKind::ScrollDown => "down",
                _ => return false,
            };
            if self.mode == Mode::Control && self.live() {
                self.herdr_line(&format!(
                    r#"{{"type":"terminal.scroll","direction":"{direction}","lines":3}}"#
                ));
            }
            return true;
        }
        let screen = self.parser.screen();
        let col = event.column - x + self.scroll.1;
        let row = event.row - y + self.scroll.0;
        match encode_mouse(
            event,
            col,
            row,
            screen.mouse_protocol_mode(),
            screen.mouse_protocol_encoding(),
        ) {
            Some(bytes) => {
                self.send(&bytes);
                true
            }
            None => false,
        }
    }

    /// Ask for control. tmux input is shared, so this never takes it from
    /// others; Herdr refuses while another client holds its attach slot.
    pub fn request_control(&mut self) {
        if !self.live() || self.mode == Mode::Control {
            return;
        }
        if let Some(held) = &self.held {
            self.notice = held_notice(held);
            return;
        }
        let argv = self.plan.control_argv.clone();
        if argv.is_empty() {
            self.notice = "This view cannot send input".into();
            return;
        }
        self.restart(&argv, Mode::Control);
    }

    pub fn observe(&mut self) {
        if self.mode == Mode::Control && self.live() {
            if self.is_tmux() {
                self.command(Purpose::Flags, "refresh-client -f read-only");
            } else {
                // Release Herdr's attach slot by switching to an observer.
                let argv = self.plan.observe_argv.clone();
                self.restart(&argv, Mode::Observe);
            }
        }
        self.mode = Mode::Observe;
        self.notice = "Observing: nothing you type reaches the terminal".into();
    }

    /// Herdr: take the attach slot from its current holder. Only on request.
    pub fn takeover(&mut self) {
        if self.is_tmux() || self.plan.takeover_argv.is_empty() {
            self.notice = "tmux input is shared; there is nothing to take over".into();
            return;
        }
        let argv = self.plan.takeover_argv.clone();
        self.restart(&argv, Mode::Control);
        self.held = None;
        self.notice = "Took Herdr's attach slot; the previous holder was detached".into();
    }

    /// tmux only: size the window from this view (explicit; like attaching from
    /// a terminal of this size, other viewers see the change).
    pub fn set_fit(&mut self, fit: bool) {
        if !self.is_tmux() || !self.live() || self.mode != Mode::Control {
            return;
        }
        self.fit = fit;
        if fit {
            self.command(Purpose::Flags, "refresh-client -f '!ignore-size'");
            let (rows, cols) = self.viewport;
            self.command(Purpose::Flags, &format!("refresh-client -C {cols}x{rows}"));
            self.notice = "Window follows this view's size for every viewer".into();
        } else {
            self.command(Purpose::Flags, "refresh-client -f ignore-size");
            self.notice = "This view no longer sizes the window".into();
        }
    }

    /// Record the visible area; resize the terminal only in fit mode.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let changed = self.viewport != (rows, cols);
        self.viewport = (rows, cols);
        if !changed || rows < 2 || cols < 2 || !self.live() {
            return;
        }
        if self.is_tmux() {
            if self.fit {
                self.command(Purpose::Flags, &format!("refresh-client -C {cols}x{rows}"));
            }
        } else if self.mode == Mode::Control {
            // A Herdr controller owns the terminal size, like a native attach.
            self.requested = (rows, cols);
            self.herdr_line(&format!(
                r#"{{"type":"terminal.resize","cols":{cols},"rows":{rows}}}"#
            ));
        } else if self.requested != (rows, cols) {
            self.respawn = true;
        }
    }

    /// Re-read the full screen, e.g. after display corruption.
    pub fn resync(&mut self) {
        if self.is_tmux() && self.live() {
            self.synced = false;
            self.info = None;
            self.query_info();
        }
    }

    /// Draw the visible part of the screen. Returns the cursor cell when shown.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer, theme: &Theme) -> Option<(u16, u16)> {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let (cursor_row, cursor_col) = screen.cursor_position();
        // Follow the cursor when the terminal is larger than the view.
        let follow = |len: u16, view: u16, at: u16| {
            if len <= view {
                0
            } else {
                at.saturating_add(1).saturating_sub(view).min(len - view)
            }
        };
        let top = follow(rows, area.height, cursor_row);
        let left = follow(cols, area.width, cursor_col);
        self.scroll = (top, left);
        let shown_w = cols.saturating_sub(left).min(area.width);
        let shown_h = rows.saturating_sub(top).min(area.height);
        self.origin = Some((area.x, area.y, shown_w, shown_h));
        draw_screen(
            screen,
            Rect::new(area.x, area.y, shown_w, shown_h),
            buf,
            theme,
            (top, left),
        );
        let visible = !screen.hide_cursor()
            && cursor_row >= top
            && cursor_col >= left
            && cursor_row - top < shown_h
            && cursor_col - left < shown_w;
        if !visible || !self.live() {
            return None;
        }
        let at = (area.x + cursor_col - left, area.y + cursor_row - top);
        if self.mode == Mode::Observe {
            // A passive cursor mark; the real cursor stays hidden in observe mode.
            buf[at].set_style(Style::default().add_modifier(Modifier::REVERSED));
            return None;
        }
        Some(at)
    }
}

impl Drop for Embed {
    fn drop(&mut self) {
        self.detach();
    }
}

/// Draw part of a VT screen: `offset` cells are hidden above and to the left.
pub fn draw_screen(
    screen: &vt100::Screen,
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    offset: (u16, u16),
) {
    let (top, left) = offset;
    let base = theme.screen();
    for y in 0..area.height {
        for x in 0..area.width {
            let Some(cell) = screen.cell(top + y, left + x) else {
                continue;
            };
            let target = &mut buf[(area.x + x, area.y + y)];
            if cell.is_wide_continuation() {
                target.set_symbol("");
                continue;
            }
            let mut style = base;
            if let Some(fg) = color(theme, cell.fgcolor()) {
                style = style.fg(fg);
            }
            if let Some(bg) = color(theme, cell.bgcolor()) {
                style = style.bg(bg);
            }
            let mut modifier = Modifier::empty();
            for (on, m) in [
                (cell.bold(), Modifier::BOLD),
                (cell.dim(), Modifier::DIM),
                (cell.italic(), Modifier::ITALIC),
                (cell.underline(), Modifier::UNDERLINED),
                (cell.inverse(), Modifier::REVERSED),
            ] {
                if on {
                    modifier |= m;
                }
            }
            let contents = cell.contents();
            let symbol = if contents.is_empty() { " " } else { contents };
            // A wide character at the right edge cannot be drawn half; control
            // characters never reach the real terminal.
            let symbol = if (cell.is_wide() && x + 1 >= area.width)
                || symbol.chars().any(char::is_control)
            {
                " "
            } else {
                symbol
            };
            target
                .set_symbol(symbol)
                .set_style(style.add_modifier(modifier));
        }
    }
}

fn color(theme: &Theme, color: vt100::Color) -> Option<Color> {
    if !theme.color {
        return None;
    }
    match color {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(Color::Indexed(i)),
        vt100::Color::Rgb(r, g, b) => Some(theme.rgb((r, g, b))),
    }
}

/// The bytes an xterm-compatible terminal sends for a key.
pub fn encode_key(key: KeyEvent, application_cursor: bool) -> Vec<u8> {
    let m = key.modifiers;
    let alt = m.contains(KeyModifiers::ALT);
    let ctrl = m.contains(KeyModifiers::CONTROL);
    let shift = m.contains(KeyModifiers::SHIFT);
    let param = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let csi = |code: &str, tail: char| -> Vec<u8> {
        if param > 1 {
            format!("\x1b[{code};{param}{tail}").into_bytes()
        } else {
            format!("\x1b[{code}{tail}").into_bytes()
        }
    };
    let cursor = |letter: char| -> Vec<u8> {
        if param > 1 {
            format!("\x1b[1;{param}{letter}").into_bytes()
        } else if application_cursor {
            format!("\x1bO{letter}").into_bytes()
        } else {
            format!("\x1b[{letter}").into_bytes()
        }
    };
    let mut out = match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                let byte = match c.to_ascii_lowercase() {
                    c @ 'a'..='z' => Some(c as u8 & 0x1f),
                    '@' | ' ' | '2' => Some(0),
                    '[' | '3' => Some(0x1b),
                    '\\' | '4' => Some(0x1c),
                    ']' | '5' => Some(0x1d),
                    '^' | '~' | '6' => Some(0x1e),
                    '_' | '/' | '-' | '7' => Some(0x1f),
                    '?' | '8' => Some(0x7f),
                    _ => None,
                };
                match byte {
                    Some(b) => vec![b],
                    None => c.to_string().into_bytes(),
                }
            } else {
                c.to_string().into_bytes()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![if ctrl { 0x08 } else { 0x7f }],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => cursor('A'),
        KeyCode::Down => cursor('B'),
        KeyCode::Right => cursor('C'),
        KeyCode::Left => cursor('D'),
        KeyCode::Home => cursor('H'),
        KeyCode::End => cursor('F'),
        KeyCode::Insert => csi("2", '~'),
        KeyCode::Delete => csi("3", '~'),
        KeyCode::PageUp => csi("5", '~'),
        KeyCode::PageDown => csi("6", '~'),
        KeyCode::F(n @ 1..=4) => {
            let letter = (b'P' + n - 1) as char;
            if param > 1 {
                format!("\x1b[1;{param}{letter}").into_bytes()
            } else {
                format!("\x1bO{letter}").into_bytes()
            }
        }
        KeyCode::F(n @ 5..=12) => {
            let code = [15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)];
            csi(&code.to_string(), '~')
        }
        _ => Vec::new(),
    };
    // Alt prefixes ESC for plain characters and editing keys.
    if alt
        && matches!(
            key.code,
            KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Enter
        )
    {
        out.insert(0, 0x1b);
    }
    out
}

/// Paste as typed text; bracketed when the application enabled it.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        // A pasted end marker must not end the bracket early.
        let text = text.replace("\x1b[201~", "");
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

pub fn encode_mouse(
    event: MouseEvent,
    col: u16,
    row: u16,
    mode: vt100::MouseProtocolMode,
    encoding: vt100::MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    use vt100::MouseProtocolMode as M;
    let button = |b: MouseButton| match b {
        MouseButton::Left => 0u16,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    let (code, release) = match event.kind {
        MouseEventKind::Down(b) => (button(b), false),
        MouseEventKind::Up(b) if mode != M::Press => (button(b), true),
        MouseEventKind::Drag(b) if matches!(mode, M::ButtonMotion | M::AnyMotion) => {
            (button(b) + 32, false)
        }
        MouseEventKind::Moved if mode == M::AnyMotion => (35, false),
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        _ => return None,
    };
    if mode == M::None {
        return None;
    }
    let m = event.modifiers;
    let code =
        code + if m.contains(KeyModifiers::SHIFT) {
            4
        } else {
            0
        } + if m.contains(KeyModifiers::ALT) { 8 } else { 0 }
            + if m.contains(KeyModifiers::CONTROL) {
                16
            } else {
                0
            };
    let (x, y) = (col + 1, row + 1);
    match encoding {
        vt100::MouseProtocolEncoding::Sgr => {
            Some(format!("\x1b[<{code};{x};{y}{}", if release { 'm' } else { 'M' }).into_bytes())
        }
        _ => {
            let code = if release { 3 + (code & !3) } else { code };
            if x > 223 || y > 223 {
                return None;
            }
            Some(vec![
                0x1b,
                b'[',
                b'M',
                (32 + code) as u8,
                (32 + x) as u8,
                (32 + y) as u8,
            ])
        }
    }
}

/// Ctrl+] arrives as `]` with Control, or as Ctrl+5 from legacy terminals.
pub fn is_prefix(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char(']' | '5'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn unescapes_control_mode_output() {
        // tmux writes a backslash as \134.
        assert_eq!(unescape(br"a\015\012b\134c"), b"a\r\nb\\c");
        assert_eq!(unescape(br"lone\x"), br"lone\x");
        assert_eq!(unescape(br"\033[1mX"), b"\x1b[1mX");
        // Incomplete escapes are kept literally.
        assert_eq!(unescape(br"tail\01"), br"tail\01");
    }

    #[test]
    fn raw_input_is_hex_encoded_for_one_exact_pane() {
        assert_eq!(hex_keys("%3", b"a\r"), "send-keys -t %3 -H 61 0d");
        assert!(valid_tmux_pane("%12"));
        assert!(!valid_tmux_pane("%1;kill-server"));
        assert!(!valid_tmux_pane("1"));
    }

    #[test]
    fn keys_encode_like_a_terminal() {
        let none = KeyModifiers::NONE;
        assert_eq!(encode_key(key(KeyCode::Char('a'), none), false), b"a");
        assert_eq!(
            encode_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL), false),
            [3]
        );
        assert_eq!(
            encode_key(key(KeyCode::Char('x'), KeyModifiers::ALT), false),
            b"\x1bx"
        );
        assert_eq!(encode_key(key(KeyCode::Up, none), false), b"\x1b[A");
        assert_eq!(encode_key(key(KeyCode::Up, none), true), b"\x1bOA");
        assert_eq!(
            encode_key(key(KeyCode::Right, KeyModifiers::CONTROL), true),
            b"\x1b[1;5C"
        );
        assert_eq!(encode_key(key(KeyCode::F(5), none), false), b"\x1b[15~");
        assert_eq!(encode_key(key(KeyCode::F(1), none), false), b"\x1bOP");
        assert_eq!(encode_key(key(KeyCode::Enter, none), false), b"\r");
        assert_eq!(
            encode_key(key(KeyCode::Char('é'), none), false),
            "é".as_bytes()
        );
    }

    #[test]
    fn paste_is_bracketed_only_when_requested_and_cannot_escape() {
        assert_eq!(encode_paste("a\nb", false), b"a\rb");
        assert_eq!(
            encode_paste("x\x1b[201~y", true),
            b"\x1b[200~xy\x1b[201~".to_vec()
        );
    }

    #[test]
    fn mouse_is_forwarded_only_when_enabled() {
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        use vt100::{MouseProtocolEncoding as E, MouseProtocolMode as M};
        assert!(encode_mouse(event, 4, 2, M::None, E::Default).is_none());
        assert_eq!(
            encode_mouse(event, 4, 2, M::PressRelease, E::Sgr).unwrap(),
            b"\x1b[<0;5;3M"
        );
        assert_eq!(
            encode_mouse(event, 4, 2, M::PressRelease, E::Default).unwrap(),
            vec![0x1b, b'[', b'M', 32, 37, 35]
        );
    }

    #[test]
    fn prefix_accepts_legacy_control_five() {
        assert!(is_prefix(&key(KeyCode::Char(']'), KeyModifiers::CONTROL)));
        assert!(is_prefix(&key(KeyCode::Char('5'), KeyModifiers::CONTROL)));
        assert!(!is_prefix(&key(KeyCode::Char(']'), KeyModifiers::NONE)));
    }

    fn pump_until(embed: &mut Embed, done: impl Fn(&Embed) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            embed.pump();
            if done(embed) {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("daedalus-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A tmux server on a private socket running `cat`; removed on drop.
    #[cfg(unix)]
    struct Tmux {
        dir: std::path::PathBuf,
        socket: String,
    }
    #[cfg(unix)]
    impl Tmux {
        fn start() -> Option<Self> {
            Command::new("tmux").arg("-V").output().ok()?;
            let dir = scratch("tmux");
            let socket = dir.join("t.sock").to_string_lossy().into_owned();
            let fixture = Self { dir, socket };
            fixture.run(&[
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "fx",
                "-x",
                "80",
                "-y",
                "24",
                "printf 'fixture-ready\\n'; exec cat",
            ]);
            Some(fixture)
        }
        fn run(&self, args: &[&str]) -> String {
            let output = Command::new("tmux")
                .args(["-S", &self.socket])
                .args(args)
                .env_remove("TMUX")
                .env_remove("TMUX_PANE")
                .output()
                .unwrap();
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }
        fn get(&self, format: &str) -> String {
            self.run(&["display-message", "-p", "-t", "fx", format])
        }
        fn plan(&self, mode: &str) -> Plan {
            let base: Vec<String> = [
                "tmux",
                "-S",
                &self.socket,
                "-C",
                "attach-session",
                "-t",
                &self.get("#{session_id}"),
                "-f",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect();
            let observe = [base.clone(), vec!["ignore-size,read-only".into()]].concat();
            let control = [base, vec!["ignore-size".into()]].concat();
            Plan {
                adapter: "tmux-control".into(),
                argv: if mode == "control" {
                    control.clone()
                } else {
                    observe.clone()
                },
                observe_argv: observe,
                control_argv: control,
                mode: mode.into(),
                terminal: self.get("#{pane_id}"),
                pid: self.get("#{pane_pid}"),
                ..Plan::default()
            }
        }
    }
    #[cfg(unix)]
    impl Drop for Tmux {
        fn drop(&mut self) {
            self.run(&["kill-server"]);
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[cfg(unix)]
    #[test]
    fn tmux_view_watches_then_types_and_detach_leaves_terminal_running() {
        let Some(fx) = Tmux::start() else {
            eprintln!("tmux not installed; skipped");
            return;
        };
        thread::sleep(Duration::from_millis(200));
        let pid = fx.get("#{pane_pid}");
        let mut embed = Embed::start(fx.plan("observe"), 24, 80).unwrap();
        assert!(pump_until(&mut embed, |e| e.live()));
        assert!(embed.parser.screen().contents().contains("fixture-ready"));
        assert_eq!(embed.mode, Mode::Observe);
        assert!(
            fx.run(&["list-clients", "-F", "#{client_flags}"])
                .contains("read-only")
        );
        // Observe mode never sends input.
        embed.send(b"observed-text\r");
        embed.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        thread::sleep(Duration::from_millis(200));
        assert!(
            !fx.run(&["capture-pane", "-p", "-t", "fx"])
                .contains("observed-text")
        );
        // Control starts a new client without the read-only flag.
        embed.request_control();
        assert!(pump_until(&mut embed, |e| e.live() && e.mode == Mode::Control));
        assert!(
            !fx.run(&["list-clients", "-F", "#{client_flags}"])
                .contains("read-only")
        );
        embed.paste("typed-through-embed\n");
        assert!(pump_until(&mut embed, |e| e
            .parser
            .screen()
            .contents()
            .contains("typed-through-embed")));
        assert!(
            fx.run(&["capture-pane", "-p", "-t", "fx"])
                .contains("typed-through-embed")
        );
        embed.detach();
        thread::sleep(Duration::from_millis(200));
        assert_eq!(fx.run(&["list-clients"]), "");
        assert_eq!(fx.get("#{pane_pid}"), pid);
    }

    #[cfg(unix)]
    #[test]
    fn tmux_view_ends_when_the_process_identity_differs() {
        let Some(fx) = Tmux::start() else {
            return;
        };
        let plan = Plan {
            pid: "1".into(),
            ..fx.plan("control")
        };
        let mut embed = Embed::start(plan, 24, 80).unwrap();
        assert!(pump_until(&mut embed, |e| matches!(
            e.phase,
            Phase::Ended(_)
        )));
        assert_eq!(embed.mode, Mode::Observe);
        embed.send(b"never-sent\r");
        thread::sleep(Duration::from_millis(200));
        assert!(
            !fx.run(&["capture-pane", "-p", "-t", "fx"])
                .contains("never-sent")
        );
        assert!(
            Embed::start(
                Plan {
                    terminal: "%1;kill-server".into(),
                    ..fx.plan("observe")
                },
                24,
                80
            )
            .is_err()
        );
    }

    /// A native JSON-lines stand-in for `herdr terminal session`.
    /// Compile in the fixture directory so no Python runtime or skipped test is needed.
    fn fake_herdr(dir: &std::path::Path) -> String {
        let executable = dir.join(if cfg!(windows) {
            "fake-herdr.exe"
        } else {
            "fake-herdr"
        });
        #[cfg(windows)]
        if let Some(built) = std::env::var_os("DAEDALUS_TEST_HERDR_EXE") {
            std::fs::copy(built, &executable).unwrap();
            return executable.to_string_lossy().into_owned();
        }
        let source = r##"
use std::{env, fs::{self, OpenOptions}, io::{self, BufRead, Write}};
fn quoted(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"), '"' => out.push_str("\\\""),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"'); out
}
fn base64(bytes: &[u8]) -> String {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = ((c[0] as u32) << 16) | ((c.get(1).copied().unwrap_or(0) as u32) << 8) | c.get(2).copied().unwrap_or(0) as u32;
        out.push(alphabet[((n >> 18) & 63) as usize] as char);
        out.push(alphabet[((n >> 12) & 63) as usize] as char);
        out.push(if c.len() > 1 { alphabet[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if c.len() > 2 { alphabet[(n & 63) as usize] as char } else { '=' });
    }
    out
}
fn main() {
    let directory = env::current_exe().unwrap().parent().unwrap().to_path_buf();
    let args = env::args().skip(1).collect::<Vec<_>>();
    let mut log = OpenOptions::new().create(true).append(true).open(directory.join("log")).unwrap();
    writeln!(log, "{{\"argv\":[{}]}}", args.iter().map(|a| quoted(a)).collect::<Vec<_>>().join(",")).unwrap();
    log.flush().unwrap();
    let mode = &args[0]; let target = &args[1];
    let mut takeover = false; let mut cols = None; let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--takeover" if mode == "control" => { takeover = true; i += 1; }
            "--cols" | "--rows" if i + 1 < args.len() => { if args[i] == "--cols" { cols = Some(args[i+1].clone()); } i += 2; }
            name => { eprintln!("unknown terminal session {} option: {}", mode, name); std::process::exit(2); }
        }
    }
    let reason = if directory.join("reading").exists() {
        fs::remove_file(directory.join("reading")).unwrap();
        Some(format!("terminal attach failed: terminal {} has a read in progress; retry", target))
    } else if mode == "control" && !takeover && directory.join("busy").exists() {
        Some("terminal session control failed: terminal already has an attached client; retry with --takeover".to_owned())
    } else { None };
    if let Some(reason) = reason { println!("{{\"type\":\"terminal.closed\",\"reason\":{}}}", quoted(&reason)); return; }
    let frame = base64(format!("\x1b[2J\x1b[1;1Hfake {} {}", mode, cols.unwrap()).as_bytes());
    println!("{{\"type\":\"terminal.frame\",\"seq\":1,\"full\":true,\"encoding\":\"ansi\",\"width\":40,\"height\":5,\"bytes\":{}}}", quoted(&frame));
    io::stdout().flush().unwrap();
    for line in io::stdin().lock().lines() {
        let line = line.unwrap(); writeln!(log, "{}", line).unwrap(); log.flush().unwrap();
        // Inputs are the production serializer's JSON objects; release has no payload.
        if line == r#"{"type":"terminal.release"}"# { break; }
    }
}
"##;
        let source_path = dir.join("fake-herdr.rs");
        std::fs::write(&source_path, source).unwrap();
        let output = Command::new("rustc")
            .args(["--edition=2024", "--crate-name", "fake_herdr"])
            .arg(&source_path)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        executable.to_string_lossy().into_owned()
    }

    fn herdr_plan(fake: &str, mode: &str, extra: &[&str]) -> Plan {
        let argv = |m: &str| -> Vec<String> {
            [fake, m, "term_1"]
                .iter()
                .chain(extra)
                .map(|s| s.to_string())
                .collect()
        };
        Plan {
            adapter: "herdr-session".into(),
            argv: argv(mode),
            observe_argv: argv("observe"),
            control_argv: argv("control"),
            mode: mode.into(),
            terminal: "w1:p1".into(),
            takeover: true,
            ..Plan::default()
        }
    }

    #[test]
    fn herdr_failure_shows_its_own_error() {
        let dir = scratch("herdr-error");
        let fake = fake_herdr(&dir);
        let mut embed = Embed::start(herdr_plan(&fake, "control", &["--bogus"]), 10, 50).unwrap();
        assert!(pump_until(&mut embed, |e| matches!(
            e.phase,
            Phase::Ended(_)
        )));
        let Phase::Ended(reason) = &embed.phase else {
            unreachable!()
        };
        assert!(
            reason.contains("unknown terminal session control option: --bogus"),
            "{reason}"
        );
    }

    #[test]
    fn herdr_retries_a_transient_read_in_progress() {
        let dir = scratch("herdr-busy-read");
        let fake = fake_herdr(&dir);
        std::fs::write(dir.join("reading"), "").unwrap();
        let mut embed = Embed::start(herdr_plan(&fake, "control", &[]), 10, 50).unwrap();
        // The refusal is retried a little later, saying so meanwhile.
        assert!(pump_until(&mut embed, |e| e.retry_at.is_some()));
        assert_eq!(embed.phase, Phase::Connecting);
        assert!(embed.notice.contains("trying again (1/5)"));
        assert!(pump_until(&mut embed, |e| e.live()));
        assert_eq!(embed.mode, Mode::Control);
        assert!(embed.parser.screen().contents().contains("fake control 50"));
        let starts = std::fs::read_to_string(dir.join("log"))
            .unwrap()
            .lines()
            .filter(|l| l.contains("\"argv\""))
            .count();
        assert_eq!(starts, 2);
        embed.detach();
    }

    #[test]
    fn a_known_holder_means_watching_with_its_name_and_no_refused_retries() {
        let dir = scratch("herdr-held");
        let fake = fake_herdr(&dir);
        let mut plan = herdr_plan(&fake, "control", &[]);
        plan.holders = vec![Holder {
            pid: 42,
            how: "herdr agent attach".into(),
            by: "project_terminal.py".into(),
        }];
        let mut embed = Embed::start(plan, 10, 50).unwrap();
        assert_eq!(embed.mode, Mode::Observe);
        assert!(
            embed
                .notice
                .contains("herdr agent attach (process 42, from project_terminal.py)")
        );
        assert!(pump_until(&mut embed, |e| e.live()));
        assert!(embed.parser.screen().contents().contains("fake observe 50"));
        // Asking for input again names the holder instead of starting over.
        embed.request_control();
        assert_eq!(embed.mode, Mode::Observe);
        assert!(embed.notice.contains("Ctrl+] then T takes over"));
        let starts = || {
            std::fs::read_to_string(dir.join("log"))
                .unwrap()
                .lines()
                .filter(|l| l.contains("\"argv\""))
                .count()
        };
        assert_eq!(starts(), 1, "no control was attempted");
        // Taking over is explicit, and clears the holder.
        let mut plan = herdr_plan(&fake, "observe", &[]);
        plan.takeover_argv = plan.control_argv.clone();
        embed.plan.takeover_argv = plan.takeover_argv;
        embed.takeover();
        assert!(embed.held.is_none());
        embed.detach();
    }

    #[test]
    fn no_screen_ends_with_a_reason_instead_of_connecting_forever() {
        #[cfg(unix)]
        let silent: Vec<String> = [crate::test_support::path("/bin/sleep"), "30"]
            .map(String::from)
            .to_vec();
        #[cfg(windows)]
        let silent: Vec<String> = ["cmd.exe", "/d", "/c", "set /p wait="]
            .map(String::from)
            .to_vec();
        let plan = Plan {
            adapter: "herdr-session".into(),
            argv: silent.clone(),
            observe_argv: silent,
            mode: "observe".into(),
            terminal: "w1:p1".into(),
            ..Plan::default()
        };
        let mut embed = Embed::start(plan, 10, 50).unwrap();
        embed.pump();
        assert_eq!(embed.phase, Phase::Connecting);
        embed.since -= NO_SCREEN;
        embed.pump();
        match &embed.phase {
            Phase::Ended(reason) => assert!(reason.contains("No screen arrived")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn herdr_view_never_takes_over_implicitly_and_releases_on_detach() {
        let dir = scratch("herdr");
        let log = dir.join("log");
        let busy = dir.join("busy");
        let fake = fake_herdr(&dir);
        let argv = |rest: &[&str]| -> Vec<String> {
            [fake.as_str()]
                .into_iter()
                .chain(rest.iter().copied())
                .map(String::from)
                .collect()
        };
        let plan = Plan {
            adapter: "herdr-session".into(),
            argv: argv(&["observe", "term_1"]),
            observe_argv: argv(&["observe", "term_1"]),
            control_argv: argv(&["control", "term_1"]),
            takeover_argv: argv(&["control", "term_1", "--takeover"]),
            mode: "observe".into(),
            terminal: "w1:p1".into(),
            takeover: true,
            ..Plan::default()
        };
        let mut embed = Embed::start(plan, 10, 50).unwrap();
        assert!(pump_until(&mut embed, |e| e.live()));
        assert!(embed.parser.screen().contents().contains("fake observe 50"));
        embed.send(b"ignored");
        // Another client holds the slot: control is refused and it keeps watching.
        std::fs::write(&busy, "").unwrap();
        embed.request_control();
        assert!(pump_until(&mut embed, |e| e.live()
            && e.notice.contains("Ctrl+] then T")));
        assert_eq!(embed.mode, Mode::Observe);
        // Takeover happens only when asked for.
        embed.takeover();
        assert!(pump_until(&mut embed, |e| e.live() && e.mode == Mode::Control));
        embed.send(b"x");
        embed.resize(12, 60);
        embed.detach();
        let deadline = Instant::now() + Duration::from_secs(2);
        let text = loop {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            // Retired clients release asynchronously. Their earlier receipt
            // does not prove this controller's input and resize were consumed.
            if text.contains("terminal.input")
                && text.contains("terminal.resize")
                && text.lines().last().is_some_and(|line| {
                    serde_json::from_str::<serde_json::Value>(line)
                        .is_ok_and(|v| v["type"] == "terminal.release")
                })
            {
                break text;
            }
            assert!(Instant::now() < deadline, "missing release receipt");
            thread::sleep(Duration::from_millis(10));
        };
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let argvs: Vec<String> = lines
            .iter()
            .filter_map(|v| v["argv"].as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        assert_eq!(argvs.iter().filter(|a| a.contains("--takeover")).count(), 1);
        assert_eq!(argvs[0], "observe term_1 --cols 50 --rows 10");
        let inputs: Vec<&serde_json::Value> = lines
            .iter()
            .filter(|v| v["type"] == "terminal.input")
            .collect();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0]["bytes"], b64_encode(b"x"));
        assert!(
            lines
                .iter()
                .any(|v| v["type"] == "terminal.resize" && v["cols"] == 60)
        );
        assert_eq!(lines.last().unwrap()["type"], "terminal.release");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn base64_round_trips() {
        for sample in [&b""[..], b"x", b"xy", b"xyz", "é\x1b[0m\0".as_bytes()] {
            assert_eq!(b64_decode(&b64_encode(sample)).unwrap(), sample);
        }
        assert_eq!(b64_encode(b"hi"), "aGk=");
        assert!(b64_decode("@@").is_none());
    }

    #[test]
    fn restore_rebuilds_alternate_screen_cursor_and_modes() {
        let info = PaneInfo::parse("42 20 4 3 1 1 0 3 1 1 0 1 0 0 1 0 0 0 1").unwrap();
        assert_eq!((info.rows, info.cols, info.cursor), (4, 20, (3, 1)));
        let mut parser = vt100::Parser::new(info.rows, info.cols, 0);
        parser.process(&info.restore(Some(b"primary"), b"line one\n\x1b[1malt\x1b[0m"));
        let screen = parser.screen();
        assert!(screen.alternate_screen());
        assert!(screen.application_cursor());
        assert_eq!(screen.cursor_position(), (1, 3));
        assert_eq!(
            screen.mouse_protocol_encoding(),
            vt100::MouseProtocolEncoding::Sgr
        );
        assert!(screen.contents().starts_with("line one\nalt"));
        assert!(screen.cell(1, 0).unwrap().bold());
        // Leaving the alternate screen reveals the captured primary screen.
        parser.process(b"\x1b[?1049l");
        assert!(parser.screen().contents().starts_with("primary"));
    }
    #[cfg(unix)]
    #[test]
    fn detach_does_not_wait_for_a_stubborn_client_and_eventually_reaps_it() {
        let plan = Plan {
            adapter: "tmux-control".into(),
            argv: vec![crate::test_support::path("/bin/sleep").into(), "30".into()],
            terminal: "%1".into(),
            mode: "observe".into(),
            ..Plan::default()
        };
        let mut embed = Embed::start(plan, 10, 50).unwrap();
        let pid = embed.client.as_ref().unwrap().child.id() as libc::pid_t;
        embed.detach();
        assert!(embed.client.is_none());
        // Cleanup happens after return, rather than blocking the next pane.
        assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
        let deadline = Instant::now() + Duration::from_secs(2);
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(
                Instant::now() < deadline,
                "detached stream client was not reaped"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}
