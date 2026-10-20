//! Standalone native hook orchestration. Registrations are private local policy,
//! not authentication against malicious code running as the same Unix user.
//! The native verifier requires both exact terminal generation and process ancestry.
//! Stdin never chooses the instance, terminal, harness, goal, or Telegram destination.
use super::hooks::{self, Event, EventKind, Features, Harness};
use crate::{
    adapters::terminals::Terminals,
    core::{
        goal_store::{self, Control, Reply, Scope, Store},
        journal::Journal,
        layouts,
    },
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::{
    fd::AsRawFd,
    unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    Identity,
    PrivateState,
    Persistence,
    UnsupportedPolicy,
    EventIdentity,
    GoalPolicy,
    Input,
    Output,
    Deadline,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Invalid(s) => s,
            Self::Identity => "Hook process does not match its registered terminal",
            Self::PrivateState => "Hook state requires private owned regular files",
            Self::Persistence => "Hook state could not be persisted",
            Self::UnsupportedPolicy => "Native tool authorization policy is not configured",
            Self::EventIdentity => "Hook event needs a stable ID or registered transcript boundary",
            Self::GoalPolicy => "Goal continuation requires inspection",
            Self::Input => "Invalid or oversized hook input",
            Self::Output => "Cannot write hook response",
            Self::Deadline => "Goal cooldown exceeded the configured hook deadline",
        })
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    pub telegram_delivery: bool,
    pub telegram_progress: bool,
    pub notifications: bool,
    pub hindsight: bool,
}
impl Options {
    /// Guided enrollment prepares Telegram events; delivery still requires an
    /// explicitly authorized recipient binding. Memory and notifications opt in.
    pub fn guided() -> Self {
        Self {
            telegram_delivery: true,
            telegram_progress: true,
            ..Self::default()
        }
    }
    fn features(&self) -> Features {
        Features {
            telegram_delivery: self.telegram_delivery,
            telegram_progress: self.telegram_progress,
            notifications: self.notifications,
            hindsight: self.hindsight,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    pub version: u32,
    pub id: String,
    pub directory: PathBuf,
    pub scope: Scope,
    pub goal_id: Option<String>,
    #[serde(default)]
    pub harness_process: Option<ProcessIdentity>,
    /// Explicit local server emitter; distinct from the terminal's foreground UI.
    #[serde(default)]
    pub server_emitter: Option<ServerEmitter>,
    pub transcript: Option<PathBuf>,
    pub events: Vec<EventKind>,
    pub options: Options,
    pub activity_lease_ms: u64,
    pub pending_timeout_ms: u64,
    pub hook_timeout_ms: u64,
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
        && !matches!(value, "." | "..")
}
impl Registration {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1
            || !identifier(&self.id)
            || self.goal_id.as_deref().is_some_and(|id| !identifier(id))
        {
            return Err(Error::Invalid("Invalid hook registration"));
        }
        self.scope
            .validate()
            .map_err(|_| Error::Invalid("Invalid registered hook scope"))?;
        layouts::check_path(&self.directory).map_err(|_| Error::PrivateState)?;
        if self.events.is_empty() || self.events.len() > 12 {
            return Err(Error::Invalid("Select specific hook lifecycle events"));
        }
        for (index, event) in self.events.iter().enumerate() {
            if self.events[..index].contains(event) {
                return Err(Error::Invalid("Duplicate hook lifecycle event"));
            }
        }
        if self.scope.harness == Harness::Opencode {
            let server = self.server_emitter.as_ref().ok_or(Error::Invalid(
                "OpenCode requires an explicit local server emitter",
            ))?;
            if self.harness_process.is_none()
                || server.process.pid == 0
                || server.process.start_ticks == 0
            {
                return Err(Error::Invalid(
                    "OpenCode requires independent terminal and server process bindings",
                ));
            }
            layouts::check_path(&server.executable).map_err(|_| Error::PrivateState)?;
        } else if self.server_emitter.is_some() {
            return Err(Error::Invalid(
                "Server emitter is only supported for OpenCode",
            ));
        }
        if !matches!(
            self.scope.harness,
            Harness::Cursor | Harness::Pi | Harness::Opencode
        ) && self.events.contains(&EventKind::AssistantResponse)
        {
            return Err(Error::Invalid(
                "Assistant response hook is unavailable for this harness",
            ));
        }
        if (self.scope.harness != Harness::Pi && self.events.contains(&EventKind::Activity))
            || (!matches!(self.scope.harness, Harness::Pi | Harness::Grok)
                && self.events.contains(&EventKind::Settled))
        {
            return Err(Error::Invalid(
                "Activity requires Pi; settlement requires Pi or reviewed Grok idle notifications",
            ));
        }
        if self.scope.harness != Harness::Claude && self.events.contains(&EventKind::PreToolUse) {
            return Err(Error::Invalid(
                "Tool denial is not verified for this harness",
            ));
        }
        if self.goal_id.is_some()
            && !matches!(
                self.scope.harness,
                Harness::Claude | Harness::Codex | Harness::Cursor | Harness::Pi | Harness::Grok
            )
        {
            return Err(Error::Invalid(
                "Goal continuation is unavailable for this harness",
            ));
        }
        if self.activity_lease_ms == 0
            || self.activity_lease_ms > 86_400_000
            || self.pending_timeout_ms == 0
            || self.pending_timeout_ms > 86_400_000
            || self.hook_timeout_ms == 0
            || self.hook_timeout_ms > 12_000
        {
            return Err(Error::Invalid("Invalid hook timing limits"));
        }
        if let Some(path) = &self.transcript {
            layouts::check_path(path).map_err(|_| Error::PrivateState)?;
        }
        Ok(())
    }
    pub fn path(&self) -> PathBuf {
        self.directory
            .join("hooks")
            .join(format!("{}.json", self.id))
    }
}
#[cfg(unix)]
fn private_directory(path: &Path) -> Result<()> {
    layouts::check_path(path).map_err(|_| Error::PrivateState)?;
    let meta = fs::metadata(path).map_err(|_| Error::PrivateState)?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(Error::PrivateState);
    }
    Ok(())
}
#[cfg(windows)]
#[allow(unused_variables)]
fn private_directory(path: &Path) -> Result<()> {
    crate::platform::validate_private_dir(path).map_err(|_| Error::PrivateState)
}
fn active_directory(directory: &Path) -> Result<()> {
    match fs::symlink_metadata(directory.join("cutover-retired.json")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(Error::Invalid("Hook instance is retired or unavailable")),
    }
}
/// Held for the complete state mutation. Cutover takes the same stable inode
/// exclusively, then writes retirement before releasing its snapshot lock.
pub(crate) struct ActiveGuard {
    _file: File,
}
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        // A concurrent fork can briefly inherit this descriptor before exec.
        // Release the lock explicitly instead of waiting for every copy to close.
        #[cfg(unix)]
        unsafe {
            libc::flock(self._file.as_raw_fd(), libc::LOCK_UN);
        }
        #[cfg(windows)]
        let _ = crate::platform::unlock(&self._file);
    }
}
#[cfg(unix)]
pub(crate) fn active_guard(directory: &Path) -> Result<ActiveGuard> {
    private_directory(directory)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(directory.join(".cutover.lock"))
        .map_err(|_| Error::PrivateState)?;
    let meta = file.metadata().map_err(|_| Error::PrivateState)?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
    {
        return Err(Error::PrivateState);
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } != 0 {
        return Err(Error::Invalid("Instance cutover is in progress"));
    }
    let guard = ActiveGuard { _file: file };
    active_directory(directory)?;
    Ok(guard)
}
#[cfg(windows)]
#[allow(unused_variables)]
pub(crate) fn active_guard(directory: &Path) -> Result<ActiveGuard> {
    private_directory(directory)?;
    let file = crate::platform::open_private(&directory.join(".cutover.lock"), true, true)
        .map_err(|_| Error::PrivateState)?;
    crate::platform::lock_shared(&file, true)
        .map_err(|_| Error::Invalid("Instance cutover is in progress"))?;
    let guard = ActiveGuard { _file: file };
    active_directory(directory)?;
    Ok(guard)
}

#[cfg(unix)]
fn private_file(path: &Path) -> Result<File> {
    layouts::check_path(path).map_err(|_| Error::PrivateState)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| Error::PrivateState)?;
    let meta = file.metadata().map_err(|_| Error::PrivateState)?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.nlink() != 1
        || meta.mode() & 0o077 != 0
    {
        return Err(Error::PrivateState);
    }
    Ok(file)
}
#[cfg(windows)]
#[allow(unused_variables)]
fn private_file(path: &Path) -> Result<File> {
    crate::platform::open_private(path, false, false).map_err(|_| Error::PrivateState)
}
fn native_transcript(path: &Path) -> Result<File> {
    #[cfg(unix)]
    {
        private_file(path)
    }
    #[cfg(windows)]
    {
        crate::platform::open_native_profile(path, false, false).map_err(|_| Error::PrivateState)
    }
}
/// Explicit local setup action; never called by receiving a hook event.
#[cfg(unix)]
pub fn install(registration: &Registration) -> Result<PathBuf> {
    registration.validate()?;
    let _active = active_guard(&registration.directory)?;
    let path = registration.path();
    let bytes = serde_json::to_vec_pretty(registration)
        .map_err(|_| Error::Invalid("Invalid hook registration"))?;
    if path.exists() {
        let old = inspect(&path)?;
        if serde_json::to_value(old).ok() != serde_json::to_value(registration).ok() {
            return Err(Error::Invalid(
                "Hook registration already exists with different policy",
            ));
        }
        return Ok(path);
    }
    let parent = path.parent().ok_or(Error::PrivateState)?;
    match fs::DirBuilder::new().mode(0o700).create(parent) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(_) => return Err(Error::PrivateState),
    }
    private_directory(parent)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|_| Error::PrivateState)?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| Error::PrivateState)?;
    File::open(parent)
        .and_then(|d| d.sync_all())
        .map_err(|_| Error::PrivateState)?;
    Ok(path)
}
#[cfg(windows)]
#[allow(unused_variables)]
pub fn install(registration: &Registration) -> Result<PathBuf> {
    registration.validate()?;
    let _active = active_guard(&registration.directory)?;
    let path = registration.path();
    let bytes = serde_json::to_vec_pretty(registration).map_err(|_| Error::Input)?;
    if path.exists() {
        if serde_json::to_value(inspect(&path)?).ok() != serde_json::to_value(registration).ok() {
            return Err(Error::Invalid(
                "Hook registration already exists with different policy",
            ));
        }
        return Ok(path);
    }
    let parent = path.parent().ok_or(Error::PrivateState)?;
    crate::platform::ensure_private_dir(parent).map_err(|_| Error::PrivateState)?;
    let mut file = crate::platform::create_new_private(&path).map_err(|_| Error::PrivateState)?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| Error::PrivateState)?;
    Ok(path)
}
pub fn inspect(path: &Path) -> Result<Registration> {
    let file = private_file(path)?;
    if file.metadata().map_err(|_| Error::PrivateState)?.len() > 64 * 1024 {
        return Err(Error::Invalid("Hook registration exceeds size limit"));
    }
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::PrivateState)?;
    if bytes.len() > 64 * 1024 {
        return Err(Error::Invalid("Hook registration exceeds size limit"));
    }
    let registration: Registration = serde_json::from_slice(&bytes)
        .map_err(|_| Error::Invalid("Invalid hook registration JSON"))?;
    registration.validate()?;
    private_directory(&registration.directory)?;
    private_directory(path.parent().ok_or(Error::PrivateState)?)?;
    if registration.path() != path {
        return Err(Error::Invalid(
            "Hook registration location does not match its instance",
        ));
    }
    active_directory(&registration.directory)?;
    Ok(registration)
}
#[cfg(unix)]
fn private_connection(directory: &Path, filename: &str) -> Result<Connection> {
    private_directory(directory)?;
    let path = directory.join(filename);
    layouts::check_path(&path).map_err(|_| Error::PrivateState)?;
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
    {
        Ok(file) => file.sync_all().map_err(|_| Error::PrivateState)?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(_) => return Err(Error::PrivateState),
    };
    private_file(&path)?;
    let connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| Error::Persistence)?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|_| Error::Persistence)?;
    connection
        .execute_batch("PRAGMA synchronous=FULL;")
        .map_err(|_| Error::Persistence)?;
    Ok(connection)
}
#[cfg(windows)]
#[allow(unused_variables)]
fn private_connection(directory: &Path, filename: &str) -> Result<Connection> {
    private_directory(directory)?;
    let path = directory.join(filename);
    let file = crate::platform::open_private(&path, true, true).map_err(|_| Error::PrivateState)?;
    file.sync_all().map_err(|_| Error::PrivateState)?;
    let connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| Error::Persistence)?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|_| Error::Persistence)?;
    connection
        .execute_batch("PRAGMA synchronous=FULL;")
        .map_err(|_| Error::Persistence)?;
    Ok(connection)
}
fn open_store(directory: &Path) -> Result<Store> {
    Store::attach_connection(private_connection(directory, "goals.sqlite3")?)
        .map_err(|_| Error::Persistence)
}
/// Injected verifier is part of the trusted application, never selected by stdin.
pub trait Verifier {
    fn verify(&mut self, registration: &Registration) -> Result<()>;
    /// Security hooks must have an actual policy; recording a plan is not approval.
    fn authorize_tool(&mut self, _event: &Event, _payload: &Value) -> Result<()> {
        Err(Error::UnsupportedPolicy)
    }
}
#[derive(Default)]
pub struct NativeVerifier {
    pub terminals: Terminals,
}
#[cfg(unix)]
impl Verifier for NativeVerifier {
    fn verify(&mut self, registration: &Registration) -> Result<()> {
        let actual = verify_live_session(registration, &self.terminals)?;
        let mut terminal_pid = actual
            .get("pid")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .filter(|pid| *pid > 0)
            .ok_or(Error::Identity)?;
        if let Some(process) = &registration.harness_process {
            verify_process(process, &self.terminals.proc_root)?;
        }
        let mut emitter_seen = registration.harness_process.is_none();
        if let Some(server) = &registration.server_emitter {
            verify_server_emitter(server, &self.terminals.proc_root)?;
            terminal_pid = u64::from(server.process.pid);
            emitter_seen = true;
        }
        let mut pid = std::process::id();
        for _ in 0..128 {
            if registration
                .harness_process
                .as_ref()
                .is_some_and(|p| p.pid == pid)
            {
                emitter_seen = true;
            }
            if u64::from(pid) == terminal_pid {
                return if emitter_seen {
                    Ok(())
                } else {
                    Err(Error::Identity)
                };
            }
            let path = self.terminals.proc_root.join(pid.to_string()).join("stat");
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
                .map_err(|_| Error::Identity)?;
            let mut bytes = Vec::new();
            file.take(8193)
                .read_to_end(&mut bytes)
                .map_err(|_| Error::Identity)?;
            if bytes.len() > 8192 {
                return Err(Error::Identity);
            }
            let text = std::str::from_utf8(&bytes).map_err(|_| Error::Identity)?;
            let parent = text
                .rsplit_once(')')
                .and_then(|(_, s)| s.split_whitespace().nth(1))
                .and_then(|s| s.parse::<u32>().ok())
                .ok_or(Error::Identity)?;
            if parent == 0 || parent == pid {
                break;
            }
            pid = parent;
        }
        Err(Error::Identity)
    }
}
pub trait Clock {
    fn now_ms(&self) -> u64;
    fn wait_ms(&mut self, ms: u64);
}
pub struct SystemClock;
impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|v| v.as_millis().min(u128::from(u64::MAX)) as u64)
            .unwrap_or(0)
    }
    fn wait_ms(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}
