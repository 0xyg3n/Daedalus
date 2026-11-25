//! Session menu application operations shared by local frontends.
//! Saved references never authorize a terminal mutation: the adapter verifies
//! the exact destination again when an explicitly accepted plan is applied.
use crate::{
    adapters::{
        harnesses, pi_sessions,
        terminal_actions::{Placement, Receipt},
        terminals::{self, Terminals},
    },
    app::{config::ControlConfig, tree},
    core::{
        checkpoints::{self, Capabilities, Checkpoint, LaunchPlan, Operation, Registry},
        identity::{Backend, TerminalId},
        layouts::{self, TerminalRef},
        native_metadata::Guard as MetadataGuard,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::{
    fd::AsRawFd,
    unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointRow {
    pub key: String,
    #[serde(flatten)]
    pub checkpoint: Checkpoint,
    pub registry: String,
}
#[derive(Clone, Debug)]
pub struct SaveRequest {
    pub name: String,
    pub sid: String,
    pub include_output: bool,
}
pub struct LaunchRequest<'a> {
    pub operation: Operation,
    pub target: &'a Capabilities,
    pub parent: &'a Value,
    pub placement: Placement,
    pub license: Option<&'a super::fork_licenses::Choice>,
}
#[derive(Clone, Debug)]
pub struct SessionStore {
    pub directory: PathBuf,
}
#[derive(Serialize)]
struct NewView<'a> {
    version: u32,
    source: &'a TerminalId,
    viewer: &'a TerminalId,
    source_label: String,
    server_pid: &'a str,
    mechanism: &'a str,
    created_at: String,
}
#[derive(Serialize)]
struct LiveView<'a> {
    version: u32,
    source: &'a TerminalId,
    viewer: &'a TerminalId,
    backend: &'a Value,
    server: &'a Value,
    pane_id: &'a Value,
    terminal_id: &'a Value,
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_label: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mechanism: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    server_pid: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<&'a str>,
}
struct StoreLock(File);
impl Drop for StoreLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
        #[cfg(windows)]
        let _ = crate::platform::unlock(&self.0);
    }
}
#[cfg(unix)]
fn lock(directory: &Path) -> Result<StoreLock> {
    layouts::check_path(directory)?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
        .map_err(|_| "Cannot create private session directory")?;
    let metadata = fs::metadata(directory).map_err(|_| "Cannot inspect session directory")?;
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.permissions().mode() & 0o077 != 0 {
        return Err(
            "Session directory must be owned by the current user and private (0700)".into(),
        );
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.join("session-actions.lock"))
        .map_err(|_| "Cannot open session state lock")?;
    let meta = file
        .metadata()
        .map_err(|_| "Cannot inspect session state lock")?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.permissions().mode() & 0o077 != 0
    {
        return Err("Unsafe session state lock".into());
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("Another session update is in progress; retry after it finishes".into());
    }
    Ok(StoreLock(file))
}
fn field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .into()
}
fn harness_name(pane: &Value) -> String {
    let name = field(pane, "agent").to_lowercase();
    harnesses::canonical_id(&name).unwrap_or(&name).into()
}
fn first(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .map(|k| field(value, k))
        .find(|s| !s.is_empty())
        .unwrap_or_default()
}
fn utc_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as libc::time_t;
    let mut time = std::mem::MaybeUninit::<libc::tm>::uninit();
    #[cfg(unix)]
    let ptr = unsafe { libc::gmtime_r(&seconds, time.as_mut_ptr()) };
    #[cfg(windows)]
    let ptr = if unsafe { libc::gmtime_s(time.as_mut_ptr(), &seconds) } == 0 {
        time.as_mut_ptr()
    } else {
        std::ptr::null_mut()
    };
    if ptr.is_null() {
        return String::new();
    }
    let time = unsafe { time.assume_init() };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        time.tm_year + 1900,
        time.tm_mon + 1,
        time.tm_mday,
        time.tm_hour,
        time.tm_min,
        time.tm_sec
    )
}
fn terminal_ref(pane: &Value) -> Result<TerminalRef> {
    Ok(TerminalRef {
        label: tree::title(pane, ""),
        place: [
            first(pane, &["session"]),
            first(pane, &["window", "tab_id"]),
            first(pane, &["pane_id"]),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" / "),
        ..terminals::reference(pane)?
    })
}
#[derive(Debug, Clone)]
struct ProcessEvidence {
    pid: String,
    start: String,
    argv: Vec<String>,
    cwd: String,
    profile: String,
}
fn metadata_guard(
    adapter: &Terminals,
    settings: &super::runtime_settings::Settings,
) -> Result<MetadataGuard<'static>> {
    Ok(
        crate::core::native_metadata::Guard::new(settings.native_metadata_policy())?.with_deadline(
            adapter
                .discovery_budget
                .as_ref()
                .map(|budget| budget.deadline),
        ),
    )
}
#[cfg(unix)]
fn stat_fields(raw: &[u8]) -> Result<Vec<&str>> {
    let end = raw
        .iter()
        .rposition(|byte| *byte == b')')
        .ok_or("Invalid process metadata")?;
    Ok(std::str::from_utf8(&raw[end + 1..])
        .map_err(|_| "Invalid process metadata")?
        .split_whitespace()
        .take(20)
        .collect())
}
/// A harness command line that identifies a conversation (or Pi's managed launch).
#[cfg(unix)]
fn session_command(harness: &str, argv: &[String]) -> bool {
    let managed_pi = harness == "pi"
        && argv.windows(2).any(|a| a[0] == "--session-dir")
        && argv.windows(2).any(|a| a[0] == "--extension")
        && argv
            .get(1)
            .is_some_and(|a| Path::new(a).file_name().is_some_and(|n| n == "cli.js"));
    managed_pi || harnesses::conversation_argv(argv, harness).is_some()
}
/// The harness profile from its variable, else `HOME` plus its default profile.
#[cfg(unix)]
fn session_profile<'a>(
    tool: &harnesses::Tool,
    environment: impl Iterator<Item = &'a [u8]>,
    guard: &mut MetadataGuard<'_>,
) -> Result<String> {
    let (mut profile, mut home) = (None, None);
    for pair in environment.filter(|p| !p.is_empty()) {
        guard.entries(1)?;
        let Some(equal) = pair.iter().position(|b| *b == b'=') else {
            continue;
        };
        if &pair[..equal] == tool.profile_variable.as_bytes() {
            profile = Some(String::from_utf8_lossy(&pair[equal + 1..]).into_owned());
        } else if &pair[..equal] == b"HOME" {
            home = Some(String::from_utf8_lossy(&pair[equal + 1..]).into_owned());
        }
    }
    Ok(profile
        .or_else(|| {
            home.map(|home| {
                Path::new(&home)
                    .join(tool.default_profile)
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .unwrap_or_default())
}
#[cfg(all(unix, not(target_os = "macos")))]
fn read_process(
    adapter: &Terminals,
    pid: &str,
    harness: &str,
    guard: &mut MetadataGuard<'_>,
) -> Result<Option<ProcessEvidence>> {
    read_process_procfs(adapter, pid, harness, guard)
}
/// macOS reads the same evidence through libproc; isolated fixtures keep procfs.
#[cfg(target_os = "macos")]
fn read_process(
    adapter: &Terminals,
    pid: &str,
    harness: &str,
    guard: &mut MetadataGuard<'_>,
) -> Result<Option<ProcessEvidence>> {
    use crate::platform::{process_argv, process_cwd, process_environment, process_info};
    if adapter.proc_root != Path::new("/proc") {
        return read_process_procfs(adapter, pid, harness, guard);
    }
    let number = pid
        .parse::<u32>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or("Invalid foreground process ID")?;
    guard.processes(1)?;
    let root = adapter.proc_root.as_path();
    let Some(info) = process_info(root, number) else {
        return Ok(None);
    };
    let start = info.start.to_string();
    let Some(argv) = process_argv(root, number) else {
        return Ok(None);
    };
    guard.entries(argv.len())?;
    let Some(tool) = harnesses::tool(harness) else {
        return Ok(None);
    };
    if !session_command(harness, &argv) {
        return Ok(None);
    }
    guard.check()?;
    let cwd = process_cwd(number).ok_or("Foreground project directory is unavailable")?;
    let cwd = cwd
        .to_str()
        .ok_or("Project directory is not UTF-8")?
        .to_owned();
    guard.byte_work(cwd.len())?;
    let Some(environment) = process_environment(root, number) else {
        return Ok(None);
    };
    let profile = session_profile(&tool, environment.iter().map(|v| v.as_bytes()), guard)?;
    if process_info(root, number).map(|after| after.start) != Some(info.start) {
        return Err("Foreground process changed while reading metadata".into());
    }
    guard.check()?;
    Ok(Some(ProcessEvidence {
        pid: pid.into(),
        start,
        argv,
        cwd,
        profile,
    }))
}
#[cfg(unix)]
fn read_process_procfs(
    adapter: &Terminals,
    pid: &str,
    harness: &str,
    guard: &mut MetadataGuard<'_>,
) -> Result<Option<ProcessEvidence>> {
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Invalid foreground process ID".into());
    }
    guard.processes(1)?;
    let directory = adapter.proc_root.join(pid);
    let Some(raw_stat) = guard.process_file_optional(&directory.join("stat"))? else {
        return Ok(None);
    };
    let fields = stat_fields(&raw_stat)?;
    let start = fields
        .get(19)
        .filter(|s| s.parse::<u64>().is_ok_and(|start| start > 0))
        .ok_or("Foreground process generation is unavailable")?
        .to_string();
    let Some(raw) = guard.process_file_optional(&directory.join("cmdline"))? else {
        return Ok(None);
    };
    let mut argv = Vec::new();
    for arg in raw.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        guard.entries(1)?;
        argv.push(String::from_utf8_lossy(arg).into_owned());
    }
    let Some(tool) = harnesses::tool(harness) else {
        return Ok(None);
    };
    if !session_command(harness, &argv) {
        return Ok(None);
    }
    guard.check()?;
    let cwd = fs::read_link(directory.join("cwd"))
        .map_err(|_| "Foreground project directory is unavailable")?;
    let cwd = cwd.to_str().ok_or("Project directory is not UTF-8")?;
    guard.byte_work(cwd.len())?;
    let Some(raw_env) = guard.process_file_optional(&directory.join("environ"))? else {
        return Ok(None);
    };
    let profile = session_profile(&tool, raw_env.split(|b| *b == 0), guard)?;
    let Some(after) = guard.process_file_optional(&directory.join("stat"))? else {
        return Err("Foreground process disappeared while reading metadata".into());
    };
    if stat_fields(&after)?.get(19).copied() != Some(start.as_str()) {
        return Err("Foreground process changed while reading metadata".into());
    }
    guard.check()?;
    Ok(Some(ProcessEvidence {
        pid: pid.into(),
        start,
        argv,
        cwd: cwd.into(),
        profile,
    }))
}
#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use super::*;
    /// A live harness process yields its command, working directory, profile
    /// and incarnation through libproc.
    #[test]
    fn live_session_evidence_is_read_through_libproc() {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "dd-session-live-{}-{}",
            std::process::id(),
            crate::test_support::unique_nanos()
        ));
        fs::create_dir_all(root.join("project")).unwrap();
        // Apple platform binaries hide their environment; this test binary does not.
        let image = std::env::current_exe().unwrap();
        let program = root.join("codex");
        std::os::unix::fs::symlink(&image, &program).unwrap();
        let project = root.join("project").canonicalize().unwrap();
        let mut child = crate::test_support::idle_process(&program)
            .current_dir(&project)
            .env("CODEX_HOME", root.join("profile"))
            .spawn()
            .unwrap();
        let adapter = Terminals::default();
        let pid = child.id().to_string();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let evidence = loop {
            let mut guard = adapter.metadata_guard().unwrap();
            if let Some(evidence) = read_process(&adapter, &pid, "codex", &mut guard).unwrap() {
                break evidence;
            }
            assert!(std::time::Instant::now() < deadline, "no session evidence");
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let mut guard = adapter.metadata_guard().unwrap();
        let other = read_process(&adapter, &pid, "claude", &mut guard).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(evidence.argv[0].ends_with("/codex"));
        assert_eq!(Path::new(&evidence.cwd), project);
        assert_eq!(Path::new(&evidence.profile), root.join("profile"));
        assert!(evidence.start.parse::<u64>().unwrap() > 0);
        assert!(other.is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
#[cfg(all(test, unix))]
fn foreground(adapter: &Terminals, pane: &Value, harness: &str) -> Result<Option<ProcessEvidence>> {
    foreground_guarded(adapter, pane, harness, &mut adapter.metadata_guard()?)
}
fn foreground_guarded(
    adapter: &Terminals,
    pane: &Value,
    harness: &str,
    guard: &mut MetadataGuard<'_>,
) -> Result<Option<ProcessEvidence>> {
    guard.check()?;
    let id = terminals::identity(pane)?;
    let mut pids = Vec::new();
    if id.backend == Backend::Herdr {
        let info =
            adapter.herdr_result(&id.endpoint, &["pane", "process-info", "--pane", &id.pane])?;
        guard.check()?;
        if let Some(rows) = info["process_info"]["foreground_processes"].as_array() {
            guard.entries(rows.len())?;
            if !rows.is_empty()
                && info["process_info"]["pane_id"].as_str() != Some(id.pane.as_str())
            {
                return Err("Foreground pane metadata changed".into());
            }
            for row in rows {
                if let Some(pid) = row.get("pid") {
                    let pid = pid
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| pid.to_string());
                    guard.byte_work(pid.len())?;
                    pids.push(pid);
                }
            }
        }
    } else {
        let pid = field(pane, "pid");
        if !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()) {
            pids.push(pid.clone());
            // macOS: the foreground group leaders on the pane's terminal, by libproc.
            #[cfg(target_os = "macos")]
            let procfs = adapter.proc_root != Path::new("/proc");
            #[cfg(target_os = "macos")]
            if !procfs {
                use crate::platform::{process_info, process_list};
                let root = adapter.proc_root.as_path();
                guard.processes(1)?;
                let tty = pid
                    .parse::<u32>()
                    .ok()
                    .and_then(|n| process_info(root, n))
                    .map(|info| info.tty)
                    .filter(|tty| *tty != 0);
                if let Some(tty) = tty {
                    for candidate in process_list(root) {
                        guard.snapshot_entries(1)?;
                        if process_info(root, candidate).is_some_and(|info| {
                            info.tty == tty && i64::from(info.pgid) == info.foreground
                        }) {
                            pids.push(candidate.to_string());
                        }
                    }
                }
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            let procfs = true;
            #[cfg(unix)]
            let raw = if procfs {
                guard.process_file_optional(&adapter.proc_root.join(&pid).join("stat"))?
            } else {
                None
            };
            #[cfg(unix)]
            if let Some(raw) = raw {
                let fields = stat_fields(&raw)?;
                if let Some(tty) = fields.get(4).filter(|tty| **tty != "0") {
                    let entries = fs::read_dir(&adapter.proc_root)
                        .map_err(|_| "Cannot enumerate foreground processes")?;
                    for entry in entries {
                        guard.snapshot_entries(1)?;
                        let entry = entry.map_err(|_| "Cannot inspect foreground process entry")?;
                        let name = entry.file_name();
                        let Some(name) = name.to_str().filter(|name| {
                            !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit())
                        }) else {
                            continue;
                        };
                        let Some(stat) = guard.process_file_optional(&entry.path().join("stat"))?
                        else {
                            continue;
                        };
                        let row = stat_fields(&stat)?;
                        if row.get(4) == Some(tty) && row.get(2) == row.get(5) {
                            pids.push(name.to_owned());
                        }
                    }
                }
            }
        }
    }
    pids.sort();
    pids.dedup();
    let mut candidates = Vec::new();
    for pid in pids {
        if let Some(evidence) = read_process(adapter, &pid, harness, guard)? {
            if id.backend == Backend::Native {
                let process = super::hook_runtime::ProcessIdentity {
                    pid: evidence.pid.parse().map_err(|_| "Invalid native PID")?,
                    start_ticks: evidence
                        .start
                        .parse()
                        .map_err(|_| "Invalid native process generation")?,
                };
                #[cfg(unix)]
                let verified = super::hook_runtime::verify_process_metadata(
                    &process,
                    &adapter.proc_root,
                    guard,
                );
                #[cfg(windows)]
                let verified = super::hook_runtime::verify_process(&process, &adapter.proc_root)
                    .map_err(|e| e.to_string());
                guard.check()?;
                if verified.is_err() {
                    continue;
                }
            }
            if !candidates.is_empty() {
                return Err("Foreground harness identity is ambiguous".into());
            }
            candidates.push(evidence);
        }
    }
    guard.check()?;
    Ok(candidates.pop())
}
fn process_fingerprint(evidence: &Option<ProcessEvidence>) -> Value {
    evidence.as_ref().map(|p|json!({"pid":p.pid,"start":p.start,"argv_sha256":format!("{:x}",Sha256::digest(serde_json::to_vec(&p.argv).expect("argv strings"))),"cwd":p.cwd,"profile":p.profile})).unwrap_or(Value::Null)
}
fn capture_fingerprint(
    _adapter: &Terminals,
    _pane: &Value,
    _harness: &str,
    evidence: &Option<ProcessEvidence>,
    guard: &mut MetadataGuard<'_>,
) -> Result<Value> {
    guard.check()?;
    let observed = process_fingerprint(evidence);
    #[cfg(windows)]
    return native_fingerprint_or(observed, || {
        windows_native_process(_adapter, _pane, _harness, guard)
    });
    #[cfg(not(windows))]
    Ok(observed)
}
#[cfg(any(windows, test))]
fn native_fingerprint_or(observed: Value, native: impl FnOnce() -> Result<Value>) -> Result<Value> {
    if observed.is_null() {
        native()
    } else {
        Ok(observed)
    }
}
fn check_process(
    adapter: &Terminals,
    evidence: &Option<ProcessEvidence>,
    harness: &str,
    guard: &mut MetadataGuard<'_>,
) -> Result<()> {
    guard.check()?;
    if let Some(before) = evidence {
        let after = read_process(adapter, &before.pid, harness, guard)?
            .ok_or("Foreground harness disappeared")?;
        if before.start != after.start
            || before.argv != after.argv
            || before.profile != after.profile
            || before.cwd != after.cwd
        {
            return Err("Foreground harness changed during checkpoint".into());
        }
    }
    Ok(())
}

/// Refresh conversation identity without reading terminal contents. Hook and
/// transport observations take precedence over an explicit launch selector.
pub fn enrich_sessions(
    directory: &Path,
    snapshot: &mut Value,
    adapter: &Terminals,
) -> Result<usize> {
    let settings = SessionStore {
        directory: directory.to_path_buf(),
    }
    .checkpoint_settings()?;
    let mut metadata = metadata_guard(adapter, &settings)?;
    enrich_sessions_guarded(directory, snapshot, adapter, &settings, &mut metadata)
}
fn enrich_sessions_guarded(
    directory: &Path,
    snapshot: &mut Value,
    adapter: &Terminals,
    settings: &super::runtime_settings::Settings,
    metadata: &mut MetadataGuard<'_>,
) -> Result<usize> {
    metadata.check()?;
    if let Some(panes) = snapshot["panes"].as_array_mut() {
        for pane in panes {
            if pane["agent_session"]["source"] == "foreground" {
                pane["agent_session"] = Value::Null;
            }
        }
    }
    let hooks = super::hook_runtime::enrich_native_sessions_guarded(
        directory,
        snapshot,
        adapter,
        Some(settings),
        metadata,
    );
    let mut enriched = 0;
    if let Some(panes) = snapshot["panes"].as_array_mut() {
        for pane in panes {
            if terminals::reported_session(pane).is_some()
                || crate::adapters::pane_observations::conflicted(pane)
            {
                continue;
            }
            let Some(harness) = pane["agent"].as_str().and_then(harnesses::canonical_id) else {
                continue;
            };
            if let Some(budget) = &adapter.discovery_budget {
                budget.time_left()?;
            }
            // Revalidate the terminal generation on both sides of discovery;
            // process evidence must never cross a replaced pane or PID.
            let Ok(actual) = adapter.verify(pane) else {
                continue;
            };
            if actual["agent"].as_str().and_then(harnesses::canonical_id) != Some(harness) {
                continue;
            }
            if let Some(sid) = terminals::reported_session(&actual) {
                pane["agent_session"] = json!({"kind":"id","agent":harness,"value":sid});
                enriched += 1;
                continue;
            }
            let discovered = foreground_session_id(adapter, &actual, harness, metadata);
            metadata.check()?;
            let Ok(Some(sid)) = discovered else {
                continue;
            };
            let Ok(again) = adapter.verify(&actual) else {
                continue;
            };
            if again["agent"].as_str().and_then(harnesses::canonical_id) != Some(harness)
                || again.get("agent_session") != actual.get("agent_session")
            {
                continue;
            }
            pane["agent"] = json!(harness);
            pane["agent_session"] =
                json!({"kind":"id","agent":harness,"value":sid,"source":"foreground"});
            enriched += 1;
        }
    }
    metadata.check()?;
    hooks
        .map(|hooks| hooks + enriched)
        .map_err(|error| error.to_string())
}

#[cfg(unix)]
fn foreground_session_id(
    adapter: &Terminals,
    pane: &Value,
    harness: &str,
    metadata: &mut MetadataGuard<'_>,
) -> Result<Option<String>> {
    let Some(process) = foreground_guarded(adapter, pane, harness, metadata)? else {
        return Ok(None);
    };
    let sid = harnesses::launch_conversation(&process.argv).map(|(_, sid)| sid);
    check_process(adapter, &Some(process), harness, metadata)?;
    Ok(sid)
}

#[cfg(any(windows, test))]
fn structured_foreground_guarded(
    info: &Value,
    pane: &Value,
    harness: &str,
    mut verify: impl FnMut(u32, &str) -> Result<String>,
    metadata: &mut MetadataGuard<'_>,
) -> Result<Option<ProcessEvidence>> {
    if info["pane_id"] != pane["pane_id"] {
        return Err("Foreground pane changed".into());
    }
    let rows = info["foreground_processes"]
        .as_array()
        .ok_or("Missing foreground processes")?;
    metadata.entries(rows.len())?;
    metadata.processes(rows.len())?;
    let mut seen = BTreeSet::new();
    let mut found = None;
    for row in rows {
        let pid = row["pid"]
            .as_u64()
            .or_else(|| row["pid"].as_str()?.parse().ok())
            .and_then(|pid| u32::try_from(pid).ok())
            .filter(|pid| *pid > 0)
            .ok_or("Invalid foreground PID")?;
        if !seen.insert(pid) {
            return Err("Ambiguous foreground PID".into());
        }
        // An argv array is structured evidence. A display cmdline is not a
        // portable quoting grammar and can contain prompts or credentials.
        let Some(values) = row["argv"].as_array() else {
            continue;
        };
        metadata.entries(values.len())?;
        let mut argv = Vec::new();
        for value in values {
            let value = value.as_str().ok_or("Invalid foreground arguments")?;
            metadata.byte_work(value.len())?;
            argv.push(value.to_owned());
        }
        if harnesses::conversation_argv(&argv, harness).is_none() {
            continue;
        }
        let start = verify(pid, &argv[0])?;
        if found.is_some() {
            return Err("Foreground harness identity is ambiguous".into());
        }
        found = Some(ProcessEvidence {
            pid: pid.to_string(),
            start,
            argv,
            cwd: first(pane, &["foreground_cwd", "cwd"]),
            profile: String::new(),
        });
    }
    metadata.check()?;
    Ok(found)
}

#[cfg(test)]
fn structured_foreground(
    info: &Value,
    pane: &Value,
    harness: &str,
    verify: impl FnMut(u32, &str) -> Result<String>,
) -> Result<Option<ProcessEvidence>> {
    structured_foreground_guarded(
        info,
        pane,
        harness,
        verify,
        &mut MetadataGuard::new(Default::default())?,
    )
}

#[cfg(windows)]
fn foreground_session_id(
    adapter: &Terminals,
    pane: &Value,
    harness: &str,
    metadata: &mut MetadataGuard<'_>,
) -> Result<Option<String>> {
    let id = terminals::identity(pane)?;
    if id.backend != Backend::Herdr {
        return Ok(None);
    }
    let mut inspect = || -> Result<Option<ProcessEvidence>> {
        let info =
            adapter.herdr_result(&id.endpoint, &["pane", "process-info", "--pane", &id.pane])?;
        metadata.check()?;
        structured_foreground_guarded(
            &info["process_info"],
            pane,
            harness,
            |pid, executable| {
                let process = crate::platform::process_metadata(pid)
                    .map_err(|_| "Foreground process changed")?;
                if process.owner
                    != crate::platform::current_identity().map_err(|e| e.to_string())?
                    || process
                        .executable
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_none_or(|name| {
                            !name.eq_ignore_ascii_case(
                                executable.rsplit(['/', '\\']).next().unwrap_or_default(),
                            )
                        })
                {
                    return Err("Foreground executable changed".into());
                }
                Ok(process.creation_time.to_string())
            },
            metadata,
        )
    };
    let before = inspect()?;
    let after = inspect()?;
    if process_fingerprint(&before) != process_fingerprint(&after) {
        return Err("Foreground process changed during discovery".into());
    }
    let Some(process) = after else {
        return Ok(None);
    };
    Ok(harnesses::launch_conversation(&process.argv).map(|(_, sid)| sid))
}

impl SessionStore {
    /// Native transport proves terminal identity; private hooks and current
    /// foreground processes independently establish harness/session metadata.
    fn verify_pane(
        &self,
        adapter: &Terminals,
        pane: &Value,
        metadata: &mut MetadataGuard<'_>,
    ) -> Result<Value> {
        metadata.check()?;
        let actual = adapter.verify(pane)?;
        metadata.check()?;
        if terminals::identity(&actual)?.backend != Backend::Native
            && terminals::reported_session(&actual).is_some()
        {
            return Ok(actual);
        }
        let mut snapshot = json!({"panes":[actual]});
        enrich_sessions_guarded(
            &self.directory,
            &mut snapshot,
            adapter,
            &self.checkpoint_settings()?,
            metadata,
        )?;
        let mut actual = snapshot["panes"][0].take();
        if terminals::identity(&actual)?.backend == Backend::Native
            && actual["agent_session"].is_null()
        {
            let mut matches = Vec::new();
            for tool in harnesses::tools() {
                #[cfg(unix)]
                if let Some(process) = foreground_guarded(adapter, &actual, tool.id, metadata)? {
                    matches.push((tool.id, Some(process)));
                }
                #[cfg(windows)]
                if !windows_native_process(adapter, &actual, tool.id, metadata)?.is_null() {
                    matches.push((tool.id, None::<ProcessEvidence>));
                }
            }
            if matches.len() > 1 {
                return Err(
                    "Native foreground harness is ambiguous; select it again when idle".into(),
                );
            }
            if let Some((harness, process)) = matches.pop() {
                actual["agent"] = json!(harness);
                if let Some(process) = process {
                    actual["foreground_cwd"] = json!(process.cwd);
                }
            }
        }
        Ok(actual)
    }
    /// Record only a confirmed new viewer terminal, never an uncertain pane
    /// creation. Registration failure must not retry the already submitted launch.
    pub fn register_view(
        &self,
        source: &Value,
        receipt: &Receipt,
        mechanism: &str,
        settings: &super::runtime_settings::Settings,
    ) -> Result<()> {
        settings.validate()?;
        if receipt.state != crate::adapters::terminal_actions::ReceiptState::Submitted {
            return Err("Viewer creation is unconfirmed; registration was skipped".into());
        }
        let viewer = receipt
            .terminal
            .as_ref()
            .ok_or("Viewer terminal identity is missing")?;
        let source_id = terminals::identity(source)?;
        let viewer_id = terminals::identity(viewer)?;
        if source_id == viewer_id {
            return Err("A terminal cannot be its own viewer".into());
        }
        let _lock = lock(&self.directory)?;
        let mut guard = super::view_history::Guard::new(settings)?;
        let mut rows = super::view_history::read(&self.directory, &mut guard)?;
        // Replace only this coordinate's previous viewer incarnation. Other
        // histories, including unresolved legacy records, are left intact.
        rows.retain(|row| {
            !super::view_records::viewer(row).is_some_and(|id| {
                id.backend == viewer_id.backend
                    && id.endpoint == viewer_id.endpoint
                    && id.pane == viewer_id.pane
            })
        });
        let record = NewView {
            version: 2,
            source: &source_id,
            viewer: &viewer_id,
            source_label: tree::title(source, "").chars().take(80).collect(),
            server_pid: viewer["server_pid"].as_str().unwrap_or(""),
            mechanism,
            created_at: utc_now(),
        };
        let record = guard.encode(&record)?;
        rows.push(serde_json::from_slice(&record).map_err(|_| "Cannot decode new viewer record")?);
        let limit = settings.terminal_view_history_limit as usize;
        if rows.len() > limit {
            rows.drain(..rows.len() - limit);
        }
        let bytes = guard.encode(&json!({"version":2,"views":rows}))?;
        guard.time_left()?;
        layouts::write_private_limit(
            &self.directory.join("views.json"),
            &bytes,
            guard.file_limit as u64,
        )
    }
    /// Project exact live viewers once. Historical/stale rows remain on disk;
    /// ambiguous legacy encodings and conflicting identities require review.
    pub fn live_views(
        &self,
        snapshot: &Value,
        settings: &super::runtime_settings::Settings,
        budget: &super::discovery_budget::Budget,
    ) -> Result<super::view_records::Observation> {
        let mut guard = super::view_history::Guard::new(settings)?;
        let rows = super::view_history::read(&self.directory, &mut guard)?;
        if rows.is_empty() {
            return Ok(super::view_records::Observation::default());
        }
        let resolved = super::view_records::resolve(&rows, || {
            guard.time_left().is_ok() && budget.time_left().is_ok()
        });
        guard.time_left()?;
        budget.time_left()?;
        let mut observation = super::view_records::Observation {
            rows: Vec::new(),
            unresolved: resolved.unresolved,
        };
        let mut panes: std::collections::HashMap<TerminalId, Vec<&Value>> =
            std::collections::HashMap::new();
        let captured_rows = snapshot
            .get("panes")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let captured = crate::adapters::pane_observations::index(captured_rows);
        for pane in captured.panes.iter().map(|pane| pane.as_ref()) {
            if let Ok(identity) = terminals::identity(pane) {
                panes.entry(identity).or_default().push(pane);
            }
        }
        for record in resolved.records {
            guard.time_left()?;
            budget.time_left()?;
            let Some(matching) = panes.get(&record.viewer) else {
                continue;
            };
            if matching
                .iter()
                .any(|pane| crate::adapters::pane_observations::conflicted(pane))
            {
                observation.unresolved += 1;
                continue;
            }
            if ["server_pid", "source_label", "mechanism", "created_at"]
                .iter()
                .any(|key| record.row.get(*key).is_some_and(|field| !field.is_string()))
            {
                observation.unresolved += 1;
                continue;
            }
            let expected = record.row["server_pid"].as_str().unwrap_or("");
            if !expected.is_empty()
                && matching
                    .iter()
                    .any(|pane| pane["server_pid"].as_str().unwrap_or("") != expected)
            {
                continue;
            }
            let pane = matching[0];
            let projected = LiveView {
                version: 2,
                source: &record.source,
                viewer: &record.viewer,
                backend: &pane["backend"],
                server: &pane["server"],
                pane_id: &pane["pane_id"],
                terminal_id: &pane["terminal_id"],
                label: tree::title(pane, ""),
                source_label: record.row["source_label"].as_str(),
                mechanism: record.row["mechanism"].as_str(),
                server_pid: record.row["server_pid"].as_str(),
                created_at: record.row["created_at"].as_str(),
            };
            if super::discovery_budget::json_size(&projected, budget.remaining_bytes()).is_err() {
                let _ = budget.capacity_error();
                observation.unresolved += 1;
                continue;
            }
            let bytes =
                serde_json::to_vec(&projected).map_err(|_| "Cannot encode viewer observation")?;
            guard.check_json(&bytes)?;
            if budget
                .admit_json(&bytes)
                .and_then(|()| budget.admit_text(1, 0))
                .is_err()
            {
                observation.unresolved += 1;
                continue;
            }
            observation.rows.push(
                serde_json::from_slice(&bytes).map_err(|_| "Cannot decode viewer observation")?,
            );
        }
        guard.time_left()?;
        budget.time_left()?;
        Ok(observation)
    }
    #[cfg(all(test, unix))]
    fn live_views_default(&self, snapshot: &Value) -> Result<super::view_records::Observation> {
        let settings = super::runtime_settings::Settings::default();
        self.live_views(
            snapshot,
            &settings,
            &super::discovery_budget::Budget::new(&settings)?,
        )
    }
    pub(crate) fn checkpoint_settings(&self) -> Result<super::runtime_settings::Settings> {
        layouts::read_private(&self.directory.join("control.json"))?
            .map(|bytes| ControlConfig::parse(&bytes).map(|config| config.runtime))
            .transpose()
            .map(|settings| settings.unwrap_or_default())
    }
    pub fn list(&self) -> Result<Vec<CheckpointRow>> {
        self.list_with_settings(&self.checkpoint_settings()?)
    }
    pub fn list_with_settings(
        &self,
        settings: &super::runtime_settings::Settings,
    ) -> Result<Vec<CheckpointRow>> {
        settings.validate()?;
        self.list_guard(&mut crate::core::checkpoint_budget::Guard::new(
            settings.checkpoint_policy(),
        )?)
        .map(|(rows, _)| rows)
    }
    fn list_guard(
        &self,
        guard: &mut crate::core::checkpoint_budget::Guard<'_>,
    ) -> Result<(Vec<CheckpointRow>, BTreeSet<String>)> {
        let removed = checkpoints::removals(&self.directory, guard)?;
        let rows = Registry::load_with(&self.directory, guard)?
            .checkpoints
            .into_iter()
            .map(|checkpoint| {
                guard.time_left()?;
                let bytes = guard.encode(&checkpoint)?;
                Ok(CheckpointRow {
                    key: format!("checkpoint:{:x}", Sha256::digest(&bytes)),
                    checkpoint,
                    registry: self
                        .directory
                        .join("dashboard-checkpoints.json")
                        .to_string_lossy()
                        .into_owned(),
                })
            })
            .filter(|result: &Result<CheckpointRow>| {
                result
                    .as_ref()
                    .map_or(true, |row| !removed.contains(&row.key))
            })
            .collect::<Result<Vec<_>>>()?;
        guard.time_left()?;
        Ok((rows, removed))
    }
    pub fn observe_checkpoints(
        &self,
        settings: &super::runtime_settings::Settings,
        budget: &super::discovery_budget::Budget,
    ) -> Result<Vec<Value>> {
        settings.validate()?;
        let progress = || budget.time_left().map(|_| ());
        let mut guard = crate::core::checkpoint_budget::Guard::new(settings.checkpoint_policy())?
            .with_progress(&progress);
        let (rows, _) = self.list_guard(&mut guard)?;
        let mut result = Vec::new();
        for row in rows {
            let bytes = guard.encode(&row)?;
            budget.admit_json(&bytes)?;
            budget.admit_text(1, 0)?;
            result.push(
                serde_json::from_slice(&bytes)
                    .map_err(|_| "Cannot encode saved checkpoint observation")?,
            );
        }
        guard.time_left()?;
        Ok(result)
    }
    pub fn get(&self, key: &str) -> Result<CheckpointRow> {
        self.get_with_settings(key, &self.checkpoint_settings()?)
    }
    pub fn get_with_settings(
        &self,
        key: &str,
        settings: &super::runtime_settings::Settings,
    ) -> Result<CheckpointRow> {
        self.list_with_settings(settings)?
            .into_iter()
            .find(|row| row.key == key)
            .ok_or_else(|| "Checkpoint changed or was removed; select it again".into())
    }
    /// Removing a reference never deletes native history or terminal output files.
    /// Tombstones also suppress matching entries in untouched legacy registries.
    pub fn remove(&self, key: &str) -> Result<()> {
        self.remove_with_settings(key, &self.checkpoint_settings()?)
    }
    pub fn remove_with_settings(
        &self,
        key: &str,
        settings: &super::runtime_settings::Settings,
    ) -> Result<()> {
        settings.validate()?;
        let _lock = lock(&self.directory)?;
        let mut guard = crate::core::checkpoint_budget::Guard::new(settings.checkpoint_policy())?;
        let (rows, mut removed) = self.list_guard(&mut guard)?;
        if !rows.iter().any(|row| row.key == key) {
            return Err("Checkpoint changed or was removed; select it again".into());
        }
        guard.entries(1)?;
        removed.insert(key.into());
        let bytes = guard.encode(&removed)?;
        guard.time_left()?;
        layouts::write_private_limit(
            &self.directory.join("checkpoint-removals.json"),
            &bytes,
            guard.policy.file_bytes,
        )
    }
    pub fn live_source(
        &self,
        adapter: &Terminals,
        config: &ControlConfig,
        environment: &BTreeMap<String, String>,
        pane: &Value,
        supplied_sid: &str,
    ) -> Result<Checkpoint> {
        let mut metadata = metadata_guard(adapter, &config.runtime)?;
        self.live_source_guarded(
            adapter,
            config,
            environment,
            pane,
            supplied_sid,
            &mut metadata,
        )
    }
    fn live_source_guarded(
        &self,
        adapter: &Terminals,
        config: &ControlConfig,
        environment: &BTreeMap<String, String>,
        pane: &Value,
        supplied_sid: &str,
        metadata: &mut MetadataGuard<'_>,
    ) -> Result<Checkpoint> {
        let actual = self.verify_pane(adapter, pane, metadata)?;
        let observed_harness = harness_name(&actual);
        let mut harness = observed_harness.clone();
        if matches!(harness.as_str(), "" | "node" | "nodejs") {
            let process = foreground_guarded(adapter, &actual, "pi", metadata)?
                .ok_or("Terminal has no verified managed harness")?;
            pi_sessions::live(
                &self.directory,
                &terminals::identity(&actual)?,
                &process.pid,
                &process.start,
                &process.argv,
                metadata,
            )?
            .ok_or("Terminal has no verified Pi session")?;
            harness = "pi".into();
        }
        let reported = terminals::reported_session(&actual)
            .unwrap_or_default()
            .to_owned();
        let evidence = foreground_guarded(adapter, &actual, &harness, metadata)?;
        let discovered_pi = if harness == "pi" {
            evidence
                .as_ref()
                .map(|process| {
                    pi_sessions::live(
                        &self.directory,
                        &terminals::identity(&actual)?,
                        &process.pid,
                        &process.start,
                        &process.argv,
                        metadata,
                    )
                })
                .transpose()?
                .flatten()
        } else {
            None
        };
        let reported = if reported.is_empty() {
            discovered_pi
                .as_ref()
                .map(|session| session.id.clone())
                .unwrap_or_default()
        } else {
            reported
        };
        #[cfg(windows)]
        let discovered_hermes = if harness == "hermes" && reported.is_empty() {
            let process = windows_native_process(adapter, &actual, &harness, metadata)?;
            let tool = harnesses::tool("hermes").ok_or("Unknown Hermes adapter")?;
            let root = harnesses::profile_directory(&tool, environment, &config.profiles)?;
            super::hermes_sessions::live(&root, &process, metadata)?
        } else {
            None
        };
        #[cfg(windows)]
        let reported = discovered_hermes
            .as_ref()
            .map(|session| session.0.clone())
            .unwrap_or(reported);
        let pi_parent = if harness == "pi" {
            evidence
                .as_ref()
                .map(|process| {
                    pi_sessions::fork_parent(&process.argv, Path::new(&process.cwd), metadata)
                })
                .transpose()?
                .flatten()
        } else {
            None
        };
        let reported = if let Some(process) = &evidence {
            checkpoints::resolve_sid_with_pi_parent(
                harnesses::conversation_argv(&process.argv, &harness).unwrap_or(&process.argv),
                &harness,
                &reported,
                pi_parent.as_ref().map(|parent| parent.id.as_str()),
            )?
            .unwrap_or_default()
        } else {
            reported
        };
        let supplied_sid = supplied_sid.trim();
        if !supplied_sid.is_empty()
            && let Some(process) = &evidence
            && checkpoints::resolve_sid_with_pi_parent(
                harnesses::conversation_argv(&process.argv, &harness).unwrap_or(&process.argv),
                &harness,
                supplied_sid,
                pi_parent.as_ref().map(|parent| parent.id.as_str()),
            )?
            .is_none()
        {
            return Err(
                "Cannot confirm this conversation ID from the process launch; forks need a distinct child ID"
                    .into(),
            );
        }
        if !supplied_sid.is_empty() && !reported.is_empty() && supplied_sid != reported {
            return Err("Entered session ID disagrees with live native metadata".into());
        }
        let sid = if reported.is_empty() {
            supplied_sid.to_owned()
        } else {
            reported.clone()
        };
        let cwd = evidence
            .as_ref()
            .map(|p| p.cwd.clone())
            .unwrap_or_else(|| first(&actual, &["foreground_cwd", "cwd"]));
        if !Path::new(&cwd).is_dir() {
            return Err("Cannot determine an existing project directory".into());
        }
        let profile = if let Some(process) = &evidence {
            process.profile.clone()
        } else if let Some(tool) = harnesses::tool(&harness) {
            harnesses::profile_directory(&tool, environment, &config.profiles)?
                .to_string_lossy()
                .into_owned()
        } else {
            String::new()
        };
        #[cfg(windows)]
        let profile = discovered_hermes
            .as_ref()
            .map(|session| session.1.to_string_lossy().into_owned())
            .unwrap_or(profile);
        let row = Checkpoint {
            name: format!("Live {harness} in {}", field(&actual, "pane_id")),
            harness: harness.clone(),
            sid,
            profile,
            cwd,
            origin: if !reported.is_empty() {
                "Native metadata"
            } else if !supplied_sid.is_empty() {
                "User supplied ID"
            } else {
                "Not reported"
            }
            .into(),
            created_at: utc_now(),
            context_file: String::new(),
            source: Some(terminal_ref(&actual)?),
        };
        let again = self.verify_pane(adapter, pane, metadata)?;
        if harness_name(&again) != observed_harness
            || again.get("agent_session") != actual.get("agent_session")
        {
            return Err("Native harness metadata changed; refresh".into());
        }
        check_process(adapter, &evidence, &harness, metadata)?;
        if let Some(parent) = &pi_parent {
            parent.verify(metadata)?;
        }
        if let Some(session) = &discovered_pi {
            session.verify(metadata)?;
        }
        #[cfg(windows)]
        if let Some(session) = &discovered_hermes {
            let process = windows_native_process(adapter, &again, &harness, metadata)?;
            let tool = harnesses::tool("hermes").ok_or("Unknown Hermes adapter")?;
            let root = harnesses::profile_directory(&tool, environment, &config.profiles)?;
            if super::hermes_sessions::live(&root, &process, metadata)?.as_ref() != Some(session) {
                return Err("Hermes conversation changed during discovery; refresh".into());
            }
        }
        metadata.check()?;
        // A live source without an ID can still be offered for context handoff.
        if !row.sid.is_empty() {
            row.validate()?;
        }
        Ok(row)
    }
    /// Freeze only ephemeral foreground evidence for a pending local dialog.
    /// It is never written to the portable checkpoint registry.
    pub fn freeze_live_source(
        &self,
        adapter: &Terminals,
        config: &ControlConfig,
        environment: &BTreeMap<String, String>,
        pane: &Value,
        supplied_sid: &str,
    ) -> Result<(Checkpoint, Value)> {
        let mut metadata = metadata_guard(adapter, &config.runtime)?;
        let before = self.verify_pane(adapter, pane, &mut metadata)?;
        let harness = harness_name(&before);
        let evidence = foreground_guarded(adapter, &before, &harness, &mut metadata)?;
        let checkpoint = self.live_source_guarded(
            adapter,
            config,
            environment,
            pane,
            supplied_sid,
            &mut metadata,
        )?;
        check_process(adapter, &evidence, &harness, &mut metadata)?;
        let after = self.verify_pane(adapter, pane, &mut metadata)?;
        if before.get("agent_session") != after.get("agent_session")
            || before.get("agent") != after.get("agent")
        {
            return Err("Handoff source changed while preparing; select it again".into());
        }
        let mut frozen = before;
        frozen["_daedalus_handoff_process"] =
            capture_fingerprint(adapter, &frozen, &harness, &evidence, &mut metadata)?;
        if checkpoint.sid.is_empty() && frozen["_daedalus_handoff_process"].is_null() {
            return Err(
                "Handoff requires a native session ID or a verifiable foreground process".into(),
            );
        }
        Ok((checkpoint, frozen))
    }
    pub fn save(
        &self,
        adapter: &Terminals,
        config: &ControlConfig,
        environment: &BTreeMap<String, String>,
        pane: &Value,
        request: &SaveRequest,
    ) -> Result<CheckpointRow> {
        let _lock = lock(&self.directory)?;
        let mut guard =
            crate::core::checkpoint_budget::Guard::new(config.runtime.checkpoint_policy())?;
        let mut metadata = metadata_guard(adapter, &config.runtime)?;
        let saved_rows = self.list_guard(&mut guard)?.0;
        guard.entries(1)?;
        let before = self.verify_pane(adapter, pane, &mut metadata)?;
        let observed = harness_name(&before);
        // A terminal excerpt is useful even when no native conversation can be
        // identified. Never promote shell/Node metadata into a provider ID.
        let context_only = harnesses::tool(&observed).is_none()
            && request.include_output
            && request.sid.trim().is_empty();
        let mut row = if context_only {
            // Preserve registered Pi identity when it is genuinely observable;
            // failure here only falls back to an explicitly context-only record.
            let live =
                self.live_source_guarded(adapter, config, environment, pane, "", &mut metadata);
            metadata.check()?;
            match live {
                Ok(row) if row.harness == "pi" => row,
                _ => self.capture_terminal_context(
                    adapter,
                    pane,
                    &before,
                    &request.name,
                    &mut metadata,
                )?,
            }
        } else {
            if harnesses::tool(&observed).is_none()
                && !matches!(observed.as_str(), "" | "node" | "nodejs")
            {
                return Err(
                    "This terminal has no verified native harness; save visible output instead"
                        .into(),
                );
            }
            self.live_source_guarded(
                adapter,
                config,
                environment,
                pane,
                &request.sid,
                &mut metadata,
            )?
        };
        let process = if row.context_file.is_empty() {
            foreground_guarded(adapter, &before, &row.harness, &mut metadata)?
        } else {
            None
        };
        row.name = request.name.trim().into();
        if request.include_output && row.context_file.is_empty() {
            let mut frozen = before.clone();
            frozen["_daedalus_handoff_process"] =
                capture_fingerprint(adapter, &frozen, &row.harness, &process, &mut metadata)?;
            row = self.capture_handoff_guarded(adapter, &frozen, row, &mut metadata)?;
        }
        row.validate()?;
        let after = self.verify_pane(adapter, pane, &mut metadata)?;
        check_process(adapter, &process, &row.harness, &mut metadata)?;
        if before.get("agent_session") != after.get("agent_session")
            || before.get("agent") != after.get("agent")
        {
            return Err("Harness changed while saving checkpoint".into());
        }
        let mut registry = Registry {
            checkpoints: saved_rows.into_iter().map(|row| row.checkpoint).collect(),
        };
        registry.add_verified(row.clone(), &terminal_ref(&before)?, &terminal_ref(&after)?)?;
        registry.save_with(&self.directory, &mut guard)?;
        Ok(CheckpointRow {
            key: checkpoints::key(&row)?,
            checkpoint: row,
            registry: self
                .directory
                .join("dashboard-checkpoints.json")
                .to_string_lossy()
                .into_owned(),
        })
    }
    fn capture_terminal_context(
        &self,
        adapter: &Terminals,
        pane: &Value,
        before: &Value,
        name: &str,
        metadata: &mut MetadataGuard<'_>,
    ) -> Result<Checkpoint> {
        let cwd = first(before, &["foreground_cwd", "cwd"]);
        if !Path::new(&cwd).is_dir() {
            return Err("Cannot determine an existing project directory".into());
        }
        let mut checkpoint = Checkpoint {
            name: name.trim().into(),
            harness: "unknown".into(),
            sid: String::new(),
            cwd,
            profile: String::new(),
            origin: "Terminal context only".into(),
            created_at: utc_now(),
            context_file: String::new(),
            source: Some(terminal_ref(before)?),
        };
        let content = adapter.capture(pane, 120)?;
        let after = self.verify_pane(adapter, pane, metadata)?;
        if !terminal_ref(before)?.same_identity(&terminal_ref(&after)?)
            || before.get("agent") != after.get("agent")
            || before.get("agent_session") != after.get("agent_session")
            || first(&after, &["foreground_cwd", "cwd"]) != checkpoint.cwd
        {
            return Err("Terminal changed during context capture; select it again".into());
        }
        metadata.check()?;
        self.write_context(&mut checkpoint, &content)?;
        Ok(checkpoint)
    }
    fn write_context(&self, checkpoint: &mut Checkpoint, content: &str) -> Result<()> {
        let captured = format!(
            "# Session handoff\n\nProject: {}\nSource harness: {}\n\nThis is a captured terminal excerpt, not a full conversation archive.\n\n```text\n{}\n```\n",
            checkpoint.cwd, checkpoint.harness, content
        );
        let path = self
            .directory
            .join("checkpoints")
            .join(format!("{:x}.md", Sha256::digest(captured.as_bytes())));
        layouts::write_private(&path, captured.as_bytes())?;
        checkpoint.context_file = path.to_string_lossy().into_owned();
        checkpoint.validate()
    }
    pub fn capture_handoff(
        &self,
        adapter: &Terminals,
        pane: &Value,
        checkpoint: Checkpoint,
    ) -> Result<Checkpoint> {
        let mut metadata = metadata_guard(adapter, &self.checkpoint_settings()?)?;
        self.capture_handoff_guarded(adapter, pane, checkpoint, &mut metadata)
    }
    fn capture_handoff_guarded(
        &self,
        adapter: &Terminals,
        pane: &Value,
        mut checkpoint: Checkpoint,
        metadata: &mut MetadataGuard<'_>,
    ) -> Result<Checkpoint> {
        let before = self.verify_pane(adapter, pane, metadata)?;
        let actual_reference = terminal_ref(&before)?;
        if checkpoint
            .source
            .as_ref()
            .is_some_and(|source| !actual_reference.same_identity(source))
        {
            return Err("Handoff source changed; select it again".into());
        }
        // The pane can survive replacement of its foreground harness while the
        // confirmation dialog is open. Bind capture to the frozen conversation,
        // not just to the outer terminal's process generation.
        let harness = harness_name(&before);
        if harness != checkpoint.harness
            || before.get("agent_session") != pane.get("agent_session")
            || harness_name(pane) != harness
        {
            return Err("Handoff harness changed; select it again".into());
        }
        let process = foreground_guarded(adapter, &before, &harness, metadata)?;
        let fingerprint = capture_fingerprint(adapter, &before, &harness, &process, metadata)?;
        if let Some(frozen) = pane.get("_daedalus_handoff_process") {
            if *frozen != fingerprint {
                return Err("Handoff foreground process changed; select it again".into());
            }
        } else if checkpoint.sid.is_empty() {
            return Err(
                "Handoff requires frozen foreground process evidence; select it again".into(),
            );
        }
        if checkpoint.sid.is_empty() && fingerprint.is_null() {
            return Err(
                "Handoff requires a native session ID or a verifiable foreground process".into(),
            );
        }
        let reported = terminals::reported_session(&before)
            .unwrap_or_default()
            .to_owned();
        let pi_parent = if harness == "pi" {
            process
                .as_ref()
                .map(|evidence| {
                    pi_sessions::fork_parent(&evidence.argv, Path::new(&evidence.cwd), metadata)
                })
                .transpose()?
                .flatten()
        } else {
            None
        };
        let current_sid = if let Some(process) = &process {
            checkpoints::resolve_sid_with_pi_parent(
                harnesses::conversation_argv(&process.argv, &harness).unwrap_or(&process.argv),
                &harness,
                &reported,
                pi_parent.as_ref().map(|parent| parent.id.as_str()),
            )?
            .unwrap_or_default()
        } else {
            reported
        };
        if (!checkpoint.sid.is_empty() && !current_sid.is_empty() && checkpoint.sid != current_sid)
            || process.as_ref().is_some_and(|p| {
                p.cwd != checkpoint.cwd
                    || (!checkpoint.profile.is_empty() && p.profile != checkpoint.profile)
            })
            || (process.is_none() && first(&before, &["foreground_cwd", "cwd"]) != checkpoint.cwd)
        {
            return Err("Handoff conversation or project changed; select it again".into());
        }
        let content = adapter.capture(pane, 120)?;
        let after = self.verify_pane(adapter, pane, metadata)?;
        check_process(adapter, &process, &harness, metadata)?;
        if let Some(parent) = &pi_parent {
            parent.verify(metadata)?;
        }
        if fingerprint != capture_fingerprint(adapter, &after, &harness, &process, metadata)? {
            return Err("Handoff foreground process changed during context capture".into());
        }
        if before.get("agent") != after.get("agent")
            || before.get("agent_session") != after.get("agent_session")
        {
            return Err("Harness changed during context capture".into());
        }
        self.write_context(&mut checkpoint, &content)?;
        Ok(checkpoint)
    }
    /// Evidence must come from a successful bounded native help query. This
    /// preparation does not execute the harness or create a terminal.
    pub fn prepare(
        &self,
        key: &str,
        request: LaunchRequest<'_>,
        config: &ControlConfig,
        environment: &BTreeMap<String, String>,
    ) -> Result<PreparedLaunch> {
        let saved = self.get_with_settings(key, &config.runtime)?;
        self.prepare_record(
            saved.checkpoint,
            Some(saved.key),
            request,
            config,
            environment,
        )
    }
    pub fn prepare_record(
        &self,
        checkpoint: Checkpoint,
        saved_key: Option<String>,
        request: LaunchRequest<'_>,
        config: &ControlConfig,
        environment: &BTreeMap<String, String>,
    ) -> Result<PreparedLaunch> {
        let LaunchRequest {
            operation,
            target,
            parent,
            placement,
            license,
        } = request;
        let destination = terminals::identity(parent)?;
        let mut launch = checkpoints::plan(
            &checkpoint,
            operation,
            target,
            &self.directory.join("checkpoints"),
        )?;
        let mut metadata =
            crate::core::native_metadata::Guard::new(config.runtime.native_metadata_policy())?;
        let pi_session = if checkpoint.harness == "pi" && operation != Operation::Handoff {
            let session = pi_sessions::saved(&self.directory, &checkpoint.sid, &mut metadata)?;
            if session.cwd != launch.cwd {
                return Err("Pi checkpoint project differs from saved native session".into());
            }
            launch.args[1] = session.path.to_string_lossy().into_owned();
            Some(session)
        } else {
            None
        };
        let tool = harnesses::tool(&target.harness).ok_or("Unknown harness adapter")?;
        let mut profiles = config.profiles.clone();
        if operation != Operation::Handoff && !checkpoint.profile.is_empty() {
            profiles.insert(target.harness.clone(), checkpoint.profile.clone());
        }
        if let Some(choice) = license {
            if operation != Operation::NativeFork
                || target.harness != "codex"
                || checkpoint.harness != "codex"
            {
                return Err("License selection is supported for native Codex forks".into());
            }
            if checkpoint.profile.is_empty() {
                return Err(
                    "Source fork profile is unknown; select an exact native source first".into(),
                );
            }
            choice.verify(&config.runtime)?;
            profiles.insert(
                "codex".into(),
                choice
                    .home
                    .to_str()
                    .ok_or("License profile must be UTF-8")?
                    .into(),
            );
        }
        let native =
            harnesses::launch_plan(&tool, &launch.binary, &launch.cwd, environment, &profiles)?;
        launch.environment = native.environment;
        if let Some(choice) = license {
            // An explicitly selected default Codex profile still needs a fixed
            // import destination; ordinary inherited launches may omit this.
            launch.environment.insert(
                "CODEX_HOME".into(),
                choice
                    .home
                    .to_str()
                    .ok_or("License profile must be UTF-8")?
                    .into(),
            );
        }
        Ok(PreparedLaunch {
            checkpoint,
            saved_key,
            pi_session,
            launch,
            parent: parent.clone(),
            destination,
            placement,
            license: license.cloned(),
            settings: config.runtime.clone(),
        })
    }
    pub fn apply(&self, adapter: &Terminals, mut prepared: PreparedLaunch) -> Result<Receipt> {
        if let Some(key) = &prepared.saved_key {
            let current = self.get(key)?;
            if key != &checkpoints::key(&prepared.checkpoint)?
                || checkpoints::key(&current.checkpoint)? != *key
            {
                return Err("Checkpoint changed before launch".into());
            }
        }
        if terminals::identity(&prepared.parent)? != prepared.destination {
            return Err("Launch destination changed".into());
        }
        let mut metadata =
            crate::core::native_metadata::Guard::new(prepared.settings.native_metadata_policy())?;
        if let Some(session) = &prepared.pi_session {
            session.verify(&mut metadata)?;
        }
        if let Some(choice) = &prepared.license {
            choice.verify(&prepared.settings)?;
            // Verify the destination before importing native history. Spawn
            // verifies it again after the bounded history preparation.
            adapter.verify(&prepared.parent)?;
            if Path::new(&prepared.checkpoint.profile) != choice.home {
                let child = super::codex_fork::create(
                    Path::new(&prepared.checkpoint.profile),
                    &prepared.checkpoint.sid,
                    &prepared.launch,
                    &prepared.settings,
                )?;
                prepared.launch.args = vec!["resume".into(), child];
            }
        }
        let mut receipt = adapter.spawn(&prepared.parent, &prepared.launch, prepared.placement)?;
        if let Some(choice) = &prepared.license {
            receipt.message.push_str(&format!(
                "\nLicense: {} ({})",
                choice.name,
                choice.home.display()
            ));
        }
        Ok(receipt)
    }
}
/// Kept in memory between the menu preview and explicit confirmation. The
/// destination and checkpoint are checked again by apply; never serialize env.
pub struct PreparedLaunch {
    pub checkpoint: Checkpoint,
    pub saved_key: Option<String>,
    pi_session: Option<pi_sessions::Session>,
    pub launch: LaunchPlan,
    pub parent: Value,
    pub destination: TerminalId,
    pub placement: Placement,
    license: Option<super::fork_licenses::Choice>,
    settings: super::runtime_settings::Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachMode {
    Observe,
    Control,
}
impl AttachMode {
    fn name(self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::Control => "control",
        }
    }
}
#[derive(Default)]
pub struct AttachContext {
    pub own_terminals: Vec<TerminalId>,
    pub holders: Vec<Value>,
    pub node: String,
    pub hostname: String,
}
/// Select self/nested dashboard views from supplied runtime evidence. Native
/// process ancestry and Herdr-client tty observations are collected separately.
pub fn own_terminals(
    snapshot: &Value,
    environment: &BTreeMap<String, String>,
    ancestors: &BTreeSet<String>,
    herdr_clients: &[(String, String)],
) -> Vec<TerminalId> {
    let panes = snapshot
        .get("panes")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let socket = environment
        .get("TMUX")
        .and_then(|s| terminals::active_tmux_socket(s))
        .unwrap_or_default();
    let tmux_pane = environment
        .get("TMUX_PANE")
        .map(String::as_str)
        .unwrap_or_default();
    let herdr_session = environment
        .get("HERDR_SESSION")
        .map(String::as_str)
        .unwrap_or_default();
    let herdr_pane = environment
        .get("HERDR_PANE_ID")
        .map(String::as_str)
        .unwrap_or_default();
    let mut found = Vec::new();
    let mut sessions = BTreeSet::new();
    for pane in panes {
        let Ok(id) = terminals::identity(pane) else {
            continue;
        };
        let direct = match id.backend {
            Backend::Native => {
                environment
                    .get("DAEDALUS_TERMINAL_ID")
                    .or_else(|| environment.get("DAEDALUS_NATIVE_SESSION"))
                    .is_some_and(|session| session == &id.endpoint)
                    && environment.get("DAEDALUS_TERMINAL_GENERATION") == Some(&id.generation)
            }
            Backend::Tmux => id.endpoint == socket && id.pane == tmux_pane,
            Backend::Herdr => {
                !herdr_pane.is_empty()
                    && id.pane == herdr_pane
                    && (herdr_session.is_empty() || id.endpoint == herdr_session)
            }
        };
        let nested_herdr = !herdr_session.is_empty()
            && (field(pane, "herdr_session") == herdr_session
                || herdr_clients
                    .iter()
                    .any(|(session, tty)| session == herdr_session && field(pane, "tty") == *tty));
        if direct || nested_herdr || ancestors.contains(&field(pane, "pid")) {
            if id.backend == Backend::Tmux {
                sessions.insert((id.endpoint.clone(), field(pane, "session_id")));
            }
            if !found.contains(&id) {
                found.push(id);
            }
        }
    }
    // A client displaying a dashboard session can itself be nested repeatedly.
    loop {
        let old = found.len();
        for client in snapshot
            .get("clients")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if !sessions.contains(&(field(client, "socket"), field(client, "session_id"))) {
                continue;
            }
            for outer in client
                .get("outer_panes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                for pane in panes {
                    if pane["backend"] == "tmux"
                        && pane.get("socket") == outer.get("socket")
                        && pane.get("pane_id") == outer.get("pane_id")
                        && let Ok(id) = terminals::identity(pane)
                    {
                        sessions.insert((id.endpoint.clone(), field(pane, "session_id")));
                        if !found.contains(&id) {
                            found.push(id);
                        }
                    }
                }
            }
        }
        if found.len() == old {
            break;
        }
    }
    found
}
fn native_binary(path: &Path, environment: &BTreeMap<String, String>) -> Result<PathBuf> {
    let candidates: Vec<_> = if path.is_absolute() {
        vec![path.to_path_buf()]
    } else {
        std::env::split_paths(
            environment
                .get("PATH")
                .map(String::as_str)
                .unwrap_or_default(),
        )
        .filter(|p| p.is_absolute())
        .flat_map(|p| harnesses::executable_candidates(&p, &path.to_string_lossy()))
        .collect()
    };
    candidates
        .into_iter()
        .find(|p| fs::metadata(p).is_ok_and(|m| m.is_file() && native_executable(&m)))
        .ok_or_else(|| "Native terminal client is not installed".into())
}
fn view_facts(pane: &Value, snapshot: &Value) -> (usize, String) {
    let identity = terminals::identity(pane).ok();
    let captured_rows = snapshot
        .get("panes")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let captured = crate::adapters::pane_observations::index(captured_rows);
    let same: Vec<_> = captured
        .panes
        .iter()
        .map(|pane| pane.as_ref())
        .filter(|candidate| {
            identity
                .as_ref()
                .is_some_and(|id| terminals::identity(candidate).ok().as_ref() == Some(id))
        })
        .collect();
    let rows = snapshot
        .get("daedalus_views")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let viewers = super::view_records::resolve(rows, || true)
        .records
        .into_iter()
        .filter(|view| identity.as_ref() == Some(&view.source))
        .count();
    if same
        .iter()
        .any(|pane| crate::adapters::pane_observations::conflicted(pane))
    {
        return (
            same.len() + viewers,
            crate::adapters::pane_observations::WARNING.into(),
        );
    }
    if pane["backend"] == "native" {
        return (same.len() + viewers, "One controller at a time; additional Daedalus windows can watch without taking input control.".into());
    }
    if pane["backend"] == "herdr" {
        return (same.len()+viewers,"Herdr has one attach slot per terminal; observers are unlimited. Herdr's own window and API input still reach it. Daedalus asks before taking over.".into());
    }
    let mut typing = 0;
    let mut watching = 0;
    let mut control = 0;
    for client in snapshot
        .get("clients")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if client["backend"] != "tmux"
            || client.get("socket") != pane.get("socket")
            || !same
                .iter()
                .any(|p| p.get("session_id") == client.get("session_id"))
        {
            continue;
        }
        let flags = field(client, "flags");
        let flags: Vec<_> = flags.split(',').collect();
        if flags.contains(&"control-mode") {
            control += 1;
        } else if client.get("pane_id") != pane.get("pane_id") || flags.contains(&"read-only") {
            watching += 1;
        } else {
            typing += 1;
        }
    }
    let mut parts = Vec::new();
    if typing > 0 {
        parts.push(format!("{typing} attached tmux client(s) type here now"));
    }
    if watching > 0 {
        parts.push(format!("{watching} client(s) only watch"));
    }
    if control > 0 {
        parts.push(format!("{control} control-mode client(s)"));
    }
    (
        same.len() + viewers,
        if parts.is_empty() {
            "No other attached client is typing into this pane".into()
        } else {
            format!(
                "{}. tmux input is shared, never exclusive.",
                parts.join("; ")
            )
        },
    )
}
/// Wire-compatible with the unchanged frontend's embed::Plan. No child starts
/// here; the frontend owns the attach process after the user accepts this plan.
pub fn prepare_attach(
    adapter: &Terminals,
    pane: &Value,
    mode: AttachMode,
    snapshot: &Value,
    context: &AttachContext,
) -> Result<Value> {
    let expected = terminals::identity(pane)?;
    if context.own_terminals.contains(&expected) {
        return Err("This terminal is showing Daedalus itself. Attaching would mirror the dashboard into itself.".into());
    }
    let actual = adapter.verify(pane)?;
    let id = terminals::identity(&actual)?;
    let (views, input) = view_facts(pane, snapshot);
    let title = [
        tree::title(pane, &context.hostname),
        field(pane, "session"),
        first(pane, &["window", "tab_id"]),
        field(pane, "pane_id"),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    let mut plan = json!({"mode":mode.name(),"title":title,"backend":id.backend,"server":first(pane,&["socket","server"]),"terminal":id.pane,"views":views,"input":input,"node":context.node});
    if id.backend == Backend::Native {
        let binary = std::env::current_exe().map_err(|_| "Cannot locate Daedalus executable")?;
        let argv = |mode: &str| {
            vec![
                binary.to_string_lossy().into_owned(),
                "terminal-native".into(),
                "stream".into(),
                id.endpoint.clone(),
                id.generation.clone(),
                "--mode".into(),
                mode.into(),
            ]
        };
        plan["backend"] = "native".into();
        plan["server"] = id.endpoint.clone().into();
        plan["adapter"] = "native-session".into();
        plan["argv"] = json!(argv(mode.name()));
        plan["observe_argv"] = json!(argv("observe"));
        plan["control_argv"] = json!(argv("control"));
        plan["takeover"] = false.into();
        plan["pid"] = field(&actual, "pid").into();
        plan["input"] = "One controller at a time; additional views can watch".into();
        return Ok(plan);
    }
    if id.backend == Backend::Tmux {
        let session = field(&actual, "session_id");
        if session.is_empty() {
            return Err("tmux session identity is missing".into());
        }
        let base = vec![
            adapter.tmux.to_string_lossy().into_owned(),
            "-S".into(),
            id.endpoint,
            "-C".into(),
            "attach-session".into(),
            "-t".into(),
            session,
            "-f".into(),
        ];
        let mut observe = base.clone();
        observe.push("ignore-size,read-only".into());
        let mut control = base;
        control.push("ignore-size".into());
        plan["adapter"] = "tmux-control".into();
        plan["argv"] = json!(if mode == AttachMode::Observe {
            &observe
        } else {
            &control
        });
        plan["observe_argv"] = json!(observe);
        plan["control_argv"] = json!(control);
        plan["pid"] = field(&actual, "pid").into();
        plan["takeover"] = false.into();
        plan["native"] = json!([]);
    } else {
        if id.generation.len() > 80
            || !id
                .generation
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_:-".contains(&b))
        {
            return Err("Herdr did not report a usable terminal ID".into());
        }
        let base = vec![
            adapter.herdr.to_string_lossy().into_owned(),
            "--session".into(),
            id.endpoint,
            "terminal".into(),
            "session".into(),
        ];
        let argv = |name: &str| {
            let mut args = base.clone();
            args.extend([name.into(), id.generation.clone()]);
            args
        };
        let control = argv("control");
        let mut takeover = control.clone();
        takeover.push("--takeover".into());
        plan["adapter"] = "herdr-session".into();
        plan["argv"] = json!(argv(mode.name()));
        plan["observe_argv"] = json!(argv("observe"));
        plan["control_argv"] = json!(control);
        plan["takeover_argv"] = json!(takeover);
        plan["pid"] = "".into();
        plan["takeover"] = true.into();
        plan["holders"] = json!(context.holders);
    }
    Ok(plan)
}
fn runtime_environment(environment: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    const RUNTIME: &[&str] = &[
        "SYSTEMROOT",
        "WINDIR",
        "COMSPEC",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "PATHEXT",
        "PSMODULEPATH",
        "HOME",
        "PATH",
        "TERM",
        "COLORTERM",
        "LANG",
        "LANGUAGE",
        "TZ",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XDG_CONFIG_HOME",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "SSH_AUTH_SOCK",
        "USER",
        "LOGNAME",
        "TMPDIR",
        "TMP",
        "TEMP",
        "TERM_PROGRAM",
    ];
    environment
        .iter()
        .filter(|(k, _)| RUNTIME.contains(&k.as_str()) || k.starts_with("LC_"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}
pub struct PreparedView {
    pub source: Value,
    pub destination: Value,
    pub launch: LaunchPlan,
    pub mechanism: String,
}
pub fn prepare_duplicate_view(
    adapter: &Terminals,
    source: &Value,
    destination: &Value,
    read_only: bool,
    environment: &BTreeMap<String, String>,
    context: &AttachContext,
) -> Result<PreparedView> {
    let source_id = terminals::identity(source)?;
    let destination_id = terminals::identity(destination)?;
    if source_id.backend == Backend::Native {
        return Err(
            "Use Watch in another Daedalus window for a second view of this native terminal".into(),
        );
    }
    if source_id == destination_id {
        return Err("Choose a different destination terminal".into());
    }
    if context.own_terminals.contains(&source_id) {
        return Err("This terminal shows Daedalus itself; duplicating it would mirror the dashboard recursively".into());
    }
    let actual = adapter.verify(source)?;
    let target = adapter.verify(destination)?;
    let cwd = first(&target, &["cwd", "foreground_cwd"]);
    if !Path::new(&cwd).is_dir() {
        return Err("Destination project directory is unavailable".into());
    }
    let mut child_environment = runtime_environment(environment);
    let (binary, args, mechanism) = if source_id.backend == Backend::Tmux {
        let name = format!(
            "daedalus-view-{:x}-{:x}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let session = field(&actual, "session_id");
        let window = field(&actual, "window_index");
        if session.is_empty() || window.is_empty() {
            return Err("Source tmux view identity is incomplete".into());
        }
        let mut args = vec![
            "-S".into(),
            source_id.endpoint,
            "new-session".into(),
            "-d".into(),
            "-t".into(),
            session,
            "-s".into(),
            name.clone(),
            ";".into(),
            "select-window".into(),
            "-t".into(),
            format!("{name}:{window}"),
            ";".into(),
            "set-option".into(),
            "-t".into(),
            name.clone(),
            "destroy-unattached".into(),
            "on".into(),
            ";".into(),
            "attach-session".into(),
        ];
        if read_only {
            args.push("-r".into());
        }
        args.extend(["-t".into(), name]);
        (
            native_binary(&adapter.tmux, environment)?,
            args,
            "tmux grouped view",
        )
    } else {
        if cfg!(windows) {
            return Err("Herdr direct terminal duplicate views are unavailable on Windows; use Watch or attach a full Herdr session".into());
        }
        if read_only {
            return Err("Herdr read-only observation is available through Watch; its native duplicate-view client does not advertise a read-only option".into());
        }
        if let Some(config) = environment.get("HERDR_CONFIG_PATH") {
            child_environment.insert("HERDR_CONFIG_PATH".into(), config.clone());
        }
        (
            native_binary(&adapter.herdr, environment)?,
            vec![
                "--session".into(),
                source_id.endpoint,
                "terminal".into(),
                "attach".into(),
                source_id.generation,
            ],
            "Herdr native client",
        )
    };
    Ok(PreparedView {
        source: actual,
        destination: target,
        launch: LaunchPlan {
            binary,
            args,
            environment: child_environment,
            cwd: cwd.into(),
            operation: Operation::Handoff,
        },
        mechanism: mechanism.into(),
    })
}
pub fn apply_duplicate_view(adapter: &Terminals, prepared: PreparedView) -> Result<Receipt> {
    adapter.verify(&prepared.source)?;
    adapter.verify(&prepared.destination)?;
    adapter.spawn(&prepared.destination, &prepared.launch, Placement::Split)
}

/// Show an existing tmux window in another session of the same server. The
/// processes and native conversation remain shared, unlike a session fork.
pub fn link_window(adapter: &Terminals, source: &Value, destination: &Value) -> Result<Receipt> {
    use crate::adapters::terminal_actions::ReceiptState;
    let source = adapter.verify(source)?;
    let destination = adapter.verify(destination)?;
    let id = terminals::identity(&source)?;
    let target = terminals::identity(&destination)?;
    if id.backend != Backend::Tmux
        || target.backend != Backend::Tmux
        || id.endpoint != target.endpoint
    {
        return Err("tmux links windows only within one tmux server".into());
    }
    let session = field(&destination, "session_id");
    let window = field(&source, "window_id");
    if session.is_empty() || window.is_empty() || session == field(&source, "session_id") {
        return Err("Choose another tmux session with complete identity".into());
    }
    let outcome = adapter.query(
        &adapter.tmux,
        &[
            "-S",
            &id.endpoint,
            "link-window",
            "-d",
            "-s",
            &window,
            "-t",
            &format!("{session}:"),
        ],
    );
    if outcome.is_err() {
        return Ok(Receipt {
            state: ReceiptState::Unconfirmed,
            message: "Window link is unconfirmed; inspect the destination before retrying".into(),
            terminal: None,
        });
    }
    let verified = adapter
        .query(
            &adapter.tmux,
            &[
                "-S",
                &id.endpoint,
                "list-windows",
                "-t",
                &session,
                "-F",
                "#{window_id}",
            ],
        )
        .is_ok_and(|rows| rows.lines().any(|line| line == window));
    Ok(Receipt{state:if verified{ReceiptState::Accepted}else{ReceiptState::Unconfirmed},message:if verified{"Window linked. Both sessions show the same window and processes; Close link removes only this session's link."}else{"Window link was requested but could not be confirmed"}.into(),terminal:None})
}
pub fn unlink_window(adapter: &Terminals, pane: &Value) -> Result<Receipt> {
    use crate::adapters::terminal_actions::ReceiptState;
    let pane = adapter.verify(pane)?;
    let id = terminals::identity(&pane)?;
    if id.backend != Backend::Tmux || pane["window_linked"] != "1" {
        return Err("This window is not linked into another tmux session".into());
    }
    let session = field(&pane, "session_id");
    let index = field(&pane, "window_index");
    if session.is_empty() || index.is_empty() {
        return Err("tmux window identity is incomplete".into());
    }
    let accepted = adapter
        .query(
            &adapter.tmux,
            &[
                "-S",
                &id.endpoint,
                "unlink-window",
                "-t",
                &format!("{session}:{index}"),
            ],
        )
        .is_ok();
    Ok(Receipt {
        state: if accepted {
            ReceiptState::Accepted
        } else {
            ReceiptState::Unconfirmed
        },
        message: if accepted {
            "Link removed from this session; the shared window keeps running"
        } else {
            "Unlink outcome is unconfirmed; inspect the session before retrying"
        }
        .into(),
        terminal: None,
    })
}

/// Supported Windows APIs prove the native executable and its incarnation.
/// Never pretend command-line/profile inspection was available on Windows.
#[cfg(windows)]
fn windows_native_process(
    adapter: &Terminals,
    pane: &Value,
    harness: &str,
    metadata: &mut MetadataGuard<'_>,
) -> Result<Value> {
    metadata.check()?;
    let id = terminals::identity(pane)?;
    let info;
    let direct;
    let rows: &[Value] = match id.backend {
        Backend::Herdr => {
            info = adapter
                .herdr_result(&id.endpoint, &["pane", "process-info", "--pane", &id.pane])?;
            metadata.check()?;
            if info["process_info"]["pane_id"].as_str() != Some(id.pane.as_str()) {
                return Err("Foreground pane changed".into());
            }
            info["process_info"]["foreground_processes"]
                .as_array()
                .ok_or("Missing foreground process information")?
        }
        // ConPTY does not establish shell-child foreground ownership. Only the
        // exact directly launched PTY child can supply process evidence here.
        Backend::Native => {
            direct = [json!({"pid":pane["pid"]})];
            &direct
        }
        Backend::Tmux => return Ok(Value::Null),
    };
    let mut candidates = Vec::new();
    metadata.entries(rows.len())?;
    metadata.processes(rows.len())?;
    for row in rows {
        metadata.check()?;
        let pid = row["pid"]
            .as_u64()
            .and_then(|p| u32::try_from(p).ok())
            .or_else(|| row["pid"].as_str().and_then(|p| p.parse().ok()))
            .ok_or("Invalid foreground PID")?;
        let actual =
            crate::platform::process_metadata(pid).map_err(|_| "Foreground process changed")?;
        if actual.owner != crate::platform::current_identity().map_err(|e| e.to_string())? {
            return Err("Foreground process belongs to another account".into());
        }
        if actual.executable.to_str().and_then(harnesses::canonical_id) == Some(harness) {
            candidates.push(actual);
        }
    }
    metadata.check()?;
    if candidates.len() > 1 {
        return Err("Foreground native executable is ambiguous".into());
    }
    let Some(actual) = candidates.pop() else {
        return Ok(Value::Null);
    };
    Ok(
        json!({"pid":actual.pid.to_string(),"start":actual.creation_time.to_string(),"executable":actual.executable,"arguments_observed":false,"argv_sha256":null,"cwd":first(pane,&["foreground_cwd","cwd"]),"profile":""}),
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        dir: PathBuf,
        store: SessionStore,
        adapter: Terminals,
        config: ControlConfig,
        environment: BTreeMap<String, String>,
        pane: Value,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
    fn publish_script(path: &Path, bytes: impl AsRef<[u8]>) {
        use std::io::Write;
        let temporary =
            path.with_extension(format!("stage-{}", SERIAL.fetch_add(1, Ordering::Relaxed)));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&temporary)
            .unwrap();
        file.write_all(bytes.as_ref()).unwrap();
        file.sync_all().unwrap();
        drop(file);
        fs::rename(temporary, path).unwrap();
    }
    fn fixture() -> Fixture {
        let dir = std::env::temp_dir().join(format!(
            "daedalus-session-app-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        fs::create_dir(dir.join("profile")).unwrap();
        fs::create_dir(dir.join("proc")).unwrap();
        let pane = json!({"backend":"herdr","server":"work","session":"work","workspace_id":"w1","tab_id":"t1","pane_id":"pane","terminal_id":"uuid","agent":"codex","cwd":dir,"agent_session":{"agent":"codex","kind":"id","value":"native-session"}});
        fs::write(
            dir.join("pane.json"),
            serde_json::to_vec(&json!({"pane":pane})).unwrap(),
        )
        .unwrap();
        let script = dir.join("herdr");
        let command = format!(
            "#!/bin/sh\ncase \"$4\" in\nget) /bin/cat '{}' ;;\nprocess-info) printf '%s\\n' '{{\"process_info\":{{\"foreground_processes\":[]}}}}' ;;\nread) printf 'private fixture transcript\\n' ;;\n*) exit 2 ;;\nesac\n",
            dir.join("pane.json").display()
        );
        publish_script(&script, command);
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let mut config = ControlConfig::fresh(&dir).unwrap();
        config.profiles.insert(
            "codex".into(),
            dir.join("profile").to_string_lossy().into_owned(),
        );
        let environment = BTreeMap::from([
            ("HOME".into(), dir.to_string_lossy().into_owned()),
            (
                "PATH".into(),
                crate::test_support::path("/usr/bin:/bin").into(),
            ),
            ("TERM".into(), "xterm-256color".into()),
            ("TELEGRAM_BOT_TOKEN".into(), "fixture-private-token".into()),
            ("GROK_HOME".into(), "/unrelated/sibling".into()),
        ]);
        Fixture {
            store: SessionStore {
                directory: dir.join("state"),
            },
            adapter: Terminals {
                herdr: script,
                proc_root: dir.join("proc"),
                ..Terminals::default()
            },
            dir,
            config,
            environment,
            pane,
        }
    }
    fn save(f: &Fixture, output: bool) -> CheckpointRow {
        f.store
            .save(
                &f.adapter,
                &f.config,
                &f.environment,
                &f.pane,
                &SaveRequest {
                    name: "Before tests".into(),
                    sid: String::new(),
                    include_output: output,
                },
            )
            .unwrap()
    }
    #[test]
    fn checkpoint_save_refuses_ambiguity_and_capacity_before_capturing_terminal_output() {
        let mut f = fixture();
        save(&f, false);
        let path = f.store.directory.join("dashboard-checkpoints.json");
        let raw = fs::read(&path).unwrap();
        let ambiguous = String::from_utf8(raw.clone()).unwrap().replacen(
            "\"profile\":",
            "\"profile\":\"/synthetic/ambiguous\",\"profile\":",
            1,
        );
        fs::write(&path, ambiguous.as_bytes()).unwrap();
        let adapter = f.adapter.herdr.clone();
        f.adapter.herdr = f.dir.join("not-an-adapter");
        let request = SaveRequest {
            name: "Second checkpoint".into(),
            sid: String::new(),
            include_output: true,
        };
        assert!(
            f.store
                .save(&f.adapter, &f.config, &f.environment, &f.pane, &request)
                .unwrap_err()
                .contains("ambiguous")
        );
        assert!(!f.store.directory.join("checkpoints").exists());
        assert_eq!(fs::read(&path).unwrap(), ambiguous.as_bytes());
        fs::write(&path, &raw).unwrap();
        f.config.runtime.checkpoint_entry_limit = 1;
        assert!(
            f.store
                .save(&f.adapter, &f.config, &f.environment, &f.pane, &request)
                .unwrap_err()
                .contains("entry limit")
        );
        assert_eq!(fs::read(&path).unwrap(), raw);
        f.adapter.herdr = adapter;
        f.config.runtime.checkpoint_entry_limit = 3;
        let request = SaveRequest {
            include_output: false,
            ..request
        };
        f.store
            .save(&f.adapter, &f.config, &f.environment, &f.pane, &request)
            .unwrap();
        assert_eq!(
            f.store.list_with_settings(&f.config.runtime).unwrap().len(),
            2
        );
    }
    fn capabilities(f: &Fixture) -> Capabilities {
        Capabilities {
            harness: "codex".into(),
            binary: f.dir.join("fake-codex"),
            resume: true,
            native_fork: true,
            handoff: true,
        }
    }
    #[test]
    fn unknown_terminal_context_is_saved_without_inventing_native_identity() {
        for agent in ["", "bash", "node", "custom-runner"] {
            let mut f = fixture();
            f.pane["agent"] = json!(agent);
            // Even stray provider-shaped metadata on a generic terminal is not
            // evidence of a native provider conversation.
            fs::write(
                f.dir.join("pane.json"),
                serde_json::to_vec(&json!({"pane":f.pane})).unwrap(),
            )
            .unwrap();
            let saved = save(&f, true);
            assert_eq!(saved.checkpoint.harness, "unknown");
            assert!(saved.checkpoint.sid.is_empty());
            assert!(saved.checkpoint.profile.is_empty());
            assert_eq!(saved.checkpoint.origin, "Terminal context only");
            assert!(
                fs::read_to_string(&saved.checkpoint.context_file)
                    .unwrap()
                    .contains("private fixture transcript")
            );
            let caps = capabilities(&f);
            for operation in [Operation::Resume, Operation::NativeFork] {
                assert!(
                    checkpoints::plan(&saved.checkpoint, operation, &caps, &f.store.directory)
                        .is_err()
                );
            }
            assert!(
                checkpoints::plan(
                    &saved.checkpoint,
                    Operation::Handoff,
                    &caps,
                    &f.store.directory
                )
                .is_ok()
            );
            assert!(
                f.store
                    .save(
                        &f.adapter,
                        &f.config,
                        &f.environment,
                        &f.pane,
                        &SaveRequest {
                            name: "False reference".into(),
                            sid: "pretend-id".into(),
                            include_output: true
                        }
                    )
                    .is_err()
            );
        }
    }
    #[test]
    fn context_capture_rejects_terminal_project_changed_during_capture() {
        let mut f = fixture();
        f.pane["agent"] = json!("bash");
        let before = f.pane.clone();
        fs::write(
            f.dir.join("pane.json"),
            serde_json::to_vec(&json!({"pane":f.pane})).unwrap(),
        )
        .unwrap();
        let mut changed = f.pane.clone();
        changed["cwd"] = json!(f.dir.join("profile"));
        fs::write(
            f.dir.join("changed.json"),
            serde_json::to_vec(&json!({"pane":changed})).unwrap(),
        )
        .unwrap();
        let script = fs::read_to_string(&f.adapter.herdr).unwrap().replace(
            "read) printf",
            &format!(
                "read) /bin/cp '{}' '{}'; printf",
                f.dir.join("changed.json").display(),
                f.dir.join("pane.json").display()
            ),
        );
        publish_script(&f.adapter.herdr, script);
        assert!(
            f.store
                .capture_terminal_context(
                    &f.adapter,
                    &f.pane,
                    &before,
                    "Context",
                    &mut metadata_guard(&f.adapter, &f.config.runtime).unwrap()
                )
                .unwrap_err()
                .contains("changed during context capture")
        );
        assert!(f.store.list().unwrap().is_empty());
        assert!(!f.store.directory.join("checkpoints").exists());
    }
    #[test]
    fn context_only_save_does_not_hide_exhausted_metadata_as_a_capture_fallback() {
        let mut f = fixture();
        f.pane["agent"] = json!("bash");
        f.pane["agent_session"] = Value::Null;
        fs::write(
            f.dir.join("pane.json"),
            serde_json::to_vec(&json!({"pane":f.pane})).unwrap(),
        )
        .unwrap();
        let info = json!({"process_info":{"pane_id":"pane","foreground_processes":[{"pid":900},{"pid":901}]}});
        let command = format!(
            "#!/bin/sh\ncase \"$4\" in\nget) /bin/cat '{}' ;;\nprocess-info) printf '%s\\n' '{info}' ;;\nread) exit 99 ;;\n*) exit 2 ;;\nesac\n",
            f.dir.join("pane.json").display()
        );
        publish_script(&f.adapter.herdr, command);

        f.config.runtime.native_metadata_process_limit = 1;
        let request = SaveRequest {
            name: "Context".into(),
            sid: String::new(),
            include_output: true,
        };
        let error = f
            .store
            .save(&f.adapter, &f.config, &f.environment, &f.pane, &request)
            .unwrap_err();
        assert!(error.contains("native_metadata_process_limit"), "{error}");
        assert!(!f.store.directory.join("checkpoints").exists());
        assert!(
            !f.store
                .directory
                .join("dashboard-checkpoints.json")
                .exists()
        );
    }
    #[test]
    fn saves_verified_reference_and_private_context() {
        let f = fixture();
        let saved = save(&f, true);
        assert_eq!(saved.checkpoint.sid, "native-session");
        assert_eq!(saved.checkpoint.origin, "Native metadata");
        assert_eq!(saved.checkpoint.profile, f.config.profiles["codex"]);
        let context = fs::read_to_string(&saved.checkpoint.context_file).unwrap();
        assert!(context.contains("private fixture transcript"));
        assert!(context.contains("not a full conversation archive"));
        assert_eq!(
            fs::metadata(saved.checkpoint.context_file)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(f.store.list().unwrap()[0].key, saved.key);
    }
    #[test]
    fn changed_or_conflicting_source_is_not_saved() {
        let f = fixture();
        let request = SaveRequest {
            name: "Wrong".into(),
            sid: "other-session".into(),
            include_output: false,
        };
        assert!(
            f.store
                .save(&f.adapter, &f.config, &f.environment, &f.pane, &request)
                .is_err()
        );
        let mut stale = f.pane.clone();
        stale["terminal_id"] = "old".into();
        assert!(
            f.store
                .save(&f.adapter, &f.config, &f.environment, &stale, &request)
                .is_err()
        );
        assert!(f.store.list().unwrap().is_empty());
    }
    #[test]
    fn stable_keys_survive_reordering_and_removal_suppresses_legacy() {
        let f = fixture();
        let saved = save(&f, false);
        let mut second = saved.checkpoint.clone();
        second.name = "Second".into();
        let legacy = Registry {
            checkpoints: vec![second.clone(), saved.checkpoint.clone()],
        };
        layouts::write_private(
            &f.store.directory.join("session-checkpoints.json"),
            &serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        let second_key = checkpoints::key(&second).unwrap();
        assert_eq!(f.store.list().unwrap().len(), 2);
        f.store.remove(&saved.key).unwrap();
        let remaining = f.store.list().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].key, second_key);
        assert!(f.store.directory.join("session-checkpoints.json").exists());
        assert!(f.store.remove(&saved.key).is_err());
    }
    #[test]
    fn preparation_uses_complete_sanitized_environment_and_rechecks_removal() {
        let f = fixture();
        let saved = save(&f, false);
        let caps = capabilities(&f);
        let plan = f
            .store
            .prepare(
                &saved.key,
                LaunchRequest {
                    license: None,
                    operation: Operation::NativeFork,
                    target: &caps,
                    parent: &f.pane,
                    placement: Placement::Split,
                },
                &f.config,
                &f.environment,
            )
            .unwrap();
        assert_eq!(plan.launch.args, vec!["fork", "native-session"]);
        assert_eq!(plan.launch.environment["HOME"], f.environment["HOME"]);
        assert_eq!(
            plan.launch.environment["CODEX_HOME"],
            f.config.profiles["codex"]
        );
        assert!(!plan.launch.environment.contains_key("TELEGRAM_BOT_TOKEN"));
        assert!(!plan.launch.environment.contains_key("GROK_HOME"));
        f.store.remove(&saved.key).unwrap();
        assert!(f.store.apply(&f.adapter, plan).is_err());
    }
    #[test]
    fn unsupported_capability_cannot_prepare() {
        let f = fixture();
        let saved = save(&f, false);
        let mut caps = capabilities(&f);
        caps.native_fork = false;
        assert!(
            f.store
                .prepare(
                    &saved.key,
                    LaunchRequest {
                        license: None,
                        operation: Operation::NativeFork,
                        target: &caps,
                        parent: &f.pane,
                        placement: Placement::Split
                    },
                    &f.config,
                    &f.environment
                )
                .is_err()
        );
    }
    #[test]
    fn fork_license_preserves_checkpoint_and_is_revalidated_before_any_native_mutation() {
        let f = fixture();
        let saved = save(&f, false);
        let target = f.dir.join("selected-profile");
        fs::create_dir(&target).unwrap();
        let default_profile = Path::new(&f.environment["HOME"]).join(".codex");
        fs::create_dir_all(&default_profile).unwrap();
        let catalog = f.store.directory.join("codex-licenses.json");
        layouts::write_private(
            &catalog,
            &serde_json::to_vec(&json!({"licenses":{"default":{"home":default_profile},"selected":{"home":target}}})).unwrap(),
        )
        .unwrap();
        let choices = super::super::fork_licenses::list(
            &f.store.directory,
            "codex",
            &BTreeMap::new(),
            &f.config.runtime,
        )
        .unwrap();
        let caps = capabilities(&f);
        for choice in &choices {
            let plan = f
                .store
                .prepare(
                    &saved.key,
                    LaunchRequest {
                        operation: Operation::NativeFork,
                        target: &caps,
                        parent: &f.pane,
                        placement: Placement::Split,
                        license: Some(choice),
                    },
                    &f.config,
                    &f.environment,
                )
                .unwrap();
            assert_eq!(
                plan.launch.environment["CODEX_HOME"],
                choice.home.to_string_lossy()
            );
        }
        let prepared = f
            .store
            .prepare(
                &saved.key,
                LaunchRequest {
                    operation: Operation::NativeFork,
                    target: &caps,
                    parent: &f.pane,
                    placement: Placement::Split,
                    license: Some(
                        choices
                            .iter()
                            .find(|choice| choice.name == "selected")
                            .unwrap(),
                    ),
                },
                &f.config,
                &f.environment,
            )
            .unwrap();
        assert_eq!(
            prepared.launch.environment["CODEX_HOME"],
            target.to_string_lossy()
        );
        assert_eq!(
            serde_json::to_value(&prepared.checkpoint).unwrap(),
            serde_json::to_value(&saved.checkpoint).unwrap()
        );
        assert_eq!(prepared.saved_key.as_deref(), Some(saved.key.as_str()));
        assert_eq!(
            serde_json::to_value(f.store.get(&saved.key).unwrap().checkpoint).unwrap(),
            serde_json::to_value(&saved.checkpoint).unwrap()
        );
        for operation in [Operation::Resume, Operation::Handoff] {
            assert!(
                f.store
                    .prepare(
                        &saved.key,
                        LaunchRequest {
                            operation,
                            target: &caps,
                            parent: &f.pane,
                            placement: Placement::Split,
                            license: Some(
                                choices
                                    .iter()
                                    .find(|choice| choice.name == "selected")
                                    .unwrap()
                            )
                        },
                        &f.config,
                        &f.environment
                    )
                    .is_err()
            );
        }
        layouts::write_private(&catalog, b"{\"licenses\":{}}").unwrap();
        assert!(
            f.store
                .apply(&f.adapter, prepared)
                .err()
                .unwrap()
                .contains("changed")
        );
        assert!(!target.join("sessions").exists());
        assert_eq!(
            serde_json::to_value(f.store.get(&saved.key).unwrap().checkpoint).unwrap(),
            serde_json::to_value(&saved.checkpoint).unwrap()
        );
    }
    #[test]
    fn attach_plan_uses_existing_protocol_and_blocks_self() {
        let f = fixture();
        let snapshot = json!({"panes":[f.pane]});
        let plan = prepare_attach(
            &f.adapter,
            &f.pane,
            AttachMode::Observe,
            &snapshot,
            &AttachContext::default(),
        )
        .unwrap();
        assert_eq!(plan["adapter"], "herdr-session");
        let args: Vec<String> = serde_json::from_value(plan["argv"].clone()).unwrap();
        assert_eq!(&args[args.len() - 2..], &["observe", "uuid"]);
        assert_eq!(
            plan["takeover_argv"].as_array().unwrap().last().unwrap(),
            "--takeover"
        );
        let context = AttachContext {
            own_terminals: vec![terminals::identity(&f.pane).unwrap()],
            ..AttachContext::default()
        };
        assert!(
            prepare_attach(
                &f.adapter,
                &f.pane,
                AttachMode::Control,
                &snapshot,
                &context
            )
            .is_err()
        );
    }
    #[test]
    fn native_self_detection_requires_own_generation_and_has_native_view_details() {
        let pane = json!({"backend":"native","session":"pty-1","server":"local","pane_id":"pty-1","terminal_id":"generation-1"});
        let snapshot = json!({"panes":[pane]});
        let mut environment = BTreeMap::from([
            ("DAEDALUS_TERMINAL_ID".into(), "pty-1".into()),
            ("DAEDALUS_TERMINAL_GENERATION".into(), "generation-1".into()),
        ]);
        assert_eq!(
            own_terminals(&snapshot, &environment, &BTreeSet::new(), &[]).len(),
            1
        );
        environment.insert("DAEDALUS_TERMINAL_GENERATION".into(), "replacement".into());
        assert!(own_terminals(&snapshot, &environment, &BTreeSet::new(), &[]).is_empty());
        let (_, detail) = view_facts(&pane, &snapshot);
        assert!(detail.contains("One controller"));
        assert!(!detail.contains("tmux"));
    }
    #[test]
    fn native_foreground_detects_real_harness_and_rejects_background_process() {
        use std::os::unix::fs::symlink;
        let f = fixture();
        for (pid, group, command) in [(42, 42, "bash"), (90, 90, "claude")] {
            let path = f.adapter.proc_root.join(pid.to_string());
            fs::create_dir(&path).unwrap();
            let mut fields = vec!["0".to_string(); 20];
            fields[0] = "S".into();
            fields[2] = group.to_string();
            fields[4] = "99".into();
            fields[5] = "90".into();
            fields[19] = "500".into();
            fs::write(
                path.join("stat"),
                format!("{pid} ({command}) {}", fields.join(" ")),
            )
            .unwrap();
            fs::write(path.join("cmdline"), format!("/usr/bin/{command}\0")).unwrap();
            fs::write(path.join("environ"), format!("HOME={}\0", f.dir.display())).unwrap();
            symlink(&f.dir, path.join("cwd")).unwrap();
        }
        let pane = json!({"backend":"native","session":"pty-1","pane_id":"pty-1","terminal_id":"generation-1","pid":"42"});
        let process = foreground(&f.adapter, &pane, "claude").unwrap().unwrap();
        assert_eq!(process.pid, "90");
        assert_eq!(process.start, "500");
        let mut limited = f.adapter.clone();
        let settings = super::super::runtime_settings::Settings {
            native_metadata_process_limit: 1,
            ..Default::default()
        };
        limited.apply_settings(&settings).unwrap();
        assert!(
            foreground(&limited, &pane, "claude")
                .is_err_and(|error| error.contains("native_metadata_process_limit")),
            "Native foreground discovery ignored the saved process limit"
        );
        for (policy, key) in [
            (
                crate::core::native_metadata::Policy {
                    snapshot_entries: 1,
                    ..Default::default()
                },
                "native_metadata_process_scan_limit",
            ),
            (
                crate::core::native_metadata::Policy {
                    entries: 1,
                    ..Default::default()
                },
                "native_metadata_entry_limit",
            ),
        ] {
            let mut guard = MetadataGuard::new(policy).unwrap();
            assert!(
                foreground_guarded(&f.adapter, &pane, "claude", &mut guard)
                    .unwrap_err()
                    .contains(key)
            );
            assert!(guard.check().unwrap_err().contains(key));
        }
        let mut guard = MetadataGuard::new(crate::core::native_metadata::Policy {
            processes: 1,
            ..Default::default()
        })
        .unwrap();
        let evidence = read_process(&f.adapter, "90", "claude", &mut guard).unwrap();
        assert!(evidence.is_some());
        assert!(
            check_process(&f.adapter, &evidence, "claude", &mut guard)
                .unwrap_err()
                .contains("native_metadata_process_limit")
        );
        // Rejected argv/environment must not become a successful partial match.
        for name in ["cmdline", "environ"] {
            let path = f.adapter.proc_root.join("90").join(name);
            let original = fs::read(&path).unwrap();
            fs::write(&path, vec![b'x'; 1025]).unwrap();
            let mut guard = MetadataGuard::new(crate::core::native_metadata::Policy {
                file_bytes: 1024,
                ..Default::default()
            })
            .unwrap();
            assert!(
                foreground_guarded(&f.adapter, &pane, "claude", &mut guard)
                    .unwrap_err()
                    .contains("native_metadata_file_limit_bytes")
            );
            fs::write(path, original).unwrap();
        }
        let mut guard = MetadataGuard::new(Default::default())
            .unwrap()
            .with_deadline(Some(std::time::Instant::now()));
        assert!(
            foreground_guarded(&f.adapter, &pane, "claude", &mut guard)
                .unwrap_err()
                .contains("native_metadata_timeout_ms")
        );
        let path = f.adapter.proc_root.join("90/stat");
        let stat = fs::read_to_string(&path).unwrap();
        fs::write(path, stat.replacen("90 0 99 90", "90 0 99 42", 1)).unwrap();
        assert!(foreground(&f.adapter, &pane, "claude").unwrap().is_none());
    }
    #[test]
    fn native_duplicate_rejects_before_attempting_a_herdr_client() {
        let f = fixture();
        let source = json!({"backend":"native","session":"pty-1","pane_id":"pty-1","terminal_id":"generation-1"});
        let result = prepare_duplicate_view(
            &f.adapter,
            &source,
            &f.pane,
            true,
            &f.environment,
            &AttachContext::default(),
        );
        assert!(matches!(result, Err(error) if error.contains("Watch in another Daedalus window")));
    }
    #[test]
    fn pi_current_native_metadata_supersedes_the_registered_launch_header() {
        use crate::app::{
            hook_launch::Plan,
            hook_runtime::{Options, ProcessIdentity, Registration},
            hooks::{EventKind, Harness},
        };
        use crate::core::{goal_store::Scope, identity::Backend};
        let mut f = fixture();
        let original = "12345678-abcd-4000-8000-123456789abc";
        let current = "22345678-abcd-4000-8000-123456789abc";
        f.pane["agent"] = json!("pi");
        f.pane["agent_session"] = json!({"kind":"id","agent":"pi","value":current});
        fs::write(f.dir.join("pane.json"), json!({"pane":f.pane}).to_string()).unwrap();
        publish_script(
            &f.adapter.herdr,
            format!(
                "#!/bin/sh\ncase \"$4\" in\nget) /bin/cat '{}' ;;\nprocess-info) printf '%s\\n' '{{\"process_info\":{{\"pane_id\":\"pane\",\"foreground_processes\":[{{\"pid\":90}}]}}}}' ;;\n*) exit 79 ;;\nesac\n",
                f.dir.join("pane.json").display()
            ),
        );
        let registration = Registration {
            version: 1,
            id: "fixture".into(),
            directory: f.store.directory.clone(),
            scope: Scope {
                instance: "example".into(),
                terminal: TerminalId {
                    backend: Backend::Herdr,
                    endpoint: "work".into(),
                    pane: "pane".into(),
                    generation: "uuid".into(),
                },
                conversation: original.into(),
                harness: Harness::Pi,
            },
            goal_id: None,
            harness_process: Some(ProcessIdentity {
                pid: 90,
                start_ticks: 500,
            }),
            server_emitter: None,
            transcript: None,
            events: vec![EventKind::Stop],
            options: Options::default(),
            activity_lease_ms: 30000,
            pending_timeout_ms: 30000,
            hook_timeout_ms: 12000,
        };
        let mut template = registration.clone();
        template.harness_process = None;
        let executable = json!({"device":0,"inode":0,"size":0,"modified":0,"modified_ns":0,"changed":0,"changed_ns":0});
        let plan:Plan=serde_json::from_value(json!({"spec":{"registration":template,"native_binary":"/fixture/node","entrypoint":"/fixture/cli.js","daedalus_binary":"/fixture/daedalus","working_directory":f.dir,"observed_version":"1.1.0","expires_ms":1,"model":null},"native":executable,"daemon":executable,"entrypoint":executable,"hooks":[]})).unwrap();
        layouts::write_private(&plan.path(), &serde_json::to_vec(&plan).unwrap()).unwrap();
        layouts::write_private(&plan.path().with_extension("claimed"), b"claimed\n").unwrap();
        let registration_bytes = serde_json::to_vec(&registration).unwrap();
        layouts::write_private(&registration.path(), &registration_bytes).unwrap();
        layouts::write_private(
            &plan.plugin().join("sessions/owned.jsonl"),
            format!(
                "{}\n",
                json!({"type":"session","version":3,"id":original,"cwd":f.dir})
            )
            .as_bytes(),
        )
        .unwrap();
        let pid = f.adapter.proc_root.join("90");
        fs::create_dir(&pid).unwrap();
        let mut fields = vec!["0"; 20];
        fields[0] = "S";
        fields[2] = "90";
        fields[4] = "99";
        fields[5] = "90";
        fields[19] = "500";
        fs::write(
            pid.join("stat"),
            format!("90 (fixture) {}", fields.join(" ")),
        )
        .unwrap();
        let mut argv = vec!["/fixture/node".into()];
        argv.extend(plan.command());
        fs::write(pid.join("cmdline"), format!("{}\0", argv.join("\0"))).unwrap();
        fs::write(pid.join("environ"), format!("HOME={}\0", f.dir.display())).unwrap();
        std::os::unix::fs::symlink(&f.dir, pid.join("cwd")).unwrap();
        let row = f
            .store
            .live_source(&f.adapter, &f.config, &f.environment, &f.pane, "")
            .unwrap();
        assert_eq!(row.sid, current);
        assert_eq!(
            fs::read(registration.path()).unwrap(),
            registration_bytes,
            "observation must not retarget hook authorization"
        );
    }
    #[test]
    fn inventory_recovers_explicit_foreground_ids_and_drops_expired_evidence() {
        use std::os::unix::fs::symlink;
        const SID: &str = "019a043e-1234-7000-8000-000000000001";
        for (harness, args) in [
            ("codex", vec!["/usr/bin/codex", "resume", SID]),
            (
                "codex",
                vec!["/usr/bin/node", "/tools/codex.js", "resume", SID],
            ),
            ("pi", vec!["/usr/bin/pi", "--session-id", SID]),
            ("cursor", vec!["/usr/bin/cursor-agent", "--resume", SID]),
            ("hermes", vec!["/usr/bin/hermes", "--resume", SID]),
            ("claude", vec!["/usr/bin/claude", "--resume", SID]),
            ("grok", vec!["/usr/bin/grok", "--session-id", SID]),
            (
                "opencode",
                vec!["/usr/bin/opencode", "--session", "ses_fixture"],
            ),
        ] {
            let mut f = fixture();
            f.pane["agent"] = json!(harness);
            f.pane["agent_session"] = Value::Null;
            fs::write(
                f.dir.join("pane.json"),
                serde_json::to_vec(&json!({"pane":f.pane})).unwrap(),
            )
            .unwrap();
            publish_script(
                &f.adapter.herdr,
                format!(
                    "#!/bin/sh\ncase \"$4\" in\nget) /bin/cat '{}' ;;\nprocess-info) printf '%s\\n' '{{\"process_info\":{{\"pane_id\":\"pane\",\"foreground_processes\":[{{\"pid\":90}}]}}}}' ;;\n*) exit 79 ;;\nesac\n",
                    f.dir.join("pane.json").display()
                ),
            );
            let pid = f.adapter.proc_root.join("90");
            fs::create_dir(&pid).unwrap();
            let mut fields = vec!["0"; 20];
            fields[0] = "S";
            fields[2] = "90";
            fields[4] = "99";
            fields[5] = "90";
            fields[19] = "500";
            fs::write(
                pid.join("stat"),
                format!("90 (fixture) {}", fields.join(" ")),
            )
            .unwrap();
            fs::write(pid.join("cmdline"), format!("{}\0", args.join("\0"))).unwrap();
            fs::write(pid.join("environ"), format!("HOME={}\0", f.dir.display())).unwrap();
            symlink(&f.dir, pid.join("cwd")).unwrap();
            let mut snapshot = json!({"panes":[f.pane]});
            assert_eq!(
                enrich_sessions(&f.store.directory, &mut snapshot, &f.adapter).unwrap(),
                1
            );
            let expected = if harness == "opencode" {
                "ses_fixture"
            } else {
                SID
            };
            assert_eq!(
                terminals::reported_session(&snapshot["panes"][0]),
                Some(expected)
            );
            assert_eq!(
                snapshot["panes"][0]["agent_session"]["source"],
                "foreground"
            );
            snapshot["panes"][0]["agent_session"] =
                json!({"kind":"id","agent":harness,"value":"new-current-selection"});
            assert_eq!(
                enrich_sessions(&f.store.directory, &mut snapshot, &f.adapter).unwrap(),
                0
            );
            assert_eq!(
                terminals::reported_session(&snapshot["panes"][0]),
                Some("new-current-selection")
            );
            snapshot["panes"][0]["agent_session"]["source"] = json!("foreground");
            // This adapter supports metadata only: a terminal-output read is a
            // test failure, including while discarding vanished-process IDs.
            fs::remove_dir_all(&pid).unwrap();
            assert_eq!(
                enrich_sessions(&f.store.directory, &mut snapshot, &f.adapter).unwrap(),
                0
            );
            assert!(terminals::reported_session(&snapshot["panes"][0]).is_none());
        }
    }
    #[test]
    fn duplicate_view_keeps_native_configuration_roots_without_caller_scope_or_secrets() {
        let mut f = fixture();
        let mut destination = f.pane.clone();
        destination["pane_id"] = json!("destination");
        destination["terminal_id"] = json!("destination-generation");
        fs::write(
            f.dir.join("destination.json"),
            serde_json::to_vec(&json!({"pane":destination})).unwrap(),
        )
        .unwrap();
        publish_script(
            &f.adapter.herdr,
            r#"#!/usr/bin/python3
from pathlib import Path
import sys
root=Path(__file__).parent
if sys.argv[4]=='get':print((root/('pane.json' if sys.argv[5]=='pane' else 'destination.json')).read_text())
elif sys.argv[4]=='process-info':print('{"process_info":{"foreground_processes":[]}}')
else:sys.exit(2)
"#,
        );
        for (key, value) in [
            (
                "XDG_CONFIG_HOME",
                f.dir.join("custom config").to_string_lossy().into_owned(),
            ),
            (
                "HERDR_CONFIG_PATH",
                f.dir
                    .join("herdr config.toml")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ("HERDR_SESSION", "another-session".into()),
            ("HERDR_PANE_ID", "another-pane".into()),
            (
                "HERDR_SOCKET_PATH",
                f.dir.join("another.sock").to_string_lossy().into_owned(),
            ),
            ("TMUX", "another-server".into()),
        ] {
            f.environment.insert(key.into(), value);
        }
        let prepared = prepare_duplicate_view(
            &f.adapter,
            &f.pane,
            &destination,
            false,
            &f.environment,
            &AttachContext::default(),
        )
        .unwrap();
        for key in ["HOME", "PATH", "XDG_CONFIG_HOME", "HERDR_CONFIG_PATH"] {
            assert_eq!(
                prepared.launch.environment.get(key),
                f.environment.get(key),
                "{key}"
            );
        }
        for key in [
            "HERDR_SESSION",
            "HERDR_PANE_ID",
            "HERDR_SOCKET_PATH",
            "TMUX",
            "GROK_HOME",
            "TELEGRAM_BOT_TOKEN",
        ] {
            assert!(!prepared.launch.environment.contains_key(key), "{key}");
        }
        assert_eq!(
            prepared.launch.args,
            ["--session", "work", "terminal", "attach", "uuid"]
        );
        assert!(!runtime_environment(&f.environment).contains_key("HERDR_CONFIG_PATH"));
    }
    #[test]
    fn own_detection_follows_nested_clients() {
        let pane = |socket: &str, pane: &str, session: &str| json!({"backend":"tmux","socket":socket,"server":socket,"pane_id":pane,"session_id":session,"terminal_id":format!("v2:{pane}")});
        for inner in ["/inner", "/inner, work"] {
            let inside = pane(inner, "%1", "$1");
            let outer = pane("/outer", "%2", "$2");
            let snapshot = json!({"panes":[inside,outer],"clients":[{"socket":inner,"session_id":"$1","outer_panes":[{"socket":"/outer","pane_id":"%2"}]}]});
            let environment = BTreeMap::from([
                ("TMUX".into(), format!("{inner},123,1")),
                ("TMUX_PANE".into(), "%1".into()),
            ]);
            assert_eq!(
                own_terminals(&snapshot, &environment, &BTreeSet::new(), &[]).len(),
                2
            );
        }
    }
    #[test]
    fn store_updates_refuse_concurrent_writer() {
        let f = fixture();
        let _guard = lock(&f.store.directory).unwrap();
        assert!(lock(&f.store.directory).is_err());
    }
    #[test]
    fn forking_parent_id_is_not_saved_even_when_supplied_manually() {
        let f = fixture();
        let pid = f.adapter.proc_root.join("90");
        fs::create_dir(&pid).unwrap();
        let mut fields = vec!["0"; 20];
        fields[19] = "100";
        fs::write(pid.join("stat"), format!("90 (codex) {}", fields.join(" "))).unwrap();
        fs::write(pid.join("cmdline"), b"codex\0fork\0native-session\0").unwrap();
        fs::write(
            pid.join("environ"),
            format!(
                "HOME={}\0CODEX_HOME={}\0UNRELATED_TOKEN=private\0",
                f.dir.display(),
                f.dir.join("profile").display()
            ),
        )
        .unwrap();
        std::os::unix::fs::symlink(&f.dir, pid.join("cwd")).unwrap();
        let script = fs::read_to_string(&f.adapter.herdr).unwrap().replace(
            "\"foreground_processes\":[]",
            "\"pane_id\":\"pane\",\"foreground_processes\":[{\"pid\":90}]",
        );
        publish_script(&f.adapter.herdr, script);
        let supplied = SaveRequest {
            name: "Fork".into(),
            sid: "native-session".into(),
            include_output: false,
        };
        assert!(
            f.store
                .save(&f.adapter, &f.config, &f.environment, &f.pane, &supplied)
                .is_err()
        );
        let saved = save(&f, true);
        assert_eq!(saved.checkpoint.sid, "");
        assert!(!saved.checkpoint.context_file.is_empty());
    }
    #[test]
    fn checkpoint_capture_keeps_native_ids_separate_from_names_and_fork_parents() {
        let mut f = fixture();
        let pid = f.adapter.proc_root.join("90");
        fs::create_dir(&pid).unwrap();
        let mut fields = vec!["0"; 20];
        fields[19] = "100";
        fs::write(
            pid.join("stat"),
            format!("90 (fixture) {}", fields.join(" ")),
        )
        .unwrap();
        std::os::unix::fs::symlink(&f.dir, pid.join("cwd")).unwrap();
        let script = fs::read_to_string(&f.adapter.herdr).unwrap().replace(
            "\"foreground_processes\":[]",
            "\"pane_id\":\"pane\",\"foreground_processes\":[{\"pid\":90}]",
        );
        publish_script(&f.adapter.herdr, script);
        for harness in ["claude", "grok", "opencode"] {
            let (parent, child, selector, fork) = if harness == "opencode" {
                ("ses_parent", "ses_child", "--session", "--fork")
            } else {
                (
                    "019a043e-1234-7000-8000-000000000001",
                    "019a043e-1234-7000-8000-000000000002",
                    "--resume",
                    "--fork-session",
                )
            };
            let tool = harnesses::tool(harness).unwrap();
            f.config.profiles.insert(
                harness.into(),
                f.dir.join("profile").to_string_lossy().into(),
            );
            fs::write(
                pid.join("environ"),
                format!(
                    "HOME={}\0{}={}\0",
                    f.dir.display(),
                    tool.profile_variable,
                    f.dir.join("profile").display()
                ),
            )
            .unwrap();
            let publish = |f: &mut Fixture, argv: &[&str], reported: Option<&str>| {
                fs::write(pid.join("cmdline"), format!("{}\0", argv.join("\0"))).unwrap();
                f.pane["agent"] = json!(harness);
                f.pane["agent_session"] = reported
                    .map(|id| json!({"agent":harness,"kind":"id","value":id}))
                    .unwrap_or(Value::Null);
                fs::write(f.dir.join("pane.json"), json!({"pane":f.pane}).to_string()).unwrap();
            };
            publish(&mut f, &[harness, selector, parent, fork], Some(parent));
            let request = SaveRequest {
                name: format!("{harness} source"),
                sid: parent.into(),
                include_output: false,
            };
            let before =
                layouts::read_private(&f.store.directory.join("dashboard-checkpoints.json"))
                    .unwrap();
            let error = f
                .store
                .save(&f.adapter, &f.config, &f.environment, &f.pane, &request)
                .unwrap_err();
            assert!(error.contains("distinct child ID"), "{harness}: {error}");
            assert_eq!(
                layouts::read_private(&f.store.directory.join("dashboard-checkpoints.json"))
                    .unwrap(),
                before
            );
            publish(&mut f, &[harness, selector, parent, fork], Some(child));
            let saved = save(&f, false);
            assert_eq!(saved.checkpoint.sid, child);
            if harness != "opencode" {
                publish(&mut f, &[harness, "--resume", "work"], Some(child));
                assert_eq!(save(&f, false).checkpoint.sid, child);
                publish(&mut f, &[harness, "--resume", "work"], None);
                let before =
                    fs::read(f.store.directory.join("dashboard-checkpoints.json")).unwrap();
                let request = SaveRequest {
                    sid: String::new(),
                    ..request
                };
                assert!(
                    f.store
                        .save(&f.adapter, &f.config, &f.environment, &f.pane, &request)
                        .is_err()
                );
                assert_eq!(
                    fs::read(f.store.directory.join("dashboard-checkpoints.json")).unwrap(),
                    before
                );
            } else {
                publish(&mut f, &[harness, selector, parent], Some(child));
                assert_eq!(save(&f, false).checkpoint.sid, child);
            }
        }
    }
    #[test]
    fn handoff_uses_target_profile_and_never_claims_native_fork() {
        let mut f = fixture();
        let saved = save(&f, true);
        fs::create_dir(f.dir.join("grok-profile")).unwrap();
        f.config.profiles.insert(
            "grok".into(),
            f.dir.join("grok-profile").to_string_lossy().into_owned(),
        );
        let caps = Capabilities {
            harness: "grok".into(),
            binary: f.dir.join("fake-grok"),
            resume: false,
            native_fork: false,
            handoff: true,
        };
        let prepared = f
            .store
            .prepare(
                &saved.key,
                LaunchRequest {
                    license: None,
                    operation: Operation::Handoff,
                    target: &caps,
                    parent: &f.pane,
                    placement: Placement::Split,
                },
                &f.config,
                &f.environment,
            )
            .unwrap();
        assert_eq!(prepared.launch.operation, Operation::Handoff);
        assert_eq!(
            prepared.launch.environment["GROK_HOME"],
            f.config.profiles["grok"]
        );
        assert!(!prepared.launch.environment.contains_key("CODEX_HOME"));
        assert!(prepared.launch.args[0].contains("terminal excerpt, not full session history"));
    }
    #[test]
    fn viewer_registry_does_not_label_reused_terminals() {
        let f = fixture();
        let mut viewer = f.pane.clone();
        viewer["pane_id"] = "viewer".into();
        viewer["terminal_id"] = "viewer-uuid".into();
        let receipt = Receipt {
            state: crate::adapters::terminal_actions::ReceiptState::Submitted,
            message: "Submitted".into(),
            terminal: Some(viewer.clone()),
        };
        f.store
            .register_view(&f.pane, &receipt, "Herdr native client", &f.config.runtime)
            .unwrap();
        assert_eq!(
            f.store
                .live_views_default(&json!({"panes":[viewer]}))
                .unwrap()
                .rows
                .len(),
            1
        );
        viewer["terminal_id"] = "replacement".into();
        assert!(
            f.store
                .live_views_default(&json!({"panes":[viewer]}))
                .unwrap()
                .rows
                .is_empty()
        );
    }
    #[test]
    fn viewer_projection_does_not_copy_unknown_saved_payloads_or_rewrite_history() {
        let f = fixture();
        let mut viewer = f.pane.clone();
        viewer["pane_id"] = json!("viewer");
        viewer["terminal_id"] = json!("viewer-generation");
        let row = json!({"version":2,"source":terminals::identity(&f.pane).unwrap(),"viewer":terminals::identity(&viewer).unwrap(),"source_label":"source","extra":vec![0;10000]});
        let bytes = serde_json::to_vec(&json!({"version":2,"views":[row]})).unwrap();
        let path = f.store.directory.join("views.json");
        layouts::write_private(&path, &bytes).unwrap();
        let observed = f
            .store
            .live_views_default(&json!({"panes":[viewer]}))
            .unwrap();
        assert_eq!(observed.rows.len(), 1);
        assert!(observed.rows[0].get("extra").is_none());
        assert!(serde_json::to_vec(&observed.rows).unwrap().len() < 4096);
        assert_eq!(layouts::read_private(&path).unwrap().unwrap(), bytes);
    }
    #[test]
    fn saved_viewer_byte_and_value_limits_refuse_without_rewriting_and_recover_when_raised() {
        let f = fixture();
        let path = f.store.directory.join("views.json");
        let mut viewer = f.pane.clone();
        viewer["pane_id"] = json!("viewer");
        viewer["terminal_id"] = json!("viewer-generation");
        for values in [false, true] {
            let mut row = json!({"version":2,"source":terminals::identity(&f.pane).unwrap(),"viewer":terminals::identity(&viewer).unwrap()});
            if values {
                row["extra"] = json!(vec![0; 200]);
            } else {
                row["mechanism"] = json!("x".repeat(5000));
            }
            let bytes = serde_json::to_vec(&json!({"version":2,"views":[row]})).unwrap();
            layouts::write_private(&path, &bytes).unwrap();
            let mut settings = super::super::runtime_settings::Settings {
                terminal_view_history_limit_bytes: 4096,
                terminal_view_history_value_limit: 128,
                ..Default::default()
            };
            let result = f.store.live_views(
                &json!({"panes":[viewer.clone()]}),
                &settings,
                &super::super::discovery_budget::Budget::new(&settings).unwrap(),
            );
            assert!(result.err().unwrap().contains(if values {
                "value budget"
            } else {
                "byte budget"
            }));
            assert_eq!(fs::read(&path).unwrap(), bytes);
            settings.terminal_view_history_limit_bytes = 8192;
            settings.terminal_view_history_value_limit = 1024;
            let result = f
                .store
                .live_views(
                    &json!({"panes":[viewer.clone()]}),
                    &settings,
                    &super::super::discovery_budget::Budget::new(&settings).unwrap(),
                )
                .unwrap();
            assert_eq!(result.rows.len(), 1);
            assert!(result.rows[0].get("extra").is_none());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }
    #[test]
    fn viewer_projection_shares_remaining_inventory_capacity_and_refuses_an_expired_pass() {
        let f = fixture();
        let path = f.store.directory.join("views.json");
        let mut viewer = f.pane.clone();
        viewer["pane_id"] = json!("viewer");
        viewer["terminal_id"] = json!("viewer-generation");
        let bytes=serde_json::to_vec(&json!({"version":2,"views":[{"version":2,"source":terminals::identity(&f.pane).unwrap(),"viewer":terminals::identity(&viewer).unwrap()}]})).unwrap();
        layouts::write_private(&path, &bytes).unwrap();
        let settings = super::super::runtime_settings::Settings {
            discovery_metadata_limit_bytes: 4096,
            ..Default::default()
        };
        let budget = super::super::discovery_budget::Budget::new(&settings).unwrap();
        budget
            .admit_text(budget.remaining_bytes() - 100, 0)
            .unwrap();
        let result = f
            .store
            .live_views(&json!({"panes":[viewer.clone()]}), &settings, &budget)
            .unwrap();
        assert!(result.rows.is_empty());
        assert_eq!(result.unresolved, 1);
        let mut snapshot = json!({"errors":[]});
        budget.finish(&mut snapshot);
        budget.finish(&mut snapshot);
        assert_eq!(snapshot["errors"].as_array().unwrap().len(), 1);
        let mut expired = super::super::discovery_budget::Budget::new(&settings).unwrap();
        expired.deadline = std::time::Instant::now();
        assert!(
            f.store
                .live_views(&json!({"panes":[viewer]}), &settings, &expired)
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    #[test]
    fn confirmed_viewer_registration_preserves_history_when_read_or_write_policy_refuses() {
        let f = fixture();
        let path = f.store.directory.join("views.json");
        let initial = br#"{"version":2,"views":[]}"#;
        layouts::write_private(&path, initial).unwrap();
        let mut viewer = f.pane.clone();
        viewer["pane_id"] = json!("viewer");
        viewer["terminal_id"] = json!("viewer-generation");
        let receipt = Receipt {
            state: crate::adapters::terminal_actions::ReceiptState::Submitted,
            message: "Already submitted".into(),
            terminal: Some(viewer),
        };
        let mut settings = super::super::runtime_settings::Settings {
            terminal_view_history_limit_bytes: 4096,
            ..Default::default()
        };
        assert!(
            f.store
                .register_view(&f.pane, &receipt, &"x".repeat(5000), &settings)
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), initial);
        assert_eq!(receipt.message, "Already submitted");
        settings.terminal_view_history_limit_bytes = 8192;
        f.store
            .register_view(&f.pane, &receipt, &"x".repeat(5000), &settings)
            .unwrap();
        let saved = fs::read(&path).unwrap();
        assert!(saved.len() > 4096);
        settings.terminal_view_history_limit_bytes = 4096;
        assert!(
            f.store
                .register_view(
                    &f.pane,
                    &receipt,
                    "second attempt after already submitted",
                    &settings
                )
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), saved);
    }
    #[test]
    fn duplicate_registry_keys_cannot_select_a_viewer_identity_or_rewrite_history() {
        let f = fixture();
        let path = f.store.directory.join("views.json");
        let mut viewer = f.pane.clone();
        viewer["pane_id"] = json!("viewer");
        viewer["terminal_id"] = json!("viewer-generation");
        let row = json!({"version":2,"source":terminals::identity(&f.pane).unwrap(),"viewer":terminals::identity(&viewer).unwrap()});
        let bytes = format!("{{\"version\":3,\"version\":2,\"views\":[{row}]}}").into_bytes();
        layouts::write_private(&path, &bytes).unwrap();
        assert!(
            f.store
                .live_views_default(&json!({"panes":[viewer]}))
                .is_err()
        );
        assert_eq!(layouts::read_private(&path).unwrap().unwrap(), bytes);
    }
    #[test]
    fn viewer_registry_records_structured_source_identity_without_delimiter_collisions() {
        let f = fixture();
        let mut source = f.pane.clone();
        source["pane_id"] = json!("a/b");
        source["terminal_id"] = json!("c");
        let mut viewer = f.pane.clone();
        viewer["pane_id"] = json!("viewer");
        viewer["terminal_id"] = json!("viewer-generation");
        f.store
            .register_view(
                &source,
                &Receipt {
                    state: crate::adapters::terminal_actions::ReceiptState::Submitted,
                    message: "Fixture".into(),
                    terminal: Some(viewer),
                },
                "fixture",
                &f.config.runtime,
            )
            .unwrap();
        let bytes = layouts::read_private(&f.store.directory.join("views.json"))
            .unwrap()
            .unwrap();
        let stored: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(stored["views"][0]["source"].is_object());
        assert_eq!(
            stored["views"][0]["source"],
            serde_json::to_value(terminals::identity(&source).unwrap()).unwrap()
        );
    }
    #[test]
    fn attach_view_counts_require_the_exact_pane_identity() {
        let pane = json!({"backend":"herdr","session":"fixture","server":"fixture","pane_id":"one","terminal_id":"shared-generation"});
        let mut other = pane.clone();
        other["pane_id"] = json!("two");
        assert_eq!(view_facts(&pane, &json!({"panes":[pane,other]})).0, 1);
    }
    #[test]
    fn legacy_view_read_preserves_ambiguous_history_and_deduplicates_linked_viewers() {
        let f = fixture();
        let mut viewer = f.pane.clone();
        viewer["pane_id"] = json!("viewer");
        viewer["terminal_id"] = json!("viewer-generation");
        let history=serde_json::to_vec(&json!({"views":[
            {"source":"herdr/work/pane/uuid","backend":"herdr","server":"work","pane_id":"viewer","viewer_terminal":"viewer-generation"},
            {"source":"herdr/work/a/b/c","backend":"herdr","server":"work","pane_id":"other","viewer_terminal":"other-generation"}
        ]})).unwrap();
        layouts::write_private(&f.store.directory.join("views.json"), &history).unwrap();
        let observation = f
            .store
            .live_views_default(&json!({"panes":[viewer.clone(),viewer]}))
            .unwrap();
        assert_eq!(observation.rows.len(), 1);
        assert_eq!(observation.unresolved, 1);
        assert_eq!(
            layouts::read_private(&f.store.directory.join("views.json"))
                .unwrap()
                .unwrap(),
            history
        );
    }
    #[test]
    fn viewer_history_retention_is_saved_policy_and_unsupported_registry_is_preserved() {
        let mut f = fixture();
        f.config.runtime.terminal_view_history_limit = 1;
        for name in ["viewer-one", "viewer-two"] {
            let mut viewer = f.pane.clone();
            viewer["pane_id"] = json!(name);
            viewer["terminal_id"] = json!(name);
            f.store
                .register_view(
                    &f.pane,
                    &Receipt {
                        state: crate::adapters::terminal_actions::ReceiptState::Submitted,
                        message: "Fixture".into(),
                        terminal: Some(viewer),
                    },
                    "fixture",
                    &f.config.runtime,
                )
                .unwrap();
        }
        let path = f.store.directory.join("views.json");
        let bytes = layouts::read_private(&path).unwrap().unwrap();
        let stored: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(stored["version"], 2);
        assert_eq!(stored["views"].as_array().unwrap().len(), 1);
        assert_eq!(stored["views"][0]["viewer"]["pane"], "viewer-two");
        let unsupported = br#"{"version":3,"views":[]}"#;
        layouts::write_private(&path, unsupported).unwrap();
        assert!(f.store.live_views_default(&json!({"panes":[]})).is_err());
        let mut viewer = f.pane.clone();
        viewer["pane_id"] = json!("viewer-three");
        let receipt = Receipt {
            state: crate::adapters::terminal_actions::ReceiptState::Submitted,
            message: "Fixture".into(),
            terminal: Some(viewer),
        };
        assert!(
            f.store
                .register_view(&f.pane, &receipt, "fixture", &f.config.runtime)
                .is_err()
        );
        assert_eq!(layouts::read_private(&path).unwrap().unwrap(), unsupported);
    }
    #[test]
    fn malformed_supplemental_generation_cannot_bypass_handoff_source_revalidation() {
        let f = fixture();
        let checkpoint = f
            .store
            .live_source(&f.adapter, &f.config, &f.environment, &f.pane, "")
            .unwrap();
        let mut changed = f.pane.clone();
        changed["generation"] = json!(17);
        fs::write(
            f.dir.join("pane.json"),
            serde_json::to_vec(&json!({"pane":changed})).unwrap(),
        )
        .unwrap();
        assert!(
            f.store
                .capture_handoff(&f.adapter, &f.pane, checkpoint)
                .is_err()
        );
        assert!(!f.store.directory.join("checkpoints").exists());
    }
    #[test]
    fn handoff_rejects_conversation_or_harness_swapped_during_confirmation() {
        let f = fixture();
        let checkpoint = f
            .store
            .live_source(&f.adapter, &f.config, &f.environment, &f.pane, "")
            .unwrap();
        for replacement in [
            json!({"agent":"codex","agent_session":{"agent":"codex","kind":"id","value":"different-session"}}),
            json!({"agent":"claude","agent_session":{"agent":"claude","kind":"id","value":"native-session"}}),
        ] {
            let mut changed = f.pane.clone();
            changed["agent"] = replacement["agent"].clone();
            changed["agent_session"] = replacement["agent_session"].clone();
            fs::write(
                f.dir.join("pane.json"),
                serde_json::to_vec(&json!({"pane":changed})).unwrap(),
            )
            .unwrap();
            let error = f
                .store
                .capture_handoff(&f.adapter, &f.pane, checkpoint.clone())
                .unwrap_err();
            assert!(error.contains("Handoff harness changed"));
            assert!(!f.store.directory.join("checkpoints").exists());
        }
    }
    #[test]
    fn handoff_rejects_restarted_same_harness_without_reported_session_id() {
        let mut f = fixture();
        f.pane["agent_session"] = Value::Null;
        fs::write(
            f.dir.join("pane.json"),
            serde_json::to_vec(&json!({"pane":f.pane})).unwrap(),
        )
        .unwrap();
        let pid = f.dir.join("proc/90");
        fs::create_dir(&pid).unwrap();
        let mut fields = vec!["0"; 20];
        fields[0] = "S";
        fields[19] = "100";
        fs::write(pid.join("stat"), format!("90 (codex) {}", fields.join(" "))).unwrap();
        fs::write(pid.join("cmdline"), b"codex\0").unwrap();
        fs::write(
            pid.join("environ"),
            format!(
                "HOME={}\0CODEX_HOME={}\0",
                f.dir.display(),
                f.dir.join("profile").display()
            ),
        )
        .unwrap();
        std::os::unix::fs::symlink(&f.dir, pid.join("cwd")).unwrap();
        let script = fs::read_to_string(&f.adapter.herdr).unwrap().replace(
            "\"foreground_processes\":[]",
            "\"pane_id\":\"pane\",\"foreground_processes\":[{\"pid\":90}]",
        );
        publish_script(&f.adapter.herdr, script);
        let (checkpoint, frozen) = f
            .store
            .freeze_live_source(&f.adapter, &f.config, &f.environment, &f.pane, "")
            .unwrap();
        assert!(checkpoint.sid.is_empty());
        assert_eq!(frozen["_daedalus_handoff_process"]["start"], "100");
        // Same PID reused, same executable/profile/cwd/pane, but a new process.
        fields[19] = "101";
        fs::write(pid.join("stat"), format!("90 (codex) {}", fields.join(" "))).unwrap();
        let error = f
            .store
            .capture_handoff(&f.adapter, &frozen, checkpoint)
            .unwrap_err();
        assert!(error.contains("foreground process changed"));
        assert!(!f.store.directory.join("checkpoints").exists());
    }
}

#[cfg(windows)]
fn lock(directory: &Path) -> Result<StoreLock> {
    crate::platform::ensure_private_dir(directory).map_err(|e| e.to_string())?;
    let file = crate::platform::open_private(&directory.join(".sessions.lock"), true, true)
        .map_err(|e| e.to_string())?;
    crate::platform::lock_exclusive(&file, true).map_err(|_| "Session store is busy")?;
    Ok(StoreLock(file))
}

#[cfg(unix)]
fn native_executable(metadata: &fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o111 != 0
}
#[cfg(windows)]
fn native_executable(_metadata: &fs::Metadata) -> bool {
    true
}

#[cfg(windows)]
fn read_process(
    _adapter: &Terminals,
    pid: &str,
    _harness: &str,
    guard: &mut MetadataGuard<'_>,
) -> Result<Option<ProcessEvidence>> {
    guard.processes(1)?;
    guard.entries(1)?;
    let pid = pid.parse().map_err(|_| "Invalid foreground process ID")?;
    let actual =
        crate::platform::process_metadata(pid).map_err(|_| "Cannot verify foreground process")?;
    if actual.owner != crate::platform::current_identity().map_err(|e| e.to_string())? {
        return Err("Foreground process belongs to another account".into());
    }
    // Windows does not expose supported argv/cwd/environment APIs for another
    // process. Herdr's correlated native session metadata remains available;
    // never invent argv evidence by treating its display label as a command.
    guard.check()?;
    Ok(None)
}

#[cfg(test)]
mod fingerprint_tests {
    use super::*;
    const SID: &str = "019a043e-1234-7000-8000-000000000001";
    #[test]
    fn windows_structured_argv_recovers_conversation_without_parsing_cmdline() {
        let pane = json!({"pane_id":"pane","agent":"CODEX.EXE","cwd":"C:\\Project"});
        let info = json!({"pane_id":"pane","foreground_processes":[{
            "pid":"42","argv":["C:\\Tools\\Codex.exe","resume",SID],
            "cmdline":"misleading prompt and secrets are ignored"}]});
        let process = structured_foreground(&info, &pane, "codex", |pid, executable| {
            assert_eq!(pid, 42);
            assert_eq!(executable, "C:\\Tools\\Codex.exe");
            Ok("500".into())
        })
        .unwrap()
        .unwrap();
        assert_eq!(process.start, "500");
        assert_eq!(
            harnesses::launch_conversation(&process.argv),
            Some(("codex", SID.into()))
        );
        let mut unstructured = info;
        unstructured["foreground_processes"][0]
            .as_object_mut()
            .unwrap()
            .remove("argv");
        assert!(
            structured_foreground(&unstructured, &pane, "codex", |_, _| panic!(
                "no argv proof"
            ))
            .unwrap()
            .is_none()
        );
    }
    #[test]
    fn structured_foreground_uses_saved_process_and_argument_limits() {
        let pane = json!({"pane_id":"pane"});
        let info = json!({"pane_id":"pane","foreground_processes":[{"pid":1,"argv":["codex","resume",SID]},{"pid":2,"argv":["shell"]}]});
        let mut guard = MetadataGuard::new(crate::core::native_metadata::Policy {
            processes: 1,
            ..Default::default()
        })
        .unwrap();
        assert!(
            structured_foreground_guarded(
                &info,
                &pane,
                "codex",
                |_, _| panic!("must refuse before process lookup"),
                &mut guard
            )
            .unwrap_err()
            .contains("native_metadata_process_limit")
        );
        let mut guard = MetadataGuard::new(crate::core::native_metadata::Policy {
            entries: 2,
            ..Default::default()
        })
        .unwrap();
        assert!(
            structured_foreground_guarded(
                &info,
                &pane,
                "codex",
                |_, _| panic!("must refuse before argv lookup"),
                &mut guard
            )
            .unwrap_err()
            .contains("native_metadata_entry_limit")
        );
    }
    #[test]
    fn structured_foreground_rejects_replaced_panes_duplicate_pids_and_ambiguous_harnesses() {
        let pane = json!({"pane_id":"pane"});
        let row = json!({"pid":42,"argv":["Codex.exe","resume",SID]});
        for info in [
            json!({"pane_id":"replacement","foreground_processes":[row]}),
            json!({"pane_id":"pane","foreground_processes":[row,row]}),
            json!({"pane_id":"pane","foreground_processes":[row,{"pid":43,"argv":["codex.exe","resume",SID]}]}),
            json!({"pane_id":"pane","foreground_processes":[{"pid":0,"argv":["codex.exe"]}]}),
        ] {
            assert!(structured_foreground(&info, &pane, "codex", |_, _| Ok("500".into())).is_err());
        }
        let info = json!({"pane_id":"pane","foreground_processes":[row]});
        assert!(
            structured_foreground(&info, &pane, "codex", |_, _| Err(
                "PID reused or wrong owner".into()
            ))
            .is_err()
        );
    }
    #[test]
    fn windows_native_fingerprint_is_reused_for_capture_and_rejects_restart() {
        let native =
            json!({"pid":"42","start":"100","executable":"fixture.exe","arguments_observed":false});
        let frozen = native_fingerprint_or(Value::Null, || Ok(native.clone())).unwrap();
        let capture = native_fingerprint_or(Value::Null, || Ok(native.clone())).unwrap();
        assert_eq!(frozen, capture);
        assert!(!capture.is_null());
        let mut restarted = native.clone();
        restarted["start"] = json!("200");
        assert_ne!(
            frozen,
            native_fingerprint_or(Value::Null, || Ok(restarted)).unwrap()
        );
        assert!(native_fingerprint_or(Value::Null, || Err("process disappeared".into())).is_err());
        assert_eq!(
            native_fingerprint_or(native.clone(), || panic!("must retain observed evidence"))
                .unwrap(),
            native
        );
    }
}
