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
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::io::Read;
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
}
#[derive(Clone, Debug)]
pub struct SessionStore {
    pub directory: PathBuf,
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
fn first(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .map(|k| field(value, k))
        .find(|s| !s.is_empty())
        .unwrap_or_default()
}
fn key(checkpoint: &Checkpoint) -> Result<String> {
    let encoded = serde_json::to_vec(checkpoint).map_err(|_| "Cannot encode checkpoint")?;
    Ok(format!("checkpoint:{:x}", Sha256::digest(encoded)))
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
    let id = terminals::identity(pane)?;
    Ok(TerminalRef {
        backend: id.backend,
        server: first(pane, &["server", "session", "socket"]),
        pane_id: id.pane,
        terminal_id: id.generation,
        generation: String::new(),
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
#[cfg(unix)]
fn read_process(adapter: &Terminals, pid: &str, harness: &str) -> Result<Option<ProcessEvidence>> {
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Invalid foreground process ID".into());
    }
    let Some(start) = terminals::process_start(&adapter.proc_root, pid) else {
        return Ok(None);
    };
    let directory = adapter.proc_root.join(pid);
    fn read(path: &Path) -> Result<Vec<u8>> {
        let file = File::open(path).map_err(|_| "Foreground process is unavailable")?;
        let mut data = Vec::new();
        file.take(1_048_577)
            .read_to_end(&mut data)
            .map_err(|_| "Cannot read foreground process metadata")?;
        if data.len() > 1_048_576 {
            return Err("Foreground process metadata exceeds limit".into());
        }
        Ok(data)
    }
    let raw = read(&directory.join("cmdline"))?;
    let argv: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect();
    let Some(tool) = harnesses::tool(harness) else {
        return Ok(None);
    };
    // Match executable names in native argv, never terminal labels or titles.
    let managed_pi = harness == "pi"
        && argv.windows(2).any(|a| a[0] == "--session-dir")
        && argv.windows(2).any(|a| a[0] == "--extension")
        && argv
            .get(1)
            .is_some_and(|a| Path::new(a).file_name().is_some_and(|n| n == "cli.js"));
    if !managed_pi
        && !argv.iter().take(4).any(|arg| {
            Path::new(arg)
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| tool.executables.contains(&name))
        })
    {
        return Ok(None);
    }
    let cwd = fs::read_link(directory.join("cwd"))
        .map_err(|_| "Foreground project directory is unavailable")?
        .to_str()
        .ok_or("Project directory is not UTF-8")?
        .to_owned();
    let raw_env = read(&directory.join("environ"))?;
    let mut profile = None;
    let mut home = None;
    for pair in raw_env.split(|b| *b == 0) {
        let Some(equal) = pair.iter().position(|b| *b == b'=') else {
            continue;
        };
        let name = &pair[..equal];
        if name == tool.profile_variable.as_bytes() {
            profile = Some(String::from_utf8_lossy(&pair[equal + 1..]).into_owned());
        } else if name == b"HOME" {
            home = Some(String::from_utf8_lossy(&pair[equal + 1..]).into_owned());
        }
    }
    let profile = profile
        .or_else(|| {
            home.map(|home| {
                Path::new(&home)
                    .join(tool.default_profile)
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .unwrap_or_default();
    if terminals::process_start(&adapter.proc_root, pid).as_deref() != Some(&start) {
        return Err("Foreground process changed while reading metadata".into());
    }
    Ok(Some(ProcessEvidence {
        pid: pid.into(),
        start,
        argv,
        cwd,
        profile,
    }))
}
fn foreground(adapter: &Terminals, pane: &Value, harness: &str) -> Result<Option<ProcessEvidence>> {
    let id = terminals::identity(pane)?;
    let mut pids = Vec::new();
    if id.backend == Backend::Herdr {
        let info =
            adapter.herdr_result(&id.endpoint, &["pane", "process-info", "--pane", &id.pane])?;
        if let Some(rows) = info["process_info"]["foreground_processes"].as_array() {
            for row in rows {
                if let Some(pid) = row.get("pid") {
                    let pid = pid
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| pid.to_string());
                    pids.push(pid);
                }
            }
        }
    } else {
        let pid = field(pane, "pid");
        if !pid.is_empty() {
            pids.push(pid.clone());
        }
        // The shell's controlling tty identifies its foreground process group.
        if let Ok(stat) = fs::read_to_string(adapter.proc_root.join(&pid).join("stat"))
            && let Some((_, tail)) = stat.rsplit_once(')')
        {
            let fields: Vec<_> = tail.split_whitespace().collect();
            if let Some(tty) = fields.get(4).filter(|tty| **tty != "0")
                && let Ok(entries) = fs::read_dir(&adapter.proc_root)
            {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if !name.bytes().all(|b| b.is_ascii_digit()) {
                        continue;
                    }
                    if let Ok(stat) = fs::read_to_string(entry.path().join("stat"))
                        && let Some((_, tail)) = stat.rsplit_once(')')
                    {
                        let row: Vec<_> = tail.split_whitespace().collect();
                        if row.get(4) == Some(tty) && row.get(2) == row.get(5) {
                            pids.push(name);
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
        if let Some(evidence) = read_process(adapter, &pid, harness)? {
            candidates.push(evidence);
        }
    }
    if candidates.len() > 1 {
        return Err("Foreground harness identity is ambiguous".into());
    }
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
) -> Result<Value> {
    let observed = process_fingerprint(evidence);
    #[cfg(windows)]
    return native_fingerprint_or(observed, || {
        windows_native_process(_adapter, _pane, _harness)
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
) -> Result<()> {
    if let Some(before) = evidence {
        let after =
            read_process(adapter, &before.pid, harness)?.ok_or("Foreground harness disappeared")?;
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

impl SessionStore {
    /// Record only a confirmed new viewer terminal, never an uncertain pane
    /// creation. Registration failure must not retry the already submitted launch.
    pub fn register_view(&self, source: &Value, receipt: &Receipt, mechanism: &str) -> Result<()> {
        if receipt.state != crate::adapters::terminal_actions::ReceiptState::Submitted {
            return Err("Viewer creation is unconfirmed; registration was skipped".into());
        }
        let viewer = receipt
            .terminal
            .as_ref()
            .ok_or("Viewer terminal identity is missing")?;
        terminals::identity(source)?;
        terminals::identity(viewer)?;
        let _lock = lock(&self.directory)?;
        let path = self.directory.join("views.json");
        let mut rows = match layouts::read_private(&path)? {
            Some(bytes) => serde_json::from_slice::<Value>(&bytes)
                .map_err(|_| "Invalid viewer registry")?["views"]
                .as_array()
                .cloned()
                .ok_or("Invalid viewer registry")?,
            None => Vec::new(),
        };
        let record = json!({
            "source": (["backend", "server", "pane_id", "terminal_id"].map(|k| field(source,k)).join("/")),
            "source_label": tree::title(source, "").chars().take(80).collect::<String>(),
            "backend": field(viewer,"backend"), "server": first(viewer,&["server","session"]),
            "server_pid": field(viewer,"server_pid"), "pane_id": field(viewer,"pane_id"),
            "viewer_terminal": field(viewer,"terminal_id"), "mechanism": mechanism, "created_at": utc_now()
        });
        rows.retain(|row| {
            !(row["backend"] == record["backend"]
                && row["server"] == record["server"]
                && row["pane_id"] == record["pane_id"])
        });
        rows.push(record);
        if rows.len() > 200 {
            rows.drain(..rows.len() - 200);
        }
        layouts::write_private(
            &path,
            &serde_json::to_vec(&json!({"views":rows}))
                .map_err(|_| "Cannot encode viewer registry")?,
        )
    }
    /// Only exact current viewer identities are projected into the tree. Missing
    /// servers are not grounds for deleting private historical references.
    pub fn live_views(&self, snapshot: &Value) -> Result<Vec<Value>> {
        let Some(bytes) = layouts::read_private(&self.directory.join("views.json"))? else {
            return Ok(Vec::new());
        };
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid viewer registry")?;
        let rows = value["views"].as_array().ok_or("Invalid viewer registry")?;
        let mut live = Vec::new();
        for row in rows {
            let matching: Vec<_> = snapshot
                .get("panes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|pane| {
                    pane.get("backend") == row.get("backend")
                        && first(pane, &["server", "session"]) == field(row, "server")
                        && pane.get("pane_id") == row.get("pane_id")
                        && !field(row, "viewer_terminal").is_empty()
                        && field(pane, "terminal_id") == field(row, "viewer_terminal")
                        && (field(row, "server_pid").is_empty()
                            || field(pane, "server_pid") == field(row, "server_pid"))
                })
                .collect();
            if let Some(pane) = matching.first() {
                let mut record = row.clone();
                record["terminal_id"] = pane["terminal_id"].clone();
                live.push(record);
            }
        }
        Ok(live)
    }
    fn removed(&self) -> Result<BTreeSet<String>> {
        let Some(bytes) = layouts::read_private(&self.directory.join("checkpoint-removals.json"))?
        else {
            return Ok(BTreeSet::new());
        };
        serde_json::from_slice(&bytes).map_err(|_| "Invalid checkpoint removal index".into())
    }
    pub fn list(&self) -> Result<Vec<CheckpointRow>> {
        let removed = self.removed()?;
        Registry::load(&self.directory)?
            .checkpoints
            .into_iter()
            .map(|checkpoint| {
                Ok(CheckpointRow {
                    key: key(&checkpoint)?,
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
            .collect()
    }
    pub fn get(&self, key: &str) -> Result<CheckpointRow> {
        self.list()?
            .into_iter()
            .find(|row| row.key == key)
            .ok_or_else(|| "Checkpoint changed or was removed; select it again".into())
    }
    /// Removing a reference never deletes native history or terminal output files.
    /// Tombstones also suppress matching entries in untouched legacy registries.
    pub fn remove(&self, key: &str) -> Result<()> {
        let _lock = lock(&self.directory)?;
        self.get(key)?;
        let mut removed = self.removed()?;
        removed.insert(key.into());
        layouts::write_private(
            &self.directory.join("checkpoint-removals.json"),
            &serde_json::to_vec(&removed).map_err(|_| "Cannot encode checkpoint removal")?,
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
        let actual = adapter.verify(pane)?;
        let observed_harness = field(&actual, "agent").to_lowercase();
        let mut harness = observed_harness.clone();
        if matches!(harness.as_str(), "" | "node" | "nodejs") {
            let process = foreground(adapter, &actual, "pi")?
                .ok_or("Terminal has no verified managed harness")?;
            pi_sessions::live(
                &self.directory,
                &terminals::identity(&actual)?,
                &process.pid,
                &process.start,
                &process.argv,
            )?
            .ok_or("Terminal has no verified Pi session")?;
            harness = "pi".into();
        }
        let native = &actual["agent_session"];
        let reported = if native["kind"] == "id" && field(native, "agent") == harness {
            field(native, "value")
        } else {
            String::new()
        };
        let evidence = foreground(adapter, &actual, &harness)?;
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
                    )
                })
                .transpose()?
                .flatten()
        } else {
            None
        };
        let reported = discovered_pi
            .as_ref()
            .map(|s| s.id.clone())
            .unwrap_or(reported);
        let reported = if let Some(process) = &evidence {
            checkpoints::resolve_sid(&process.argv, &harness, &reported)?.unwrap_or_default()
        } else {
            reported
        };
        let supplied_sid = supplied_sid.trim();
        if !supplied_sid.is_empty()
            && let Some(process) = &evidence
            && checkpoints::resolve_sid(&process.argv, &harness, supplied_sid)?.is_none()
        {
            return Err(
                "A forking process still reports its parent ID; wait for the child session ID"
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
        let again = adapter.verify(pane)?;
        if field(&again, "agent").to_lowercase() != observed_harness
            || again.get("agent_session") != actual.get("agent_session")
        {
            return Err("Native harness metadata changed; refresh".into());
        }
        check_process(adapter, &evidence, &harness)?;
        if let Some(session) = &discovered_pi {
            session.verify()?;
        }
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
        let before = adapter.verify(pane)?;
        let harness = field(&before, "agent").to_lowercase();
        let evidence = foreground(adapter, &before, &harness)?;
        let checkpoint = self.live_source(adapter, config, environment, pane, supplied_sid)?;
        check_process(adapter, &evidence, &harness)?;
        let after = adapter.verify(pane)?;
        if before.get("agent_session") != after.get("agent_session")
            || before.get("agent") != after.get("agent")
        {
            return Err("Handoff source changed while preparing; select it again".into());
        }
        let mut frozen = before;
        frozen["_daedalus_handoff_process"] =
            capture_fingerprint(adapter, &frozen, &harness, &evidence)?;
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
        let before = adapter.verify(pane)?;
        let observed = field(&before, "agent").to_lowercase();
        // A terminal excerpt is useful even when no native conversation can be
        // identified. Never promote shell/Node metadata into a provider ID.
        let context_only = harnesses::tool(&observed).is_none()
            && request.include_output
            && request.sid.trim().is_empty();
        let mut row = if context_only {
            // Preserve registered Pi identity when it is genuinely observable;
            // failure here only falls back to an explicitly context-only record.
            match self.live_source(adapter, config, environment, pane, "") {
                Ok(row) if row.harness == "pi" => row,
                _ => self.capture_terminal_context(adapter, pane, &before, &request.name)?,
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
            self.live_source(adapter, config, environment, pane, &request.sid)?
        };
        let process = if row.context_file.is_empty() {
            foreground(adapter, &before, &row.harness)?
        } else {
            None
        };
        row.name = request.name.trim().into();
        if request.include_output && row.context_file.is_empty() {
            let mut frozen = before.clone();
            frozen["_daedalus_handoff_process"] =
                capture_fingerprint(adapter, &frozen, &row.harness, &process)?;
            row = self.capture_handoff(adapter, &frozen, row)?;
        }
        row.validate()?;
        let after = adapter.verify(pane)?;
        check_process(adapter, &process, &row.harness)?;
        if before.get("agent_session") != after.get("agent_session")
            || before.get("agent") != after.get("agent")
        {
            return Err("Harness changed while saving checkpoint".into());
        }
        let mut registry = Registry {
            checkpoints: self.list()?.into_iter().map(|row| row.checkpoint).collect(),
        };
        registry.add_verified(row.clone(), &terminal_ref(&before)?, &terminal_ref(&after)?)?;
        registry.save(&self.directory)?;
        Ok(CheckpointRow {
            key: key(&row)?,
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
        let after = adapter.verify(pane)?;
        if !terminal_ref(before)?.same_identity(&terminal_ref(&after)?)
            || before.get("agent") != after.get("agent")
            || before.get("agent_session") != after.get("agent_session")
            || first(&after, &["foreground_cwd", "cwd"]) != checkpoint.cwd
        {
            return Err("Terminal changed during context capture; select it again".into());
        }
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
        mut checkpoint: Checkpoint,
    ) -> Result<Checkpoint> {
        let before = adapter.verify(pane)?;
        if checkpoint.source.as_ref().is_some_and(|source| {
            terminal_ref(&before).is_ok_and(|actual| !actual.same_identity(source))
        }) {
            return Err("Handoff source changed; select it again".into());
        }
        // The pane can survive replacement of its foreground harness while the
        // confirmation dialog is open. Bind capture to the frozen conversation,
        // not just to the outer terminal's process generation.
        let harness = field(&before, "agent").to_lowercase();
        if harness != checkpoint.harness
            || before.get("agent_session") != pane.get("agent_session")
            || field(pane, "agent").to_lowercase() != harness
        {
            return Err("Handoff harness changed; select it again".into());
        }
        let process = foreground(adapter, &before, &harness)?;
        let fingerprint = capture_fingerprint(adapter, &before, &harness, &process)?;
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
        let reported = if before["agent_session"]["kind"] == "id"
            && field(&before["agent_session"], "agent") == harness
        {
            field(&before["agent_session"], "value")
        } else {
            String::new()
        };
        let current_sid = if let Some(process) = &process {
            checkpoints::resolve_sid(&process.argv, &harness, &reported)?.unwrap_or_default()
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
        let after = adapter.verify(pane)?;
        check_process(adapter, &process, &harness)?;
        if fingerprint != capture_fingerprint(adapter, &after, &harness, &process)? {
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
        let saved = self.get(key)?;
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
        } = request;
        let destination = terminals::identity(parent)?;
        let mut launch = checkpoints::plan(
            &checkpoint,
            operation,
            target,
            &self.directory.join("checkpoints"),
        )?;
        let pi_session = if checkpoint.harness == "pi" && operation != Operation::Handoff {
            let session = pi_sessions::saved(&self.directory, &checkpoint.sid)?;
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
        let native =
            harnesses::launch_plan(&tool, &launch.binary, &launch.cwd, environment, &profiles)?;
        launch.environment = native.environment;
        Ok(PreparedLaunch {
            checkpoint,
            saved_key,
            pi_session,
            launch,
            parent: parent.clone(),
            destination,
            placement,
        })
    }
    pub fn apply(&self, adapter: &Terminals, prepared: PreparedLaunch) -> Result<Receipt> {
        if let Some(key) = &prepared.saved_key {
            let current = self.get(key)?;
            if key != &self::key(&prepared.checkpoint)? || self::key(&current.checkpoint)? != *key {
                return Err("Checkpoint changed before launch".into());
            }
        }
        if terminals::identity(&prepared.parent)? != prepared.destination {
            return Err("Launch destination changed".into());
        }
        if let Some(session) = &prepared.pi_session {
            session.verify()?;
        }
        adapter.spawn(&prepared.parent, &prepared.launch, prepared.placement)
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
        .and_then(|s| s.split(',').next())
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
    let same: Vec<_> = snapshot
        .get("panes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|p| {
            p.get("backend") == pane.get("backend")
                && first(p, &["socket", "server"]) == first(pane, &["socket", "server"])
                && p.get("terminal_id") == pane.get("terminal_id")
        })
        .collect();
    let viewers = snapshot
        .get("daedalus_views")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|v| {
            field(v, "source")
                == ["backend", "server", "pane_id", "terminal_id"]
                    .map(|k| field(pane, k))
                    .join("/")
        })
        .count();
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
    let mut plan = json!({"mode":mode.name(),"title":title,"backend":if id.backend==Backend::Tmux{"tmux"}else{"herdr"},"server":first(pane,&["socket","server"]),"terminal":id.pane,"views":views,"input":input,"node":context.node});
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
            environment: runtime_environment(environment),
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
fn windows_native_process(adapter: &Terminals, pane: &Value, harness: &str) -> Result<Value> {
    let id = terminals::identity(pane)?;
    if id.backend != Backend::Herdr {
        return Ok(Value::Null);
    }
    let info = adapter.herdr_result(&id.endpoint, &["pane", "process-info", "--pane", &id.pane])?;
    let rows = info["process_info"]["foreground_processes"]
        .as_array()
        .ok_or("Missing foreground process information")?;
    let mut candidates = Vec::new();
    for row in rows.iter().take(128) {
        let pid = row["pid"]
            .as_u64()
            .and_then(|p| u32::try_from(p).ok())
            .or_else(|| row["pid"].as_str().and_then(|p| p.parse().ok()))
            .ok_or("Invalid foreground PID")?;
        let actual =
            crate::platform::process_identity(pid).map_err(|_| "Foreground process changed")?;
        if actual.owner != crate::platform::current_identity().map_err(|e| e.to_string())? {
            return Err("Foreground process belongs to another account".into());
        }
        if actual
            .executable
            .file_stem()
            .is_some_and(|name| name.eq_ignore_ascii_case(harness))
        {
            candidates.push(actual);
        }
    }
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
                .capture_terminal_context(&f.adapter, &f.pane, &before, "Context")
                .unwrap_err()
                .contains("changed during context capture")
        );
        assert!(f.store.list().unwrap().is_empty());
        assert!(!f.store.directory.join("checkpoints").exists());
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
        let second_key = key(&second).unwrap();
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
    fn own_detection_follows_nested_clients() {
        let pane = |socket: &str, pane: &str, session: &str| json!({"backend":"tmux","socket":socket,"server":socket,"pane_id":pane,"session_id":session,"terminal_id":format!("v2:{pane}")});
        let inside = pane("/inner", "%1", "$1");
        let outer = pane("/outer", "%2", "$2");
        let snapshot = json!({"panes":[inside,outer],"clients":[{"socket":"/inner","session_id":"$1","outer_panes":[{"socket":"/outer","pane_id":"%2"}]}]});
        let environment = BTreeMap::from([
            ("TMUX".into(), "/inner,123,1".into()),
            ("TMUX_PANE".into(), "%1".into()),
        ]);
        assert_eq!(
            own_terminals(&snapshot, &environment, &BTreeSet::new(), &[]).len(),
            2
        );
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
            "\"foreground_processes\":[{\"pid\":90}]",
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
            .register_view(&f.pane, &receipt, "Herdr native client")
            .unwrap();
        assert_eq!(
            f.store
                .live_views(&json!({"panes":[viewer]}))
                .unwrap()
                .len(),
            1
        );
        viewer["terminal_id"] = "replacement".into();
        assert!(
            f.store
                .live_views(&json!({"panes":[viewer]}))
                .unwrap()
                .is_empty()
        );
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
            "\"foreground_processes\":[{\"pid\":90}]",
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
) -> Result<Option<ProcessEvidence>> {
    let pid = pid.parse().map_err(|_| "Invalid foreground process ID")?;
    let actual =
        crate::platform::process_identity(pid).map_err(|_| "Cannot verify foreground process")?;
    if actual.owner != crate::platform::current_identity().map_err(|e| e.to_string())? {
        return Err("Foreground process belongs to another account".into());
    }
    // Windows does not expose supported argv/cwd/environment APIs for another
    // process. Herdr's correlated native session metadata remains available;
    // never invent argv evidence by treating its display label as a command.
    Ok(None)
}

#[cfg(test)]
mod fingerprint_tests {
    use super::*;
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