fn transcript_boundary(registration: &Registration) -> Result<String> {
    transcript_boundary_for(registration, None)
}
fn assistant_text(value: &Value) -> Option<String> {
    let content = value.pointer("/message/content");
    content
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            content
                .and_then(Value::as_array)
                .filter(|blocks| blocks.iter().all(|b| b["type"] == "text"))
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|b| b["text"].as_str())
                        .collect::<String>()
                })
        })
}
/// Claude Code can run Stop before its transcript writer flushes the final
/// assistant row. Wait a bounded time for that row so the event key and the
/// reply proof read the same evidence; after the bound, Stop proceeds unproven.
fn await_final_row(registration: &Registration, payload: &Value, clock: &mut impl Clock) {
    const POLL_MS: u64 = 50;
    const POLLS: u64 = 40;
    let Some(text) = payload
        .get("last_assistant_message")
        .and_then(Value::as_str)
    else {
        return;
    };
    if registration.transcript.is_none() {
        return;
    }
    for _ in 0..POLLS {
        if transcript_boundary_for(registration, Some(text)).is_ok() {
            return;
        }
        clock.wait_ms(POLL_MS);
    }
}
fn transcript_boundary_for(registration: &Registration, expected: Option<&str>) -> Result<String> {
    if registration.scope.harness != Harness::Claude {
        return Err(Error::EventIdentity);
    }
    let path = registration
        .transcript
        .as_ref()
        .ok_or(Error::EventIdentity)?;
    let mut file = native_transcript(path)?;
    let before = file.metadata().map_err(|_| Error::PrivateState)?;
    let start = before.len().saturating_sub(8 * 1024 * 1024);
    file.seek(SeekFrom::Start(start))
        .map_err(|_| Error::PrivateState)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(before.len() - start)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::PrivateState)?;
    if file.metadata().map_err(|_| Error::PrivateState)?.len() != before.len() {
        return Err(Error::EventIdentity);
    }
    let mut lines = bytes.split(|b| *b == b'\n');
    if start > 0 {
        lines.next();
    }
    let mut latest = None;
    for line in lines {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let sid = value
            .get("sessionId")
            .or_else(|| value.get("session_id"))
            .and_then(Value::as_str);
        if sid != Some(registration.scope.conversation.as_str()) {
            continue;
        }
        if value.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        if expected.is_some_and(|text| assistant_text(&value).as_deref() != Some(text)) {
            latest = None;
            continue;
        }
        if let Some(uuid) = value
            .get("uuid")
            .and_then(Value::as_str)
            .filter(|uuid| identifier(uuid))
        {
            latest = Some(uuid.to_owned());
        }
    }
    latest.ok_or(Error::EventIdentity)
}
/// OpenCode's canonical context must end at the durable success event's idle
/// projection. The latest user must precede that marker with no newer boundary.
/// Missing/compacted context and volatile notifications are not completion proof.
fn opencode_completion(payload: &Value) -> Option<(&str, &str)> {
    let session = payload.get("session_id")?.as_str()?;
    let event = payload.get("native_event")?;
    let id = event.get("id")?.as_str()?;
    let suffix = id.strip_prefix("evt_")?.trim();
    if suffix.is_empty()
        || !identifier(id)
        || event.get("type")?.as_str()? != "session.execution.succeeded"
        || event.get("data")?.get("sessionID")?.as_str()? != session
        || event.get("durable")?.get("aggregateID")?.as_str()? != session
        || event.get("durable")?.get("seq")?.as_u64()? == 0
        || event.get("durable")?.get("version")?.as_u64()? == 0
        || payload.get("event_id")?.as_str()? != id
    {
        return None;
    }
    let context = payload.get("context")?.as_array()?;
    if context.is_empty() || context.len() > 512 {
        return None;
    }
    let idle = context.last()?;
    if idle.get("type")?.as_str()? != "idle"
        || idle.get("outcome")?.as_str()? != "succeeded"
        || idle.get("id")?.as_str()? != format!("msg_{suffix}")
    {
        return None;
    }
    for message in context[..context.len() - 1].iter().rev() {
        match message.get("type")?.as_str()? {
            "idle" => return None,
            "user" => {
                let turn = message.get("id")?.as_str()?;
                if !turn.starts_with("msg_") || !identifier(turn) {
                    return None;
                }
                return Some((turn, message.get("text")?.as_str()?));
            }
            _ => (),
        }
    }
    None
}
/// Claude has no native turn ID in most hooks. Bind only through unique,
/// exact transcript ancestry; a delayed event must never inherit the latest turn.
fn claude_stream_turn(
    registration: &Registration,
    kind: EventKind,
    payload: &Value,
) -> Option<String> {
    let mut file = native_transcript(registration.transcript.as_deref()?).ok()?;
    let length = file.metadata().ok()?.len();
    let start = length.saturating_sub(1024 * 1024);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(length - start)
        .read_to_end(&mut bytes)
        .ok()?;
    if file.metadata().ok()?.len() != length {
        return None;
    }
    let mut lines = bytes.split(|b| *b == b'\n');
    if start > 0 {
        lines.next();
    }
    let mut rows = Vec::new();
    let mut ids = std::collections::BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let row: Value = serde_json::from_slice(line).ok()?;
        if row.get("sessionId").and_then(Value::as_str) != Some(&registration.scope.conversation) {
            continue;
        }
        let Some(id) = row
            .get("uuid")
            .and_then(Value::as_str)
            .filter(|s| identifier(s))
        else {
            continue;
        };
        if ids.insert(id.to_owned(), rows.len()).is_some() {
            return None;
        }
        rows.push(row);
    }
    let is_prompt = |row: &Value| {
        row["type"] == "user"
            && !row
                .pointer("/message/content")
                .and_then(Value::as_array)
                .is_some_and(|blocks| {
                    !blocks.is_empty() && blocks.iter().all(|b| b["type"] == "tool_result")
                })
    };
    let current = rows.iter().rposition(is_prompt)?;
    let prompt_text = assistant_text(&rows[current])?;
    // Repeated identical prompts have no unique event-to-transcript proof.
    if rows
        .iter()
        .filter(|row| is_prompt(row) && assistant_text(row).as_deref() == Some(&prompt_text))
        .count()
        != 1
    {
        return None;
    }
    let mut selected = match kind {
        EventKind::UserPromptSubmit => {
            if payload.get("prompt").and_then(Value::as_str) != Some(&prompt_text) {
                return None;
            }
            current
        }
        EventKind::Stop => {
            let text = payload.get("last_assistant_message")?.as_str()?;
            let matches: Vec<usize> = rows
                .iter()
                .enumerate()
                .filter(|(_, row)| {
                    row["type"] == "assistant" && assistant_text(row).as_deref() == Some(text)
                })
                .map(|(i, _)| i)
                .collect();
            if matches.len() != 1 {
                return None;
            }
            *matches.first()?
        }
        EventKind::PostToolUse => {
            let tool = payload.get("tool_use_id")?.as_str()?;
            let matches: Vec<usize> = rows
                .iter()
                .enumerate()
                .filter(|(_, row)| {
                    row["type"] == "assistant"
                        && row
                            .pointer("/message/content")
                            .and_then(Value::as_array)
                            .is_some_and(|blocks| {
                                blocks
                                    .iter()
                                    .any(|b| b["type"] == "tool_use" && b["id"] == tool)
                            })
                })
                .map(|(i, _)| i)
                .collect();
            if matches.len() != 1 {
                return None;
            }
            *matches.first()?
        }
        _ => return None,
    };
    for _ in 0..rows.len() {
        if selected < current {
            return None;
        }
        if selected == current {
            return rows[current].get("uuid")?.as_str().map(str::to_owned);
        }
        let parent = rows[selected].get("parentUuid")?.as_str()?;
        let next = *ids.get(parent)?;
        if next >= selected {
            return None;
        }
        selected = next;
    }
    None
}

fn native_turn(harness: Harness, payload: &Value) -> Option<&str> {
    if harness == Harness::Opencode && payload.get("hook_event_name")?.as_str()? == "Stop" {
        return opencode_completion(payload).map(|(turn, _)| turn);
    }
    let value = if harness == Harness::Hermes {
        &payload["extra"]
    } else {
        payload
    };
    value
        .get(if harness == Harness::Grok {
            "promptId"
        } else if harness == Harness::Cursor {
            "generation_id"
        } else {
            "turn_id"
        })
        .and_then(Value::as_str)
        .filter(|id| identifier(id))
}
fn native_prompt(harness: Harness, payload: &Value) -> Option<&str> {
    if harness == Harness::Hermes {
        payload["extra"].get("user_message").and_then(Value::as_str)
    } else {
        payload.get("prompt").and_then(Value::as_str)
    }
}
fn native_completed(harness: Harness, payload: &Value) -> bool {
    if harness == Harness::Grok {
        matches!(
            payload.get("hookEventName").and_then(Value::as_str),
            Some("notification" | "Notification")
        ) && payload.get("notificationType").and_then(Value::as_str) == Some("idle_prompt")
            && payload.get("subagentType").is_none_or(Value::is_null)
    } else if harness == Harness::Opencode {
        opencode_completion(payload).is_some()
    } else if harness == Harness::Pi {
        matches!(
            payload.get("status").and_then(Value::as_str),
            Some("completed" | "settled")
        )
    } else if harness == Harness::Cursor {
        payload.get("status").and_then(Value::as_str) == Some("completed")
    } else if harness == Harness::Hermes {
        payload["extra"]
            .get("assistant_response")
            .and_then(Value::as_str)
            .is_some()
    } else {
        payload
            .get("last_assistant_message")
            .and_then(Value::as_str)
            .is_some()
    }
}
fn event_key(
    registration: &Registration,
    event: &Event,
    payload: &Value,
    explicit: Option<&str>,
) -> Result<String> {
    let native = if let Some(id) = explicit {
        if !identifier(id) {
            return Err(Error::EventIdentity);
        }
        id.to_owned()
    } else if let Some(tool) = payload
        .get("tool_use_id")
        .and_then(Value::as_str)
        .filter(|id| identifier(id))
    {
        tool.to_owned()
    } else if let Some(id) = payload
        .get("event_id")
        .or_else(|| payload.get("uuid"))
        .and_then(Value::as_str)
        .filter(|id| identifier(id))
    {
        id.to_owned()
    } else if registration.scope.harness == Harness::Cursor
        && event.kind == EventKind::AssistantResponse
    {
        let turn = native_turn(registration.scope.harness, payload).ok_or(Error::EventIdentity)?;
        let text = payload
            .get("text")
            .and_then(Value::as_str)
            .ok_or(Error::Input)?;
        format!("{turn}:{:x}", Sha256::digest(text.as_bytes()))
    } else if matches!(
        registration.scope.harness,
        Harness::Codex | Harness::Cursor | Harness::Hermes | Harness::Pi
    ) && matches!(event.kind, EventKind::Stop | EventKind::UserPromptSubmit)
        && let Some(turn) = native_turn(registration.scope.harness, payload)
    {
        turn.to_owned()
    } else if registration.scope.harness == Harness::Grok
        && event.kind == EventKind::UserPromptSubmit
    {
        native_turn(Harness::Grok, payload)
            .ok_or(Error::EventIdentity)?
            .to_owned()
    } else if registration.scope.harness == Harness::Grok && event.kind == EventKind::Settled {
        let timestamp = payload
            .get("timestamp")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 128)
            .ok_or(Error::EventIdentity)?;
        format!(
            "idle:{:x}",
            Sha256::digest(serde_json::to_vec(&(timestamp, payload)).map_err(|_| Error::Input)?)
        )
    } else if event.kind == EventKind::Stop && registration.scope.harness == Harness::Claude {
        if let Some(final_text) = payload
            .get("last_assistant_message")
            .and_then(Value::as_str)
        {
            match transcript_boundary_for(registration, Some(final_text)) {
                Ok(id) => id,
                Err(_) => format!(
                    "{}-{:x}",
                    invocation_id()?,
                    Sha256::digest(serde_json::to_vec(payload).map_err(|_| Error::Input)?)
                ),
            }
        } else {
            transcript_boundary(registration)?
        }
    } else {
        format!(
            "{}-{:x}",
            invocation_id()?,
            Sha256::digest(serde_json::to_vec(payload).map_err(|_| Error::Input)?)
        )
    };
    let encoded = serde_json::to_vec(&(
        registration.id.as_str(),
        &registration.scope,
        event.kind,
        native,
    ))
    .map_err(|_| Error::Input)?;
    Ok(format!("hook:{:x}", Sha256::digest(encoded)))
}
/// Events without provider IDs are distinct hook invocations. This deduplicates
/// retries inside the same process, not a provider restarting the hook process.
/// Stop always requires a stable assistant boundary or explicit provider ID.
#[cfg(unix)]
fn invocation_id() -> Result<String> {
    let stat = fs::read_to_string("/proc/self/stat").map_err(|_| Error::EventIdentity)?;
    let start = stat
        .rsplit_once(')')
        .and_then(|(_, s)| s.split_whitespace().nth(19))
        .ok_or(Error::EventIdentity)?;
    let boot =
        fs::read_to_string("/proc/sys/kernel/random/boot_id").map_err(|_| Error::EventIdentity)?;
    Ok(format!(
        "invocation-{:x}",
        Sha256::digest(format!("{}:{}:{}", boot.trim(), std::process::id(), start))
    ))
}
#[cfg(windows)]
fn invocation_id() -> Result<String> {
    let process =
        crate::platform::process_identity(std::process::id()).map_err(|_| Error::EventIdentity)?;
    let encoded = serde_json::to_vec(&(process.owner, process.pid, process.creation_time))
        .map_err(|_| Error::EventIdentity)?;
    Ok(format!("invocation-{:x}", Sha256::digest(encoded)))
}
#[derive(Debug)]
pub struct Outcome {
    pub reply: Reply,
    pub event_id: String,
    pub planned_effects: Vec<String>,
    pub admitted: bool,
}
fn effect_name(effect: hooks::Effect) -> &'static str {
    use hooks::Effect::*;
    match effect {
        RegisterIdentity => "register_identity",
        RecordActivity => "record_activity",
        CheckToolAuthorization => "check_tool_authorization",
        StartProgress => "start_progress",
        UpdateProgress => "update_progress",
        FinishProgress => "finish_progress",
        QueueFinalReply => "queue_final_reply",
        QueueAssistantReply => "queue_assistant_reply",
        Notify => "notify",
        RecallMemory => "recall_memory",
        PersistMemory => "persist_memory",
    }
}

/// Complete one bounded hook invocation. Plans are durably admitted to the
/// journal's `hooks` inbox; no Telegram delivery/memory retention is claimed.
/// Only goal continuation is executed here. Normal harness approvals remain on.
pub fn execute(
    reader: impl Read,
    registration: &Registration,
    event_id: Option<&str>,
    verifier: &mut impl Verifier,
    clock: &mut impl Clock,
) -> Result<Outcome> {
    registration.validate()?;
    let _active = active_guard(&registration.directory)?;
    verifier.verify(registration)?;
    let bytes = hooks::read_payload(reader).map_err(|_| Error::Input)?;
    let event = hooks::parse(
        registration.scope.harness,
        &bytes,
        &registration.scope.binding().map_err(|_| Error::Identity)?,
    )
    .map_err(|_| Error::Input)?;
    if !registration.events.contains(&event.kind) {
        return Err(Error::Invalid(
            "Lifecycle event is not enabled in this registration",
        ));
    }
    let payload: Value = serde_json::from_slice(&bytes).map_err(|_| Error::Input)?;
    if matches!(registration.scope.harness, Harness::Cursor | Harness::Pi)
        && event.kind == EventKind::Stop
        && payload.get("status").and_then(Value::as_str) != Some("completed")
    {
        return Err(Error::Invalid("Stop is not a completed provider boundary"));
    }
    if event.kind == EventKind::PreToolUse {
        verifier.authorize_tool(&event, &payload)?;
    }
    // Codex can move the same conversation to a new rollout file on resume.
    // Its session ID and pinned process/terminal establish the hook identity;
    // a rollout path is only a hint and was never required by the parser.
    if registration.scope.harness != Harness::Codex
        && let Some(hint) = &event.transcript_path
        && registration.transcript.as_deref() != Some(Path::new(hint))
    {
        return Err(Error::Invalid(
            "Hook transcript does not match its registration",
        ));
    }
    if event.kind == EventKind::Stop && registration.scope.harness == Harness::Claude {
        await_final_row(registration, &payload, clock);
    }
    let key = event_key(registration, &event, &payload, event_id)?;
    let effects = hooks::dispatch(&event, registration.options.features())
        .into_iter()
        .map(|e| effect_name(e).to_owned())
        .collect::<Vec<_>>();
    let stream_prompt_sha256 = (event.kind == EventKind::UserPromptSubmit)
        .then(|| native_prompt(registration.scope.harness, &payload))
        .flatten()
        .map(|prompt| format!("{:x}", Sha256::digest(prompt.as_bytes())));
    let stream_turn_id = if payload.get("subagentType").is_some_and(|v| !v.is_null()) {
        None
    } else if registration.scope.harness == Harness::Claude {
        claude_stream_turn(registration, event.kind, &payload)
    } else {
        native_turn(registration.scope.harness, &payload).map(str::to_owned)
    };
    let stream_evidence = if stream_turn_id.is_some() {
        "verified_event_turn"
    } else if registration.scope.harness == Harness::Claude {
        "missing_or_ambiguous_transcript_ancestry"
    } else {
        "missing_native_turn_identity"
    };
    let plan = json!({"stream_evidence":stream_evidence,"stream_prompt_sha256":stream_prompt_sha256,"stream_turn_id":stream_turn_id,"registration_id":registration.id,"final_message":if matches!(registration.scope.harness,Harness::Cursor|Harness::Pi|Harness::Opencode) && event.kind==EventKind::AssistantResponse {payload.get("text").and_then(Value::as_str)}else if registration.scope.harness==Harness::Grok {payload.get("lastAssistantMessage").and_then(Value::as_str)}else if registration.scope.harness==Harness::Hermes {payload["extra"].get("assistant_response").and_then(Value::as_str)}else{payload.get("last_assistant_message").and_then(Value::as_str)},"scope":registration.scope,"event":event.kind,"transcript":registration.transcript,"tool_name":event.tool_name,"effects":effects,"payload_sha256":format!("{:x}",Sha256::digest(serde_json::to_vec(&payload).map_err(|_|Error::Input)?)),"disposition":"planned_not_delivered"});
    let now = clock.now_ms();
    let mut journal = Journal::open(&registration.directory.join("delivery.sqlite3"))
        .map_err(|_| Error::Persistence)?;
    let admitted = journal
        .admit(
            "hooks",
            &registration.scope.instance,
            &key,
            &plan,
            now as f64 / 1000.0,
        )
        .map_err(|_| Error::Persistence)?;
    observe_readiness(registration, &key, &payload, event.kind, now, None)?;
    let Some(goal_id) = &registration.goal_id else {
        observe_readiness(
            registration,
            &key,
            &payload,
            event.kind,
            now,
            Some(&Reply::Empty),
        )?;
        return Ok(Outcome {
            reply: Reply::Empty,
            event_id: key,
            planned_effects: effects,
            admitted,
        });
    };
    let mut store = open_store(&registration.directory)?;
    // A configured but absent goal is an explicit setup error, never implicitly armed.
    store
        .get(goal_id, &registration.scope)
        .map_err(|_| Error::GoalPolicy)?;
    if matches!(
        event.kind,
        EventKind::UserPromptSubmit
            | EventKind::PostToolUse
            | EventKind::Activity
            | EventKind::Stop
    ) {
        store
            .observe_activity(
                goal_id,
                &registration.scope,
                &key,
                &bytes,
                now,
                registration.activity_lease_ms,
                registration.pending_timeout_ms,
            )
            .map_err(|_| Error::GoalPolicy)?;
    }
    if event.kind != EventKind::Stop
        || (registration.scope.harness == Harness::Grok && !event.grok_turn_gate)
    {
        if event.kind == EventKind::Settled {
            observe_readiness(
                registration,
                &key,
                &payload,
                event.kind,
                now,
                Some(&Reply::Empty),
            )?;
        }
        return Ok(Outcome {
            reply: Reply::Empty,
            event_id: key,
            planned_effects: effects,
            admitted,
        });
    }
    let deadline = now.saturating_add(registration.hook_timeout_ms);
    loop {
        verifier.verify(registration)?;
        let now = clock.now_ms();
        let receipt = store
            .stop(
                goal_id,
                &registration.scope,
                &key,
                &bytes,
                now,
                registration.pending_timeout_ms,
            )
            .map_err(|_| Error::GoalPolicy)?;
        match receipt.reply {
            Reply::WaitUntil(until) => {
                if until > deadline || now >= deadline {
                    return Err(Error::Deadline);
                }
                clock.wait_ms(until.saturating_sub(now).min(250));
            }
            reply => {
                observe_readiness(registration, &key, &payload, event.kind, now, Some(&reply))?;
                return Ok(Outcome {
                    reply,
                    event_id: key,
                    planned_effects: effects,
                    admitted,
                });
            }
        }
    }
}
pub fn record_run_result(
    path: &Path,
    event: Option<EventKind>,
    error: Option<&Error>,
) -> Result<()> {
    let registration = inspect(path)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Input)?
        .as_millis();
    let value = json!({"state":if error.is_some(){"failed"}else{"ok"},"event":event,"observed_at_ms":now,"error":error.map(ToString::to_string)});
    let event_name = serde_json::to_value(event)
        .map_err(|_| Error::Output)?
        .as_str()
        .unwrap_or("unspecified")
        .to_owned();
    layouts::write_private(
        &registration
            .directory
            .join("hook-status")
            .join(format!("{}-{event_name}.json", registration.id)),
        &serde_json::to_vec(&value).map_err(|_| Error::Output)?,
    )
    .map_err(|_| Error::Persistence)
}

