//! Private bounded control receipts. No user text, target labels, paths, tokens,
//! provider URLs, error strings, or dialog responses are part of this schema.
use crate::core::layouts;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, String>;
const MAX: u64 = 1024 * 1024;
const KEEP: usize = 1000;
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Workspace,
    NativeAttach,
    Configure,
    CheckpointSave,
    CheckpointRemove,
    CheckpointLaunch,
    Attach,
    DuplicateView,
    LinkView,
    UnlinkView,
    Terminal,
    Send,
    Keys,
    Interrupt,
    Close,
    Focus,
    Rename,
    Account,
    Gateway,
    HookInstall,
    HookRollback,
    Setup,
    Telegram,
    Layout,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Attempted,
    Accepted,
    Submitted,
    Unconfirmed,
    Completed,
    Rejected,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    version: u8,
    operation: String,
    action: Action,
    state: State,
    time: f64,
}
fn directory(path: &Path) -> Result<()> {
    layouts::check_path(path).map_err(|_| "Unsafe audit directory")?;
    let meta = fs::symlink_metadata(path).map_err(|_| "Audit directory is unavailable")?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err("Audit requires a private owned directory".into());
    }
    Ok(())
}
fn read(path: &Path) -> Result<Vec<Entry>> {
    layouts::check_path(path).map_err(|_| "Unsafe audit path")?;
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("Audit history is unavailable".into()),
    };
    let meta = file
        .metadata()
        .map_err(|_| "Audit metadata is unavailable")?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || meta.nlink() != 1
        || meta.len() > MAX
    {
        return Err("Unsafe or oversized audit history".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read audit history")?;
    if bytes.len() as u64 > MAX {
        return Err("Audit history exceeds limit".into());
    }
    let entries: Vec<Entry> =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid audit history")?;
    if entries.len() > KEEP
        || entries.iter().any(|e| {
            e.version != 1
                || !e.time.is_finite()
                || e.time < 0.0
                || e.operation.is_empty()
                || e.operation.len() > 96
                || !e
                    .operation
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() || b == b'-')
        })
    {
        return Err("Invalid audit history".into());
    }
    Ok(entries)
}
struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
fn operation_id() -> Result<String> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "Invalid audit time")?
        .as_nanos();
    Ok(format!(
        "{:x}-{:x}-{:x}",
        std::process::id(),
        now,
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}
pub fn record(path: &Path, action: Action, state: State) -> Result<()> {
    record_operation(path, action, state, &operation_id()?)
}
fn record_operation(path: &Path, action: Action, state: State, operation: &str) -> Result<()> {
    directory(path)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path.join("control-audit.lock"))
        .map_err(|_| "Cannot lock control audit")?;
    let meta = lock.metadata().map_err(|_| "Cannot inspect audit lock")?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || meta.nlink() != 1
    {
        return Err("Unsafe audit lock".into());
    }
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("Control audit is busy; action was not retried".into());
    }
    let _lock = Lock(lock);
    let file = path.join("control-audit.json");
    let mut entries = read(&file)?;
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "Invalid audit time")?
        .as_secs_f64();
    if entries.len() >= KEEP {
        entries.remove(0);
    }
    entries.push(Entry {
        version: 1,
        operation: operation.into(),
        action,
        state,
        time,
    });
    layouts::write_private_limit(
        &file,
        &serde_json::to_vec(&entries).map_err(|_| "Cannot encode audit receipt")?,
        MAX,
    )
}
pub fn recent(path: &Path) -> Result<Value> {
    directory(path)?;
    let file = path.join("control-audit.json");
    let exists = file
        .try_exists()
        .map_err(|_| "Cannot inspect audit history")?;
    let mut entries = read(&file)?;
    entries.reverse();
    entries.truncate(50);
    Ok(json!({"status":if exists{"ok"}else{"missing"},"events":entries,"retained_limit":KEEP}))
}

