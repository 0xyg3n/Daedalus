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
    let process = crate::platform::process_identity(record.pid)
        .map_err(|_| "Service process is unavailable")?;
    let owner =
        crate::platform::current_identity().map_err(|_| "Cannot verify current identity")?;
    if record.version != 1
        || record.boot_id != owner
        || process.owner != owner
        || record.start_ticks != process.creation_time
        || record.executable != process.executable
        || record.executable
            != std::env::current_exe().map_err(|_| "Cannot inspect native executable")?
    {
        return Err("Service process identity changed".into());
    }
    Ok(())
}
#[cfg(windows)]
fn held(directory: &Path) -> Result<bool> {
    let path = directory.join(".telegram-service.lock");
    let file = match crate::platform::open_private(&path, true, false) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err("Cannot inspect service ownership".into()),
    };
    match crate::platform::lock_exclusive(&file, true) {
        Ok(()) => {
            crate::platform::unlock(&file).map_err(|_| "Cannot release owner probe")?;
            Ok(false)
        }
        Err(error) if error.raw_os_error() == Some(33) => Ok(true),
        Err(_) => Err("Cannot determine service ownership".into()),
    }
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
    let request = StopRequest {
        pid: expected.pid,
        start_ticks: expected.start_ticks,
        instance: expected.instance.clone(),
        directory: directory.into(),
    };
    layouts::write_private(
        &directory.join("telegram-stop.json"),
        &serde_json::to_vec(&request).map_err(|_| "Cannot encode stop request")?,
    )?;
    Ok(
        json!({"state":"submitted","operation":"stop","message":"Cooperative stop requested. Ownership remains observed until shutdown finishes."}),
    )
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
pub fn start(directory: &Path) -> Result<Value> {
    use std::os::windows::process::CommandExt;
    let config = ServiceConfig::load(&directory.join("telegram-service.json"))?;
    config.read_token()?;
    config.check_registrations()?;
    super::cutover::activation_check(directory)?;
    if inspect(directory)?.state != "stopped" {
        return Err("Service already owns or may be starting for this instance".into());
    }
    let path = directory.join("telegram-service.log");
    let mut log = crate::platform::open_private(&path, true, true)
        .map_err(|_| "Cannot open private service log")?;
    use std::io::Seek;
    log.seek(std::io::SeekFrom::End(0))
        .map_err(|_| "Cannot position service log")?;
    let mut command = std::process::Command::new(
        std::env::current_exe().map_err(|_| "Cannot locate native executable")?,
    );
    command
        .args(["telegram", "serve", "--config-dir"])
        .arg(directory)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone().map_err(|_| "Cannot attach service log")?)
        .stderr(log)
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
        .env_clear();
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
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    let mut child = command
        .spawn()
        .map_err(|_| "Cannot start native Telegram service")?;
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(
        json!({"state":"submitted","operation":"start","pid":pid,"message":"Start requested; service ownership and transport identity are checked separately."}),
    )
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