/// CLI integration: registration path and optional stable event ID are explicit
/// arguments. Stdout contains only the harness hook response, never status logs.
pub fn run(
    reader: impl Read,
    writer: &mut impl Write,
    registration_path: &Path,
    event_id: Option<&str>,
) -> Result<Outcome> {
    run_expected(reader, writer, registration_path, event_id, None)
}
/// Expected event is local installation policy; stdin cannot change it. The CLI
/// must use blocking exit status only for expected PreToolUse errors, never Stop.
pub fn run_expected(
    reader: impl Read,
    writer: &mut impl Write,
    registration_path: &Path,
    event_id: Option<&str>,
    expected: Option<EventKind>,
) -> Result<Outcome> {
    let registration = inspect(registration_path)?;
    run_verified(
        reader,
        writer,
        &registration,
        event_id,
        expected,
        &mut NativeVerifier::default(),
        &mut SystemClock,
    )
}
fn run_verified(
    reader: impl Read,
    writer: &mut impl Write,
    registration: &Registration,
    event_id: Option<&str>,
    expected: Option<EventKind>,
    verifier: &mut impl Verifier,
    clock: &mut impl Clock,
) -> Result<Outcome> {
    registration.validate()?;
    let started = std::time::Instant::now();
    let bytes = hooks::read_payload(reader).map_err(|_| Error::Input)?;
    let payload: Value = serde_json::from_slice(&bytes).map_err(|_| Error::Input)?;
    let kind =
        hooks::native_kind(registration.scope.harness, &payload).map_err(|_| Error::Input)?;
    if expected.is_some_and(|expected| {
        expected != kind
            && !(registration.scope.harness == Harness::Grok
                && expected == EventKind::Notification
                && kind == EventKind::Settled)
    }) {
        return Err(Error::Invalid(
            "Hook event does not match its installed command",
        ));
    }
    // Codex loads profile hooks for every conversation using that profile.
    // An event for another session has no authority over this registration;
    // acknowledge it without touching persistent state or reporting failure.
    if registration.scope.harness == Harness::Codex
        && matches!(
            hooks::parse(
                Harness::Codex,
                &bytes,
                &registration.scope.binding().map_err(|_| Error::Identity)?,
            ),
            Err(hooks::Error::IdentityMismatch)
        )
    {
        writer
            .write_all(b"{}\n")
            .and_then(|_| writer.flush())
            .map_err(|_| Error::Output)?;
        return Ok(Outcome {
            reply: Reply::Empty,
            event_id: "unbound-codex-session".into(),
            planned_effects: Vec::new(),
            admitted: false,
        });
    }
    let _active = active_guard(&registration.directory)?;
    let mut outcome = match execute(bytes.as_slice(), registration, event_id, verifier, clock) {
        Ok(outcome) => outcome,
        Err(Error::UnsupportedPolicy)
            if registration.scope.harness == Harness::Claude && kind == EventKind::PreToolUse =>
        {
            Outcome {
                reply: Reply::Json(
                    json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"Native tool authorization policy is not configured"}}),
                ),
                event_id: format!("denied:{}", invocation_id()?),
                planned_effects: Vec::new(),
                admitted: false,
            }
        }
        Err(error) => return Err(error),
    };
    if registration.options.hindsight
        && let Some(memory_kind) = memory_event(registration.scope.harness, kind, &payload)
    {
        let remaining =
            Duration::from_millis(registration.hook_timeout_ms).saturating_sub(started.elapsed());
        match super::memory::hook(
            registration,
            memory_kind,
            &payload,
            &outcome.event_id,
            remaining,
        ) {
            Ok(Some(context)) => {
                if matches!(outcome.reply, Reply::Empty)
                    && let Some(value) = memory_context(registration.scope.harness, kind, &context)
                {
                    outcome.reply = Reply::Json(value);
                }
            }
            Ok(None) => (),
            Err(_) => {
                eprintln!("Daedalus optional memory hook failed; inspect private memory receipts")
            }
        }
    }
    if matches!(
        registration.scope.harness,
        Harness::Codex | Harness::Cursor | Harness::Hermes | Harness::Pi
    ) && matches!(outcome.reply, Reply::Empty)
    {
        let empty: &[u8] = if registration.scope.harness == Harness::Cursor
            && kind == EventKind::UserPromptSubmit
        {
            b"{\"continue\":true}\n"
        } else {
            b"{}\n"
        };
        writer
            .write_all(empty)
            .and_then(|_| writer.flush())
            .map_err(|_| Error::Output)?;
    }
    if let Reply::Json(value) = &outcome.reply {
        serde_json::to_writer(&mut *writer, value).map_err(|_| Error::Output)?;
        writer
            .write_all(b"\n")
            .and_then(|_| writer.flush())
            .map_err(|_| Error::Output)?;
    }
    if matches!(outcome.reply, Reply::Blocked(_)) {
        return Err(Error::GoalPolicy);
    }
    Ok(outcome)
}
// Match only provider events with a verified context consumer or assistant text.
fn memory_event(harness: Harness, kind: EventKind, payload: &Value) -> Option<EventKind> {
    if (kind == EventKind::SessionStart
        && matches!(
            harness,
            Harness::Claude | Harness::Codex | Harness::Cursor | Harness::Pi | Harness::Opencode
        ))
        || (kind == EventKind::UserPromptSubmit && harness == Harness::Hermes)
    {
        return Some(EventKind::SessionStart);
    }
    // Grok's native Stop payload uses camelCase. Do not accept guessed aliases
    // or lifecycle events that do not carry a verified assistant response.
    if harness == Harness::Grok {
        return (kind == EventKind::Stop
            && payload
                .get("lastAssistantMessage")
                .and_then(Value::as_str)
                .is_some_and(|text| !text.trim().is_empty()))
        .then_some(EventKind::Stop);
    }
    if matches!(
        kind,
        EventKind::Stop
            | EventKind::AssistantResponse
            | EventKind::PreCompact
            | EventKind::SessionEnd
    ) && payload
        .get("last_assistant_message")
        .or_else(|| payload.get("text"))
        .or_else(|| payload["extra"].get("assistant_response"))
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty())
    {
        return Some(kind);
    }
    None
}
fn memory_context(harness: Harness, kind: EventKind, text: &str) -> Option<Value> {
    match (harness, kind) {
        (Harness::Claude | Harness::Codex, EventKind::SessionStart) => Some(
            json!({"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":text}}),
        ),
        (Harness::Cursor | Harness::Pi | Harness::Opencode, EventKind::SessionStart) => {
            Some(json!({"additional_context":text}))
        }
        (Harness::Hermes, EventKind::UserPromptSubmit) => Some(json!({"context":text})),
        _ => None,
    }
}
/// Explicit local goal controls for setup/CLI. Does not launch a harness.
pub fn arm(registration_path: &Path, directive: &str, now: u64) -> Result<()> {
    let registration = inspect(registration_path)?;
    let _active = active_guard(&registration.directory)?;
    let id = registration
        .goal_id
        .as_deref()
        .ok_or(Error::Invalid("Registration has no goal ID"))?;
    open_store(&registration.directory)?
        .arm(id, &registration.scope, directive, now)
        .map_err(|_| Error::GoalPolicy)?;
    Ok(())
}
pub fn control(registration_path: &Path, action: Control, now: u64) -> Result<()> {
    let registration = inspect(registration_path)?;
    let _active = active_guard(&registration.directory)?;
    let id = registration
        .goal_id
        .as_deref()
        .ok_or(Error::Invalid("Registration has no goal ID"))?;
    open_store(&registration.directory)?
        .control(id, &registration.scope, action, now)
        .map_err(|_| Error::GoalPolicy)?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::core::{
        goals::Status,
        identity::{Backend, TerminalId},
    };
    use std::{
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    #[test]
    fn native_conversation_enrichment_requires_unique_exact_live_evidence() {
        let scope = Scope {
            instance: "example".into(),
            terminal: TerminalId {
                backend: Backend::Native,
                endpoint: "pty-1".into(),
                pane: "pty-1".into(),
                generation: "generation-1".into(),
            },
            conversation: "conversation-1".into(),
            harness: Harness::Claude,
        };
        let native = json!({"backend":"native","server":"pty-1","pane_id":"pty-1","terminal_id":"generation-1","agent_session":{"kind":"id","value":"stale"}});
        let external = json!({"backend":"herdr","agent_session":{"kind":"id","value":"untouched"}});
        let mut snapshot = json!({"panes":[native,external]});
        assert_eq!(
            apply_native_sessions(&mut snapshot, std::slice::from_ref(&scope)),
            1
        );
        assert_eq!(
            snapshot["panes"][0]["agent_session"]["value"],
            "conversation-1"
        );
        assert_eq!(snapshot["panes"][0]["agent"], "claude");
        assert_eq!(snapshot["panes"][1]["agent_session"]["value"], "untouched");
        let mut replaced = scope.clone();
        replaced.terminal.generation = "replacement".into();
        assert_eq!(apply_native_sessions(&mut snapshot, &[replaced]), 0);
        assert!(snapshot["panes"][0]["agent_session"].is_null());
        assert_eq!(
            apply_native_sessions(&mut snapshot, &[scope.clone(), scope]),
            0
        );
        assert!(snapshot["panes"][0]["agent_session"].is_null());
    }

    #[test]
    fn native_enrichment_does_not_trust_a_saved_unbound_registration() {
        let mut fixture = Fixture::new();
        fixture.0.scope.terminal = TerminalId {
            backend: Backend::Native,
            endpoint: "a".repeat(64),
            pane: "a".repeat(64),
            generation: "b".repeat(64),
        };
        install(&fixture.0).unwrap();
        let terminal = &fixture.0.scope.terminal;
        let mut snapshot = json!({"panes":[{
            "backend":"native", "server":terminal.endpoint, "pane_id":terminal.pane,
            "terminal_id":terminal.generation, "agent_session":{"kind":"id","value":"unverified"}
        }]});
        assert_eq!(
            enrich_native_sessions(&fixture.0.directory, &mut snapshot, &Terminals::default())
                .unwrap(),
            0
        );
        assert!(snapshot["panes"][0]["agent_session"].is_null());
    }

    struct Fixture(Registration);
    impl Fixture {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "daedalus-hook-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&directory).unwrap();
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
            Self(Registration {
                version: 1,
                id: "hook".into(),
                directory,
                scope: Scope {
                    instance: "example".into(),
                    terminal: TerminalId {
                        backend: Backend::Herdr,
                        endpoint: "example".into(),
                        pane: "pane".into(),
                        generation: "terminal".into(),
                    },
                    conversation: "session".into(),
                    harness: Harness::Claude,
                },
                goal_id: Some("goal".into()),
                harness_process: None,
                server_emitter: None,
                transcript: None,
                events: vec![
                    EventKind::Stop,
                    EventKind::PostToolUse,
                    EventKind::PreToolUse,
                ],
                options: Options::default(),
                activity_lease_ms: 30000,
                pending_timeout_ms: 30000,
                hook_timeout_ms: 12000,
            })
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0.directory);
        }
    }
    #[test]
    fn hook_status_preserves_failure_when_another_event_succeeds() {
        let f = Fixture::new();
        let path = install(&f.0).unwrap();
        record_run_result(&path, Some(EventKind::Stop), Some(&Error::Input)).unwrap();
        record_run_result(&path, Some(EventKind::PostToolUse), None).unwrap();
        let failed: Value = serde_json::from_slice(
            &fs::read(f.0.directory.join("hook-status/hook-Stop.json")).unwrap(),
        )
        .unwrap();
        let passed: Value = serde_json::from_slice(
            &fs::read(f.0.directory.join("hook-status/hook-PostToolUse.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(failed["state"], "failed");
        assert_eq!(passed["state"], "ok");
        assert_eq!(failed["error"], Error::Input.to_string());
        assert_eq!(failed.as_object().unwrap().len(), 4);
    }
    struct Verified;
    impl Verifier for Verified {
        fn verify(&mut self, _: &Registration) -> Result<()> {
            Ok(())
        }
    }
    struct FakeClock(u64);
    impl Clock for FakeClock {
        fn now_ms(&self) -> u64 {
            self.0
        }
        fn wait_ms(&mut self, ms: u64) {
            self.0 += ms;
        }
    }
    fn input(event: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({"session_id":"session","hook_event_name":event})).unwrap()
    }
    fn invoke(r: &Registration, event: &str, id: &str, c: &mut FakeClock) -> Result<Outcome> {
        execute(input(event).as_slice(), r, Some(id), &mut Verified, c)
    }
    #[test]
    fn standalone_attach_preserves_pending_and_pause_overrides_reply() {
        let f = Fixture::new();
        let p = install(&f.0).unwrap();
        assert_eq!(install(&f.0).unwrap(), p);
        arm(&p, "Finish verified work", 0).unwrap();
        let mut c = FakeClock(0);
        let first = invoke(&f.0, "Stop", "first", &mut c).unwrap();
        assert!(matches!(first.reply, Reply::Json(_)));
        assert!(matches!(
            open_store(&f.0.directory)
                .unwrap()
                .get("goal", &f.0.scope)
                .unwrap()
                .goal
                .status,
            Status::Pending { .. }
        ));
        let duplicate = invoke(&f.0, "Stop", "first", &mut c).unwrap();
        assert!(!duplicate.admitted);
        assert_eq!(first.reply, duplicate.reply);
        control(&p, Control::Pause, 1).unwrap();
        assert_eq!(
            invoke(&f.0, "Stop", "first", &mut c).unwrap().reply,
            Reply::Empty
        );
    }
    #[test]
    fn successive_turns_wait_cooldown_and_old_observation_cannot_ack_new_turn() {
        let f = Fixture::new();
        let p = install(&f.0).unwrap();
        arm(&p, "Continue", 0).unwrap();
        let mut c = FakeClock(0);
        invoke(&f.0, "Stop", "one", &mut c).unwrap();
        c.0 = 100;
        invoke(&f.0, "PostToolUse", "tool", &mut c).unwrap();
        assert!(matches!(
            open_store(&f.0.directory)
                .unwrap()
                .get("goal", &f.0.scope)
                .unwrap()
                .goal
                .status,
            Status::Running { .. }
        ));
        assert!(matches!(
            invoke(&f.0, "Stop", "two", &mut c).unwrap().reply,
            Reply::Json(_)
        ));
        assert_eq!(c.0, 8000);
        invoke(&f.0, "PostToolUse", "tool", &mut c).unwrap();
        assert!(matches!(
            open_store(&f.0.directory)
                .unwrap()
                .get("goal", &f.0.scope)
                .unwrap()
                .goal
                .status,
            Status::Pending { .. }
        ));
    }
    fn opencode_stop(turn: &str, text: &str, event: &str) -> Value {
        json!({"session_id":"session","hook_event_name":"Stop","event_id":event,
            "native_event":{"id":event,"type":"session.execution.succeeded","data":{"sessionID":"session"},
                "durable":{"aggregateID":"session","seq":10,"version":1}},
            "context":[{"id":turn,"type":"user","text":text},{"id":"msg_answer","type":"assistant"},
                {"id":event.replacen("evt_","msg_",1),"type":"idle","outcome":"succeeded"}]})
    }
    #[test]
    fn opencode_canonical_completion_rejects_stale_missing_or_foreign_boundaries() {
        let proof = opencode_stop("msg_prompt", "hello", "evt_completed");
        assert_eq!(opencode_completion(&proof), Some(("msg_prompt", "hello")));
        for replacement in [
            json!({"id":"msg_new","type":"user","text":"newer"}),
            json!({"id":"msg_other","type":"idle","outcome":"succeeded"}),
            json!({"id":"msg_completed","type":"idle","outcome":"interrupted"}),
        ] {
            let mut invalid = proof.clone();
            invalid["context"][2] = replacement;
            assert_eq!(opencode_completion(&invalid), None);
        }
        let mut foreign = proof.clone();
        foreign["native_event"]["durable"]["aggregateID"] = "other".into();
        assert_eq!(opencode_completion(&foreign), None);
        let mut volatile = proof.clone();
        volatile["native_event"]
            .as_object_mut()
            .unwrap()
            .remove("durable");
        assert_eq!(opencode_completion(&volatile), None);
        let mut compacted = proof.clone();
        compacted["context"].as_array_mut().unwrap().remove(0);
        assert_eq!(opencode_completion(&compacted), None);
    }
    #[test]
    fn opencode_readiness_releases_only_current_prompt_and_exact_reserved_text() {
        let mut f = Fixture::new();
        f.0.scope.harness = Harness::Opencode;
        f.0.goal_id = None;
        let observe = |payload: &Value, kind| {
            observe_readiness(
                &f.0,
                payload["event_id"].as_str().unwrap(),
                payload,
                kind,
                1,
                Some(&Reply::Empty),
            )
            .unwrap()
        };
        let state = || -> Readiness {
            let con = readiness_connection(&f.0.directory).unwrap();
            let value: String = con
                .query_row(
                    "SELECT state FROM hook_observations ORDER BY seq DESC LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            serde_json::from_str(&value).unwrap()
        };
        observe(
            &json!({"session_id":"session","hook_event_name":"UserPromptSubmit","event_id":"prompt-1","turn_id":"msg_prompt1","prompt":"first"}),
            EventKind::UserPromptSubmit,
        );
        let old_stop = opencode_stop("msg_prompt1", "first", "evt_first");
        observe(&old_stop, EventKind::Stop);
        assert_eq!(state(), Readiness::Idle);
        let scope = serde_json::to_string(&f.0.scope).unwrap();
        let con = readiness_connection(&f.0.directory).unwrap();
        con.execute(
            "INSERT INTO hook_reservations(scope,correlation) VALUES(?,?)",
            params![scope, format!("{:x}", Sha256::digest(b"reserved prompt"))],
        )
        .unwrap();
        observe(
            &json!({"session_id":"session","hook_event_name":"UserPromptSubmit","event_id":"prompt-2","turn_id":"msg_prompt2","prompt":"reserved prompt"}),
            EventKind::UserPromptSubmit,
        );
        assert_eq!(state(), Readiness::Busy);
        observe(&old_stop, EventKind::Stop);
        assert_eq!(
            state(),
            Readiness::Busy,
            "duplicate earlier Stop cannot advance latest observation"
        );
        observe(
            &opencode_stop("msg_prompt1", "first", "evt_delayed"),
            EventKind::Stop,
        );
        assert_eq!(state(), Readiness::Unknown);
        observe(
            &opencode_stop("msg_prompt2", "modified prompt", "evt_wrongtext"),
            EventKind::Stop,
        );
        assert_eq!(state(), Readiness::Busy);
        assert_eq!(
            con.query_row("SELECT count(*) FROM hook_reservations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        observe(
            &opencode_stop("msg_prompt2", "reserved prompt", "evt_second"),
            EventKind::Stop,
        );
        assert_eq!(state(), Readiness::Idle);
        assert_eq!(
            con.query_row("SELECT count(*) FROM hook_reservations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn wrong_identity_payload_and_unsupported_tool_policy_write_nothing() {
        let f = Fixture::new();
        let mut c = FakeClock(0);
        struct Denied;
        impl Verifier for Denied {
            fn verify(&mut self, _: &Registration) -> Result<()> {
                Err(Error::Identity)
            }
        }
        assert_eq!(
            execute(
                input("Stop").as_slice(),
                &f.0,
                Some("one"),
                &mut Denied,
                &mut c
            )
            .unwrap_err(),
            Error::Identity
        );
        assert_eq!(
            invoke(&f.0, "PreToolUse", "one", &mut c).unwrap_err(),
            Error::UnsupportedPolicy
        );
        assert!(
            execute(
                br#"{"session_id":"other","hook_event_name":"Stop"}"#.as_slice(),
                &f.0,
                Some("one"),
                &mut Verified,
                &mut c
            )
            .is_err()
        );
        assert!(!f.0.directory.join("goals.sqlite3").exists());
        assert!(!f.0.directory.join("delivery.sqlite3").exists());
    }
    #[test]
    fn transcript_requires_exact_session_and_stable_boundary() {
        let mut f = Fixture::new();
        let path = f.0.directory.join("transcript.jsonl");
        layouts::write_private(
            &path,
            b"{\"type\":\"assistant\",\"sessionId\":\"other\",\"uuid\":\"wrong\"}\n",
        )
        .unwrap();
        f.0.transcript = Some(path.clone());
        assert_eq!(transcript_boundary(&f.0).unwrap_err(), Error::EventIdentity);
        layouts::write_private(
            &path,
            b"{\"type\":\"assistant\",\"sessionId\":\"session\",\"uuid\":\"right\"}\n",
        )
        .unwrap();
        assert_eq!(transcript_boundary(&f.0).unwrap(), "right");
    }
    #[test]
    fn readiness_is_fresh_scoped_and_duplicates_do_not_make_it_idle() {
        let mut f = Fixture::new();
        f.0.goal_id = None;
        let p = install(&f.0).unwrap();
        let mut c = FakeClock(100);
        assert_eq!(readiness(&p, 100, 1000).unwrap(), Readiness::Unknown);
        invoke(&f.0, "Stop", "one", &mut c).unwrap();
        assert_eq!(readiness(&p, 100, 1000).unwrap(), Readiness::Idle);
        c.0 = 200;
        invoke(&f.0, "PostToolUse", "tool", &mut c).unwrap();
        invoke(&f.0, "Stop", "one", &mut c).unwrap();
        assert_eq!(readiness(&p, 200, 1000).unwrap(), Readiness::Busy);
        assert_eq!(readiness(&p, 1201, 1000).unwrap(), Readiness::Unknown);
        assert_eq!(readiness(&p, 0, 1000).unwrap(), Readiness::Unknown);
    }
    #[test]
    fn unfinished_stop_never_claims_idle_and_continuation_is_busy() {
        let f = Fixture::new();
        let p = install(&f.0).unwrap();
        arm(&p, "Continue", 0).unwrap();
        let payload = json!({"session_id":"session","hook_event_name":"Stop"});
        observe_readiness(&f.0, "crash", &payload, EventKind::Stop, 0, None).unwrap();
        assert_eq!(readiness(&p, 0, 1000).unwrap(), Readiness::Unknown);
        invoke(&f.0, "Stop", "one", &mut FakeClock(1)).unwrap();
        assert_eq!(readiness(&p, 1, 1000).unwrap(), Readiness::Busy);
    }
    #[test]
    fn persistent_idle_requires_process_incarnation_and_reservation_is_single_use() {
        let mut f = Fixture::new();
        f.0.goal_id = None;
        f.0.harness_process = Some(ProcessIdentity {
            pid: 42,
            start_ticks: 500,
        });
        let proc = f.0.directory.join("proc");
        fs::create_dir_all(proc.join("42")).unwrap();
        let mut fields = vec!["0"; 20];
        fields[0] = "S";
        fields[19] = "500";
        fields[2] = "42";
        fields[5] = "42";
        fields[4] = "99";
        fs::write(
            proc.join("42/stat"),
            format!("42 (fixture) {}", fields.join(" ")),
        )
        .unwrap();
        let p = install(&f.0).unwrap();
        invoke(&f.0, "Stop", "one", &mut FakeClock(0)).unwrap();
        let snapshot = ready_snapshot(&p, &proc).unwrap();
        assert_eq!(snapshot.state, Readiness::Idle);
        assert!(consume_ready_for_control(&p, &proc, snapshot.sequence).unwrap());
        assert!(!consume_ready_for_control(&p, &proc, snapshot.sequence).unwrap());
        assert_eq!(ready_snapshot(&p, &proc).unwrap().state, Readiness::Unknown);
        // Replayed old evidence cannot clear a control; a genuinely new hook can.
        invoke(&f.0, "Stop", "one", &mut FakeClock(0)).unwrap();
        assert_eq!(ready_snapshot(&p, &proc).unwrap().state, Readiness::Unknown);
        invoke(&f.0, "Stop", "after-control", &mut FakeClock(1)).unwrap();
        let snapshot = ready_snapshot(&p, &proc).unwrap();
        assert_eq!(snapshot.state, Readiness::Idle);
        assert!(reserve_ready(&p, &proc, snapshot.sequence).unwrap());
        assert!(!reserve_ready(&p, &proc, snapshot.sequence).unwrap());
        invoke(&f.0, "Stop", "one", &mut FakeClock(100)).unwrap();
        assert_eq!(ready_snapshot(&p, &proc).unwrap().state, Readiness::Busy);
        fields[19] = "501";
        fs::write(
            proc.join("42/stat"),
            format!("42 (fixture) {}", fields.join(" ")),
        )
        .unwrap();
        assert_eq!(ready_snapshot(&p, &proc).unwrap_err(), Error::Identity);
        assert_eq!(
            reserve_ready(&p, &proc, snapshot.sequence).unwrap_err(),
            Error::Identity
        );
    }
    #[test]
    fn reserved_claude_stop_waits_for_late_transcript_flush() {
        struct FlushingClock {
            now: u64,
            transcript: PathBuf,
            pending: Option<String>,
        }
        impl Clock for FlushingClock {
            fn now_ms(&self) -> u64 {
                self.now
            }
            fn wait_ms(&mut self, ms: u64) {
                self.now += ms;
                if let Some(row) = self.pending.take() {
                    let mut rows = fs::read_to_string(&self.transcript).unwrap();
                    rows.push_str(&row);
                    layouts::write_private(&self.transcript, rows.as_bytes()).unwrap();
                }
            }
        }
        let mut f = Fixture::new();
        f.0.goal_id = None;
        f.0.events.push(EventKind::UserPromptSubmit);
        f.0.harness_process = Some(ProcessIdentity {
            pid: 42,
            start_ticks: 500,
        });
        let proc = f.0.directory.join("proc");
        fs::create_dir_all(proc.join("42")).unwrap();
        let mut fields = vec!["0"; 20];
        fields[0] = "S";
        fields[19] = "500";
        fields[2] = "42";
        fields[5] = "42";
        fields[4] = "99";
        fs::write(
            proc.join("42/stat"),
            format!("42 (fixture) {}", fields.join(" ")),
        )
        .unwrap();
        let transcript = f.0.directory.join("transcript.jsonl");
        f.0.transcript = Some(transcript.clone());
        layouts::write_private(
            &transcript,
            format!(
                "{}\n",
                json!({"type":"user","sessionId":"session","uuid":"user","message":{"content":"exact reserved prompt"}})
            )
            .as_bytes(),
        )
        .unwrap();
        let p = install(&f.0).unwrap();
        invoke(&f.0, "Stop", "boot", &mut FakeClock(0)).unwrap();
        let snapshot = ready_snapshot(&p, &proc).unwrap();
        assert_eq!(snapshot.state, Readiness::Idle);
        assert!(reserve_ready_for(&p, &proc, snapshot.sequence, "exact reserved prompt").unwrap());
        let prompt = serde_json::to_vec(&json!({"session_id":"session","hook_event_name":"UserPromptSubmit","prompt":"exact reserved prompt"})).unwrap();
        execute(
            prompt.as_slice(),
            &f.0,
            Some("prompt"),
            &mut Verified,
            &mut FakeClock(1),
        )
        .unwrap();
        assert_eq!(ready_snapshot(&p, &proc).unwrap().state, Readiness::Busy);
        // Claude Code may run Stop before its writer flushes the final row.
        let stop = serde_json::to_vec(&json!({"session_id":"session","hook_event_name":"Stop","last_assistant_message":"done"})).unwrap();
        let mut clock = FlushingClock {
            now: 2,
            transcript: transcript.clone(),
            pending: Some(format!(
                "{}\n",
                json!({"type":"assistant","sessionId":"session","uuid":"answer","parentUuid":"user","message":{"content":[{"type":"text","text":"done"}],"stop_reason":null}})
            )),
        };
        execute(stop.as_slice(), &f.0, None, &mut Verified, &mut clock).unwrap();
        assert!(clock.pending.is_none());
        assert_eq!(ready_snapshot(&p, &proc).unwrap().state, Readiness::Idle);
    }
    #[test]
    fn memory_context_is_provider_specific_and_cannot_continue_stop() {
        assert_eq!(
            memory_context(Harness::Claude, EventKind::SessionStart, "reference").unwrap()["hookSpecificOutput"]
                ["additionalContext"],
            "reference"
        );
        assert_eq!(
            memory_context(Harness::Codex, EventKind::SessionStart, "reference").unwrap()["hookSpecificOutput"]
                ["hookEventName"],
            "SessionStart"
        );
        assert_eq!(
            memory_context(Harness::Cursor, EventKind::SessionStart, "reference").unwrap()["additional_context"],
            "reference"
        );
        assert_eq!(
            memory_context(Harness::Hermes, EventKind::UserPromptSubmit, "reference").unwrap()["context"],
            "reference"
        );
        for harness in [
            Harness::Claude,
            Harness::Codex,
            Harness::Cursor,
            Harness::Grok,
            Harness::Pi,
            Harness::Hermes,
            Harness::Opencode,
        ] {
            assert!(memory_context(harness, EventKind::Stop, "never continue").is_none());
        }
        assert_eq!(
            memory_event(Harness::Hermes, EventKind::UserPromptSubmit, &json!({})),
            Some(EventKind::SessionStart)
        );
        assert!(memory_event(Harness::Grok, EventKind::SessionStart, &json!({})).is_none());
        assert!(memory_event(Harness::Claude, EventKind::SessionEnd, &json!({})).is_none());
    }
    #[test]
    fn grok_memory_dispatch_requires_native_stop_assistant_text() {
        let native = json!({"lastAssistantMessage":"verified native result"});
        assert_eq!(
            memory_event(Harness::Grok, EventKind::Stop, &native),
            Some(EventKind::Stop)
        );
        for payload in [
            json!({}),
            json!({"lastAssistantMessage":"  "}),
            json!({"lastAssistantMessage":42}),
            json!({"last_assistant_message":"wrong alias"}),
            json!({"text":"wrong alias"}),
            json!({"extra":{"assistant_response":"wrong provider"}}),
        ] {
            assert!(memory_event(Harness::Grok, EventKind::Stop, &payload).is_none());
        }
        for kind in [
            EventKind::SessionStart,
            EventKind::AssistantResponse,
            EventKind::PreCompact,
            EventKind::SessionEnd,
        ] {
            assert!(memory_event(Harness::Grok, kind, &native).is_none());
        }
    }
    #[test]
    fn optional_memory_configuration_failure_preserves_successful_hook_reply() {
        let mut f = Fixture::new();
        f.0.goal_id = None;
        f.0.options.hindsight = true;
        f.0.events = vec![EventKind::SessionStart, EventKind::Stop];
        layouts::write_private(
            &f.0.directory.join("hindsight.json"),
            b"not valid configuration",
        )
        .unwrap();
        for (event, extra) in [
            ("SessionStart", json!({})),
            (
                "Stop",
                json!({"last_assistant_message":"verified final text"}),
            ),
        ] {
            let mut payload = json!({"session_id":"session","hook_event_name":event});
            payload
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let mut output = Vec::new();
            let result = run_verified(
                serde_json::to_vec(&payload).unwrap().as_slice(),
                &mut output,
                &f.0,
                Some(event),
                None,
                &mut Verified,
                &mut FakeClock(0),
            )
            .unwrap();
            assert!(matches!(result.reply, Reply::Empty));
            assert!(output.is_empty());
        }
    }
    #[test]
    fn first_lifecycle_events_do_not_require_existing_assistant_transcript() {
        let mut f = Fixture::new();
        f.0.goal_id = None;
        f.0.events = vec![EventKind::SessionStart, EventKind::UserPromptSubmit];
        for event in ["SessionStart", "UserPromptSubmit"] {
            let bytes=serde_json::to_vec(&json!({"session_id":"session","hook_event_name":event,"prompt":"first user input"})).unwrap();
            let first = execute(
                bytes.as_slice(),
                &f.0,
                None,
                &mut Verified,
                &mut FakeClock(0),
            )
            .unwrap();
            let repeated = execute(
                bytes.as_slice(),
                &f.0,
                None,
                &mut Verified,
                &mut FakeClock(0),
            )
            .unwrap();
            assert!(first.admitted);
            assert!(!repeated.admitted);
            assert_eq!(first.event_id, repeated.event_id);
        }
    }
    #[test]
    fn stop_final_text_works_before_transcript_flush_without_old_row_identity() {
        let mut f = Fixture::new();
        let transcript = f.0.directory.join("transcript.jsonl");
        f.0.transcript = Some(transcript.clone());
        let p = install(&f.0).unwrap();
        arm(&p, "Continue", 0).unwrap();
        let payload=serde_json::to_vec(&json!({"session_id":"session","hook_event_name":"Stop","last_assistant_message":"new final message"})).unwrap();
        let first = execute(
            payload.as_slice(),
            &f.0,
            None,
            &mut Verified,
            &mut FakeClock(0),
        )
        .unwrap();
        assert!(matches!(first.reply, Reply::Json(_)));
        layouts::write_private(&transcript,serde_json::to_string(&json!({"sessionId":"session","type":"assistant","uuid":"old","message":{"content":[{"type":"text","text":"older message"}],"stop_reason":null}})).unwrap().as_bytes()).unwrap();
        let retry = execute(
            payload.as_slice(),
            &f.0,
            None,
            &mut Verified,
            &mut FakeClock(0),
        )
        .unwrap();
        assert_eq!(first.event_id, retry.event_id);
        assert_eq!(first.reply, retry.reply);
    }
    #[test]
    fn completion_proof_rejects_unrelated_newer_user_and_unmatched_final_text() {
        let mut f = Fixture::new();
        let path = f.0.directory.join("transcript.jsonl");
        f.0.transcript = Some(path.clone());
        let rows = format!(
            "{}\n{}\n",
            json!({"type":"user","sessionId":"session","uuid":"user","message":{"content":"exact prompt"}}),
            json!({"type":"assistant","sessionId":"session","uuid":"answer","parentUuid":"user","message":{"content":[{"type":"text","text":"done"}],"stop_reason":null}})
        );
        layouts::write_private(&path, rows.as_bytes()).unwrap();
        let sha = format!("{:x}", Sha256::digest(b"exact prompt"));
        assert!(transcript_proves_reply(&f.0, &sha, Some("done")).unwrap());
        assert!(!transcript_proves_reply(&f.0, &sha, Some("other")).unwrap());
        let rows=rows+&serde_json::to_string(&json!({"type":"user","sessionId":"session","uuid":"next","parentUuid":"answer","message":{"content":"new turn"}})).unwrap();
        layouts::write_private(&path, rows.as_bytes()).unwrap();
        assert!(!transcript_proves_reply(&f.0, &sha, Some("done")).unwrap());
    }
    #[test]
    fn cutover_lock_and_any_retirement_marker_prevent_hook_writes() {
        let mut f = Fixture::new();
        f.0.goal_id = None;
        install(&f.0).unwrap();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(f.0.directory.join(".cutover.lock"))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert!(
            execute(
                input("PostToolUse").as_slice(),
                &f.0,
                Some("blocked"),
                &mut Verified,
                &mut FakeClock(0)
            )
            .is_err()
        );
        assert!(!f.0.directory.join("delivery.sqlite3").exists());
        unsafe {
            libc::flock(lock.as_raw_fd(), libc::LOCK_UN);
        }
        layouts::write_private(&f.0.directory.join("cutover-retired.json"), b"malformed").unwrap();
        assert!(inspect(&f.0.path()).is_err());
        assert!(install(&f.0).is_err());
        assert!(
            execute(
                input("PostToolUse").as_slice(),
                &f.0,
                Some("retired"),
                &mut Verified,
                &mut FakeClock(0)
            )
            .is_err()
        );
    }
    #[test]
    fn grok_native_passive_stop_is_not_send_readiness_or_goal_continuation() {
        let mut f = Fixture::new();
        f.0.scope.harness = Harness::Grok;
        f.0.goal_id = None;
        f.0.events.retain(|e| *e != EventKind::PreToolUse);
        let bytes = br#"{"sessionId":"session","hookEventName":"stop","cwd":"/tmp","lastAssistantMessage":"native answer"}"#;
        let mut output = Vec::new();
        let outcome = run_verified(
            bytes.as_slice(),
            &mut output,
            &f.0,
            None,
            Some(EventKind::Stop),
            &mut Verified,
            &mut FakeClock(10),
        )
        .unwrap();
        assert!(matches!(outcome.reply, Reply::Empty));
        assert!(output.is_empty());
        let journal = Journal::open(&f.0.directory.join("delivery.sqlite3")).unwrap();
        let events = journal
            .events("hooks", &f.0.scope.instance, &["admitted"])
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload["final_message"], "native answer");

        install(&f.0).unwrap();
        assert_eq!(readiness(&f.0.path(), 10, 100).unwrap(), Readiness::Unknown);
        assert!(
            run_verified(
                bytes.as_slice(),
                &mut output,
                &f.0,
                None,
                Some(EventKind::UserPromptSubmit),
                &mut Verified,
                &mut FakeClock(10)
            )
            .is_err()
        );
    }
    #[test]
    fn tool_policy_failure_emits_explicit_deny_but_stop_errors_emit_no_json() {
        let f = Fixture::new();
        let mut output = Vec::new();
        let outcome = run_verified(
            input("PreToolUse").as_slice(),
            &mut output,
            &f.0,
            None,
            Some(EventKind::PreToolUse),
            &mut Verified,
            &mut FakeClock(0),
        )
        .unwrap();
        assert!(!outcome.admitted);
        let deny: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(deny["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(deny["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        output.clear();
        assert!(
            run_verified(
                input("Stop").as_slice(),
                &mut output,
                &f.0,
                Some("one"),
                Some(EventKind::Stop),
                &mut Verified,
                &mut FakeClock(0)
            )
            .is_err()
        );
        assert!(output.is_empty());
    }
    #[test]
    fn mismatched_expected_event_and_unverified_security_harness_refuse_before_effects() {
        let mut f = Fixture::new();
        let mut output = Vec::new();
        assert!(
            run_verified(
                input("Stop").as_slice(),
                &mut output,
                &f.0,
                None,
                Some(EventKind::PreToolUse),
                &mut Verified,
                &mut FakeClock(0)
            )
            .is_err()
        );
        assert!(output.is_empty());
        assert!(!f.0.directory.join("delivery.sqlite3").exists());
        f.0.goal_id = None;
        for harness in [Harness::Codex, Harness::Grok] {
            f.0.scope.harness = harness;
            assert!(f.0.validate().is_err());
        }
    }
    #[test]
    fn codex_native_turn_id_null_transcript_and_noop_json_work_without_claude_files() {
        let mut f = Fixture::new();
        f.0.scope.harness = Harness::Codex;
        f.0.goal_id = None;
        f.0.events = vec![EventKind::Stop, EventKind::UserPromptSubmit];
        let data=serde_json::to_vec(&json!({"session_id":"session","hook_event_name":"Stop","turn_id":"turn-1","transcript_path":null,"last_assistant_message":null,"stop_hook_active":false})).unwrap();
        let mut output = Vec::new();
        let first = run_verified(
            data.as_slice(),
            &mut output,
            &f.0,
            None,
            Some(EventKind::Stop),
            &mut Verified,
            &mut FakeClock(0),
        )
        .unwrap();
        assert_eq!(output, b"{}\n");
        assert!(first.admitted);
        output.clear();
        let duplicate = run_verified(
            data.as_slice(),
            &mut output,
            &f.0,
            None,
            Some(EventKind::Stop),
            &mut Verified,
            &mut FakeClock(100),
        )
        .unwrap();
        assert!(!duplicate.admitted);
        assert_eq!(first.event_id, duplicate.event_id);
    }
    #[test]
    fn codex_profile_hook_ignores_other_sessions_and_accepts_resumed_transcript() {
        let mut fixture = Fixture::new();
        fixture.0.scope.harness = Harness::Codex;
        fixture.0.goal_id = None;
        fixture.0.events = vec![EventKind::PostToolUse];
        fixture.0.transcript = Some(crate::test_support::path("/tmp/old-rollout.jsonl").into());
        let resumed = crate::test_support::path("/tmp/new-rollout.jsonl");
        let mut output = Vec::new();
        let other = serde_json::to_vec(&json!({"session_id":"other-session","hook_event_name":"PostToolUse","tool_use_id":"tool-other","transcript_path":resumed})).unwrap();
        let ignored = run_verified(
            other.as_slice(),
            &mut output,
            &fixture.0,
            None,
            Some(EventKind::PostToolUse),
            &mut Verified,
            &mut FakeClock(0),
        )
        .unwrap();
        assert_eq!(output, b"{}\n");
        assert!(!ignored.admitted);
        assert!(!fixture.0.directory.join("delivery.sqlite3").exists());

        output.clear();
        let matching = serde_json::to_vec(&json!({"session_id":"session","hook_event_name":"PostToolUse","tool_use_id":"tool-matching","transcript_path":resumed})).unwrap();
        let accepted = run_verified(
            matching.as_slice(),
            &mut output,
            &fixture.0,
            None,
            Some(EventKind::PostToolUse),
            &mut Verified,
            &mut FakeClock(1),
        )
        .unwrap();
        assert_eq!(output, b"{}\n");
        assert!(accepted.admitted);
    }
    #[test]
    fn grok_idle_notifications_correlate_reservations_and_ignore_other_turns() {
        let mut fixture = Fixture::new();
        fixture.0.scope.harness = Harness::Grok;
        fixture.0.goal_id = None;
        fixture.0.options.telegram_progress = true;
        fixture.0.events = vec![
            EventKind::UserPromptSubmit,
            EventKind::Stop,
            EventKind::Settled,
            EventKind::Notification,
        ];
        let path = install(&fixture.0).unwrap();
        let invoke = |id: &str, mut data: Value, now| {
            data["sessionId"] = "session".into();
            execute(
                serde_json::to_vec(&data).unwrap().as_slice(),
                &fixture.0,
                Some(id),
                &mut Verified,
                &mut FakeClock(now),
            )
        };
        let idle = json!({"hookEventName":"notification","notificationType":"idle_prompt"});
        let bootstrap = invoke("initial-idle", idle.clone(), 1).unwrap();
        assert!(
            !bootstrap
                .planned_effects
                .iter()
                .any(|e| e.contains("reply"))
        );
        assert_eq!(readiness(&path, 1, 100).unwrap(), Readiness::Idle);
        let scope = serde_json::to_string(&fixture.0.scope).unwrap();
        let db = readiness_connection(&fixture.0.directory).unwrap();
        db.execute(
            "INSERT INTO hook_reservations(scope,correlation) VALUES(?,?)",
            params![scope, format!("{:x}", Sha256::digest(b"queued prompt"))],
        )
        .unwrap();
        invoke("unconsumed-idle", idle.clone(), 2).unwrap();
        assert_eq!(readiness(&path, 2, 100).unwrap(), Readiness::Busy);
        invoke("accepted", json!({"hookEventName":"user_prompt_submit","promptId":"current","prompt":"queued prompt"}), 3).unwrap();
        invoke("stop-gate", json!({"hookEventName":"stop","promptId":"current","reason":"end_turn","stopHookActive":false,"lastAssistantMessage":"answer"}), 4).unwrap();
        assert_eq!(readiness(&path, 4, 100).unwrap(), Readiness::Unknown);
        invoke("older-idle", json!({"hookEventName":"notification","notificationType":"idle_prompt","promptId":"old"}), 5).unwrap();
        assert_eq!(readiness(&path, 5, 100).unwrap(), Readiness::Unknown);
        invoke("settled", idle.clone(), 6).unwrap();
        assert_eq!(readiness(&path, 6, 100).unwrap(), Readiness::Idle);
        assert_eq!(
            db.query_row("SELECT count(*) FROM hook_reservations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        invoke("new-prompt", json!({"hookEventName":"user_prompt_submit","promptId":"new","prompt":"another prompt"}), 7).unwrap();
        invoke("old-completion", json!({"hookEventName":"notification","notificationType":"idle_prompt","promptId":"current"}), 8).unwrap();
        invoke(
            "permission",
            json!({"hookEventName":"notification","notificationType":"permission_prompt"}),
            9,
        )
        .unwrap();
        assert!(invoke("nested", json!({"hookEventName":"notification","notificationType":"idle_prompt","subagentType":"worker"}), 10).is_err());
        assert_eq!(readiness(&path, 10, 100).unwrap(), Readiness::Busy);
    }
    #[test]
    fn session_scoped_idle_while_the_newest_prompt_is_open_stays_busy() {
        let mut fixture = Fixture::new();
        fixture.0.scope.harness = Harness::Grok;
        fixture.0.goal_id = None;
        fixture.0.events = vec![
            EventKind::UserPromptSubmit,
            EventKind::Stop,
            EventKind::Settled,
        ];
        let path = install(&fixture.0).unwrap();
        let invoke = |id: &str, mut data: Value, now| {
            data["sessionId"] = "session".into();
            execute(
                serde_json::to_vec(&data).unwrap().as_slice(),
                &fixture.0,
                Some(id),
                &mut Verified,
                &mut FakeClock(now),
            )
        };
        let idle = json!({"hookEventName":"notification","notificationType":"idle_prompt"});
        invoke(
            "prompt",
            json!({"hookEventName":"user_prompt_submit","promptId":"current","prompt":"work"}),
            1,
        )
        .unwrap();
        invoke("stop", json!({"hookEventName":"stop","promptId":"current","reason":"end_turn","stopHookActive":false,"lastAssistantMessage":"answer"}), 2).unwrap();
        assert_eq!(readiness(&path, 2, 100).unwrap(), Readiness::Unknown);
        invoke("settled", idle.clone(), 3).unwrap();
        assert_eq!(readiness(&path, 3, 100).unwrap(), Readiness::Idle);
        invoke(
            "newer",
            json!({"hookEventName":"user_prompt_submit","promptId":"new","prompt":"working"}),
            4,
        )
        .unwrap();
        assert_eq!(readiness(&path, 4, 100).unwrap(), Readiness::Busy);
        invoke("stale-session-idle", idle.clone(), 5).unwrap();
        assert_eq!(readiness(&path, 5, 100).unwrap(), Readiness::Busy);
        invoke("stop-new", json!({"hookEventName":"stop","promptId":"new","reason":"end_turn","stopHookActive":false,"lastAssistantMessage":"next"}), 6).unwrap();
        assert_eq!(readiness(&path, 6, 100).unwrap(), Readiness::Unknown);
        invoke("settled-new", idle, 7).unwrap();
        assert_eq!(readiness(&path, 7, 100).unwrap(), Readiness::Idle);
    }
    #[test]
    fn grok_native_idle_is_deduplicated_across_notification_wrappers() {
        let mut fixture = Fixture::new();
        fixture.0.scope.harness = Harness::Grok;
        fixture.0.goal_id = None;
        fixture.0.events = vec![EventKind::Settled, EventKind::Notification];
        install(&fixture.0).unwrap();
        let data = serde_json::to_vec(&json!({"sessionId":"session","hookEventName":"notification","notificationType":"idle_prompt","timestamp":"2026-10-15T12:00:00Z"})).unwrap();
        let mut output = Vec::new();
        let first = run_verified(
            data.as_slice(),
            &mut output,
            &fixture.0,
            None,
            Some(EventKind::Settled),
            &mut Verified,
            &mut FakeClock(1),
        )
        .unwrap();
        let duplicate = run_verified(
            data.as_slice(),
            &mut output,
            &fixture.0,
            None,
            Some(EventKind::Notification),
            &mut Verified,
            &mut FakeClock(2),
        )
        .unwrap();
        assert!(first.admitted);
        assert!(!duplicate.admitted);
        assert_eq!(first.event_id, duplicate.event_id);
    }
    #[test]
    fn grok_goal_gate_correlates_continuations_and_never_rearms_on_timeout() {
        let mut f = Fixture::new();
        f.0.scope.harness = Harness::Grok;
        f.0.events = vec![EventKind::Stop, EventKind::PostToolUse];
        let path = install(&f.0).unwrap();
        arm(&path, "Finish work", 0).unwrap();
        let invoke = |id: &str, turn: &str, active: bool, reason: &str, now| {
            let payload=serde_json::to_vec(&json!({"sessionId":"session","hookEventName":"stop","promptId":turn,"reason":reason,"stopHookActive":active,"lastAssistantMessage":"answer"})).unwrap();
            execute(
                payload.as_slice(),
                &f.0,
                Some(id),
                &mut Verified,
                &mut FakeClock(now),
            )
            .unwrap()
        };
        assert!(matches!(
            invoke("teardown", "turn1", false, "shutdown", 0).reply,
            Reply::Empty
        ));
        let first = invoke("first", "turn1", false, "end_turn", 1);
        assert!(matches!(first.reply,Reply::Json(ref v) if v["decision"]=="block"));
        assert!(matches!(
            invoke("late", "older", true, "end_turn", 8000).reply,
            Reply::Blocked(_)
        ));
        assert!(matches!(
            invoke("continued", "turn1", true, "end_turn", 8001).reply,
            Reply::Json(_)
        ));
        assert_eq!(readiness(&path, 8001, 30000).unwrap(), Readiness::Unknown);
        control(&path, Control::Pause, 8002).unwrap();
        assert!(matches!(
            invoke("continued", "turn1", true, "end_turn", 8001).reply,
            Reply::Empty
        ));
        control(&path, Control::Resume, 9000).unwrap();
        assert!(matches!(
            invoke("next", "turn2", false, "end_turn", 16002).reply,
            Reply::Json(_)
        ));
        // Native provider caps continuations; silence after the cap is blocked,
        // never reported running or automatically rearmed.
        assert!(matches!(
            status(&path, 50000).unwrap().goal.status,
            Status::Blocked { .. }
        ));
    }
    #[test]
    fn streaming_admission_records_exact_turn_and_prompt_hash_without_prompt_text() {
        let mut f = Fixture::new();
        f.0.goal_id = None;
        f.0.scope.harness = Harness::Codex;
        f.0.events = vec![EventKind::UserPromptSubmit];
        let prompt = "private accepted Telegram envelope";
        let bytes=serde_json::to_vec(&json!({"session_id":"session","hook_event_name":"UserPromptSubmit","turn_id":"native-turn","prompt":prompt})).unwrap();
        execute(
            bytes.as_slice(),
            &f.0,
            Some("native-prompt"),
            &mut Verified,
            &mut FakeClock(0),
        )
        .unwrap();
        let journal = Journal::open(&f.0.directory.join("delivery.sqlite3")).unwrap();
        let rows = journal
            .events("hooks", &f.0.scope.instance, &["admitted"])
            .unwrap();
        assert_eq!(rows[0].payload["stream_turn_id"], "native-turn");
        assert_eq!(
            rows[0].payload["stream_prompt_sha256"],
            format!("{:x}", Sha256::digest(prompt.as_bytes()))
        );
        assert_eq!(rows[0].payload["stream_evidence"], "verified_event_turn");
        assert!(!rows[0].payload.to_string().contains(prompt));
    }
    #[test]
    fn claude_stream_provenance_requires_unique_exact_transcript_ancestry() {
        let mut f = Fixture::new();
        let path = f.0.directory.join("stream.jsonl");
        f.0.transcript = Some(path.clone());
        let user =
            json!({"sessionId":"session","type":"user","uuid":"u1","message":{"content":"hello"}});
        let assistant = json!({"sessionId":"session","type":"assistant","uuid":"a1","parentUuid":"u1","message":{"content":[{"type":"text","text":"answer"}]}});
        let write = |rows: &[Value]| {
            let mut bytes = Vec::new();
            for row in rows {
                serde_json::to_writer(&mut bytes, row).unwrap();
                bytes.push(b'\n');
            }
            layouts::write_private(&path, &bytes).unwrap();
        };
        write(&[user.clone(), assistant.clone()]);
        assert_eq!(
            claude_stream_turn(
                &f.0,
                EventKind::UserPromptSubmit,
                &json!({"prompt":"hello"})
            )
            .as_deref(),
            Some("u1")
        );
        assert_eq!(
            claude_stream_turn(
                &f.0,
                EventKind::Stop,
                &json!({"last_assistant_message":"answer"})
            )
            .as_deref(),
            Some("u1")
        );
        assert!(
            claude_stream_turn(
                &f.0,
                EventKind::Stop,
                &json!({"last_assistant_message":"different"})
            )
            .is_none()
        );
        let tool = json!({"sessionId":"session","type":"assistant","uuid":"tool-a","parentUuid":"u1","message":{"content":[{"type":"tool_use","id":"tool-1","name":"Bash","input":{"command":"private"}}]}});
        write(&[user.clone(), tool]);
        assert_eq!(
            claude_stream_turn(
                &f.0,
                EventKind::PostToolUse,
                &json!({"tool_use_id":"tool-1"})
            )
            .as_deref(),
            Some("u1")
        );
        assert!(
            claude_stream_turn(
                &f.0,
                EventKind::PostToolUse,
                &json!({"tool_use_id":"other"})
            )
            .is_none()
        );
        // Historical records beyond the bounded window do not disable a fully
        // visible current turn and its exact parent ancestry.
        let mut bytes = vec![b' '; 1024 * 1024 + 16];
        bytes.push(b'\n');
        for row in [&user, &assistant] {
            serde_json::to_writer(&mut bytes, row).unwrap();
            bytes.push(b'\n');
        }
        layouts::write_private(&path, &bytes).unwrap();
        assert_eq!(
            claude_stream_turn(
                &f.0,
                EventKind::Stop,
                &json!({"last_assistant_message":"answer"})
            )
            .as_deref(),
            Some("u1")
        );
        let newer = json!({"sessionId":"session","type":"user","uuid":"u2","parentUuid":"a1","message":{"content":"next"}});
        write(&[user.clone(), assistant.clone(), newer]);
        assert!(
            claude_stream_turn(
                &f.0,
                EventKind::Stop,
                &json!({"last_assistant_message":"answer"})
            )
            .is_none()
        );
        let repeated = json!({"sessionId":"session","type":"user","uuid":"u2","parentUuid":"a1","message":{"content":"hello"}});
        write(&[user, assistant, repeated]);
        assert!(
            claude_stream_turn(
                &f.0,
                EventKind::UserPromptSubmit,
                &json!({"prompt":"hello"})
            )
            .is_none()
        );
    }
    #[test]
    fn codex_goal_continuation_is_durable_and_pause_overrides_duplicate() {
        let mut f = Fixture::new();
        f.0.scope.harness = Harness::Codex;
        f.0.events = vec![EventKind::Stop];
        let path = install(&f.0).unwrap();
        arm(&path, "Finish work", 0).unwrap();
        let data=serde_json::to_vec(&json!({"session_id":"session","hook_event_name":"Stop","turn_id":"turn-1","transcript_path":null,"last_assistant_message":"done","stop_hook_active":false})).unwrap();
        let mut output = Vec::new();
        let result = run_verified(
            data.as_slice(),
            &mut output,
            &f.0,
            None,
            Some(EventKind::Stop),
            &mut Verified,
            &mut FakeClock(0),
        )
        .unwrap();
        assert!(matches!(result.reply, Reply::Json(_)));
        assert_eq!(
            serde_json::from_slice::<Value>(&output).unwrap()["decision"],
            "block"
        );
        control(&path, Control::Pause, 1).unwrap();
        output.clear();
        run_verified(
            data.as_slice(),
            &mut output,
            &f.0,
            None,
            Some(EventKind::Stop),
            &mut Verified,
            &mut FakeClock(1),
        )
        .unwrap();
        assert_eq!(output, b"{}\n");
    }
    #[test]
    fn pi_goal_boundary_never_claims_settled_and_final_event_releases_exact_turn() {
        let mut f = Fixture::new();
        f.0.scope.harness = Harness::Pi;
        f.0.events = vec![
            EventKind::UserPromptSubmit,
            EventKind::Stop,
            EventKind::Activity,
            EventKind::Settled,
        ];
        let path = install(&f.0).unwrap();
        arm(&path, "Finish fixture", 0).unwrap();
        let invoke = |kind: &str, id: &str, turn: &str, now: u64| {
            let data=serde_json::to_vec(&json!({"bridge_version":"daedalus.pi.v1","session_id":"session","hook_event_name":kind,"event_id":id,"turn_id":turn,"prompt":"exact prompt","status":"completed"})).unwrap();
            execute(
                data.as_slice(),
                &f.0,
                None,
                &mut Verified,
                &mut FakeClock(now),
            )
            .unwrap()
        };
        invoke("UserPromptSubmit", "prompt", "turn", 1);
        let stop = invoke("Stop", "stop", "turn", 2);
        let Reply::Json(value) = stop.reply else {
            panic!("missing Pi continuation")
        };
        assert_eq!(value["continue"], true);
        assert_eq!(value["entries"][0]["type"], "custom_message");
        assert_eq!(readiness(&path, 2, 100).unwrap(), Readiness::Unknown);
        invoke("Activity", "continued", "turn", 3);
        assert!(matches!(
            status(&path, 3).unwrap().goal.status,
            Status::Running { .. }
        ));
        control(&path, Control::Complete, 4).unwrap();
        invoke("Stop", "final-stop", "turn", 5);
        assert_eq!(readiness(&path, 5, 100).unwrap(), Readiness::Unknown);
        invoke("Settled", "stale-settled", "old", 6);
        assert_eq!(readiness(&path, 6, 100).unwrap(), Readiness::Busy);
        invoke("Settled", "settled", "turn", 7);
        assert_eq!(readiness(&path, 7, 100).unwrap(), Readiness::Idle);
    }
    #[test]
    fn hermes_shell_payload_uses_nested_native_turn_and_final_response() {
        let mut f = Fixture::new();
        f.0.scope.harness = Harness::Hermes;
        f.0.goal_id = None;
        f.0.options.telegram_delivery = true;
        f.0.events = vec![EventKind::UserPromptSubmit, EventKind::Stop];
        let prompt=br#"{"session_id":"session","hook_event_name":"pre_llm_call","tool_name":null,"extra":{"turn_id":"turn","user_message":"exact prompt"}}"#;
        let stop=br#"{"session_id":"session","hook_event_name":"post_llm_call","tool_name":null,"extra":{"turn_id":"turn","user_message":"exact prompt","assistant_response":"final result"}}"#;
        run_verified(
            prompt.as_slice(),
            &mut Vec::new(),
            &f.0,
            None,
            Some(EventKind::UserPromptSubmit),
            &mut Verified,
            &mut FakeClock(0),
        )
        .unwrap();
        let mut output = Vec::new();
        let end = run_verified(
            stop.as_slice(),
            &mut output,
            &f.0,
            None,
            Some(EventKind::Stop),
            &mut Verified,
            &mut FakeClock(1),
        )
        .unwrap();
        assert_eq!(output, b"{}\n");
        assert!(matches!(end.reply, Reply::Empty));
        let journal = Journal::open(&f.0.directory.join("delivery.sqlite3")).unwrap();
        let rows = journal.events("hooks", "example", &["admitted"]).unwrap();
        assert_eq!(
            rows.last().unwrap().payload["final_message"],
            "final result"
        );
        let path = install(&f.0).unwrap();
        assert_eq!(readiness(&path, 1, 1000).unwrap(), Readiness::Idle);
        f.0.goal_id = Some("goal".into());
        assert!(f.0.validate().is_err());
    }
    #[test]
    fn cursor_response_capture_is_immutable_deduplicated_and_separate_from_stop() {
        let mut f = Fixture::new();
        f.0.scope.harness = Harness::Cursor;
        f.0.goal_id = None;
        f.0.options.telegram_delivery = true;
        f.0.events = vec![EventKind::AssistantResponse, EventKind::Stop];
        let data=br#"{"conversation_id":"session","generation_id":"generation","hook_event_name":"afterAgentResponse","text":"captured response"}"#;
        let mut output = Vec::new();
        let first = run_verified(
            data.as_slice(),
            &mut output,
            &f.0,
            None,
            Some(EventKind::AssistantResponse),
            &mut Verified,
            &mut FakeClock(0),
        )
        .unwrap();
        assert_eq!(output, b"{}\n");
        assert_eq!(first.planned_effects, vec!["queue_assistant_reply"]);
        let repeated = run_verified(
            data.as_slice(),
            &mut output,
            &f.0,
            None,
            Some(EventKind::AssistantResponse),
            &mut Verified,
            &mut FakeClock(1),
        )
        .unwrap();
        assert!(!repeated.admitted);
        let journal = Journal::open(&f.0.directory.join("delivery.sqlite3")).unwrap();
        let rows = journal.events("hooks", "example", &["admitted"]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].payload["final_message"], "captured response");
        let stop=br#"{"conversation_id":"session","generation_id":"generation","hook_event_name":"stop","status":"completed"}"#;
        let stopped = run_verified(
            stop.as_slice(),
            &mut output,
            &f.0,
            None,
            Some(EventKind::Stop),
            &mut Verified,
            &mut FakeClock(2),
        )
        .unwrap();
        assert!(
            !stopped
                .planned_effects
                .iter()
                .any(|e| e == "queue_final_reply")
        );
        let aborted=br#"{"conversation_id":"session","generation_id":"generation","hook_event_name":"stop","status":"aborted"}"#;
        assert!(
            run_verified(
                aborted.as_slice(),
                &mut Vec::new(),
                &f.0,
                None,
                Some(EventKind::Stop),
                &mut Verified,
                &mut FakeClock(3)
            )
            .is_err()
        );
    }
    #[test]
    fn cursor_goal_continuation_is_durable_and_pause_overrides_duplicate() {
        let mut f = Fixture::new();
        f.0.scope.harness = Harness::Cursor;
        f.0.events = vec![EventKind::Stop];
        let path = install(&f.0).unwrap();
        arm(&path, "Finish work", 0).unwrap();
        let data=serde_json::to_vec(&json!({"conversation_id":"session","hook_event_name":"stop","generation_id":"turn-1","status":"completed","transcript_path":null,"last_assistant_message":"done","stop_hook_active":false})).unwrap();
        let mut output = Vec::new();
        let result = run_verified(
            data.as_slice(),
            &mut output,
            &f.0,
            None,
            Some(EventKind::Stop),
            &mut Verified,
            &mut FakeClock(0),
        )
        .unwrap();
        assert!(matches!(result.reply, Reply::Json(_)));
        assert!(
            serde_json::from_slice::<Value>(&output).unwrap()["followup_message"]
                .as_str()
                .unwrap()
                .contains("Finish work")
        );
        control(&path, Control::Pause, 1).unwrap();
        output.clear();
        run_verified(
            data.as_slice(),
            &mut output,
            &f.0,
            None,
            Some(EventKind::Stop),
            &mut Verified,
            &mut FakeClock(1),
        )
        .unwrap();
        assert_eq!(output, b"{}\n");
    }
    #[test]
    fn codex_readiness_requires_exact_prompt_turn_and_rejects_stale_stops() {
        let mut f = Fixture::new();
        f.0.scope.harness = Harness::Codex;
        f.0.goal_id = None;
        f.0.events = vec![EventKind::UserPromptSubmit, EventKind::Stop];
        let path = install(&f.0).unwrap();
        let observe = |key: &str, kind: EventKind, payload: Value, reply: Option<&Reply>| {
            observe_readiness(&f.0, key, &payload, kind, 1, reply).unwrap()
        };
        observe(
            "prompt-0",
            EventKind::UserPromptSubmit,
            json!({"turn_id":"turn-0","prompt":"initial"}),
            None,
        );
        observe(
            "stop-0",
            EventKind::Stop,
            json!({"turn_id":"turn-0","last_assistant_message":"initial answer"}),
            Some(&Reply::Empty),
        );
        assert_eq!(readiness(&path, 1, 1000).unwrap(), Readiness::Idle);
        let con = readiness_connection(&f.0.directory).unwrap();
        let scope = serde_json::to_string(&f.0.scope).unwrap();
        let text = "exact injected Telegram envelope";
        let sha = format!("{:x}", Sha256::digest(text.as_bytes()));
        con.execute(
            "INSERT INTO hook_reservations(scope,correlation) VALUES(?,?)",
            params![scope, sha],
        )
        .unwrap();
        observe(
            "prompt-1",
            EventKind::UserPromptSubmit,
            json!({"turn_id":"turn-1","prompt":text}),
            None,
        );
        observe(
            "late-stop",
            EventKind::Stop,
            json!({"turn_id":"turn-old","last_assistant_message":"old answer"}),
            Some(&Reply::Empty),
        );
        assert_eq!(readiness(&path, 1, 1000).unwrap(), Readiness::Busy);
        observe(
            "stop-1",
            EventKind::Stop,
            json!({"turn_id":"turn-1","last_assistant_message":"new answer"}),
            Some(&Reply::Empty),
        );
        assert_eq!(readiness(&path, 1, 1000).unwrap(), Readiness::Idle);
        observe(
            "prompt-2",
            EventKind::UserPromptSubmit,
            json!({"turn_id":"turn-2","prompt":"a new local user input"}),
            None,
        );
        observe(
            "delayed-old-stop",
            EventKind::Stop,
            json!({"turn_id":"turn-1","last_assistant_message":"new answer"}),
            Some(&Reply::Empty),
        );
        assert_eq!(readiness(&path, 1, 1000).unwrap(), Readiness::Busy);
        observe(
            "stop-2",
            EventKind::Stop,
            json!({"turn_id":"turn-2","last_assistant_message":"latest answer"}),
            Some(&Reply::Empty),
        );
        assert_eq!(readiness(&path, 1, 1000).unwrap(), Readiness::Idle);
    }
    #[test]
    fn cursor_readiness_requires_exact_prompt_turn_and_rejects_stale_stops() {
        let mut f = Fixture::new();
        f.0.scope.harness = Harness::Cursor;
        f.0.goal_id = None;
        f.0.events = vec![EventKind::UserPromptSubmit, EventKind::Stop];
        let path = install(&f.0).unwrap();
        let observe = |key: &str, kind: EventKind, payload: Value, reply: Option<&Reply>| {
            observe_readiness(&f.0, key, &payload, kind, 1, reply).unwrap()
        };
        observe(
            "prompt-0",
            EventKind::UserPromptSubmit,
            json!({"generation_id":"turn-0","prompt":"initial"}),
            None,
        );
        observe(
            "stop-0",
            EventKind::Stop,
            json!({"generation_id":"turn-0","status":"completed"}),
            Some(&Reply::Empty),
        );
        assert_eq!(readiness(&path, 1, 1000).unwrap(), Readiness::Idle);
        let con = readiness_connection(&f.0.directory).unwrap();
        let scope = serde_json::to_string(&f.0.scope).unwrap();
        let text = "exact injected Telegram envelope";
        let sha = format!("{:x}", Sha256::digest(text.as_bytes()));
        con.execute(
            "INSERT INTO hook_reservations(scope,correlation) VALUES(?,?)",
            params![scope, sha],
        )
        .unwrap();
        observe(
            "prompt-1",
            EventKind::UserPromptSubmit,
            json!({"generation_id":"turn-1","prompt":text}),
            None,
        );
        observe(
            "late-stop",
            EventKind::Stop,
            json!({"generation_id":"turn-old","status":"completed"}),
            Some(&Reply::Empty),
        );
        assert_eq!(readiness(&path, 1, 1000).unwrap(), Readiness::Busy);
        observe(
            "stop-1",
            EventKind::Stop,
            json!({"generation_id":"turn-1","status":"completed"}),
            Some(&Reply::Empty),
        );
        assert_eq!(readiness(&path, 1, 1000).unwrap(), Readiness::Idle);
        observe(
            "prompt-2",
            EventKind::UserPromptSubmit,
            json!({"generation_id":"turn-2","prompt":"a new local user input"}),
            None,
        );
        observe(
            "delayed-old-stop",
            EventKind::Stop,
            json!({"generation_id":"turn-1","status":"completed"}),
            Some(&Reply::Empty),
        );
        assert_eq!(readiness(&path, 1, 1000).unwrap(), Readiness::Busy);
        observe(
            "stop-2",
            EventKind::Stop,
            json!({"generation_id":"turn-2","status":"completed"}),
            Some(&Reply::Empty),
        );
        assert_eq!(readiness(&path, 1, 1000).unwrap(), Readiness::Idle);
    }
    #[test]
    fn hermes_readiness_requires_exact_prompt_turn_and_rejects_stale_stops() {
        let mut f = Fixture::new();
        f.0.scope.harness = Harness::Hermes;
        f.0.goal_id = None;
        f.0.events = vec![EventKind::UserPromptSubmit, EventKind::Stop];
        let path = install(&f.0).unwrap();
        let observe = |key: &str, kind: EventKind, payload: Value, reply: Option<&Reply>| {
            observe_readiness(&f.0, key, &payload, kind, 1, reply).unwrap()
        };
        observe(
            "prompt-0",
            EventKind::UserPromptSubmit,
            json!({"extra":{"turn_id":"turn-0","user_message":"initial"}}),
            None,
        );
        observe(
            "stop-0",
            EventKind::Stop,
            json!({"extra":{"turn_id":"turn-0","assistant_response":"answer"}}),
            Some(&Reply::Empty),
        );
        assert_eq!(readiness(&path, 1, 1000).unwrap(), Readiness::Idle);
        let con = readiness_connection(&f.0.directory).unwrap();
        let scope = serde_json::to_string(&f.0.scope).unwrap();
        let text = "exact injected Telegram envelope";
        let sha = format!("{:x}", Sha256::digest(text.as_bytes()));
        con.execute(
            "INSERT INTO hook_reservations(scope,correlation) VALUES(?,?)",
            params![scope, sha],
        )
        .unwrap();
        observe(
            "prompt-1",
            EventKind::UserPromptSubmit,
            json!({"extra":{"turn_id":"turn-1","user_message":text}}),
            None,
        );
        observe(
            "late-stop",
            EventKind::Stop,
            json!({"extra":{"turn_id":"turn-old","assistant_response":"answer"}}),
            Some(&Reply::Empty),
        );
        assert_eq!(readiness(&path, 1, 1000).unwrap(), Readiness::Busy);
        observe(
            "stop-1",
            EventKind::Stop,
            json!({"extra":{"turn_id":"turn-1","assistant_response":"answer"}}),
            Some(&Reply::Empty),
        );
        assert_eq!(readiness(&path, 1, 1000).unwrap(), Readiness::Idle);
        observe(
            "prompt-2",
            EventKind::UserPromptSubmit,
            json!({"extra":{"turn_id":"turn-2","user_message":"a new local user input"}}),
            None,
        );
        observe(
            "delayed-old-stop",
            EventKind::Stop,
            json!({"extra":{"turn_id":"turn-1","assistant_response":"answer"}}),
            Some(&Reply::Empty),
        );
        assert_eq!(readiness(&path, 1, 1000).unwrap(), Readiness::Busy);
        observe(
            "stop-2",
            EventKind::Stop,
            json!({"extra":{"turn_id":"turn-2","assistant_response":"answer"}}),
            Some(&Reply::Empty),
        );
        assert_eq!(readiness(&path, 1, 1000).unwrap(), Readiness::Idle);
    }
    #[test]
    fn codex_reserved_goal_continuation_tracks_only_committed_reason_into_new_turn() {
        let mut f = Fixture::new();
        f.0.scope.harness = Harness::Codex;
        f.0.events = vec![EventKind::UserPromptSubmit, EventKind::Stop];
        let path = install(&f.0).unwrap();
        arm(&path, "Finish verified work", 0).unwrap();
        let text = "exact Telegram goal prompt";
        let con = readiness_connection(&f.0.directory).unwrap();
        con.execute(
            "INSERT INTO hook_reservations(scope,correlation) VALUES(?,?)",
            params![
                serde_json::to_string(&f.0.scope).unwrap(),
                format!("{:x}", Sha256::digest(text.as_bytes()))
            ],
        )
        .unwrap();
        drop(con);
        let invoke = |event: &str, turn: &str, prompt: Option<&str>, now: u64| {
            let bytes=serde_json::to_vec(&json!({"session_id":"session","hook_event_name":event,"turn_id":turn,"prompt":prompt,"last_assistant_message":"done","transcript_path":null})).unwrap();
            execute(
                bytes.as_slice(),
                &f.0,
                None,
                &mut Verified,
                &mut FakeClock(now),
            )
            .unwrap()
        };
        invoke("UserPromptSubmit", "turn-1", Some(text), 1);
        let response = invoke("Stop", "turn-1", None, 100);
        let Reply::Json(value) = response.reply else {
            panic!("expected goal continuation")
        };
        invoke("UserPromptSubmit", "turn-2", value["reason"].as_str(), 200);
        assert_eq!(readiness(&path, 200, 1000).unwrap(), Readiness::Busy);
        control(&path, Control::Complete, 300).unwrap();
        invoke("Stop", "turn-2", None, 400);
        assert_eq!(readiness(&path, 400, 1000).unwrap(), Readiness::Idle);
    }
    #[test]
    fn registration_rejects_public_file_and_conflicting_policy() {
        let f = Fixture::new();
        let p = install(&f.0).unwrap();
        let mut changed = f.0.clone();
        changed.pending_timeout_ms += 1;
        assert!(install(&changed).is_err());
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(inspect(&p).unwrap_err(), Error::PrivateState);
    }
}

/// Local status read expires uncertainty before returning it to the CLI/TUI.
pub fn status(path: &Path, now: u64) -> Result<goal_store::StoredGoal> {
    let registration = inspect(path)?;
    let _active = active_guard(&registration.directory)?;
    let id = registration.goal_id.as_deref().ok_or(Error::GoalPolicy)?;
    open_store(&registration.directory)?
        .inspect(
            id,
            &registration.scope,
            now,
            registration.pending_timeout_ms,
        )
        .map_err(|_| Error::GoalPolicy)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    Unknown,
    Busy,
    Idle,
}
fn readiness_connection(directory: &Path) -> Result<Connection> {
    let con = private_connection(directory, "hook-observations.sqlite3")?;
    con.execute_batch("CREATE TABLE IF NOT EXISTS hook_observations(seq INTEGER PRIMARY KEY AUTOINCREMENT,scope TEXT NOT NULL,event_id TEXT NOT NULL UNIQUE,payload TEXT NOT NULL,state TEXT NOT NULL,observed_at INTEGER NOT NULL,finalized INTEGER NOT NULL); CREATE INDEX IF NOT EXISTS hook_observations_scope ON hook_observations(scope,seq); CREATE TABLE IF NOT EXISTS hook_reservations(scope TEXT PRIMARY KEY,correlation TEXT NOT NULL); CREATE TABLE IF NOT EXISTS hook_prompt_receipts(scope TEXT PRIMARY KEY,correlation TEXT NOT NULL); CREATE TABLE IF NOT EXISTS hook_prompt_turns(scope TEXT PRIMARY KEY,turn_id TEXT NOT NULL); CREATE TABLE IF NOT EXISTS hook_active_turns(scope TEXT PRIMARY KEY,turn_id TEXT NOT NULL); CREATE TABLE IF NOT EXISTS hook_turn_release(scope TEXT PRIMARY KEY,turn_id TEXT NOT NULL);").map_err(|_|Error::Persistence)?;
    Ok(con)
}
fn observe_readiness(
    r: &Registration,
    key: &str,
    payload: &Value,
    kind: EventKind,
    now: u64,
    reply: Option<&Reply>,
) -> Result<()> {
    // Stop starts unknown: a crash before the continuation decision must never
    // advertise an idle shell. A continuation response keeps it busy.
    let state = match kind {
        EventKind::UserPromptSubmit
        | EventKind::PreToolUse
        | EventKind::PostToolUse
        | EventKind::Activity => Readiness::Busy,
        EventKind::Stop | EventKind::Settled => match reply {
            Some(Reply::Empty) => Readiness::Idle,
            Some(Reply::Json(_)) => Readiness::Busy,
            _ => Readiness::Unknown,
        },
        EventKind::SessionStart | EventKind::SessionEnd => Readiness::Unknown,
        _ => return Ok(()),
    };
    let scope = serde_json::to_string(&r.scope).map_err(|_| Error::Input)?;
    let hash = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(payload).map_err(|_| Error::Input)?)
    );
    let state = serde_json::to_string(&state).map_err(|_| Error::Input)?;
    let now = i64::try_from(now).map_err(|_| Error::Input)?;
    let mut con = readiness_connection(&r.directory)?;
    let tx = con
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| Error::Persistence)?;
    let old: Option<(String, String, bool)> = tx
        .query_row(
            "SELECT scope,payload,finalized FROM hook_observations WHERE event_id=?",
            [key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|_| Error::Persistence)?;
    if old.is_none()
        && kind == EventKind::UserPromptSubmit
        && matches!(
            r.scope.harness,
            Harness::Codex
                | Harness::Cursor
                | Harness::Hermes
                | Harness::Pi
                | Harness::Opencode
                | Harness::Grok
        )
    {
        if let Some(turn) = native_turn(r.scope.harness, payload) {
            tx.execute("INSERT INTO hook_active_turns(scope,turn_id) VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET turn_id=excluded.turn_id",params![scope,turn]).map_err(|_|Error::Persistence)?;
        } else {
            tx.execute("DELETE FROM hook_active_turns WHERE scope=?", [&scope])
                .map_err(|_| Error::Persistence)?;
        }
        if r.scope.harness == Harness::Grok {
            // A new prompt opens the turn again. The previous Stop gate no longer applies.
            tx.execute("DELETE FROM hook_turn_release WHERE scope=?", [&scope])
                .map_err(|_| Error::Persistence)?;
        }
    }
    let active_turn: Option<String> = tx
        .query_row(
            "SELECT turn_id FROM hook_active_turns WHERE scope=?",
            [&scope],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| Error::Persistence)?;
    let grok_idle = r.scope.harness == Harness::Grok
        && kind == EventKind::Settled
        && native_completed(Harness::Grok, payload);
    if grok_idle
        && native_turn(Harness::Grok, payload)
            .is_some_and(|turn| active_turn.as_deref().is_some_and(|active| active != turn))
    {
        return Ok(()); // A delayed older turn must not supersede the newest observation.
    }
    if r.scope.harness == Harness::Grok
        && kind == EventKind::Stop
        && let Some(active) = active_turn.as_deref()
        && native_turn(Harness::Grok, payload) == Some(active)
    {
        if matches!(reply, Some(Reply::Empty)) {
            tx.execute("INSERT INTO hook_turn_release(scope,turn_id) VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET turn_id=excluded.turn_id",params![scope,active]).map_err(|_|Error::Persistence)?;
        } else if matches!(reply, Some(Reply::Json(_))) {
            tx.execute(
                "DELETE FROM hook_turn_release WHERE scope=? AND turn_id=?",
                params![scope, active],
            )
            .map_err(|_| Error::Persistence)?;
        }
    }
    let released_turn: Option<String> = tx
        .query_row(
            "SELECT turn_id FROM hook_turn_release WHERE scope=?",
            [&scope],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| Error::Persistence)?;
    // A promptless idle is the session backstop. It may initialize readiness, or
    // settle the newest prompt after that prompt's Stop gate returned empty. It
    // must not overwrite a newer prompt that is still open.
    let open_prompt_blocks_session_idle = grok_idle
        && native_turn(Harness::Grok, payload).is_none()
        && active_turn
            .as_deref()
            .is_some_and(|active| released_turn.as_deref() != Some(active));
    let active_provider_stop = if r.scope.harness == Harness::Grok {
        grok_idle
            && !open_prompt_blocks_session_idle
            && native_turn(Harness::Grok, payload)
                .is_none_or(|turn| active_turn.as_deref().is_none_or(|active| active == turn))
    } else {
        matches!(
            r.scope.harness,
            Harness::Codex
                | Harness::Cursor
                | Harness::Hermes
                | Harness::Pi
                | Harness::Opencode
                | Harness::Grok
        ) && active_turn.as_ref().is_some_and(|turn| {
            native_turn(r.scope.harness, payload) == Some(turn)
                && native_completed(r.scope.harness, payload)
        })
    };
    let reservation: Option<String> = tx
        .query_row(
            "SELECT correlation FROM hook_reservations WHERE scope=?",
            [&scope],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| Error::Persistence)?;
    let correlated = old.is_none()
        && kind == EventKind::UserPromptSubmit
        && reservation.as_ref().is_some_and(|marker| {
            native_prompt(r.scope.harness, payload)
                .is_some_and(|p| format!("{:x}", Sha256::digest(p.as_bytes())) == *marker)
        });
    if correlated {
        if matches!(
            r.scope.harness,
            Harness::Codex
                | Harness::Cursor
                | Harness::Hermes
                | Harness::Pi
                | Harness::Opencode
                | Harness::Grok
        ) && let Some(turn) = native_turn(r.scope.harness, payload)
        {
            tx.execute("INSERT INTO hook_prompt_turns(scope,turn_id) VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET turn_id=excluded.turn_id",params![scope,turn]).map_err(|_|Error::Persistence)?;
        }
        tx.execute("INSERT INTO hook_prompt_receipts(scope,correlation) VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET correlation=excluded.correlation",params![scope,reservation]).map_err(|_|Error::Persistence)?;
    }
    let prompt_seen: Option<String> = tx
        .query_row(
            "SELECT correlation FROM hook_prompt_receipts WHERE scope=?",
            [&scope],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| Error::Persistence)?;
    // Codex represents a stored Stop continuation as a new user turn. Only
    // exact committed goal receipt text may advance a reserved turn's identity.
    if old.is_none()
        && kind == EventKind::UserPromptSubmit
        && matches!(
            r.scope.harness,
            Harness::Codex
                | Harness::Cursor
                | Harness::Hermes
                | Harness::Pi
                | Harness::Opencode
                | Harness::Grok
        )
        && reservation.is_some()
        && prompt_seen == reservation
        && let (Some(goal_id), Some(prompt), Some(turn)) = (
            r.goal_id.as_deref(),
            native_prompt(r.scope.harness, payload),
            native_turn(r.scope.harness, payload),
        )
    {
        let mut store = open_store(&r.directory)?;
        store
            .inspect(
                goal_id,
                &r.scope,
                u64::try_from(now).map_err(|_| Error::Input)?,
                r.pending_timeout_ms,
            )
            .map_err(|_| Error::GoalPolicy)?;
        if store
            .continuation_prompt(goal_id, &r.scope)
            .map_err(|_| Error::GoalPolicy)?
            .as_deref()
            == Some(prompt)
        {
            tx.execute("INSERT INTO hook_prompt_turns(scope,turn_id) VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET turn_id=excluded.turn_id",params![scope,turn]).map_err(|_|Error::Persistence)?;
        }
    }
    let codex_turn: Option<String> = tx
        .query_row(
            "SELECT turn_id FROM hook_prompt_turns WHERE scope=?",
            [&scope],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| Error::Persistence)?;
    let provider_completed = active_provider_stop
        && codex_turn.as_ref().is_some_and(|turn| {
            if r.scope.harness == Harness::Grok {
                active_turn.as_ref() == Some(turn)
                    && native_turn(Harness::Grok, payload)
                        .is_none_or(|event_turn| event_turn == turn)
            } else {
                native_turn(r.scope.harness, payload) == Some(turn)
            }
        })
        && native_completed(r.scope.harness, payload);
    let completed = ((kind == EventKind::Stop && r.scope.harness != Harness::Pi)
        || (matches!(r.scope.harness, Harness::Pi | Harness::Grok) && kind == EventKind::Settled))
        && matches!(reply, Some(Reply::Empty))
        && reservation.as_ref().is_some_and(|sha| {
            prompt_seen.as_ref() == Some(sha)
                && ((provider_completed
                    && (r.scope.harness != Harness::Opencode
                        || opencode_completion(payload).is_some_and(|(_, text)| {
                            format!("{:x}", Sha256::digest(text.as_bytes())) == *sha
                        })))
                    || transcript_proves_reply(
                        r,
                        sha,
                        payload
                            .get("last_assistant_message")
                            .and_then(Value::as_str),
                    )
                    .unwrap_or(false))
        });
    if completed {
        tx.execute("DELETE FROM hook_reservations WHERE scope=?", [&scope])
            .map_err(|_| Error::Persistence)?;
        tx.execute("DELETE FROM hook_prompt_receipts WHERE scope=?", [&scope])
            .map_err(|_| Error::Persistence)?;
        tx.execute("DELETE FROM hook_prompt_turns WHERE scope=?", [&scope])
            .map_err(|_| Error::Persistence)?;
    }
    // A canonical idle projection must match the exact current prompt and
    // durable terminal event before an OpenCode session can become idle.
    let state = if r.scope.harness == Harness::Opencode
        && matches!(kind, EventKind::Stop | EventKind::Settled)
        && !active_provider_stop
    {
        serde_json::to_string(&Readiness::Unknown).unwrap()
    } else if matches!(r.scope.harness, Harness::Pi | Harness::Grok) && kind == EventKind::Stop {
        // Grok Stop is a gate: another hook may continue the same native turn.
        // A matching prompt ID alone is not global settlement proof.
        serde_json::to_string(&Readiness::Unknown).map_err(|_| Error::Input)?
    } else if (reservation.is_some() && !completed)
        || (matches!(
            r.scope.harness,
            Harness::Codex
                | Harness::Cursor
                | Harness::Hermes
                | Harness::Pi
                | Harness::Opencode
                | Harness::Grok
        ) && matches!(kind, EventKind::Stop | EventKind::Settled)
            && !active_provider_stop)
    {
        serde_json::to_string(&Readiness::Busy).map_err(|_| Error::Input)?
    } else {
        state
    };
    if let Some((binding, sha, finalized)) = old {
        if binding != scope || sha != hash {
            return Err(Error::EventIdentity);
        }
        if !finalized && reply.is_some() {
            tx.execute(
                "UPDATE hook_observations SET state=?,finalized=1 WHERE event_id=?",
                params![state, key],
            )
            .map_err(|_| Error::Persistence)?;
        }
    } else {
        tx.execute("INSERT INTO hook_observations(scope,event_id,payload,state,observed_at,finalized) VALUES(?,?,?,?,?,?)",params![scope,key,hash,state,now,!matches!(kind,EventKind::Stop|EventKind::Settled)||reply.is_some()]).map_err(|_|Error::Persistence)?;
    }
    tx.commit().map_err(|_| Error::Persistence)
}
/// Observation only. Caller must also verify live exact terminal and claim the
/// destination immediately before injecting input. Registration is local policy,
/// not an IPC authentication token. Unknown/stale observations prohibit feeding.
pub fn readiness(path: &Path, now: u64, max_age_ms: u64) -> Result<Readiness> {
    if max_age_ms == 0 || max_age_ms > 30000 {
        return Err(Error::Invalid(
            "Readiness freshness must be 1..30000 milliseconds",
        ));
    }
    let r = inspect(path)?;
    let _active = active_guard(&r.directory)?;
    if !r.directory.join("hook-observations.sqlite3").exists() {
        return Ok(Readiness::Unknown);
    }
    let con = readiness_connection(&r.directory)?;
    let scope = serde_json::to_string(&r.scope).map_err(|_| Error::Input)?;
    let row:Option<(String,i64)>=con.query_row("SELECT state,observed_at FROM hook_observations WHERE scope=? ORDER BY seq DESC LIMIT 1",[scope],|row|Ok((row.get(0)?,row.get(1)?))).optional().map_err(|_|Error::Persistence)?;
    let Some((state, time)) = row else {
        return Ok(Readiness::Unknown);
    };
    if time < 0 || now < (time as u64) || now - (time as u64) > max_age_ms {
        return Ok(Readiness::Unknown);
    }
    serde_json::from_str(&state).map_err(|_| Error::Persistence)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_ticks: u64,
}
/// Explicit local OpenCode server identity. This authorizes a configured emitter
/// under the same Unix account; it does not authenticate arbitrary remote servers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerEmitter {
    pub process: ProcessIdentity,
    pub executable: PathBuf,
    pub device: u64,
    pub inode: u64,
}
#[cfg(unix)]
pub fn capture_server_emitter(
    process: ProcessIdentity,
    executable: &Path,
    proc_root: &Path,
) -> Result<ServerEmitter> {
    layouts::check_path(executable).map_err(|_| Error::PrivateState)?;
    let meta = fs::metadata(executable).map_err(|_| Error::Identity)?;
    let pin = ServerEmitter {
        process,
        executable: executable.to_path_buf(),
        device: meta.dev(),
        inode: meta.ino(),
    };
    verify_server_emitter(&pin, proc_root)?;
    Ok(pin)
}
#[cfg(windows)]
pub fn capture_server_emitter(
    process: ProcessIdentity,
    executable: &Path,
    proc_root: &Path,
) -> Result<ServerEmitter> {
    let file = crate::platform::open_regular(executable).map_err(|_| Error::Identity)?;
    let (device, inode) = crate::platform::file_identity(&file).map_err(|_| Error::Identity)?;
    let pin = ServerEmitter {
        process,
        executable: executable.to_path_buf(),
        device,
        inode,
    };
    verify_server_emitter(&pin, proc_root)?;
    Ok(pin)
}
#[cfg(unix)]
pub fn verify_server_emitter(server: &ServerEmitter, proc_root: &Path) -> Result<()> {
    if server.process.pid == 0 || server.process.start_ticks == 0 {
        return Err(Error::Identity);
    }
    layouts::check_path(&server.executable).map_err(|_| Error::PrivateState)?;
    let mut executable = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&server.executable)
        .map_err(|_| Error::Identity)?;
    let meta = executable.metadata().map_err(|_| Error::Identity)?;
    let process_dir = proc_root.join(server.process.pid.to_string());
    if fs::metadata(&process_dir)
        .map_err(|_| Error::Identity)?
        .uid()
        != unsafe { libc::geteuid() }
        || !meta.is_file()
        || meta.mode() & 0o022 != 0
        || meta.mode() & 0o111 == 0
        || ![0, unsafe { libc::geteuid() }].contains(&meta.uid())
        || meta.dev() != server.device
        || meta.ino() != server.inode
    {
        return Err(Error::Identity);
    }
    let mut magic = [0; 4];
    executable
        .read_exact(&mut magic)
        .map_err(|_| Error::Identity)?;
    if &magic != b"\x7fELF" {
        return Err(Error::Invalid(
            "OpenCode server requires a pinned native executable",
        ));
    }
    let live = fs::metadata(process_dir.join("exe")).map_err(|_| Error::Identity)?;
    if live.dev() != server.device || live.ino() != server.inode {
        return Err(Error::Identity);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(process_dir.join("stat"))
        .map_err(|_| Error::Identity)?;
    let mut bytes = Vec::new();
    file.take(8193)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Identity)?;
    if bytes.len() > 8192 {
        return Err(Error::Identity);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| Error::Identity)?;
    let fields = text
        .rsplit_once(')')
        .ok_or(Error::Identity)?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    if fields
        .first()
        .is_none_or(|s| matches!(*s, "Z" | "X" | "T" | "t"))
        || fields.get(19).and_then(|s| s.parse::<u64>().ok()) != Some(server.process.start_ticks)
    {
        return Err(Error::Identity);
    }
    Ok(())
}
#[cfg(windows)]
pub fn verify_server_emitter(server: &ServerEmitter, proc_root: &Path) -> Result<()> {
    verify_process(&server.process, proc_root)?;
    let observed =
        crate::platform::process_identity(server.process.pid).map_err(|_| Error::Identity)?;
    let mut pinned =
        crate::platform::open_regular(&server.executable).map_err(|_| Error::Identity)?;
    let running =
        crate::platform::open_regular(&observed.executable).map_err(|_| Error::Identity)?;
    let expected = (server.device, server.inode);
    if crate::platform::file_identity(&pinned).map_err(|_| Error::Identity)? != expected
        || crate::platform::file_identity(&running).map_err(|_| Error::Identity)? != expected
    {
        return Err(Error::Identity);
    }
    let mut magic = [0; 2];
    pinned.read_exact(&mut magic).map_err(|_| Error::Identity)?;
    if magic != *b"MZ" {
        return Err(Error::Invalid(
            "OpenCode server requires a pinned native executable",
        ));
    }
    verify_process(&server.process, proc_root)
}
/// Linux process incarnation, supplied by trusted setup after launch/discovery.
#[cfg(unix)]
pub fn verify_process(process: &ProcessIdentity, proc_root: &Path) -> Result<()> {
    if process.pid == 0 || process.start_ticks == 0 {
        return Err(Error::Identity);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(proc_root.join(process.pid.to_string()).join("stat"))
        .map_err(|_| Error::Identity)?;
    let mut bytes = Vec::new();
    file.take(8193)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Identity)?;
    if bytes.len() > 8192 {
        return Err(Error::Identity);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| Error::Identity)?;
    let fields = text
        .rsplit_once(')')
        .ok_or(Error::Identity)?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    if fields
        .first()
        .is_some_and(|s| matches!(*s, "Z" | "X" | "T" | "t"))
        || !fields
            .get(2)
            .and_then(|s| s.parse::<i64>().ok())
            .is_some_and(|group| {
                group > 0 && fields.get(5).and_then(|s| s.parse::<i64>().ok()) == Some(group)
            })
        || fields.get(19).and_then(|s| s.parse::<u64>().ok()) != Some(process.start_ticks)
    {
        return Err(Error::Identity);
    }
    Ok(())
}
#[cfg(windows)]
#[allow(unused_variables)]
pub fn verify_process(process: &ProcessIdentity, proc_root: &Path) -> Result<()> {
    let observed = crate::platform::process_identity(process.pid).map_err(|_| Error::Identity)?;
    if process.start_ticks == 0
        || observed.creation_time != process.start_ticks
        || observed.owner != crate::platform::current_identity().map_err(|_| Error::Identity)?
    {
        return Err(Error::Identity);
    }
    Ok(())
}
#[derive(Clone, Debug, Serialize)]
pub struct ReadySnapshot {
    pub sequence: i64,
    pub state: Readiness,
    pub scope: Scope,
}
// Revalidate native bootstrap evidence each time it could authorize input.
// A later provider log write invalidates it even if no hook was delivered.
fn native_observation_valid(
    con: &Connection,
    r: &Registration,
    proc_root: &Path,
    sequence: i64,
) -> Result<bool> {
    let evidence: Option<(String, String)> = con
        .query_row(
            "SELECT event_id,payload FROM hook_observations WHERE seq=? AND scope=?",
            params![
                sequence,
                serde_json::to_string(&r.scope).map_err(|_| Error::Input)?
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|_| Error::Persistence)?;
    let Some((event, proof)) = evidence else {
        return Ok(false);
    };
    if !event.starts_with("native-grok:") {
        return Ok(true);
    }
    Ok(matches!(super::grok_state::idle_proof(r,proc_root), Ok(Some(current)) if current==proof))
}
/// Persistent Idle requires a pinned live harness process, not just a terminal
/// shell. Caller must independently verify terminal identity before injection.
pub fn ready_snapshot(path: &Path, proc_root: &Path) -> Result<ReadySnapshot> {
    let r = inspect(path)?;
    let _active = active_guard(&r.directory)?;
    if let Some(server) = &r.server_emitter {
        verify_server_emitter(server, proc_root)?;
    }
    verify_process(
        r.harness_process.as_ref().ok_or(Error::Identity)?,
        proc_root,
    )?;
    let con = readiness_connection(&r.directory)?;
    let scope = serde_json::to_string(&r.scope).map_err(|_| Error::Input)?;
    let row: Option<(i64, String)> = con
        .query_row(
            "SELECT seq,state FROM hook_observations WHERE scope=? ORDER BY seq DESC LIMIT 1",
            [scope],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|_| Error::Persistence)?;
    // Existing Grok sessions can predate hook installation. Bootstrap only
    // from an exact, still-open native event log; this is source evidence,
    // not a synthesized hook or a successful Telegram delivery.
    if row.is_none()
        && let Ok(Some(proof)) = super::grok_state::idle_proof(&r, proc_root)
    {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_millis()).ok())
            .ok_or(Error::Input)?;
        let event = format!("native-grok:{}:{proof}", r.id);
        con.execute(
            "INSERT OR IGNORE INTO hook_observations(scope,event_id,payload,state,observed_at,finalized) SELECT ?1,?2,?3,'\"idle\"',?4,1 WHERE NOT EXISTS(SELECT 1 FROM hook_observations WHERE scope=?1) AND NOT EXISTS(SELECT 1 FROM hook_reservations WHERE scope=?1)",
            params![serde_json::to_string(&r.scope).map_err(|_|Error::Input)?, event, proof, now],
        ).map_err(|_|Error::Persistence)?;
        let row: Option<(i64, String)> = con
            .query_row(
                "SELECT seq,state FROM hook_observations WHERE scope=? ORDER BY seq DESC LIMIT 1",
                [serde_json::to_string(&r.scope).map_err(|_| Error::Input)?],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|_| Error::Persistence)?;
        return Ok(ReadySnapshot {
            sequence: row.as_ref().map_or(0, |(seq, _)| *seq),
            state: row
                .map(|(_, state)| serde_json::from_str(&state).map_err(|_| Error::Persistence))
                .transpose()?
                .unwrap_or(Readiness::Unknown),
            scope: r.scope,
        });
    }
    let (sequence, state) = match row {
        Some((seq, s)) => (
            seq,
            serde_json::from_str(&s).map_err(|_| Error::Persistence)?,
        ),
        None => (0, Readiness::Unknown),
    };
    let state =
        if state == Readiness::Idle && !native_observation_valid(&con, &r, proc_root, sequence)? {
            Readiness::Unknown
        } else {
            state
        };
    Ok(ReadySnapshot {
        sequence,
        state,
        scope: r.scope,
    })
}
/// Atomically consume an idle observation. Never release to Idle after uncertain
/// writes; a subsequent verified lifecycle event must establish state again.
pub fn consume_ready_for_control(path: &Path, proc_root: &Path, sequence: i64) -> Result<bool> {
    let r = inspect(path)?;
    let _active = active_guard(&r.directory)?;
    verify_process(
        r.harness_process.as_ref().ok_or(Error::Identity)?,
        proc_root,
    )?;
    let mut con = readiness_connection(&r.directory)?;
    let scope = serde_json::to_string(&r.scope).map_err(|_| Error::Input)?;
    let tx = con
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| Error::Persistence)?;
    if !native_observation_valid(&tx, &r, proc_root, sequence)? {
        return Ok(false);
    }
    // An interactive control is not a model prompt. Invalidate the observation
    // without inventing a prompt hash that no provider event could ever satisfy.
    let changed = tx.execute("UPDATE hook_observations SET state='\"unknown\"' WHERE seq=? AND scope=? AND state='\"idle\"' AND seq=(SELECT MAX(seq) FROM hook_observations WHERE scope=?) AND NOT EXISTS(SELECT 1 FROM hook_reservations WHERE scope=?)", params![sequence,scope,scope,scope]).map_err(|_| Error::Persistence)?;
    tx.commit().map_err(|_| Error::Persistence)?;
    Ok(changed == 1)
}

pub fn reserve_ready(path: &Path, proc_root: &Path, sequence: i64) -> Result<bool> {
    reserve_ready_for(
        path,
        proc_root,
        sequence,
        &format!("daedalus-reservation:{sequence}"),
    )
}
pub fn reserve_ready_for(
    path: &Path,
    proc_root: &Path,
    sequence: i64,
    correlation: &str,
) -> Result<bool> {
    if correlation.len() < 16 || correlation.len() > 64000 || correlation.contains('\0') {
        return Err(Error::Input);
    }
    let r = inspect(path)?;
    let _active = active_guard(&r.directory)?;
    if let Some(server) = &r.server_emitter {
        verify_server_emitter(server, proc_root)?;
    }
    verify_process(
        r.harness_process.as_ref().ok_or(Error::Identity)?,
        proc_root,
    )?;
    let mut con = readiness_connection(&r.directory)?;
    let scope = serde_json::to_string(&r.scope).map_err(|_| Error::Input)?;
    let tx = con
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| Error::Persistence)?;
    if !native_observation_valid(&tx, &r, proc_root, sequence)? {
        return Ok(false);
    }
    let n=tx.execute("UPDATE hook_observations SET state='\"busy\"' WHERE seq=? AND scope=? AND state='\"idle\"' AND seq=(SELECT MAX(seq) FROM hook_observations WHERE scope=?) AND NOT EXISTS(SELECT 1 FROM hook_reservations WHERE scope=?)",params![sequence,scope,scope,scope]).map_err(|_|Error::Persistence)?;
    if n == 1 {
        tx.execute(
            "INSERT INTO hook_reservations(scope,correlation) VALUES(?,?)",
            params![
                scope,
                format!("{:x}", Sha256::digest(correlation.as_bytes()))
            ],
        )
        .map_err(|_| Error::Persistence)?;
    }
    tx.commit().map_err(|_| Error::Persistence)?;
    Ok(n == 1)
}

