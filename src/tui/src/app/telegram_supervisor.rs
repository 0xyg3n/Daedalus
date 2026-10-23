//! Native service process ownership. Status comes from an owned lock and an
//! exact process incarnation; a PID file by itself is never running evidence.
use super::telegram_config::{self, ServiceConfig};
use crate::core::layouts;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::{
    fs::{self, OpenOptions},
    io::Read,
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawFd,
    },
};
pub type Result<T> = std::result::Result<T, String>;
const STATE: &str = "telegram-runtime.json";
/// The installed file is not the image this process launched.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn image_was_replaced(running: (u64, u64), installed: Option<(u64, u64)>) -> bool {
    matches!(installed, Some(identity) if identity != running)
}
/// A byte-range lock Windows does not honor must not hide a live mapped image.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn service_blocks_replace(lock_visible: bool, live_image_matches: bool) -> bool {
    lock_visible || live_image_matches
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Starting,
    Running,
    Stopping,
    Stopped,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub version: u32,
    pub pid: u32,
    pub start_ticks: u64,
    pub boot_id: String,
    pub executable: PathBuf,
    pub directory: PathBuf,
    pub instance: String,
    pub bot_id: i64,
    pub phase: Phase,
}
#[derive(Clone, Debug, Serialize)]
pub struct Observation {
    pub state: &'static str,
    pub process: Option<Record>,
}
#[cfg(unix)]
fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| "Service process evidence unavailable")?;
    if file
        .metadata()
        .map_err(|_| "Service process metadata unavailable")?
        .uid()
        != unsafe { libc::geteuid() }
    {
        return Err("Service process is owned by another user".into());
    }
    let mut bytes = Vec::new();
    file.take(8193)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read service process evidence")?;
    if bytes.len() > 8192 {
        return Err("Service process evidence exceeds limit".into());
    }
    Ok(bytes)
}
#[cfg(unix)]
fn incarnation(proc_root: &Path, pid: u32) -> Result<(u64, bool)> {
    if pid == 0 {
        return Err("Invalid service process".into());
    }
    let bytes = read_bounded(&proc_root.join(pid.to_string()).join("stat"))?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "Invalid service process stat")?;
    let fields = text
        .rsplit_once(')')
        .ok_or("Invalid service process stat")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let ticks = fields
        .get(19)
        .and_then(|s| s.parse().ok())
        .filter(|ticks| *ticks > 0)
        .ok_or("Missing service incarnation")?;
    let alive = fields.first().is_some_and(|s| !matches!(*s, "Z" | "X"));
    Ok((ticks, alive))
}
#[cfg(unix)]
fn boot(proc_root: &Path) -> Result<String> {
    // boot_id is kernel-owned rather than process-owned.
    let value = fs::read_to_string(proc_root.join("sys/kernel/random/boot_id"))
        .map_err(|_| "Cannot read system boot identity")?;
    let value = value.trim();
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err("Invalid system boot identity".into());
    }
    Ok(value.into())
}
#[cfg(unix)]
fn verify(record: &Record, proc_root: &Path) -> Result<()> {
    if record.version != 1 || record.boot_id != boot(proc_root)? {
        return Err("Service belongs to another system boot".into());
    }
    let (ticks, alive) = incarnation(proc_root, record.pid)?;
    if ticks != record.start_ticks || !alive {
        return Err("Service process is no longer live".into());
    }
    let actual = fs::read_link(proc_root.join(record.pid.to_string()).join("exe"))
        .map_err(|_| "Cannot verify service executable")?;
    if actual != record.executable {
        return Err("Service executable changed".into());
    }
    let bytes = read_bounded(&proc_root.join(record.pid.to_string()).join("cmdline"))?;
    let args = bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    use std::os::unix::ffi::OsStrExt;
    if args.len() != 5
        || args[1] != b"telegram"
        || args[2] != b"serve"
        || args[3] != b"--config-dir"
        || args[4] != record.directory.as_os_str().as_bytes()
    {
        return Err("Service command does not match this instance".into());
    }
    Ok(())
}
#[cfg(unix)]
fn held(directory: &Path) -> Result<bool> {
    let path = directory.join(".telegram-service.lock");
    layouts::check_path(&path)?;
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err("Cannot inspect service ownership".into()),
    };
    let meta = file
        .metadata()
        .map_err(|_| "Cannot inspect service ownership")?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.nlink() != 1
        || meta.mode() & 0o077 != 0
    {
        return Err("Unsafe service owner lock".into());
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(false);
    }
    if std::io::Error::last_os_error().raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(true)
    } else {
        Err("Cannot determine service ownership".into())
    }
}
pub fn inspect(directory: &Path) -> Result<Observation> {
    inspect_at(directory, Path::new("/proc"))
}
pub(crate) fn inspect_at(directory: &Path, proc_root: &Path) -> Result<Observation> {
    let occupied = held(directory)?;
    if !occupied {
        return Ok(Observation {
            state: "stopped",
            process: None,
        });
    }
    let Some(bytes) = telegram_config::read_private(&directory.join(STATE), 8192)? else {
        return Ok(Observation {
            state: "starting",
            process: None,
        });
    };
    let record: Record =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid private service record")?;
    if record.directory != directory || verify(&record, proc_root).is_err() {
        return Ok(Observation {
            state: "unknown",
            process: None,
        });
    }
    let state = match record.phase {
        Phase::Starting => "starting",
        Phase::Running => "running",
        Phase::Stopping => "stopping",
        Phase::Stopped => "stopping",
    };
    Ok(Observation {
        state,
        process: Some(record),
    })
}
/// Called by the service itself only while its Owner guards remain held.
#[cfg(unix)]
pub fn record(directory: &Path, config: &ServiceConfig, phase: Phase) -> Result<()> {
    let pid = std::process::id();
    let proc_root = Path::new("/proc");
    let (start_ticks, alive) = incarnation(proc_root, pid)?;
    if !alive {
        return Err("Service process is not live".into());
    }
    let value = Record {
        version: 1,
        pid,
        start_ticks,
        boot_id: boot(proc_root)?,
        executable: std::env::current_exe().map_err(|_| "Cannot inspect service executable")?,
        directory: directory.into(),
        instance: config.instance.clone(),
        bot_id: config.bot_id,
        phase,
    };
    layouts::write_private(
        &directory.join(STATE),
        &serde_json::to_vec(&value).map_err(|_| "Cannot encode service state")?,
    )
}
#[cfg(unix)]
pub fn stop(directory: &Path, expected: &Record) -> Result<Value> {
    use std::os::fd::{FromRawFd, OwnedFd};
    if expected.directory != directory {
        return Err("Stop request belongs to another instance".into());
    }
    let observed = inspect(directory)?;
    if observed.process.as_ref() != Some(expected) {
        return Err("Service changed; review the stop request again".into());
    }
    // Pin the kernel process before the final incarnation check: never signal a
    // recycled PID. Unsupported kernels fail closed rather than using kill(pid).
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, expected.pid, 0) };
    if raw < 0 {
        return Err("Cannot pin service process for stop".into());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    verify(expected, Path::new("/proc"))?;
    if !held(directory)? {
        return Err("Service already released ownership".into());
    }
    if unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            libc::SIGTERM,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    } < 0
    {
        return Err("Service stop was not accepted by the kernel".into());
    }
    Ok(
        json!({"state":"submitted","operation":"stop","message":"Stop requested. Running state remains observed until the process exits."}),
    )
}
#[cfg(unix)]
pub fn start(directory: &Path) -> Result<Value> {
    use std::os::unix::process::CommandExt;
    let config = ServiceConfig::load(&directory.join("telegram-service.json"))?;
    config.read_token()?;
    config.check_registrations()?;
    super::cutover::activation_check(directory)?;
    if inspect(directory)?.state != "stopped" {
        return Err("Service already owns or may be starting for this instance".into());
    }
    let path = directory.join("telegram-service.log");
    layouts::check_path(&path)?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|_| "Cannot open private service log")?;
    let meta = log.metadata().map_err(|_| "Cannot inspect service log")?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || meta.nlink() != 1
    {
        return Err("Service log must be a private owned regular file".into());
    }
    let mut command = std::process::Command::new(
        std::env::current_exe().map_err(|_| "Cannot locate native service executable")?,
    );
    command
        .args(["telegram", "serve", "--config-dir"])
        .arg(directory)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone().map_err(|_| "Cannot attach service log")?)
        .stderr(log);
    // Only generic tool-discovery/session environment is inherited. Bot tokens
    // and account secrets are read from private files by their owning adapter.
    command.env_clear();
    for key in [
        "HOME",
        "PATH",
        "USER",
        "LANG",
        "LC_ALL",
        "XDG_RUNTIME_DIR",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "TMUX_TMPDIR",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = command
        .spawn()
        .map_err(|_| "Cannot start native Telegram service")?;
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(
        json!({"state":"submitted","operation":"start","pid":pid,"message":"Start requested. Service ownership and transport identity are checked separately."}),
    )
}

