//! Private bounded control receipts. No user text, target labels, paths, tokens,
//! provider URLs, error strings, or dialog responses are part of this schema.
use crate::core::layouts;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
#[cfg(unix)]
use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::os::{
    fd::AsRawFd,
    unix::fs::{MetadataExt, OpenOptionsExt},
};
use std::{
    fs::File,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, String>;
pub(crate) const MAX: u64 = 8 * 1024 * 1024;
pub(crate) const MAX_ENTRIES: usize = 8192;
const MAX_ENTRY_BYTES: usize = 256;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Attempted,
    Accepted,
    Submitted,
    Unconfirmed,
    Completed,
    Rejected,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    version: u8,
    pub(crate) operation: String,
    pub(crate) action: Action,
    pub(crate) state: State,
    pub(crate) time: f64,
}
#[cfg(unix)]
pub(crate) fn directory(path: &Path) -> Result<()> {
    layouts::check_path(path).map_err(|_| "Unsafe audit directory")?;
    let meta = fs::symlink_metadata(path).map_err(|_| "Audit directory is unavailable")?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err("Audit requires a private owned directory".into());
    }
    Ok(())
}
pub(crate) fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    super::telegram_config::read_private(path, MAX)
}
pub(crate) fn read(path: &Path) -> Result<Vec<Entry>> {
    read_bytes(path)?.map_or_else(|| Ok(Vec::new()), |bytes| decode(&bytes))
}
pub(crate) fn decode(bytes: &[u8]) -> Result<Vec<Entry>> {
    if bytes.len() as u64 > MAX {
        return Err("Audit history exceeds format bounds".into());
    }
    let entries: Vec<Entry> = serde_json::from_slice(bytes).map_err(|_| "Invalid audit history")?;
    if entries.len() > MAX_ENTRIES
        || entries.iter().any(|e| {
            e.version != 1
                || !e.time.is_finite()
                || e.time < 0.0
                || e.operation.is_empty()
                || e.operation.len() > 96
                || !e.operation.bytes().any(|b| b.is_ascii_hexdigit())
                || !e
                    .operation
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() || b == b'-')
                || serde_json::to_vec(e).map_or(true, |bytes| bytes.len() > MAX_ENTRY_BYTES)
        })
    {
        return Err("Invalid audit history".into());
    }
    groups(&entries)?;
    Ok(entries)
}
struct Group {
    action: Action,
    attempted: bool,
    outcome: Option<State>,
    entries: usize,
    bytes: usize,
}
impl Group {
    fn protected(&self) -> bool {
        !matches!(self.outcome, Some(State::Completed | State::Rejected))
    }
    fn pending(&self) -> bool {
        self.attempted && self.outcome.is_none()
    }
}
fn groups(entries: &[Entry]) -> Result<BTreeMap<&str, Group>> {
    let mut groups = BTreeMap::<&str, Group>::new();
    for entry in entries {
        let group = groups.entry(&entry.operation).or_insert(Group {
            action: entry.action,
            attempted: false,
            outcome: None,
            entries: 0,
            bytes: 0,
        });
        group.entries += 1;
        group.bytes += serde_json::to_vec(entry)
            .map_err(|_| "Cannot encode audit receipt")?
            .len()
            + 1;
        if group.action != entry.action {
            return Err("Conflicting audit operation actions".into());
        }
        if entry.state == State::Attempted {
            if group.attempted || group.outcome.is_some() {
                return Err("Conflicting audit operation attempts".into());
            }
            group.attempted = true;
        } else {
            if group.outcome.is_some() {
                return Err("Conflicting audit operation outcomes".into());
            }
            group.outcome = Some(entry.state);
        }
    }
    Ok(groups)
}
pub(crate) fn protected_counts(entries: &[Entry]) -> Result<(usize, usize)> {
    let groups = groups(entries)?;
    Ok((
        groups.values().filter(|g| g.protected()).count(),
        groups.values().filter(|g| g.pending()).count(),
    ))
}
/// Completed/rejected operations rotate as whole groups. Pending operations
/// reserve one maximum-size outcome slot, even while their process is idle.
pub(crate) fn fit(
    entries: &[Entry],
    settings: &super::history::Settings,
    current: Option<&str>,
) -> Result<Vec<Entry>> {
    settings.validate()?;
    let grouped = groups(entries)?;
    let pending = grouped.values().filter(|g| g.pending()).count();
    let mut remaining = entries.len();
    let mut bytes = serde_json::to_vec(entries)
        .map_err(|_| "Cannot encode audit history")?
        .len();
    let mut removed = std::collections::BTreeSet::new();
    let mut visited = std::collections::BTreeSet::new();
    let fits = |count: usize, bytes: usize, pending: usize| {
        count + pending <= settings.entry_limit as usize
            && bytes + pending * (MAX_ENTRY_BYTES + 1) <= settings.byte_limit as usize
    };
    for entry in entries {
        if fits(remaining, bytes, pending) {
            break;
        }
        if !visited.insert(entry.operation.as_str()) || Some(entry.operation.as_str()) == current {
            continue;
        }
        let group = &grouped[entry.operation.as_str()];
        if group.protected() {
            continue;
        }
        removed.insert(entry.operation.as_str());
        bytes = bytes.saturating_sub(group.bytes);
        remaining -= group.entries;
    }
    let retained = entries
        .iter()
        .filter(|e| !removed.contains(e.operation.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&retained).map_err(|_| "Cannot encode audit history")?;
    if !fits(retained.len(), bytes.len(), pending) {
        return Err("Control history is full of protected receipts; archive history or raise its limits before trying a new action".into());
    }
    Ok(retained)
}
pub(crate) struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
        #[cfg(windows)]
        let _ = crate::platform::unlock(&self.0);
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
pub(crate) fn lock(path: &Path) -> Result<Lock> {
    directory(path)?;
    #[cfg(unix)]
    let lock = {
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
        lock
    };
    #[cfg(windows)]
    let lock = {
        let file = crate::platform::open_private(&path.join("control-audit.lock"), true, true)
            .map_err(|_| "Cannot open audit lock")?;
        crate::platform::lock_exclusive(&file, true)
            .map_err(|_| "Another audit update is active")?;
        file
    };
    Ok(Lock(lock))
}
fn record_operation(path: &Path, action: Action, state: State, operation: &str) -> Result<()> {
    let _lock = lock(path)?;
    let settings = super::history::Settings::load(path)?;
    let file = path.join("control-audit.json");
    let mut entries = read(&file)?;
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "Invalid audit time")?
        .as_secs_f64();
    let entry = Entry {
        version: 1,
        operation: operation.into(),
        action,
        state,
        time,
    };
    if serde_json::to_vec(&entry)
        .map_err(|_| "Cannot encode audit receipt")?
        .len()
        > MAX_ENTRY_BYTES
    {
        return Err("Audit receipt exceeds reserved size".into());
    }
    entries.push(entry);
    let entries = fit(&entries, &settings, Some(operation))?;
    layouts::write_private_limit(
        &file,
        &serde_json::to_vec(&entries).map_err(|_| "Cannot encode audit receipt")?,
        MAX,
    )
}
pub fn recent(path: &Path) -> Result<Value> {
    list(path, 0, false)
}
pub fn list(path: &Path, offset: usize, unresolved: bool) -> Result<Value> {
    if path.exists() {
        directory(path)?;
    } else {
        layouts::check_path(path)?;
    }
    let (settings, policy_valid) = super::history::view_settings(path);
    let original = read_bytes(&path.join("control-audit.json"))?;
    let entries = original
        .as_ref()
        .map_or_else(|| Ok(Vec::new()), |bytes| decode(bytes))?;
    let mut page = page(&entries, &settings, offset, unresolved)?;
    page["status"] = json!(if original.is_some() { "ok" } else { "missing" });
    page["policy_valid"] = json!(policy_valid);
    page["source"] = json!("active");
    Ok(page)
}
pub(crate) fn page(
    entries: &[Entry],
    settings: &super::history::Settings,
    offset: usize,
    unresolved: bool,
) -> Result<Value> {
    if offset > MAX_ENTRIES {
        return Err("Invalid history page offset".into());
    }
    let grouped = groups(entries)?;
    let (protected, pending) = protected_counts(entries)?;
    let selected = entries
        .iter()
        .rev()
        .filter(|e| !unresolved || grouped[e.operation.as_str()].protected())
        .collect::<Vec<_>>();
    let next = offset
        .saturating_add(settings.display_limit as usize)
        .min(selected.len());
    Ok(
        json!({"events":selected.iter().skip(offset).take(settings.display_limit as usize).collect::<Vec<_>>(),"retained_limit":settings.entry_limit,"byte_limit":settings.byte_limit,"display_limit":settings.display_limit,"entries":entries.len(),"protected_operations":protected,"pending_operations":pending,"offset":offset,"next_offset":if next<selected.len(){Some(next)}else{None},"unresolved_only":unresolved,"notice":"Accepted, submitted and unconfirmed outcomes remain protected. Archiving preserves their states and never retries an action."}),
    )
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

#[cfg(all(test, unix))]
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
    const KEEP: usize = 1000;
    fn policy(f: &Fixture, entries: u64, bytes: u64, display: u64) {
        let settings = super::super::history::Settings {
            entry_limit: entries,
            byte_limit: bytes,
            display_limit: display,
            ..Default::default()
        };
        layouts::write_private(
            &f.0.join("control-history.json"),
            &serde_json::to_vec(&settings).unwrap(),
        )
        .unwrap();
    }
    fn seed(f: &Fixture, entries: Vec<Entry>) {
        layouts::write_private(
            &f.0.join("control-audit.json"),
            &serde_json::to_vec(&entries).unwrap(),
        )
        .unwrap();
    }
    fn entry(op: &str, state: State) -> Entry {
        Entry {
            version: 1,
            operation: op.into(),
            action: Action::Configure,
            state,
            time: 1.0,
        }
    }
    #[test]
    fn concurrent_admission_reserves_the_running_effects_final_receipt() {
        let f = Fixture::new();
        policy(&f, 2, 4096, 2);
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let path = &f.0;
            let worker = scope.spawn(move || {
                perform(
                    path,
                    Action::Account,
                    || {
                        started_tx.send(()).unwrap();
                        finish_rx
                            .recv_timeout(std::time::Duration::from_secs(5))
                            .map_err(|_| "Fixture finish signal missing".to_owned())?;
                        Ok(())
                    },
                    |_| State::Unconfirmed,
                )
            });
            started_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            let before = fs::read(f.0.join("control-audit.json")).unwrap();
            let called = Cell::new(false);
            assert!(
                perform(
                    &f.0,
                    Action::Configure,
                    || {
                        called.set(true);
                        Ok(())
                    },
                    |_| State::Completed
                )
                .is_err()
            );
            assert!(!called.get());
            assert_eq!(fs::read(f.0.join("control-audit.json")).unwrap(), before);
            finish_tx.send(()).unwrap();
            worker.join().unwrap().unwrap();
        });
        let saved = read(&f.0.join("control-audit.json")).unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[1].state, State::Unconfirmed);
    }
    #[test]
    fn rotation_keeps_uncertain_operations_and_prunes_completed_pairs_together() {
        let f = Fixture::new();
        policy(&f, 6, 1048576, 1);
        seed(
            &f,
            vec![
                entry("a", State::Attempted),
                entry("a", State::Unconfirmed),
                entry("b", State::Attempted),
                entry("b", State::Completed),
                entry("c", State::Submitted),
            ],
        );
        perform(&f.0, Action::Configure, || Ok(()), |_| State::Completed).unwrap();
        let saved = read(&f.0.join("control-audit.json")).unwrap();
        assert_eq!(saved.len(), 5);
        assert_eq!(saved.iter().filter(|e| e.operation == "a").count(), 2);
        assert!(saved.iter().any(|e| e.operation == "c"));
        assert!(!saved.iter().any(|e| e.operation == "b"));
        let view = list(&f.0, 0, true).unwrap();
        assert_eq!(view["events"].as_array().unwrap().len(), 1);
        assert_eq!(view["next_offset"], 1);
        assert_eq!(view["protected_operations"], 2);
        let next = list(&f.0, 1, true).unwrap();
        assert_eq!(next["events"][0]["operation"], "a");
    }
    #[test]
    fn reserved_outcome_capacity_blocks_second_effect_and_survives_policy_edits() {
        let f = Fixture::new();
        policy(&f, 2, 4096, 1);
        let called = Cell::new(false);
        perform(
            &f.0,
            Action::Configure,
            || {
                let before = fs::read(f.0.join("control-audit.json")).unwrap();
                assert!(
                    perform(
                        &f.0,
                        Action::Account,
                        || {
                            called.set(true);
                            Ok(())
                        },
                        |_| State::Completed
                    )
                    .is_err()
                );
                assert!(!called.get());
                assert_eq!(fs::read(f.0.join("control-audit.json")).unwrap(), before);
                let state = super::super::history::State::load(&f.0).unwrap();
                let updated = state.settings.set("display_limit", "2").unwrap();
                let plan = super::super::history::Plan::edit(state, updated).unwrap();
                let approval = plan.approval();
                plan.apply(&approval).unwrap();
                Ok(())
            },
            |_| State::Completed,
        )
        .unwrap();
        assert_eq!(read(&f.0.join("control-audit.json")).unwrap().len(), 2);
        let state = super::super::history::State::load(&f.0).unwrap();
        let updated = state.settings.set("entry_limit", "4").unwrap();
        let plan = super::super::history::Plan::edit(state, updated).unwrap();
        let approval = plan.approval();
        plan.apply(&approval).unwrap();
        perform(&f.0, Action::Account, || Ok(()), |_| State::Submitted).unwrap();
        assert!(
            perform(
                &f.0,
                Action::Configure,
                || {
                    called.set(true);
                    Ok(())
                },
                |_| State::Completed
            )
            .is_ok()
        );
        assert_eq!(read(&f.0.join("control-audit.json")).unwrap().len(), 4); // Completed pair rotates; submitted pair remains.
        assert!(
            read(&f.0.join("control-audit.json"))
                .unwrap()
                .iter()
                .any(|e| e.state == State::Submitted)
        );
    }
    #[test]
    fn protected_byte_pressure_refuses_before_effect_and_policy_change_cannot_hide_it() {
        let f = Fixture::new();
        policy(&f, 64, 4096, 50);
        let entries = (0..24)
            .map(|i| entry(&format!("{i:096x}"), State::Unconfirmed))
            .collect();
        seed(&f, entries);
        let before = fs::read(f.0.join("control-audit.json")).unwrap();
        let called = Cell::new(false);
        assert!(
            perform(
                &f.0,
                Action::Configure,
                || {
                    called.set(true);
                    Ok(())
                },
                |_| State::Completed
            )
            .is_err()
        );
        assert!(!called.get());
        assert_eq!(fs::read(f.0.join("control-audit.json")).unwrap(), before);
        assert_eq!(
            list(&f.0, 0, true).unwrap()["events"]
                .as_array()
                .unwrap()
                .len(),
            24
        );
        let state = super::super::history::State::load(&f.0).unwrap();
        assert!(super::super::history::Plan::edit(state.clone(), state.settings.clone()).is_err());
        let updated = state.settings.set("byte_limit", "8192").unwrap();
        let plan = super::super::history::Plan::edit(state, updated).unwrap();
        let approval = plan.approval();
        plan.apply(&approval).unwrap();
        perform(&f.0, Action::Configure, || Ok(()), |_| State::Completed).unwrap();
        assert_eq!(
            list(&f.0, 0, true).unwrap()["events"]
                .as_array()
                .unwrap()
                .len(),
            24
        );
    }
    #[test]
    fn conflicting_operation_actions_or_outcomes_are_refused_without_writes() {
        let f = Fixture::new();
        for entries in [
            vec![entry("a", State::Attempted), entry("a", State::Attempted)],
            vec![entry("a", State::Unconfirmed), entry("a", State::Completed)],
            vec![
                entry("a", State::Completed),
                Entry {
                    action: Action::Account,
                    ..entry("a", State::Attempted)
                },
            ],
        ] {
            seed(&f, entries);
            let before = fs::read(f.0.join("control-audit.json")).unwrap();
            let called = Cell::new(false);
            assert!(
                perform(
                    &f.0,
                    Action::Configure,
                    || {
                        called.set(true);
                        Ok(())
                    },
                    |_| State::Completed
                )
                .is_err()
            );
            assert!(!called.get());
            assert_eq!(fs::read(f.0.join("control-audit.json")).unwrap(), before);
        }
    }
    fn record(path: &Path, action: Action, state: State) -> Result<()> {
        record_operation(path, action, state, &operation_id()?)
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

#[cfg(windows)]
pub(crate) fn directory(path: &Path) -> Result<()> {
    crate::platform::validate_private_dir(path).map_err(|e| e.to_string())
}