/// Add native conversation metadata only from an unambiguous registration whose
/// exact terminal generation and harness process are still verified. This is
/// discovery evidence, not readiness: feeders must still reserve an Idle hook.
pub fn enrich_native_sessions(
    directory: &Path,
    snapshot: &mut Value,
    terminals: &Terminals,
) -> Result<usize> {
    // A reused snapshot must not retain a conversation after its evidence expires.
    apply_native_sessions(snapshot, &[]);
    if !snapshot["panes"]
        .as_array()
        .is_some_and(|panes| panes.iter().any(|pane| pane["backend"] == "native"))
    {
        return Ok(0);
    }
    let entries = match fs::read_dir(directory.join("hooks")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(_) => return Err(Error::PrivateState),
    };
    let mut verified = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= 4096 {
            return Err(Error::Invalid("Hook registration limit exceeded"));
        }
        let path = entry.map_err(|_| Error::PrivateState)?.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let registration = inspect(&path)?;
        if registration.directory != directory {
            return Err(Error::Identity);
        }
        if registration.scope.terminal.backend == crate::core::identity::Backend::Native
            && registration.harness_process.is_some()
            && verify_live_session(&registration, terminals).is_ok()
        {
            verified.push(registration.scope);
        }
    }
    Ok(apply_native_sessions(snapshot, &verified))
}
fn apply_native_sessions(snapshot: &mut Value, verified: &[Scope]) -> usize {
    let Some(panes) = snapshot["panes"].as_array_mut() else {
        return 0;
    };
    let mut enriched = 0;
    for pane in panes {
        if pane["backend"] != "native" {
            continue;
        }
        pane["agent_session"] = Value::Null;
        let Ok(identity) = crate::adapters::terminals::identity(pane) else {
            continue;
        };
        let mut matches = verified.iter().filter(|scope| scope.terminal == identity);
        let Some(scope) = matches.next() else {
            continue;
        };
        // Even identical duplicates may refer to independently configured hooks.
        // Match the feeder's rule: one live registration per terminal.
        if matches.next().is_some() {
            continue;
        }
        pane["agent"] = json!(scope.harness);
        pane["agent_session"] =
            json!({"kind":"id","agent":scope.harness,"value":scope.conversation});
        enriched += 1;
    }
    enriched
}

