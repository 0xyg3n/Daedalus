//! Detached, user-owned PTY sessions. The daemon owns the terminal, not its viewer.
//! Loopback RPC is authenticated with a random secret in owner-only local state.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use portable_pty::{CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::process::{Command, Stdio};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, String>;
const MAX_REQUEST: u64 = 128 * 1024;
const MAX_REPLY: u64 = 8 * 1024 * 1024;
const LEASE: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StartOptions {
    pub name: String,
    pub workspace: String,
    pub cwd: PathBuf,
    pub argv: Vec<String>,
    #[serde(default)]
    pub environment: Option<std::collections::BTreeMap<String, String>>,
    pub cols: u16,
    pub rows: u16,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub generation: String,
    pub name: String,
    pub workspace: String,
    pub cwd: PathBuf,
    /// Executable basename only. Arguments can contain secrets.
    pub command: String,
    pub pid: u32,
    pub foreground_pid: Option<u32>,
    pub cols: u16,
    pub rows: u16,
    pub controlled: bool,
    #[serde(default)]
    pub exited: Option<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capture {
    pub session: SessionInfo,
    pub ansi: String,
    pub text: String,
}
#[derive(Serialize, Deserialize)]
struct Launch {
    id: String,
    generation: String,
    token: String,
    options: StartOptions,
}
#[derive(Serialize, Deserialize)]
struct Endpoint {
    id: String,
    generation: String,
    token: String,
    port: u16,
}
fn random() -> Result<String> {
    let mut bytes = [0; 32];
    crate::platform::random_bytes(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
fn identity(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err("Invalid native terminal identity".into());
    }
    Ok(())
}
fn label(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 120 || value.chars().any(char::is_control) {
        return Err("Names must contain 1–120 printable characters".into());
    }
    Ok(())
}
fn size(cols: u16, rows: u16) -> Result<PtySize> {
    if !(2..=512).contains(&cols) || !(2..=256).contains(&rows) {
        return Err("Terminal size must be 2–512 columns and 2–256 rows".into());
    }
    Ok(PtySize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    })
}
/// Global per-user location: discovery does not depend on the current workspace.
pub fn registry_dir() -> Result<PathBuf> {
    #[cfg(unix)]
    let root = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is unavailable")?
        .join(".local/share/daedalus/terminals");
    #[cfg(windows)]
    let root = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or("LOCALAPPDATA is unavailable")?
        .join("Daedalus/terminals");
    Ok(root)
}
fn validate_registry(root: &Path) -> Result<()> {
    crate::core::layouts::check_path(root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = fs::symlink_metadata(root).map_err(|e| e.to_string())?;
        if !meta.is_dir()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.permissions().mode() & 0o077 != 0
        {
            return Err("Native terminal registry must be owned by you and private (0700)".into());
        }
    }
    #[cfg(windows)]
    crate::platform::validate_private_dir(root).map_err(|e| e.to_string())?;
    Ok(())
}
fn ensure_private(root: &Path) -> Result<()> {
    crate::core::layouts::check_path(root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)
            .map_err(|e| e.to_string())?;
    }
    #[cfg(windows)]
    crate::platform::ensure_private_dir(root).map_err(|e| e.to_string())?;
    validate_registry(root)
}
fn read_private<T: for<'a> Deserialize<'a>>(path: &Path) -> Result<T> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if !meta.is_file()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.permissions().mode() & 0o077 != 0
            || meta.nlink() != 1
        {
            return Err("Native terminal state must be an owner-only regular file".into());
        }
    }
    let bytes = crate::core::layouts::read_private_limit(path, MAX_REQUEST)?
        .ok_or("Native terminal is no longer registered")?;
    serde_json::from_slice(&bytes).map_err(|_| "Invalid native terminal state".into())
}
fn save<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    crate::core::layouts::write_private(
        path,
        &serde_json::to_vec(value).map_err(|e| e.to_string())?,
    )
}
fn endpoint(root: &Path, id: &str, generation: &str) -> Result<Endpoint> {
    identity(id)?;
    identity(generation)?;
    validate_registry(root)?;
    let endpoint: Endpoint = read_private(&root.join(format!("{id}.json")))?;
    if endpoint.id != id || endpoint.generation != generation {
        return Err("Native terminal identity changed; refresh discovery".into());
    }
    Ok(endpoint)
}
// HMAC-SHA256 authenticates both directions; stale ports never receive credentials.
fn mac(token: &str, bytes: &[u8]) -> String {
    let mut inner = [0x36; 64];
    let mut outer = [0x5c; 64];
    for (index, byte) in token.bytes().enumerate().take(64) {
        inner[index] ^= byte;
        outer[index] ^= byte;
    }
    let mut hash = Sha256::new();
    hash.update(inner);
    hash.update(bytes);
    let digest = hash.finalize();
    let mut hash = Sha256::new();
    hash.update(outer);
    hash.update(digest);
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn seal(token: &str, payload: Value) -> Result<Value> {
    let bytes = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;
    Ok(json!({"payload":payload,"mac":mac(token,&bytes)}))
}
fn unseal(token: &str, envelope: &Value) -> Result<Value> {
    let received = envelope["mac"]
        .as_str()
        .ok_or("Native terminal authentication failed")?;
    let bytes = serde_json::to_vec(&envelope["payload"]).map_err(|e| e.to_string())?;
    let expected = mac(token, &bytes);
    if received.len() != expected.len()
        || received
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            != 0
    {
        return Err("Native terminal authentication failed".into());
    }
    Ok(envelope["payload"].clone())
}
fn rpc_at(root: &Path, id: &str, generation: &str, request: Value) -> Result<Value> {
    rpc_timeout(root, id, generation, request, Duration::from_secs(2))
}
fn rpc_timeout(
    root: &Path,
    id: &str,
    generation: &str,
    mut request: Value,
    timeout: Duration,
) -> Result<Value> {
    let endpoint = endpoint(root, id, generation)?;
    let nonce = random()?;
    request["nonce"] = Value::String(nonce.clone());
    request["generation"] = Value::String(generation.into());
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(250))
        .map_err(|_| "Native terminal is unavailable")?;
    stream.set_nodelay(true).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    serde_json::to_writer(&mut stream, &json!({"nonce":nonce})).map_err(|e| e.to_string())?;
    stream.write_all(b"\n").map_err(|e| e.to_string())?;
    // Authenticate the listening daemon before sending terminal input or metadata.
    // A per-connection challenge also makes captured requests unusable on another connection.
    let mut hello = Vec::new();
    BufReader::new(&mut stream)
        .take(4097)
        .read_until(b'\n', &mut hello)
        .map_err(|e| e.to_string())?;
    if hello.len() > 4096 {
        return Err("Native terminal handshake exceeds limit".into());
    }
    let envelope: Value =
        serde_json::from_slice(&hello).map_err(|_| "Invalid native terminal handshake")?;
    let challenge = unseal(&endpoint.token, &envelope)?;
    if challenge["generation"].as_str() != Some(generation)
        || challenge["nonce"].as_str() != Some(&nonce)
    {
        return Err("Native terminal handshake identity mismatch".into());
    }
    let challenge = challenge["challenge"]
        .as_str()
        .ok_or("Native terminal challenge missing")?;
    identity(challenge)?;
    request["challenge"] = Value::String(challenge.into());
    let request = seal(&endpoint.token, request)?;
    let bytes = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    if bytes.len() as u64 + 1 > MAX_REQUEST {
        return Err("Native terminal request exceeds size limit".into());
    }
    stream.write_all(&bytes).map_err(|e| e.to_string())?;
    stream.write_all(b"\n").map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    BufReader::new(stream)
        .take(MAX_REPLY + 1)
        .read_until(b'\n', &mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_REPLY {
        return Err("Native terminal reply exceeds limit".into());
    }
    let envelope: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid native terminal reply")?;
    let mut response = unseal(&endpoint.token, &envelope)?;
    if response["nonce"].as_str() != Some(&nonce) {
        return Err("Native terminal response identity mismatch".into());
    }
    if let Some(error) = response["error"].as_str() {
        return Err(error.into());
    }
    Ok(response["result"].take())
}
fn rpc(id: &str, generation: &str, request: Value) -> Result<Value> {
    rpc_at(&registry_dir()?, id, generation, request)
}
/// Starts a daemon from this executable; the shell survives viewer detach/exit.
pub fn start(mut options: StartOptions) -> Result<SessionInfo> {
    label(&options.name)?;
    label(&options.workspace)?;
    size(options.cols, options.rows)?;
    options.cwd = options
        .cwd
        .canonicalize()
        .map_err(|_| "Working directory does not exist")?;
    if !options.cwd.is_dir() {
        return Err("Working directory is not a directory".into());
    }
    if options.argv.is_empty() {
        options.argv.push(default_shell());
    }
    if options.argv[0].is_empty() || options.argv.iter().any(|s| s.contains('\0')) {
        return Err("Invalid shell command".into());
    }
    let root = registry_dir()?;
    ensure_private(&root)?;
    let launch = Launch {
        id: random()?,
        generation: random()?,
        token: random()?,
        options,
    };
    let plan = root.join(format!("{}.launch", launch.id));
    save(&plan, &launch)?;
    #[cfg(unix)]
    let mut command = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
    #[cfg(unix)]
    command
        .args(["terminal-native", "serve", &launch.id])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Only async-signal-safe setsid runs after fork.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    #[cfg(unix)]
    let spawned = command.spawn();
    #[cfg(windows)]
    let spawned = crate::platform::spawn_background(
        &std::env::current_exe().map_err(|e| e.to_string())?,
        &["terminal-native", "serve", &launch.id],
    );
    let mut daemon = match spawned {
        Ok(child) => child,
        Err(error) => {
            let _ = fs::remove_file(plan);
            return Err(error.to_string());
        }
    };
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if let Ok(value) = rpc_at(&root, &launch.id, &launch.generation, json!({"op":"info"})) {
            // Reap this daemon if it eventually exits, without tying its lifetime to this client.
            thread::spawn(move || {
                let _ = daemon.wait();
            });
            return serde_json::from_value(value).map_err(|e| e.to_string());
        }
        if daemon.try_wait().map_err(|e| e.to_string())?.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(40));
    }
    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = fs::remove_file(plan);
    let _ = fs::remove_file(root.join(format!("{}.json", launch.id)));
    Err("The native terminal could not start. Check the shell path and working directory.".into())
}
pub fn default_shell() -> String {
    #[cfg(unix)]
    {
        std::env::var("SHELL")
            .ok()
            .filter(|s| Path::new(s).is_absolute())
            .unwrap_or_else(|| "/bin/sh".into())
    }
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into())
    }
}
pub fn list() -> Result<Vec<SessionInfo>> {
    discover().map(|(sessions, _)| sessions)
}
/// A bounded inventory refresh. Dead endpoints are omitted; damaged/authentication
/// failures are reported separately so callers can show incomplete discovery.
pub fn discover() -> Result<(Vec<SessionInfo>, Vec<String>)> {
    let root = registry_dir()?;
    match fs::symlink_metadata(&root) {
        Ok(_) => validate_registry(&root)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), Vec::new()));
        }
        Err(error) => return Err(error.to_string()),
    }
    let mut pending = Vec::new();
    let mut warnings = Vec::new();
    for (index, entry) in fs::read_dir(&root).map_err(|e| e.to_string())?.enumerate() {
        if index >= 1024 {
            warnings.push(
                "Native terminal registry has more than 1024 entries; discovery is incomplete"
                    .into(),
            );
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                warnings.push("A native terminal registry entry could not be read".into());
                continue;
            }
        };
        if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        match read_private::<Endpoint>(&entry.path()) {
            Ok(endpoint) => pending.push((endpoint.id, endpoint.generation)),
            Err(_) => {
                warnings.push("A native terminal registry entry is invalid or not private".into())
            }
        }
    }
    let queue = Mutex::new(pending);
    let sessions = Mutex::new(Vec::new());
    let warnings = Mutex::new(warnings);
    let deadline = Instant::now() + Duration::from_secs(1);
    thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                while Instant::now()<deadline {
                    let item=queue.lock().unwrap().pop();
                    let Some((id,generation))=item else {break;};
                    match rpc_timeout(&root,&id,&generation,json!({"op":"info"}),Duration::from_millis(250)) {
                        Ok(value)=>match serde_json::from_value::<SessionInfo>(value) {
                            Ok(info) if info.exited.is_none()=>sessions.lock().unwrap().push(info),
                            Ok(_)=>(),
                            Err(_)=>warnings.lock().unwrap().push("A native terminal returned invalid session metadata".into()),
                        },
                        Err(error) if error=="Native terminal is unavailable"=>(),
                        Err(_)=>warnings.lock().unwrap().push("A native terminal could not be authenticated or did not respond in time".into()),
                    }
                }
            });
        }
    });
    if !queue.into_inner().unwrap().is_empty() {
        warnings
            .lock()
            .unwrap()
            .push("Native terminal discovery timed out; refresh to retry".into());
    }
    let mut sessions = sessions.into_inner().unwrap();
    sessions.sort_by(|a, b| (&a.workspace, &a.name, &a.id).cmp(&(&b.workspace, &b.name, &b.id)));
    let mut warnings = warnings.into_inner().unwrap();
    warnings.sort();
    warnings.dedup();
    Ok((sessions, warnings))
}
pub fn info(id: &str, generation: &str) -> Result<SessionInfo> {
    let info: SessionInfo = serde_json::from_value(rpc(id, generation, json!({"op":"info"}))?)
        .map_err(|e| e.to_string())?;
    if info.exited.is_some() {
        return Err("Native terminal has exited".into());
    }
    Ok(info)
}
pub fn capture(id: &str, generation: &str) -> Result<Capture> {
    serde_json::from_value(rpc(id, generation, json!({"op":"capture"}))?).map_err(|e| e.to_string())
}
pub fn input(id: &str, generation: &str, bytes: &[u8]) -> Result<()> {
    if bytes.len() > 65536 {
        return Err("Terminal input exceeds 64 KiB".into());
    }
    rpc(
        id,
        generation,
        json!({"op":"input","bytes":STANDARD.encode(bytes)}),
    )
    .map(|_| ())
}
pub fn resize(id: &str, generation: &str, cols: u16, rows: u16) -> Result<()> {
    size(cols, rows)?;
    rpc(
        id,
        generation,
        json!({"op":"resize","cols":cols,"rows":rows}),
    )
    .map(|_| ())
}
pub fn close(id: &str, generation: &str) -> Result<()> {
    rpc(id, generation, json!({"op":"close"})).map(|_| ())
}
pub fn rename(id: &str, generation: &str, name: &str, workspace: &str) -> Result<()> {
    label(name)?;
    label(workspace)?;
    rpc(
        id,
        generation,
        json!({"op":"rename","name":name,"workspace":workspace}),
    )
    .map(|_| ())
}