/// A final audit failure cannot undo a completed effect; report uncertainty and
/// never retry the operation. An attempted entry remains visible in that case.
pub fn perform<T>(
    directory: &Path,
    action: Action,
    operation: impl FnOnce() -> Result<T>,
    state: impl FnOnce(&T) -> State,
) -> Result<T> {
    let operation_id = operation_id()?;
    record_operation(directory, action, State::Attempted, &operation_id)?;
    let result = operation();
    let outcome = match &result {
        Ok(value) => state(value),
        Err(_) => State::Unconfirmed,
    };
    if record_operation(directory, action, outcome, &operation_id).is_err() {
        return Err("Action may have applied but its audit receipt could not be saved; inspect state before retrying".into());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        os::unix::fs::{PermissionsExt, symlink},
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "daedalus-control-audit-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn effects_are_recorded_without_errors_or_payloads_and_reader_does_not_write() {
        let f = Fixture::new();
        assert_eq!(recent(&f.0).unwrap()["status"], "missing");
        assert_eq!(fs::read_dir(&f.0).unwrap().count(), 0);
        perform(
            &f.0,
            Action::CheckpointLaunch,
            || Ok("PRIVATE-TOKEN"),
            |_| State::Submitted,
        )
        .unwrap();
        let _: Result<()> = perform(
            &f.0,
            Action::Account,
            || Err("PRIVATE-ERROR-URL".into()),
            |_| State::Completed,
        );
        let bytes = fs::read(f.0.join("control-audit.json")).unwrap();
        let report = recent(&f.0).unwrap();
        assert_eq!(report["events"][0]["state"], "unconfirmed");
        assert_eq!(report["events"][2]["state"], "submitted");
        assert!(
            !String::from_utf8(bytes.clone())
                .unwrap()
                .contains("PRIVATE")
        );
        assert_eq!(bytes, fs::read(f.0.join("control-audit.json")).unwrap());
    }
    #[test]
    fn unsafe_audit_blocks_effects_and_post_effect_failure_reports_uncertainty() {
        let f = Fixture::new();
        let called = Cell::new(false);
        fs::write(f.0.join("target"), b"not-audit").unwrap();
        symlink(f.0.join("target"), f.0.join("control-audit.json")).unwrap();
        assert!(
            perform(
                &f.0,
                Action::Terminal,
                || {
                    called.set(true);
                    Ok(())
                },
                |_| State::Submitted
            )
            .is_err()
        );
        assert!(!called.get());
        fs::remove_file(f.0.join("control-audit.json")).unwrap();
        let error = perform(
            &f.0,
            Action::Terminal,
            || {
                called.set(true);
                fs::write(f.0.join("control-audit.json"), b"corrupted").unwrap();
                Ok(())
            },
            |_| State::Submitted,
        )
        .unwrap_err();
        assert!(called.get());
        assert!(error.contains("may have applied"));
    }
    #[test]
    fn retention_and_unknown_schema_are_explicit() {
        let f = Fixture::new();
        let entries = (0..KEEP)
            .map(|i| Entry {
                version: 1,
                operation: format!("{i:x}"),
                action: Action::Layout,
                state: State::Completed,
                time: i as f64,
            })
            .collect::<Vec<_>>();
        layouts::write_private(
            &f.0.join("control-audit.json"),
            &serde_json::to_vec(&entries).unwrap(),
        )
        .unwrap();
        record(&f.0, Action::Attach, State::Accepted).unwrap();
        assert_eq!(read(&f.0.join("control-audit.json")).unwrap().len(), KEEP);
        assert_eq!(read(&f.0.join("control-audit.json")).unwrap()[0].time, 1.0);
        assert_eq!(
            recent(&f.0).unwrap()["events"].as_array().unwrap().len(),
            50
        );
        layouts::write_private(
            &f.0.join("control-audit.json"),
            b"[{\"secret\":\"PRIVATE\"}]",
        )
        .unwrap();
        assert!(recent(&f.0).is_err());
    }
}