/// Daemon-side live verification deliberately does not require daemon ancestry.
/// Only registrations with a pinned harness process can be feeder destinations.
pub fn verify_live_session(registration: &Registration, terminals: &Terminals) -> Result<Value> {
    registration.validate()?;
    if let Some(server) = &registration.server_emitter {
        verify_server_emitter(server, &terminals.proc_root)?;
    }
    if let Some(process) = &registration.harness_process {
        verify_process(process, &terminals.proc_root)?;
    }
    let id = &registration.scope.terminal;
    let expected = json!({"backend":id.backend,"server":id.endpoint,"socket":id.endpoint,"session":id.endpoint,"pane_id":id.pane,"terminal_id":id.generation});
    let actual = terminals.verify(&expected).map_err(|_| Error::Identity)?;
    if let Some(process) = &registration.harness_process {
        let terminal_pid = actual
            .get("pid")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .and_then(|n| u32::try_from(n).ok())
            .ok_or(Error::Identity)?;
        #[cfg(unix)]
        {
            let tty = process_tty(process.pid, &terminals.proc_root)?;
            if tty == 0 || tty != process_tty(terminal_pid, &terminals.proc_root)? {
                return Err(Error::Identity);
            }
        }
        #[cfg(windows)]
        {
            windows_descendant(process.pid, terminal_pid)?;
            match id.backend {
                crate::core::identity::Backend::Herdr => {
                    let info = terminals
                        .herdr_result(&id.endpoint, &["pane", "process-info", "--pane", &id.pane])
                        .map_err(|_| Error::Identity)?;
                    let foreground = info["process_info"]["foreground_processes"]
                        .as_array()
                        .ok_or(Error::Identity)?;
                    if !foreground
                        .iter()
                        .any(|p| p["pid"].as_u64() == Some(u64::from(process.pid)))
                    {
                        return Err(Error::Identity);
                    }
                }
                crate::core::identity::Backend::Native => {
                    // ConPTY does not expose Unix foreground process groups.
                    // Only a harness launched directly as this exact PTY child
                    // has a verifiable input owner. Shell descendants alone do
                    // not prove which process will receive a submitted prompt.
                    if process.pid != terminal_pid {
                        return Err(Error::Invalid(
                            "Launch the harness directly in a Daedalus native terminal to verify Windows Telegram input ownership",
                        ));
                    }
                }
                crate::core::identity::Backend::Tmux => {
                    return Err(Error::Invalid(
                        "Native tmux foreground process verification is not available",
                    ));
                }
            }
            verify_process(process, &terminals.proc_root)?;
        }
    }
    Ok(actual)
}
#[cfg(unix)]
fn process_tty(pid: u32, proc_root: &Path) -> Result<i64> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(proc_root.join(pid.to_string()).join("stat"))
        .map_err(|_| Error::Identity)?;
    let mut bytes = Vec::new();
    file.take(8193)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Identity)?;
    if bytes.len() > 8192 {
        return Err(Error::Identity);
    }
    std::str::from_utf8(&bytes)
        .ok()
        .and_then(|s| s.rsplit_once(')'))
        .and_then(|(_, s)| s.split_whitespace().nth(4))
        .and_then(|s| s.parse().ok())
        .ok_or(Error::Identity)
}