#[cfg(windows)]
fn verify(record: &Record, _: &Path) -> Result<()> {
    verify_windows_executable(
        record,
        &std::env::current_exe().map_err(|_| "Cannot inspect native executable")?,
    )
}
#[cfg(windows)]
fn verify_windows_executable(record: &Record, executable: &Path) -> Result<()> {
    let process = crate::platform::process_identity(record.pid)
        .map_err(|_| "Service process is unavailable")?;
    let owner =
        crate::platform::current_identity().map_err(|_| "Cannot verify current identity")?;
    if record.version != 1
        || record.boot_id != owner
        || process.owner != owner
        || record.start_ticks != process.creation_time
        || record.executable != process.executable
        || crate::platform::file_identity(
            &crate::platform::open_regular(&record.executable)
                .map_err(|_| "Cannot inspect service executable")?,
        )
        .map_err(|_| "Cannot identify service executable")?
            != crate::platform::file_identity(
                &crate::platform::open_regular(executable)
                    .map_err(|_| "Cannot inspect expected executable")?,
            )
            .map_err(|_| "Cannot identify expected executable")?
    {
        return Err("Service process identity changed".into());
    }
    Ok(())
}
#[cfg(windows)]
fn held(directory: &Path) -> Result<bool> {
    let path = directory.join(".telegram-service.lock");
    super::telegram_service::probe_owner(&path)
        .map_err(|_| "Cannot inspect service ownership".into())
}
#[cfg(windows)]
pub fn record(directory: &Path, config: &ServiceConfig, phase: Phase) -> Result<()> {
    let process = crate::platform::process_identity(std::process::id())
        .map_err(|_| "Cannot inspect service process")?;
    let owner = crate::platform::current_identity().map_err(|_| "Cannot verify service owner")?;
    if process.owner != owner {
        return Err("Service owner changed".into());
    }
    let value = Record {
        version: 1,
        pid: process.pid,
        start_ticks: process.creation_time,
        boot_id: owner,
        executable: process.executable,
        directory: directory.into(),
        instance: config.instance.clone(),
        bot_id: config.bot_id,
        phase,
    };
    layouts::write_private(
        &directory.join(STATE),
        &serde_json::to_vec(&value).map_err(|_| "Cannot encode service state")?,
    )
}
#[cfg(windows)]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StopRequest {
    pid: u32,
    start_ticks: u64,
    instance: String,
    directory: PathBuf,
}
#[cfg(windows)]
pub fn stop(directory: &Path, expected: &Record) -> Result<Value> {
    if expected.directory != directory || inspect(directory)?.process.as_ref() != Some(expected) {
        return Err("Service changed; review stop again".into());
    }
    verify(expected, Path::new(""))?;
    if !held(directory)? {
        return Err("Service already released ownership".into());
    }
    write_stop_request(directory, expected)?;
    Ok(
        json!({"state":"submitted","operation":"stop","message":"Cooperative stop requested. Ownership remains observed until shutdown finishes."}),
    )
}
#[cfg(windows)]
fn write_stop_request(directory: &Path, expected: &Record) -> Result<()> {
    let request = StopRequest {
        pid: expected.pid,
        start_ticks: expected.start_ticks,
        instance: expected.instance.clone(),
        directory: directory.into(),
    };
    layouts::write_private(
        &directory.join("telegram-stop.json"),
        &serde_json::to_vec(&request).map_err(|_| "Cannot encode stop request")?,
    )
}
/// Only the installer uses an explicit, receipt-verified executable. Ordinary
/// service controls still require the caller's own executable identity.
#[cfg(windows)]
pub(crate) fn stop_for_uninstall(
    directory: &Path,
    executable: &Path,
    timeout: std::time::Duration,
) -> Result<()> {
    let lock_visible = held(directory)?;
    let orphan = if lock_visible {
        None
    } else {
        // A whole-file Windows lock can succeed without excluding anyone.
        // The live process still maps the executable until it exits.
        live_service_holding_image(directory, executable)?
    };
    if !service_blocks_replace(lock_visible, orphan.is_some()) {
        return Ok(());
    }
    if let Some(record) = orphan {
        return shutdown_pinned(directory, &record, timeout, |record| {
            verify_windows_executable(record, executable)
        });
    }
    let bytes = telegram_config::read_private(&directory.join(STATE), 8192)?
        .ok_or("Telegram is starting; wait and retry uninstall")?;
    let record: Record =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid private service record")?;
    let config = ServiceConfig::load(&directory.join("telegram-service.json"))?;
    if record.directory != directory
        || record.instance != config.instance
        || record.bot_id != config.bot_id
    {
        return Err("Telegram instance identity changed; installation has been kept".into());
    }
    verify_windows_executable(&record, executable)?;
    shutdown_pinned(directory, &record, timeout, |record| {
        verify_windows_executable(record, executable)
    })
}
#[cfg(windows)]
fn shutdown_pinned(
    directory: &Path,
    record: &Record,
    timeout: std::time::Duration,
    confirm: impl Fn(&Record) -> Result<()>,
) -> Result<()> {
    let process = crate::platform::pin_owned_process(record.pid, record.start_ticks)
        .map_err(|_| "Cannot pin Telegram service for shutdown")?;
    confirm(record)?;
    write_stop_request(directory, record)?;
    if !process
        .wait_timeout(timeout)
        .map_err(|_| "Cannot observe Telegram shutdown")?
        || held(directory)?
    {
        return Err("Telegram has not finished shutting down; installation has been kept. Retry after shutdown completes.".into());
    }
    Ok(())
}
#[cfg(windows)]
fn live_service_holding_image(directory: &Path, executable: &Path) -> Result<Option<Record>> {
    let Some(record) = live_service(directory)? else {
        return Ok(None);
    };
    if verify_windows_executable(&record, executable).is_err() {
        return Ok(None);
    }
    Ok(Some(record))
}
#[cfg(windows)]
fn live_service(directory: &Path) -> Result<Option<Record>> {
    let Some(bytes) = telegram_config::read_private(&directory.join(STATE), 8192)? else {
        return Ok(None);
    };
    let record: Record = match serde_json::from_slice(&bytes) {
        Ok(record) => record,
        Err(_) => return Ok(None),
    };
    if record.directory != directory || record.pid == 0 || record.pid == std::process::id() {
        return Ok(None);
    }
    let process = match crate::platform::process_identity(record.pid) {
        Ok(process) => process,
        Err(_) => return Ok(None),
    };
    let owner = crate::platform::current_identity().map_err(|_| "Cannot verify current identity")?;
    if record.version != 1
        || record.boot_id != owner
        || process.owner != owner
        || record.start_ticks != process.creation_time
        || process.executable != record.executable
    {
        return Ok(None);
    }
    let name = process.executable.file_name().and_then(|name| name.to_str());
    if !name.is_some_and(|name| name.eq_ignore_ascii_case("daedalus.exe")) {
        return Ok(None);
    }
    let config = ServiceConfig::load(&directory.join("telegram-service.json"))?;
    if record.instance != config.instance || record.bot_id != config.bot_id {
        return Err("Telegram instance identity changed; installation has been kept".into());
    }
    Ok(Some(record))
}
/// Stop services that still map `executable`, or any live Daedalus service for
/// a saved instance, so the image can be replaced. Cooperative stop only.
/// Returns instance directories that were running and should be restarted from
/// the installed image. Configuration and chat state are not deleted.
///
/// `install_current` must call this before replacing `destination`, then
/// [`restart_services_after_install`] after the new image is in place.
#[cfg(windows)]
pub(crate) fn stop_running_services_for_install(
    preferences: &Path,
    executable: &Path,
    timeout: std::time::Duration,
) -> Result<Vec<PathBuf>> {
    if !executable.is_absolute() {
        return Err("Installed executable path must be absolute".into());
    }
    let deadline = std::time::Instant::now() + timeout;
    let mut restart = Vec::new();
    for directory in super::instances::saved_directories(preferences)? {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err("Telegram has not finished shutting down; installation has been kept. Retry after shutdown completes.".into());
        }
        if stop_one_for_install(&directory, remaining)? {
            restart.push(directory);
        }
    }
    Ok(restart)
}
#[cfg(windows)]
fn stop_one_for_install(directory: &Path, timeout: std::time::Duration) -> Result<bool> {
    let lock_visible = held(directory)?;
    let orphan = if lock_visible {
        None
    } else {
        live_service(directory)?
    };
    if !service_blocks_replace(lock_visible, orphan.is_some()) {
        return Ok(false);
    }
    if let Some(record) = orphan {
        shutdown_pinned(directory, &record, timeout, confirm_same_process)?;
        return Ok(true);
    }
    let bytes = telegram_config::read_private(&directory.join(STATE), 8192)?
        .ok_or("Telegram is starting; wait and retry install")?;
    let record: Record =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid private service record")?;
    let config = ServiceConfig::load(&directory.join("telegram-service.json"))?;
    if record.directory != directory
        || record.instance != config.instance
        || record.bot_id != config.bot_id
    {
        return Err("Telegram instance identity changed; installation has been kept".into());
    }
    confirm_same_process(&record)?;
    shutdown_pinned(directory, &record, timeout, confirm_same_process)?;
    Ok(true)
}
#[cfg(windows)]
fn confirm_same_process(record: &Record) -> Result<()> {
    let process = crate::platform::process_identity(record.pid)
        .map_err(|_| "Service process is unavailable")?;
    let owner = crate::platform::current_identity().map_err(|_| "Cannot verify current identity")?;
    if record.version != 1
        || process.owner != owner
        || record.boot_id != owner
        || process.creation_time != record.start_ticks
        || process.executable != record.executable
    {
        return Err("Service process identity changed".into());
    }
    Ok(())
}
#[cfg(unix)]
pub fn stop_requested(_: &Path) -> Result<bool> {
    Ok(false)
}
#[cfg(windows)]
pub fn stop_requested(directory: &Path) -> Result<bool> {
    let path = directory.join("telegram-stop.json");
    let Some(bytes) = telegram_config::read_private(&path, 8192)? else {
        return Ok(false);
    };
    let request: StopRequest =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid private stop request")?;
    let process = crate::platform::process_identity(std::process::id())
        .map_err(|_| "Cannot inspect service process")?;
    let config = ServiceConfig::load(&directory.join("telegram-service.json"))?;
    let matches = request.pid == process.pid
        && request.start_ticks == process.creation_time
        && request.directory == directory
        && request.instance == config.instance;
    // Stale requests remain inert evidence. A subsequent reviewed stop replaces them.
    if matches {
        std::fs::remove_file(path).map_err(|_| "Cannot consume stop request")?;
    }
    Ok(matches)
}
#[cfg(windows)]
fn installed_executable() -> Result<PathBuf> {
    Ok(crate::platform::user_local_data_directory()
        .map_err(|_| "Cannot resolve authoritative user data directory")?
        .join("Daedalus")
        .join("bin")
        .join("daedalus.exe"))
}
#[cfg(windows)]
fn file_identity(path: &Path) -> Result<(u64, u64)> {
    let file = crate::platform::open_regular(path)
        .map_err(|_| "Cannot inspect executable".to_string())?;
    crate::platform::file_identity(&file).map_err(|_| "Cannot identify executable".to_string())
}
#[cfg(windows)]
fn path_key(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    let stripped = text.strip_prefix("\\\\?\\").unwrap_or(text.as_str());
    stripped.to_ascii_lowercase()
}
#[cfg(windows)]
fn installed_target(executable: &Path) -> Result<bool> {
    let installed = match installed_executable() {
        Ok(path) => path,
        Err(_) => return Ok(false),
    };
    if !installed.exists() || !executable.exists() {
        return Ok(false);
    }
    Ok(path_key(executable) == path_key(&installed)
        && file_identity(executable)? == file_identity(&installed)?)
}
/// Identity of the image this process launched. Compared later with the
/// installed file so a replaced or temporary updater image does not keep polling.
#[cfg(windows)]
pub(crate) fn capture_running_image() -> Result<(u64, u64)> {
    file_identity(&std::env::current_exe().map_err(|_| "Cannot inspect service executable")?)
}
#[cfg(windows)]
pub(crate) fn replacement_for(running: &(u64, u64)) -> Result<Option<PathBuf>> {
    let installed = match installed_executable() {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    if !installed.exists() {
        return Ok(None);
    }
    let installed_id = match file_identity(&installed) {
        Ok(identity) => identity,
        Err(_) => return Ok(None),
    };
    if image_was_replaced(*running, Some(installed_id)) {
        Ok(Some(installed))
    } else {
        Ok(None)
    }
}
#[cfg(windows)]
fn caller_service_executable() -> Result<PathBuf> {
    let current = std::env::current_exe().map_err(|_| "Cannot locate native executable")?;
    let installed = match installed_executable() {
        Ok(path) => path,
        Err(_) => return Ok(current),
    };
    if !installed.exists() {
        return Ok(current);
    }
    if file_identity(&current)? != file_identity(&installed)? {
        return Err("Refusing to start Telegram from a temporary updater. Use the installed Daedalus executable.".into());
    }
    Ok(installed)
}
#[cfg(windows)]
fn service_environment() -> std::collections::BTreeMap<String, String> {
    let mut environment = std::collections::BTreeMap::new();
    for key in [
        "PATH",
        "SystemRoot",
        "WINDIR",
        "USERPROFILE",
        "HOMEDRIVE",
        "HOMEPATH",
        "LOCALAPPDATA",
        "APPDATA",
        "TEMP",
        "TMP",
        "PATHEXT",
        "COMSPEC",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
    ] {
        if let Ok(value) = std::env::var(key) {
            environment.insert(key.into(), value);
        }
    }
    environment
}
#[cfg(windows)]
fn spawn_service(directory: &Path, executable: &Path) -> Result<Value> {
    let config = ServiceConfig::load(&directory.join("telegram-service.json"))?;
    config.read_token()?;
    config.check_registrations()?;
    super::cutover::activation_check(directory)?;
    if inspect(directory)?.state != "stopped" {
        return Err("Service already owns or may be starting for this instance".into());
    }
    let environment = service_environment();
    // The daemon opens its own log. Inheriting any parent pipe here can keep a
    // PowerShell pipeline waiting forever after the start command has exited.
    let directory = directory
        .to_str()
        .ok_or("Configuration path must be UTF-8")?;
    let mut child = crate::platform::spawn_background_with_environment(
        executable,
        &["telegram", "serve", "--config-dir", directory],
        Some(&environment),
    )
    .map_err(|_| "Cannot start native Telegram service")?;
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(
        json!({"state":"submitted","operation":"start","pid":pid,"message":"Start requested; service ownership and transport identity are checked separately."}),
    )
}
#[cfg(windows)]
pub fn start(directory: &Path) -> Result<Value> {
    spawn_service(directory, &caller_service_executable()?)
}
/// Spawn `telegram serve` from the installed image only. A temporary updater
/// path is rejected even when it is the caller's `current_exe`.
#[cfg(windows)]
pub(crate) fn spawn_installed_service(directory: &Path, executable: &Path) -> Result<Value> {
    if !installed_target(executable)? {
        return Err(
            "Refusing to start Telegram from a temporary updater path".into(),
        );
    }
    spawn_service(directory, executable)
}
/// Restart instances stopped for an install. `executable` must be the installed
/// image (`%LOCALAPPDATA%\Daedalus\bin\daedalus.exe`), never a temp updater.
///
#[cfg(windows)]
pub(crate) fn restart_services_after_install(
    executable: &Path,
    directories: &[PathBuf],
) -> Result<()> {
    for directory in directories {
        spawn_installed_service(directory, executable)?;
    }
    Ok(())
}

#[cfg(windows)]
pub fn attach_service_log(directory: &Path) -> Result<std::fs::File> {
    use std::{io::Seek, os::windows::io::AsRawHandle};
    use windows_sys::Win32::System::Console::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};
    let mut log =
        crate::platform::open_private(&directory.join("telegram-service.log"), true, true)
            .map_err(|_| "Cannot open private service log")?;
    log.seek(std::io::SeekFrom::End(0))
        .map_err(|_| "Cannot position service log")?;
    if unsafe { SetStdHandle(STD_OUTPUT_HANDLE, log.as_raw_handle()) } == 0
        || unsafe { SetStdHandle(STD_ERROR_HANDLE, log.as_raw_handle()) } == 0
    {
        return Err("Cannot attach private service log".into());
    }
    Ok(log)
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    #[test]
    fn cooperative_stop_rejects_stale_process_and_keeps_ownership_until_release() {
        let directory = std::env::temp_dir().join(format!(
            "daedalus-supervisor-win-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        crate::platform::ensure_private_dir(&directory).unwrap();
        let global = directory.join("owners");
        let config = ServiceConfig {
            version: 1,
            instance: "fixture".into(),
            bot_id: 123,
            token_file: directory.join("token"),
            grants: vec![],
            listen_chats: Default::default(),
            registrations: vec![],
            hook_bindings: vec![],
        };
        layouts::write_private(
            &directory.join("telegram-service.json"),
            &serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let owner =
            super::super::telegram_service::Owner::acquire(&directory, &global, config.bot_id)
                .unwrap();
        record(&directory, &config, Phase::Running).unwrap();
        let expected = inspect(&directory).unwrap().process.unwrap();
        let mut stale = expected.clone();
        stale.start_ticks += 1;
        assert!(stop(&directory, &stale).is_err());
        assert!(!stop_requested(&directory).unwrap());
        stop(&directory, &expected).unwrap();
        assert!(stop_requested(&directory).unwrap());
        assert!(!stop_requested(&directory).unwrap());
        assert_eq!(inspect(&directory).unwrap().state, "running");
        drop(owner);
        assert_eq!(inspect(&directory).unwrap().state, "stopped");
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "daedalus-supervisor-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            layouts::write_private(&root.join("fixture"), b"fixture").unwrap();
            Self(root)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn stale_record_does_not_claim_running_without_owner() {
        let fixture = Fixture::new();
        layouts::write_private(&fixture.0.join(STATE), b"invalid stale record").unwrap();
        assert_eq!(inspect(&fixture.0).unwrap().state, "stopped");
    }
    #[test]
    fn owner_without_record_is_starting_and_released_owner_is_stopped() {
        let fixture = Fixture::new();
        let global = Fixture::new();
        let owner =
            super::super::telegram_service::Owner::acquire(&fixture.0, &global.0, 123).unwrap();
        assert_eq!(inspect(&fixture.0).unwrap().state, "starting");
        drop(owner);
        assert_eq!(inspect(&fixture.0).unwrap().state, "stopped");
    }
    #[test]
    fn process_evidence_rejects_reused_pid_and_wrong_command() {
        let fixture = Fixture::new();
        let proc_root = &fixture.0;
        layouts::write_private(&proc_root.join("sys/kernel/random/boot_id"), b"test-boot\n")
            .unwrap();
        let mut fields = vec!["0"; 20];
        fields[0] = "S";
        fields[19] = "42";
        layouts::write_private(
            &proc_root.join("17/stat"),
            format!("17 (worker with spaces) {}", fields.join(" ")).as_bytes(),
        )
        .unwrap();
        layouts::write_private(
            &proc_root.join("17/cmdline"),
            b"daedalus\0telegram\0serve\0--config-dir\0/private/instance\0",
        )
        .unwrap();
        symlink(
            crate::test_support::path("/bin/daedalus"),
            proc_root.join("17/exe"),
        )
        .unwrap();
        let mut record = Record {
            version: 1,
            pid: 17,
            start_ticks: 42,
            boot_id: "test-boot".into(),
            executable: crate::test_support::path("/bin/daedalus").into(),
            directory: crate::test_support::path("/private/instance").into(),
            instance: "test".into(),
            bot_id: 123,
            phase: Phase::Running,
        };
        verify(&record, proc_root).unwrap();
        record.start_ticks = 43;
        assert!(verify(&record, proc_root).is_err());
        record.start_ticks = 42;
        layouts::write_private(&proc_root.join("17/cmdline"), b"daedalus\0tui\0").unwrap();
        assert!(verify(&record, proc_root).is_err());
    }
}

#[cfg(test)]
mod decision_tests {
    use super::{image_was_replaced, service_blocks_replace};
    #[test]
    fn replaced_or_temporary_image_hands_off_and_same_image_stays() {
        assert!(!image_was_replaced((1, 2), None));
        assert!(!image_was_replaced((1, 2), Some((1, 2))));
        assert!(image_was_replaced((1, 2), Some((1, 3))));
        assert!(image_was_replaced((9, 9), Some((1, 2))));
    }
    #[test]
    fn blind_lock_still_blocks_replace_when_the_live_image_matches() {
        assert!(!service_blocks_replace(false, false));
        assert!(service_blocks_replace(true, false));
        assert!(service_blocks_replace(false, true));
        assert!(service_blocks_replace(true, true));
    }
}