type WriteRequest = (Vec<u8>, Option<mpsc::Sender<Result<()>>>);
struct InputWriter {
    tx: mpsc::SyncSender<WriteRequest>,
    busy: Arc<AtomicBool>,
}
struct TerminalReplies {
    tx: mpsc::SyncSender<WriteRequest>,
}
impl TerminalReplies {
    fn reply(&self, bytes: Vec<u8>) {
        let _ = self.tx.try_send((bytes, None));
    }
}
impl vt100::Callbacks for TerminalReplies {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let param = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        let response = match (i1, i2, c, param) {
            (None, None, 'n', 5) => b"\x1b[0n".to_vec(),
            (None, None, 'n', 6) => {
                let (row, col) = screen.cursor_position();
                format!("\x1b[{};{}R", row + 1, col + 1).into_bytes()
            }
            (Some(b'?'), None, 'n', 6) => {
                let (row, col) = screen.cursor_position();
                format!("\x1b[?{};{}R", row + 1, col + 1).into_bytes()
            }
            (None, None, 'c', 0) => b"\x1b[?1;2c".to_vec(),
            (Some(b'>'), None, 'c', 0) => b"\x1b[>0;1;0c".to_vec(),
            (Some(b'?'), None, 'u', 0) => b"\x1b[?0u".to_vec(),
            (Some(b'?'), Some(b'$'), 'p', mode) => format!("\x1b[?{mode};0$y").into_bytes(),
            (None, None, 't', 18) => {
                let (rows, cols) = screen.size();
                format!("\x1b[8;{rows};{cols}t").into_bytes()
            }
            _ => return,
        };
        self.reply(response);
    }
    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        if params.len() != 2 || params[1] != b"?" {
            return;
        }
        let response = match params[0] {
            b"10" => b"\x1b]10;rgb:cccc/cccc/cccc\x1b\\".to_vec(),
            b"11" => b"\x1b]11;rgb:0000/0000/0000\x1b\\".to_vec(),
            _ => return,
        };
        self.reply(response);
    }
}
struct State {
    info: SessionInfo,
    screen: Arc<Mutex<vt100::Parser<TerminalReplies>>>,
    master: Box<dyn MasterPty + Send>,
    input: InputWriter,
    lease: Option<(String, Instant)>,
}
impl State {
    fn expire(&mut self) {
        if self
            .lease
            .as_ref()
            .is_some_and(|(_, when)| when.elapsed() > LEASE)
        {
            self.lease = None;
        }
        self.info.controlled = self.lease.is_some() && self.info.exited.is_none();
        #[cfg(unix)]
        {
            self.info.foreground_pid = self.master.as_raw_fd().and_then(|fd| {
                let pid = unsafe { libc::tcgetpgrp(fd) };
                (pid > 0).then_some(pid as u32)
            });
            #[cfg(target_os = "linux")]
            if let Some(pid) = self.info.foreground_pid {
                use std::os::unix::fs::MetadataExt;
                let process = PathBuf::from(format!("/proc/{pid}"));
                if fs::metadata(&process).is_ok_and(|meta| meta.uid() == unsafe { libc::geteuid() })
                {
                    if let Ok(cwd) = fs::read_link(process.join("cwd")) {
                        self.info.cwd = cwd;
                    }
                    if let Ok(name) = fs::read_to_string(process.join("comm")) {
                        let name = name.trim();
                        if !name.is_empty() && !name.chars().any(char::is_control) {
                            self.info.command = name.into();
                        }
                    }
                }
            }
        }
    }
    fn lease(&mut self, request: &Value) -> Result<()> {
        self.expire();
        if let Some((owner, when)) = &mut self.lease {
            if request["lease"].as_str() != Some(owner.as_str()) {
                return Err(
                    "Terminal already has an attached client; use observe or detach that client"
                        .into(),
                );
            }
            *when = Instant::now();
        } else if request["lease"].as_str().is_some() {
            return Err("Terminal control lease expired; reconnect before sending input".into());
        }
        Ok(())
    }
    fn request(&mut self, request: &Value) -> Result<Value> {
        self.expire();
        if self.info.exited.is_some()
            && !matches!(
                request["op"].as_str(),
                Some("info" | "capture" | "release" | "close")
            )
        {
            return Err("Native terminal has exited".into());
        }
        match request["op"].as_str().unwrap_or("") {
            "info" => Ok(json!(self.info)),
            "acquire" => {
                let owner = request["lease"]
                    .as_str()
                    .ok_or("Missing control identity")?;
                identity(owner)?;
                if self.lease.as_ref().is_some_and(|(held, _)| held != owner) {
                    return Err(
                        "Terminal already has an attached client; observing remains available"
                            .into(),
                    );
                }
                self.lease = Some((owner.into(), Instant::now()));
                Ok(Value::Null)
            }
            "release" => {
                self.lease(request)?;
                self.lease = None;
                Ok(Value::Null)
            }
            "capture" => {
                if request["lease"].is_string() {
                    self.lease(request)?;
                }
                let parser = self
                    .screen
                    .lock()
                    .map_err(|_| "Terminal screen unavailable")?;
                Ok(json!(Capture {
                    session: self.info.clone(),
                    ansi: String::from_utf8_lossy(&parser.screen().state_formatted()).into_owned(),
                    text: parser.screen().contents()
                }))
            }
            "input" => {
                self.lease(request)?;
                let bytes = STANDARD
                    .decode(request["bytes"].as_str().ok_or("Missing input bytes")?)
                    .map_err(|_| "Invalid input encoding")?;
                if bytes.len() > 65536 {
                    return Err("Terminal input exceeds 64 KiB".into());
                }
                if self.input.busy.swap(true, Ordering::AcqRel) {
                    return Err("Terminal input is busy; no input was queued".into());
                }
                let (tx, rx) = mpsc::channel();
                if self.input.tx.try_send((bytes, Some(tx))).is_err() {
                    self.input.busy.store(false, Ordering::Release);
                    return Err("Terminal input is busy; no input was queued".into());
                }
                rx.recv_timeout(Duration::from_millis(700))
                    .map_err(|_| "Input outcome is unconfirmed; do not retry automatically")??;
                Ok(Value::Null)
            }
            "resize" => {
                self.lease(request)?;
                let cols = request["cols"]
                    .as_u64()
                    .and_then(|n| u16::try_from(n).ok())
                    .ok_or("Invalid columns")?;
                let rows = request["rows"]
                    .as_u64()
                    .and_then(|n| u16::try_from(n).ok())
                    .ok_or("Invalid rows")?;
                let dimensions = size(cols, rows)?;
                let mut parser = self
                    .screen
                    .lock()
                    .map_err(|_| "Terminal screen unavailable")?;
                self.master.resize(dimensions).map_err(|e| e.to_string())?;
                parser.screen_mut().set_size(rows, cols);
                self.info.cols = cols;
                self.info.rows = rows;
                Ok(Value::Null)
            }
            "rename" => {
                let name = request["name"].as_str().ok_or("Missing name")?;
                let workspace = request["workspace"].as_str().ok_or("Missing workspace")?;
                label(name)?;
                label(workspace)?;
                self.info.name = name.into();
                self.info.workspace = workspace.into();
                Ok(Value::Null)
            }
            "close" => Ok(Value::Null),
            _ => Err("Unknown native terminal operation".into()),
        }
    }
}
struct OwnedChild(Box<dyn portable_pty::Child + Send + Sync>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
struct RemoveFile(PathBuf);
impl Drop for RemoveFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn serve(root: &Path, id: &str) -> Result<()> {
    identity(id)?;
    ensure_private(root)?;
    let launch_path = root.join(format!("{id}.launch"));
    let launch: Launch = read_private(&launch_path)?;
    let _launch_cleanup = RemoveFile(launch_path);
    if launch.id != id {
        return Err("Launch identity mismatch".into());
    }
    identity(&launch.generation)?;
    identity(&launch.token)?;
    let opts = &launch.options;
    label(&opts.name)?;
    label(&opts.workspace)?;
    let pair = portable_pty::native_pty_system()
        .openpty(size(opts.cols, opts.rows)?)
        .map_err(|e| e.to_string())?;
    let mut command = CommandBuilder::new(opts.argv.first().ok_or("Missing shell executable")?);
    command.args(opts.argv.iter().skip(1));
    command.cwd(&opts.cwd);
    if let Some(environment) = &opts.environment {
        command.env_clear();
        for (key, value) in environment {
            command.env(key, value);
        }
    }
    // Nested terminals must identify their own transport, not the launcher's.
    for key in [
        "TMUX",
        "TMUX_PANE",
        "HERDR_ENV",
        "HERDR_PANE_ID",
        "HERDR_SESSION",
        "HERDR_TERM_ID",
    ] {
        command.env_remove(key);
    }
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env("DAEDALUS_TERMINAL_ID", id);
    command.env("DAEDALUS_NATIVE_SESSION", id);
    command.env("DAEDALUS_TERMINAL_GENERATION", &launch.generation);
    let mut child = OwnedChild(
        pair.slave
            .spawn_command(command)
            .map_err(|e| e.to_string())?,
    );
    drop(pair.slave);
    let info = SessionInfo {
        id: id.into(),
        generation: launch.generation.clone(),
        name: opts.name.clone(),
        workspace: opts.workspace.clone(),
        cwd: opts.cwd.clone(),
        command: Path::new(&opts.argv[0])
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        pid: child.0.process_id().unwrap_or(0),
        foreground_pid: None,
        cols: opts.cols,
        rows: opts.rows,
        controlled: false,
        exited: None,
    };
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let mut writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let (input, inputs) = mpsc::sync_channel::<WriteRequest>(8);
    let busy = Arc::new(AtomicBool::new(false));
    let writer_busy = busy.clone();
    thread::spawn(move || {
        while let Ok((bytes, reply)) = inputs.recv() {
            let result = writer
                .write_all(&bytes)
                .and_then(|_| writer.flush())
                .map_err(|_| {
                    "Input may have been partially written; inspect the terminal before retrying"
                        .into()
                });
            if let Some(reply) = reply {
                writer_busy.store(false, Ordering::Release);
                let _ = reply.send(result);
            }
        }
    });
    let callbacks = TerminalReplies { tx: input.clone() };
    let screen = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
        opts.rows, opts.cols, 1000, callbacks,
    )));
    let input = InputWriter { tx: input, busy };
    let reader_screen = screen.clone();
    thread::spawn(move || {
        let mut bytes = [0; 16384];
        while let Ok(n) = reader.read(&mut bytes) {
            if n == 0 {
                break;
            }
            if let Ok(mut parser) = reader_screen.lock() {
                parser.process(&bytes[..n]);
            } else {
                break;
            }
        }
    });
    let mut state = State {
        info,
        screen,
        master: pair.master,
        input,
        lease: None,
    };
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let descriptor = root.join(format!("{id}.json"));
    let _descriptor_cleanup = RemoveFile(descriptor.clone());
    save(
        &descriptor,
        &Endpoint {
            id: id.into(),
            generation: launch.generation.clone(),
            token: launch.token.clone(),
            port: listener.local_addr().map_err(|e| e.to_string())?.port(),
        },
    )?;
    // The launch file contains argv and is not needed once the shell is running.
    let _ = fs::remove_file(&_launch_cleanup.0);
    let mut ended_at = None;
    loop {
        if ended_at.is_none()
            && let Some(status) = child.0.try_wait().map_err(|e| e.to_string())?
        {
            state.info.exited = Some(status.exit_code());
            ended_at = Some(Instant::now());
        }
        // Deliver authenticated completion to viewers, then remove the registry entry.
        // No tombstone files or background terminal history are retained.
        if ended_at.is_some_and(|ended: Instant| ended.elapsed() >= Duration::from_secs(1)) {
            break;
        }

        match listener.accept() {
            Ok((mut stream, _)) => {
                // Winsock accepted sockets inherit the listener's nonblocking mode.
                // The authenticated exchange needs bounded blocking reads on every OS.
                stream.set_nonblocking(false).map_err(|e| e.to_string())?;
                let _ = stream.set_nodelay(true);
                let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
                let _ = stream.set_write_timeout(Some(Duration::from_millis(300)));
                let mut greeting = Vec::new();
                let read = BufReader::new(&mut stream)
                    .take(4097)
                    .read_until(b'\n', &mut greeting);
                let greeting: Value = match read {
                    Ok(_) if greeting.len() <= 4096 => {
                        serde_json::from_slice(&greeting).unwrap_or(Value::Null)
                    }
                    _ => Value::Null,
                };
                let Some(client_nonce) = greeting["nonce"].as_str() else {
                    continue;
                };
                if identity(client_nonce).is_err() {
                    continue;
                }
                let challenge = random()?;
                let hello = seal(
                    &launch.token,
                    json!({"challenge":challenge,"generation":launch.generation,"nonce":client_nonce}),
                )?;
                if serde_json::to_writer(&mut stream, &hello).is_err()
                    || stream.write_all(b"\n").is_err()
                {
                    continue;
                }
                let mut line = Vec::new();
                let read = BufReader::new(&mut stream)
                    .take(MAX_REQUEST + 1)
                    .read_until(b'\n', &mut line);
                let request: Value = match read {
                    Ok(_) if line.len() as u64 <= MAX_REQUEST => {
                        serde_json::from_slice(&line).unwrap_or(Value::Null)
                    }
                    _ => Value::Null,
                };
                // Check credentials before inspecting operations. Tokens never appear in replies.
                let Ok(request) = unseal(&launch.token, &request) else {
                    continue;
                };
                if request["generation"].as_str() != Some(&launch.generation)
                    || request["challenge"].as_str() != Some(&challenge)
                    || request["nonce"].as_str() != Some(client_nonce)
                {
                    continue;
                }
                let mut result = state.request(&request);
                let close = request["op"] == "close" && result.is_ok();
                if close && state.info.exited.is_none() {
                    #[cfg(unix)]
                    if let Some(group) = state.master.process_group_leader()
                        && group > 0
                    {
                        unsafe {
                            libc::kill(-group, libc::SIGHUP);
                        }
                    }
                    result = child
                        .0
                        .kill()
                        .and_then(|_| child.0.wait())
                        .map(|_| Value::Null)
                        .map_err(|_| "Terminal close could not be confirmed".into());
                }
                let mut response = match result {
                    Ok(value) => json!({"result":value}),
                    Err(error) => json!({"error":error}),
                };
                response["nonce"] = request["nonce"].clone();
                let response = seal(&launch.token, response)?;
                let _ = serde_json::to_writer(&mut stream, &response);
                let _ = stream.write_all(b"\n");
                if close {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10))
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

struct ControlLease {
    id: String,
    generation: String,
    owner: String,
}
impl ControlLease {
    fn acquire(id: &str, generation: &str) -> Result<Self> {
        let lease = Self {
            id: id.into(),
            generation: generation.into(),
            owner: random()?,
        };
        rpc(id, generation, json!({"op":"acquire","lease":lease.owner}))?;
        Ok(lease)
    }
    fn request(&self, mut value: Value) -> Result<Value> {
        value["lease"] = Value::String(self.owner.clone());
        rpc(&self.id, &self.generation, value)
    }
}
impl Drop for ControlLease {
    fn drop(&mut self) {
        let _ = self.request(json!({"op":"release"}));
    }
}
fn emit(value: &Value) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, value).map_err(|e| e.to_string())?;
    stdout
        .write_all(b"\n")
        .and_then(|_| stdout.flush())
        .map_err(|e| e.to_string())
}
fn stream(id: &str, generation: &str, control: bool, cols: u16, rows: u16) -> Result<()> {
    let lease = if control {
        Some(ControlLease::acquire(id, generation)?)
    } else {
        None
    };
    if let Some(lease) = &lease {
        size(cols, rows)?;
        lease.request(json!({"op":"resize","cols":cols,"rows":rows}))?;
    }
    let (tx, rx) = mpsc::sync_channel(32);
    thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        loop {
            let mut bytes = Vec::new();
            match (&mut stdin)
                .take(MAX_REQUEST + 1)
                .read_until(b'\n', &mut bytes)
            {
                Ok(n) if n > 0 && n as u64 <= MAX_REQUEST => {
                    let value = serde_json::from_slice::<Value>(&bytes)
                        .map_err(|_| "Invalid stream command".to_string());
                    if tx.send(value).is_err() {
                        break;
                    }
                }
                _ => break,
            }
        }
    });
    let mut previous = String::new();
    loop {
        for _ in 0..32 {
            let value = match rx.try_recv() {
                Ok(value) => value?,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            };
            match value["type"].as_str() {
                Some("terminal.release") => return Ok(()),
                Some("terminal.input") => {
                    let lease = lease
                        .as_ref()
                        .ok_or("Observer cannot send terminal input")?;
                    lease.request(json!({"op":"input","bytes":value["bytes"]}))?;
                }
                Some("terminal.resize") => {
                    let lease = lease
                        .as_ref()
                        .ok_or("Observer cannot resize the source terminal")?;
                    lease.request(
                        json!({"op":"resize","cols":value["cols"],"rows":value["rows"]}),
                    )?;
                }
                _ => return Err("Unknown terminal stream command".into()),
            }
        }
        let value = match &lease {
            Some(lease) => lease.request(json!({"op":"capture"}))?,
            None => rpc(id, generation, json!({"op":"capture"}))?,
        };
        let capture: Capture = serde_json::from_value(value).map_err(|e| e.to_string())?;
        // Full frames avoid replaying OSC/clipboard sequences from untrusted applications.
        if capture.ansi != previous {
            emit(
                &json!({"type":"terminal.frame","width":capture.session.cols,"height":capture.session.rows,"bytes":STANDARD.encode(capture.ansi.as_bytes()),"full":true}),
            )?;
            previous = capture.ansi;
        }
        if let Some(code) = capture.session.exited {
            emit(
                &json!({"type":"terminal.closed","reason":if code==0 {"Terminal exited normally".into()}else{format!("Terminal exited with status {code}")}}),
            )?;
            return Ok(());
        }
        thread::sleep(Duration::from_millis(80));
    }
}
struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::DisableBracketedPaste,
            crossterm::cursor::Show,
            crossterm::terminal::LeaveAlternateScreen
        );
    }
}
/// Attach locally; Ctrl+] then D detaches without terminating the shell.
pub fn attach(id: &str, generation: &str) -> Result<()> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(
            "Attach requires an interactive terminal; use capture or stream for agent tools".into(),
        );
    }
    let lease = ControlLease::acquire(id, generation)?;
    crossterm::terminal::enable_raw_mode().map_err(|e| e.to_string())?;
    let _guard = TerminalGuard;
    crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        event::EnableBracketedPaste
    )
    .map_err(|e| e.to_string())?;
    let (cols, rows) = crossterm::terminal::size().map_err(|e| e.to_string())?;
    lease.request(json!({"op":"resize","cols":cols.clamp(2,512),"rows":rows.clamp(2,256)}))?;
    let mut previous = String::new();
    let mut prefix = false;
    loop {
        let value = lease.request(json!({"op":"capture"}))?;
        let capture: Capture = serde_json::from_value(value).map_err(|e| e.to_string())?;
        let mut parser = vt100::Parser::new(capture.session.rows, capture.session.cols, 0);
        parser.process(capture.ansi.as_bytes());
        if capture.ansi != previous {
            let mut stdout = std::io::stdout().lock();
            stdout
                .write_all(&parser.screen().contents_formatted())
                .and_then(|_| stdout.flush())
                .map_err(|e| e.to_string())?;
            previous = capture.ansi;
        }
        if let Some(code) = capture.session.exited {
            return if code == 0 {
                Ok(())
            } else {
                Err(format!("Terminal exited with status {code}"))
            };
        }
        if !event::poll(Duration::from_millis(60)).map_err(|e| e.to_string())? {
            continue;
        }
        let bytes = match event::read().map_err(|e| e.to_string())? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if prefix {
                    prefix = false;
                    if matches!(key.code, KeyCode::Char('d' | 'D')) {
                        return Ok(());
                    }
                    let mut bytes = vec![0x1d];
                    bytes.extend(key_bytes(key, parser.screen().application_cursor()));
                    bytes
                } else if key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char(']' | '5'))
                {
                    prefix = true;
                    continue;
                } else {
                    key_bytes(key, parser.screen().application_cursor())
                }
            }
            Event::Paste(text) => {
                // Preserve the application's bracketed paste choice, not the viewer's.
                if parser.screen().bracketed_paste() {
                    format!("\x1b[200~{text}\x1b[201~").into_bytes()
                } else {
                    text.into_bytes()
                }
            }
            Event::Resize(cols, rows) => {
                lease.request(
                    json!({"op":"resize","cols":cols.clamp(2,512),"rows":rows.clamp(2,256)}),
                )?;
                continue;
            }
            _ => continue,
        };
        if !bytes.is_empty() {
            lease.request(json!({"op":"input","bytes":STANDARD.encode(bytes)}))?;
        }
    }
}
fn key_bytes(key: crossterm::event::KeyEvent, application_cursor: bool) -> Vec<u8> {
    use crossterm::event::{KeyCode, KeyModifiers};
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let modifier = 1
        + u8::from(key.modifiers.contains(KeyModifiers::SHIFT))
        + 2 * u8::from(alt)
        + 4 * u8::from(control);
    let cursor = |suffix: char| {
        if modifier > 1 {
            format!("\x1b[1;{modifier}{suffix}").into_bytes()
        } else if application_cursor {
            format!("\x1bO{suffix}").into_bytes()
        } else {
            format!("\x1b[{suffix}").into_bytes()
        }
    };
    let mut result = match key.code {
        KeyCode::Char(c) if control && (c.is_ascii_alphabetic() || ('@'..='_').contains(&c)) => {
            vec![(c as u8) & 0x1f]
        }
        KeyCode::Char(' ' | '2') if control => vec![0],
        KeyCode::Char('3') if control => vec![0x1b],
        KeyCode::Char(c @ '4'..='7') if control => vec![c as u8 - b'4' + 0x1c],
        KeyCode::Char('8') if control => vec![0x7f],
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Backspace => vec![127],
        KeyCode::Esc => vec![27],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Up => cursor('A'),
        KeyCode::Down => cursor('B'),
        KeyCode::Right => cursor('C'),
        KeyCode::Left => cursor('D'),
        KeyCode::Home => cursor('H'),
        KeyCode::End => cursor('F'),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::F(n @ 1..=4) => format!("\x1bO{}", (b'P' + n - 1) as char).into_bytes(),
        KeyCode::F(n @ 5..=12) => format!(
            "\x1b[{}~",
            [15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)]
        )
        .into_bytes(),
        _ => Vec::new(),
    };
    if alt
        && matches!(
            key.code,
            KeyCode::Char(_) | KeyCode::Enter | KeyCode::Tab | KeyCode::Backspace | KeyCode::Esc
        )
    {
        result.insert(0, 27);
    }
    result
}
fn flag(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|v| v == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
fn number(args: &[String], key: &str, default: u16) -> Result<u16> {
    flag(args, key)
        .map(|v| v.parse().map_err(|_| format!("Invalid {key}")))
        .unwrap_or(Ok(default))
}
fn help() {
    println!(
        "daedalus shell [--name NAME] [--workspace NAME] [--cwd PATH] [-- PROGRAM ARGS...]\n\nNative sessions: terminal-native start | list | capture | input | resize | rename | close | attach | stream\nOperations use ID GENERATION from list. Attach: Ctrl+] then D detaches; exit closes the shell.\nExisting unmanaged shells cannot be retroactively captured. Launch them with daedalus shell."
    );
}
fn options(args: &[String], values: &[&str]) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    let mut index = 0;
    while index < args.len() {
        let key = args[index].as_str();
        if !seen.insert(key) {
            return Err(format!("Duplicate option: {key}"));
        }
        if key == "--json" {
            index += 1;
            continue;
        }
        if !values.contains(&key) {
            return Err(format!(
                "Unknown option: {key}. Use -- before the shell command."
            ));
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("Missing value for {key}"))?;
        if value.starts_with("--") || value == "-h" {
            return Err(format!("Missing value for {key}"));
        }
        index += 2;
    }
    Ok(())
}
pub fn cli(args: &[String]) -> Result<()> {
    let split = args
        .iter()
        .position(|value| value == "--")
        .unwrap_or(args.len());
    if args[..split]
        .iter()
        .any(|value| value == "--help" || value == "-h")
    {
        help();
        return Ok(());
    }
    let verb = args.first().map(String::as_str).unwrap_or("help");
    match verb {
        "serve" => {
            if args.len() != 2 {
                return Err("Expected one native terminal identity".into());
            }
            serve(&registry_dir()?, &args[1])
        }
        "start" | "shell" => {
            options(
                &args[1..split],
                &["--name", "--workspace", "--cwd", "--cols", "--rows"],
            )?;
            if verb == "shell" {
                use std::io::IsTerminal;
                if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
                    return Err("Shell requires an interactive terminal; use terminal-native start for a detached session".into());
                }
            }
            let split = args.iter().position(|v| v == "--").unwrap_or(args.len());
            let options = &args[..split];
            let cwd = flag(options, "--cwd")
                .map(PathBuf::from)
                .unwrap_or(std::env::current_dir().map_err(|e| e.to_string())?);
            let workspace = flag(options, "--workspace").unwrap_or_else(|| {
                cwd.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
            let info = start(StartOptions {
                name: flag(options, "--name").unwrap_or_else(|| "Shell".into()),
                workspace: if workspace.is_empty() {
                    "Workspace".into()
                } else {
                    workspace
                },
                cwd,
                argv: args.get(split + 1..).unwrap_or_default().to_vec(),
                environment: None,
                cols: number(options, "--cols", 120)?,
                rows: number(options, "--rows", 36)?,
            })?;
            if verb == "shell" {
                attach(&info.id, &info.generation)
            } else {
                emit(&json!(info))
            }
        }
        "list" => {
            options(&args[1..], &[])?;
            emit(&json!(list()?))
        }
        "capture" | "input" | "resize" | "close" | "rename" | "attach" | "stream" => {
            let id = args.get(1).ok_or("Missing terminal ID")?;
            let generation = args.get(2).ok_or("Missing terminal generation")?;
            let allowed: &[&str] = match verb {
                "input" => &["--text", "--base64"],
                "resize" => &["--cols", "--rows"],
                "rename" => &["--name", "--workspace"],
                "stream" => &["--mode", "--cols", "--rows"],
                _ => &[],
            };
            options(&args[3..], allowed)?;
            match verb {
                "capture" => emit(&json!(capture(id, generation)?)),
                "input" => {
                    if flag(args, "--text").is_some() && flag(args, "--base64").is_some() {
                        return Err("Use either --text or --base64, not both".into());
                    }
                    let bytes = if let Some(encoded) = flag(args, "--base64") {
                        STANDARD
                            .decode(encoded)
                            .map_err(|_| "Invalid base64 input")?
                    } else {
                        flag(args, "--text")
                            .ok_or("Use --text TEXT or --base64 BYTES")?
                            .into_bytes()
                    };
                    input(id, generation, &bytes)
                }
                "resize" => resize(
                    id,
                    generation,
                    number(args, "--cols", 120)?,
                    number(args, "--rows", 36)?,
                ),
                "close" => close(id, generation),
                "rename" => rename(
                    id,
                    generation,
                    &flag(args, "--name").ok_or("Missing --name")?,
                    &flag(args, "--workspace").ok_or("Missing --workspace")?,
                ),
                "attach" => attach(id, generation),
                "stream" => {
                    let mode = flag(args, "--mode").unwrap_or_else(|| "observe".into());
                    if mode != "control" && mode != "observe" {
                        return Err("Mode must be control or observe".into());
                    }
                    let result = stream(
                        id,
                        generation,
                        mode == "control",
                        number(args, "--cols", 120)?,
                        number(args, "--rows", 36)?,
                    );
                    if let Err(error) = &result {
                        let _ = emit(&json!({"type":"terminal.closed","reason":error}));
                    }
                    result
                }
                _ => unreachable!(),
            }
        }
        "help" => {
            help();
            Ok(())
        }
        _ => Err(format!(
            "Unknown native terminal command: {verb}. Use --help."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        root: PathBuf,
        id: String,
        generation: String,
        worker: Option<thread::JoinHandle<Result<()>>>,
    }
    impl Fixture {
        fn call(&self, value: Value) -> Result<Value> {
            rpc_at(&self.root, &self.id, &self.generation, value)
        }
        fn shell() -> Self {
            let root = std::env::temp_dir().join(format!("daedalus-native-{}", random().unwrap()));
            ensure_private(&root).unwrap();
            let launch = Launch {
                id: random().unwrap(),
                generation: random().unwrap(),
                token: random().unwrap(),
                options: StartOptions {
                    name: "Fixture shell".into(),
                    workspace: "Fixture workspace".into(),
                    cwd: root.clone(),
                    #[cfg(unix)]
                    argv: vec!["/bin/sh".into()],
                    #[cfg(windows)]
                    argv: vec!["cmd.exe".into(), "/D".into(), "/Q".into()],
                    #[cfg(unix)]
                    environment: Some(std::collections::BTreeMap::from([
                        ("PATH".into(), "/usr/bin:/bin".into()),
                        ("PS1".into(), String::new()),
                    ])),
                    #[cfg(windows)]
                    environment: None,
                    cols: 80,
                    rows: 24,
                },
            };
            save(&root.join(format!("{}.launch", launch.id)), &launch).unwrap();
            let serve_root = root.clone();
            let id = launch.id.clone();
            let worker = thread::spawn(move || serve(&serve_root, &id));
            let fixture = Self {
                root,
                id: launch.id,
                generation: launch.generation,
                worker: Some(worker),
            };
            let deadline = Instant::now() + Duration::from_secs(5);
            while let Err(error) = fixture.call(json!({"op":"info"})) {
                assert!(
                    Instant::now() < deadline,
                    "Fixture shell failed to start: {error}"
                );
                thread::sleep(Duration::from_millis(20));
            }
            fixture
        }
        fn wait_text(&self, text: &str) -> Capture {
            let deadline = Instant::now() + Duration::from_secs(4);
            loop {
                let capture: Capture =
                    serde_json::from_value(self.call(json!({"op":"capture"})).unwrap()).unwrap();
                if capture.text.contains(text) {
                    return capture;
                }
                assert!(
                    Instant::now() < deadline,
                    "Shell did not produce expected output: {}",
                    capture.text
                );
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.call(json!({"op":"close"}));
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn authentication_binds_payload_and_never_transmits_secret() {
        let secret = random().unwrap();
        let mut envelope = seal(&secret, json!({"op":"close","nonce":"fixture"})).unwrap();
        assert!(!envelope.to_string().contains(&secret));
        assert_eq!(unseal(&secret, &envelope).unwrap()["op"], "close");
        assert!(unseal(&random().unwrap(), &envelope).is_err());
        envelope["payload"]["op"] = json!("input");
        assert!(unseal(&secret, &envelope).is_err());
        // RFC 4231, HMAC-SHA256 case 1 (20 bytes of 0x0b).
        assert_eq!(
            mac(&"\x0b".repeat(20), b"Hi There"),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }
    #[test]
    fn stale_port_receives_no_input_before_server_authentication() {
        unauthenticated_listener(false);
        unauthenticated_listener(true);
    }
    fn unauthenticated_listener(replay: bool) {
        let root =
            std::env::temp_dir().join(format!("daedalus-native-handshake-{}", random().unwrap()));
        ensure_private(&root).unwrap();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let endpoint = Endpoint {
            id: random().unwrap(),
            generation: random().unwrap(),
            token: random().unwrap(),
            port: listener.local_addr().unwrap().port(),
        };
        save(&root.join(format!("{}.json", endpoint.id)), &endpoint).unwrap();
        let generation = endpoint.generation.clone();
        let token = endpoint.token.clone();
        let fake = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut initial = Vec::new();
            BufReader::new(&mut stream)
                .read_until(b'\n', &mut initial)
                .unwrap();
            let greeting: Value = serde_json::from_slice(&initial).unwrap();
            assert_eq!(
                greeting.as_object().unwrap().len(),
                1,
                "Only a fresh public nonce is sent before server authentication"
            );
            let fake_hello = seal(
                &if replay {token}else{random().unwrap()},
                json!({"challenge":random().unwrap(),"generation":generation,"nonce":if replay {json!(random().unwrap())}else{greeting["nonce"].clone()}}),
            )
            .unwrap();
            serde_json::to_writer(&mut stream, &fake_hello).unwrap();
            stream.write_all(b"\n").unwrap();
            let mut bytes = Vec::new();
            let _ = stream.read_to_end(&mut bytes);
            bytes
        });
        assert!(
            rpc_at(
                &root,
                &endpoint.id,
                &endpoint.generation,
                json!({"op":"input","bytes":STANDARD.encode(b"private input")})
            )
            .is_err()
        );
        assert!(
            fake.join().unwrap().is_empty(),
            "No operation payload may reach an unauthenticated listener"
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[cfg(unix)]
    fn captured_request_cannot_be_replayed_on_another_connection() {
        let fixture = Fixture::shell();
        let endpoint: Endpoint =
            read_private(&fixture.root.join(format!("{}.json", fixture.id))).unwrap();
        let connect = || {
            let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, endpoint.port)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let nonce = random().unwrap();
            serde_json::to_writer(&mut stream, &json!({"nonce":nonce})).unwrap();
            stream.write_all(b"\n").unwrap();
            let mut bytes = Vec::new();
            BufReader::new(&mut stream)
                .read_until(b'\n', &mut bytes)
                .unwrap();
            let hello = unseal(
                &endpoint.token,
                &serde_json::from_slice::<Value>(&bytes).unwrap(),
            )
            .unwrap();
            (stream, hello)
        };
        let (mut first, hello) = connect();
        let request=seal(&endpoint.token,json!({"op":"rename","name":"Once","workspace":"Fixture workspace","generation":fixture.generation,"nonce":hello["nonce"],"challenge":hello["challenge"]})).unwrap();
        serde_json::to_writer(&mut first, &request).unwrap();
        first.write_all(b"\n").unwrap();
        let mut reply = String::new();
        BufReader::new(first).read_line(&mut reply).unwrap();
        assert!(!reply.is_empty());
        fixture
            .call(
                json!({"op":"rename","name":"After first request","workspace":"Fixture workspace"}),
            )
            .unwrap();
        let (mut replay, _) = connect();
        serde_json::to_writer(&mut replay, &request).unwrap();
        replay.write_all(b"\n").unwrap();
        let mut reply = String::new();
        let _ = BufReader::new(replay).read_line(&mut reply);
        assert!(reply.is_empty());
        assert_eq!(
            fixture.call(json!({"op":"info"})).unwrap()["name"],
            "After first request"
        );
    }
    #[test]
    fn parser_answers_terminal_queries_without_exposing_clipboard() {
        let (tx, rx) = mpsc::sync_channel(16);
        let mut parser = vt100::Parser::new_with_callbacks(24, 80, 0, TerminalReplies { tx });
        parser.process(
            b"\x1b[3;5H\x1b[6n\x1b[5n\x1b[c\x1b[>c\x1b[?u\x1b[18t\x1b]11;?\x07\x1b]52;c;?\x07",
        );
        let responses: Vec<Vec<u8>> = rx.try_iter().map(|(bytes, _)| bytes).collect();
        assert_eq!(responses.len(), 7);
        assert_eq!(responses[0], b"\x1b[3;5R");
        assert_eq!(responses[1], b"\x1b[0n");
        assert_eq!(responses[5], b"\x1b[8;24;80t");
        assert!(
            !responses
                .iter()
                .any(|bytes| bytes.windows(3).any(|w| w == b"52;"))
        );
    }
    #[test]
    fn invalid_identities_names_and_sizes_are_rejected() {
        for value in ["../escape", "", "ABCD", "0"] {
            assert!(identity(value).is_err());
        }
        assert!(identity(&"a".repeat(64)).is_ok());
        assert!(label("Name\x1b]52;secret").is_err());
        assert!(size(0, 24).is_err());
        assert!(size(80, 65535).is_err());
    }
    #[test]
    #[cfg(windows)]
    fn windows_conpty_capture_input_resize_identity_and_cleanup() {
        let fixture = Fixture::shell();
        fixture
            .call(json!({"op":"input","bytes":STANDARD.encode(b"echo WINDOWS_NATIVE_OK\r")}))
            .unwrap();
        let capture = fixture.wait_text("WINDOWS_NATIVE_OK");
        assert!(capture.session.pid > 0);
        fixture
            .call(json!({"op":"resize","cols":93,"rows":31}))
            .unwrap();
        fixture
            .call(json!({"op":"rename","name":"Renamed","workspace":"Another project"}))
            .unwrap();
        let info = fixture.call(json!({"op":"info"})).unwrap();
        assert_eq!(info["cols"], 93);
        assert_eq!(info["rows"], 31);
        assert_eq!(info["workspace"], "Another project");
        assert!(
            rpc_at(
                &fixture.root,
                &fixture.id,
                &random().unwrap(),
                json!({"op":"close"})
            )
            .is_err()
        );
        fixture.call(json!({"op":"close"})).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while fixture.root.join(format!("{}.json", fixture.id)).exists() {
            assert!(Instant::now() < deadline, "Native fixture was not removed");
            thread::sleep(Duration::from_millis(20));
        }
    }
    #[test]
    #[cfg(unix)]
    fn real_shell_capture_input_resize_identity_and_cleanup() {
        let fixture = Fixture::shell();
        fixture.call(json!({"op":"input","bytes":STANDARD.encode(b"printf '\\033[38;2;12;34;56mNATIVE_COLOR\\033[0m\\n'\r")})).unwrap();
        let capture = fixture.wait_text("NATIVE_COLOR");
        assert!(capture.ansi.contains("38;2;12;34;56"));
        assert!(capture.session.pid > 0);
        assert!(capture.session.foreground_pid.is_some());
        assert_eq!(capture.session.command, "sh");
        fixture
            .call(json!({"op":"resize","cols":93,"rows":31}))
            .unwrap();
        fixture
            .call(json!({"op":"input","bytes":STANDARD.encode(b"stty size\r")}))
            .unwrap();
        let capture = fixture.wait_text("31 93");
        assert_eq!((capture.session.cols, capture.session.rows), (93, 31));
        fixture
            .call(json!({"op":"rename","name":"Renamed","workspace":"Another project"}))
            .unwrap();
        assert_eq!(
            fixture.call(json!({"op":"info"})).unwrap()["workspace"],
            "Another project"
        );
        assert!(
            rpc_at(
                &fixture.root,
                &fixture.id,
                &random().unwrap(),
                json!({"op":"close"})
            )
            .is_err()
        );
        fixture.call(json!({"op":"close"})).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while fixture.root.join(format!("{}.json", fixture.id)).exists() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(20));
        }
    }
    #[test]
    #[cfg(unix)]
    fn one_controller_observers_and_release_are_enforced() {
        let fixture = Fixture::shell();
        let first = random().unwrap();
        let second = random().unwrap();
        fixture.call(json!({"op":"acquire","lease":first})).unwrap();
        assert!(
            fixture
                .call(json!({"op":"acquire","lease":second}))
                .unwrap_err()
                .contains("attached client")
        );
        assert!(
            fixture
                .call(json!({"op":"input","bytes":STANDARD.encode(b"echo bad\r")}))
                .is_err()
        );
        assert!(
            fixture
                .call(json!({"op":"resize","cols":90,"rows":30}))
                .is_err()
        );
        assert!(
            fixture.call(json!({"op":"capture"})).is_ok(),
            "Observers remain available"
        );
        fixture.call(json!({"op":"input","lease":first,"bytes":STANDARD.encode(b"printf 'CONTROL_OK\\n'\r")})).unwrap();
        fixture.wait_text("CONTROL_OK");
        assert!(
            fixture
                .call(json!({"op":"release","lease":second}))
                .is_err()
        );
        fixture.call(json!({"op":"release","lease":first})).unwrap();
        fixture
            .call(json!({"op":"acquire","lease":second}))
            .unwrap();
        assert!(
            fixture
                .call(json!({"op":"input","lease":first,"bytes":""}))
                .is_err()
        );
    }
    #[test]
    #[cfg(unix)]
    fn private_registry_rejects_symlinks_and_public_credentials() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root =
            std::env::temp_dir().join(format!("daedalus-native-private-{}", random().unwrap()));
        ensure_private(&root).unwrap();
        let target = root.join("state");
        save(&target, &json!({"token":"fixture"})).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_private::<Value>(&target).is_err());
        symlink(&target, root.join("link")).unwrap();
        assert!(read_private::<Value>(&root.join("link")).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