/// Claude transcript ancestry is required before a fed turn can become Idle.
/// Missing/truncated/foreign/ambiguous evidence leaves the reservation in place.
fn transcript_proves_reply(
    r: &Registration,
    prompt_sha: &str,
    final_text: Option<&str>,
) -> Result<bool> {
    let Some(final_text) = final_text else {
        return Ok(false);
    };
    if r.scope.harness != Harness::Claude {
        return Ok(false);
    }
    let Some(path) = &r.transcript else {
        return Ok(false);
    };
    let mut file = native_transcript(path)?;
    let length = file.metadata().map_err(|_| Error::PrivateState)?.len();
    let offset = length.saturating_sub(8 * 1024 * 1024);
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| Error::PrivateState)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(length - offset)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::PrivateState)?;
    if file.metadata().map_err(|_| Error::PrivateState)?.len() != length {
        return Ok(false);
    }
    let mut lines = bytes.split(|b| *b == b'\n');
    if offset > 0 {
        lines.next();
    }
    let mut parents = std::collections::HashMap::new();
    let mut user = None;
    let mut assistant = None;
    for line in lines {
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if v.get("sessionId")
            .or_else(|| v.get("session_id"))
            .and_then(Value::as_str)
            != Some(r.scope.conversation.as_str())
        {
            continue;
        }
        let Some(id) = v.get("uuid").and_then(Value::as_str) else {
            continue;
        };
        let parent = v
            .get("parentUuid")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if parents.insert(id.to_owned(), parent).is_some() {
            return Ok(false);
        }
        if v["type"] == "user" {
            let content = v.pointer("/message/content");
            let text = content.and_then(Value::as_str).or_else(|| {
                content
                    .and_then(Value::as_array)
                    .filter(|a| a.len() == 1)
                    .and_then(|a| a[0].get("text"))
                    .and_then(Value::as_str)
            });
            assistant = None;
            if text.is_some_and(|s| format!("{:x}", Sha256::digest(s.as_bytes())) == prompt_sha) {
                user = Some(id.to_owned());
            } else if !content.and_then(Value::as_array).is_some_and(|blocks| {
                !blocks.is_empty() && blocks.iter().all(|block| block["type"] == "tool_result")
            }) {
                user = None;
            }
        }
        if v["type"] == "assistant" {
            let content = v.pointer("/message/content");
            let text = content
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    content
                        .and_then(Value::as_array)
                        .filter(|blocks| blocks.iter().all(|b| b["type"] == "text"))
                        .map(|blocks| {
                            blocks
                                .iter()
                                .filter_map(|b| b["text"].as_str())
                                .collect::<String>()
                        })
                });
            assistant = if text.as_deref() == Some(final_text)
                && v.pointer("/message/stop_reason").and_then(Value::as_str) != Some("tool_use")
            {
                Some(id.to_owned())
            } else {
                None
            };
        }
    }
    let (Some(user), Some(mut cursor)) = (user, assistant) else {
        return Ok(false);
    };
    for _ in 0..4096 {
        let Some(Some(parent)) = parents.get(&cursor) else {
            return Ok(false);
        };
        if *parent == user {
            return Ok(true);
        }
        cursor = parent.clone();
    }
    Ok(false)
}

#[cfg(windows)]
fn windows_descendant(mut child: u32, ancestor: u32) -> Result<()> {
    let owner = crate::platform::current_identity().map_err(|_| Error::Identity)?;
    let mut youngest = u64::MAX;
    for _ in 0..128 {
        let process = crate::platform::process_identity(child).map_err(|_| Error::Identity)?;
        if process.owner != owner || process.creation_time > youngest {
            return Err(Error::Identity);
        }
        if child == ancestor {
            return Ok(());
        }
        if process.parent_pid == 0 || process.parent_pid == child {
            return Err(Error::Identity);
        }
        youngest = process.creation_time;
        child = process.parent_pid;
    }
    Err(Error::Identity)
}
#[cfg(windows)]
impl Verifier for NativeVerifier {
    fn verify(&mut self, registration: &Registration) -> Result<()> {
        verify_live_session(registration, &self.terminals)?;
        let process = registration
            .harness_process
            .as_ref()
            .ok_or(Error::Identity)?;
        let emitter = registration
            .server_emitter
            .as_ref()
            .map(|server| &server.process)
            .unwrap_or(process);
        windows_descendant(std::process::id(), emitter.pid)?;
        verify_process(emitter, &self.terminals.proc_root)
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use crate::core::identity::{Backend, TerminalId};
    #[test]
    fn windows_registration_storage_and_incarnation_are_verified() {
        let directory =
            std::env::temp_dir().join(format!("daedalus-hooks-windows-{}", std::process::id()));
        crate::platform::ensure_private_dir(&directory).unwrap();
        let observed = crate::platform::process_identity(std::process::id()).unwrap();
        let process = ProcessIdentity {
            pid: observed.pid,
            start_ticks: observed.creation_time,
        };
        verify_process(&process, Path::new("unused")).unwrap();
        assert!(
            verify_process(
                &ProcessIdentity {
                    pid: process.pid,
                    start_ticks: process.start_ticks + 1
                },
                Path::new("unused")
            )
            .is_err()
        );
        let registration = Registration {
            version: 1,
            id: "fixture".into(),
            directory: directory.clone(),
            scope: Scope {
                instance: "fixture".into(),
                terminal: TerminalId {
                    backend: Backend::Herdr,
                    endpoint: "fixture".into(),
                    pane: "pane".into(),
                    generation: "generation".into(),
                },
                harness: Harness::Claude,
                conversation: "session".into(),
            },
            goal_id: None,
            harness_process: Some(process),
            server_emitter: None,
            transcript: None,
            events: vec![EventKind::SessionStart, EventKind::Stop],
            options: Options::default(),
            activity_lease_ms: 60_000,
            pending_timeout_ms: 30_000,
            hook_timeout_ms: 10_000,
        };
        let path = install(&registration).unwrap();
        assert_eq!(inspect(&path).unwrap().id, "fixture");
        let lock =
            crate::platform::open_private(&directory.join(".cutover.lock"), true, false).unwrap();
        crate::platform::lock_exclusive(&lock, true).unwrap();
        assert!(active_guard(&directory).is_err());
        crate::platform::unlock(&lock).unwrap();
        drop(lock);
        let connection = private_connection(&directory, "fixture.sqlite3").unwrap();
        connection
            .execute_batch("CREATE TABLE fixture(id INTEGER);")
            .unwrap();
        drop(connection);
        fs::remove_dir_all(directory).unwrap();
    }
}
