//! Read-only terminal discovery. Wire rows retain the existing TUI field names.
use super::process;
use crate::core::identity::{Backend, TerminalId};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

type Result<T> = std::result::Result<T, String>;
const LIMIT: usize = 8_000_000;

/// A conversation belongs to its reported harness, never to its terminal ID.
pub fn reported_session(pane: &Value) -> Option<&str> {
    let session = &pane["agent_session"];
    let observed = pane["agent"].as_str()?;
    let reported = session["agent"].as_str()?;
    let same_agent = if let Some(agent) = super::harnesses::canonical_id(observed) {
        super::harnesses::canonical_id(reported) == Some(agent)
    } else {
        // Preserve an exact transport report for an unregistered tool without
        // granting that tool native launch/control capabilities.
        !observed.is_empty() && observed == reported
    };
    (session["kind"] == "id" && same_agent)
        .then(|| session["value"].as_str())?
        .filter(|sid| !sid.is_empty() && !sid.chars().any(char::is_control))
}
fn normalize_agent(pane: &mut Map<String, Value>) {
    if let Some(agent) = pane
        .get("agent")
        .and_then(Value::as_str)
        .and_then(super::harnesses::canonical_id)
    {
        pane.insert("agent".into(), agent.into());
    }
}
/// The final two TMUX fields follow the full socket path, which may have commas.
pub(crate) fn active_tmux_socket(value: &str) -> Option<&str> {
    let socket = value.rsplitn(3, ',').nth(2)?;
    (Path::new(socket).is_absolute() && !socket.chars().any(char::is_control)).then_some(socket)
}
const KEYS: [&str; 21] = [
    "session",
    "session_id",
    "window",
    "window_id",
    "pane_id",
    "pid",
    "agent",
    "active",
    "cwd",
    "server_pid",
    "title",
    "tty",
    "herdr_session",
    "dead",
    "window_index",
    "width",
    "height",
    "window_linked",
    "left",
    "top",
    "session_group",
];
const FORMATS: [&str; 21] = [
    "session_name",
    "session_id",
    "window_name",
    "window_id",
    "pane_id",
    "pane_pid",
    "pane_current_command",
    "pane_active",
    "pane_current_path",
    "pid",
    "pane_title",
    "pane_tty",
    "@herdr_session",
    "pane_dead",
    "window_index",
    "pane_width",
    "pane_height",
    "window_linked",
    "pane_left",
    "pane_top",
    "session_group",
];

#[derive(Clone, Debug)]
pub struct Terminals {
    pub tmux: PathBuf,
    pub herdr: PathBuf,
    pub proc_root: PathBuf,
    pub timeout: Duration,
    pub output_limit: usize,
    pub discovery_budget: Option<std::sync::Arc<crate::app::discovery_budget::Budget>>,
    pub native_metadata_policy: crate::core::native_metadata::Policy,
    pub native_launch_cols: u16,
    pub native_launch_rows: u16,
    pub native_scrollback_rows: u64,
}
impl Default for Terminals {
    fn default() -> Self {
        let settings = crate::app::runtime_settings::Settings::default();
        Self {
            tmux: default_mux_binary(),
            herdr: default_herdr_binary(),
            proc_root: "/proc".into(),
            timeout: Duration::from_millis(settings.terminal_query_timeout_ms),
            output_limit: settings.terminal_query_limit_bytes as usize,
            discovery_budget: None,
            native_metadata_policy: settings.native_metadata_policy(),
            native_launch_cols: settings.native_launch_cols,
            native_launch_rows: settings.native_launch_rows,
            native_scrollback_rows: settings.native_scrollback_rows,
        }
    }
}
impl Terminals {
    pub fn configured(&self, settings: &crate::app::runtime_settings::Settings) -> Result<Self> {
        let mut configured = self.clone();
        configured.apply_settings(settings)?;
        Ok(configured)
    }

    pub fn apply_settings(
        &mut self,
        settings: &crate::app::runtime_settings::Settings,
    ) -> Result<()> {
        settings.validate()?;
        self.timeout = Duration::from_millis(settings.terminal_query_timeout_ms);
        self.output_limit = settings.terminal_query_limit_bytes as usize;
        self.native_metadata_policy = settings.native_metadata_policy();
        self.native_launch_cols = settings.native_launch_cols;
        self.native_launch_rows = settings.native_launch_rows;
        self.native_scrollback_rows = settings.native_scrollback_rows;
        Ok(())
    }

    pub fn metadata_guard(&self) -> Result<crate::core::native_metadata::Guard<'static>> {
        Ok(
            crate::core::native_metadata::Guard::new(self.native_metadata_policy)?
                .with_deadline(self.discovery_budget.as_ref().map(|budget| budget.deadline)),
        )
    }
    pub(crate) fn query(&self, executable: &Path, args: &[&str]) -> Result<String> {
        let timeout = match &self.discovery_budget {
            Some(budget) => self.timeout.min(budget.time_left()?),
            None => self.timeout,
        };
        let limit = self
            .discovery_budget
            .as_ref()
            .map_or(self.output_limit, |budget| {
                self.output_limit.min(budget.remaining_bytes())
            });
        if limit == 0 {
            return Err(self.discovery_budget.as_ref().map_or_else(
                || "Terminal query output limit is zero".into(),
                |budget| budget.capacity_error(),
            ));
        }
        let mut command = Command::new(executable);
        command.args(args);
        if let Some(home) = user_home() {
            command.current_dir(home);
        }
        let output = process::run_terminal(&mut command, timeout, limit).map_err(|error| {
            if let Some(budget) = &self.discovery_budget {
                if let Err(error) = budget.time_left() {
                    return error;
                }
                if error.kind() == std::io::ErrorKind::InvalidData && limit < self.output_limit {
                    return budget.capacity_error();
                }
            }
            format!("Terminal query failed ({:?})", error.kind())
        })?;
        if let Some(budget) = &self.discovery_budget {
            budget.time_left()?;
        }
        if !output.status.success() {
            return Err("Terminal server rejected discovery query".into());
        }
        String::from_utf8(output.stdout)
            .map_err(|_| "Terminal server returned invalid UTF-8".into())
    }
    pub fn tmux_inventory(&self, socket: &str) -> Result<Value> {
        if !Path::new(socket).is_absolute() || socket.chars().any(char::is_control) {
            return Err("tmux socket must be an absolute path".into());
        }
        let format = FORMATS
            .iter()
            .map(|f| format!("#{{{f}}}"))
            .collect::<Vec<_>>()
            .join("\t");
        let output = self.query(
            &self.tmux,
            &["-S", socket, "list-panes", "-a", "-F", &format],
        )?;
        if let Some(budget) = &self.discovery_budget {
            budget.admit_text(
                output.len(),
                output.lines().count().saturating_mul(KEYS.len() * 2 + 1),
            )?;
        }
        let panes = parse_tmux(&output, socket, |pid| process_start(&self.proc_root, pid))?;
        #[cfg(windows)]
        let panes = windows_harness_labels(panes, self);
        let format = "#{client_name}\t#{client_pid}\t#{session_id}\t#{window_id}\t#{pane_id}\t#{client_tty}\t#{client_flags}";
        let output = self.query(&self.tmux, &["-S", socket, "list-clients", "-F", format])?;
        if let Some(budget) = &self.discovery_budget {
            budget.admit_text(output.len(), output.lines().count().saturating_mul(15))?;
        }
        let keys = [
            "client",
            "pid",
            "session_id",
            "window_id",
            "pane_id",
            "tty",
            "flags",
        ];
        let mut clients = Vec::new();
        for line in output.lines() {
            let fields: Vec<_> = line.split('\t').take(keys.len() + 1).collect();
            if fields.len() != keys.len() {
                return Err("Malformed tmux client inventory".into());
            }
            let mut row: Map<String, Value> = keys
                .iter()
                .zip(fields)
                .map(|(k, v)| ((*k).into(), v.into()))
                .collect();
            row.insert("backend".into(), "tmux".into());
            row.insert("socket".into(), socket.into());
            clients.push(Value::Object(row));
        }
        let mut snapshot = json!({"panes":panes,"clients":clients});
        super::pane_observations::normalize(&mut snapshot);
        Ok(snapshot)
    }
    pub fn herdr_result(&self, session: &str, args: &[&str]) -> Result<Value> {
        TerminalId {
            backend: Backend::Herdr,
            endpoint: session.into(),
            pane: "validation".into(),
            generation: "validation".into(),
        }
        .validate()
        .map_err(str::to_owned)?;
        let mut argv = vec!["--session", session];
        argv.extend_from_slice(args);
        let output = self.query(&self.herdr, &argv)?;
        if output.trim().is_empty() {
            return Ok(json!({}));
        }
        if let Some(budget) = &self.discovery_budget {
            budget.admit_json(output.as_bytes())?;
        }
        let value: Value = serde_json::from_str(&output).map_err(|_| "Malformed Herdr response")?;
        if value
            .get("error")
            .is_some_and(|v| !v.is_null() && v != false)
        {
            return Err("Herdr rejected discovery query".into());
        }
        Ok(value.get("result").cloned().unwrap_or(value))
    }
    pub fn herdr_inventory(&self, session: &str) -> Result<Value> {
        normalize_herdr(self.herdr_result(session, &["api", "snapshot"])?, session)
    }
    pub fn herdr_sessions(&self, limit: usize) -> Result<Vec<String>> {
        let text = self.query(&self.herdr, &["session", "list"])?;
        if let Some(budget) = &self.discovery_budget {
            budget.admit_text(text.len(), text.lines().count())?;
        }
        let mut sessions = BTreeSet::new();
        for line in text.lines() {
            let mut fields = line.split_whitespace();
            if let Some(name) = fields.next()
                && fields.next() == Some("running")
            {
                sessions.insert(name.to_owned());
                // Keep one excess unique endpoint so the caller reports truncation.
                if sessions.len() > limit {
                    break;
                }
            }
        }
        Ok(sessions.into_iter().collect())
    }
    /// Caller revalidates immediately before a mutation. Linked tmux views must
    /// agree on the same terminal generation; a session hint selects the view.
    pub fn verify(&self, saved: &Value) -> Result<Value> {
        if super::pane_observations::conflicted(saved) {
            return Err(super::pane_observations::WARNING.into());
        }
        let expected = identity(saved)?;
        let candidates = match expected.backend {
            Backend::Native => vec![super::native::pane(&crate::app::native_terminal::info(
                &expected.endpoint,
                &expected.generation,
            )?)],
            Backend::Tmux => self.tmux_inventory(&expected.endpoint)?["panes"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
            Backend::Herdr => {
                let result =
                    self.herdr_result(&expected.endpoint, &["pane", "get", &expected.pane])?;
                let mut pane = result
                    .get("pane")
                    .and_then(Value::as_object)
                    .cloned()
                    .ok_or("Missing Herdr pane")?;
                pane.insert("backend".into(), "herdr".into());
                pane.insert("session".into(), expected.endpoint.clone().into());
                pane.insert("server".into(), expected.endpoint.clone().into());
                normalize_agent(&mut pane);
                // Herdr reports the pane's shell PID only through process-info;
                // process ancestry checks need it like tmux's pane_pid.
                if pane.get("pid").is_none()
                    && let Ok(info) = self.herdr_result(
                        &expected.endpoint,
                        &["pane", "process-info", "--pane", &expected.pane],
                    )
                    && info["process_info"]["pane_id"].as_str() == Some(expected.pane.as_str())
                    && let Some(pid) = info["process_info"]["shell_pid"]
                        .as_u64()
                        .filter(|pid| *pid > 0)
                {
                    pane.insert("pid".into(), pid.into());
                }
                vec![Value::Object(pane)]
            }
        };
        let same_pane: Vec<_> = candidates
            .iter()
            .filter(|p| p.get("pane_id") == saved.get("pane_id"))
            .collect();
        if same_pane
            .iter()
            .any(|pane| super::pane_observations::conflicted(pane))
        {
            return Err(super::pane_observations::WARNING.into());
        }
        if same_pane.is_empty()
            || same_pane
                .iter()
                .any(|p| identity(p).ok().as_ref() != Some(&expected))
        {
            return Err("Terminal disappeared or changed; refresh and select again".into());
        }
        let selected: Vec<_> = same_pane
            .into_iter()
            .filter(|p| {
                saved.get("session_id").is_none() || p.get("session_id") == saved.get("session_id")
            })
            .collect();
        if selected.is_empty() {
            return Err("Terminal view no longer exists".into());
        }
        Ok(selected[0].clone())
    }
}
pub fn identity(row: &Value) -> Result<TerminalId> {
    let get = |key| {
        row.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let backend = match get("backend").as_str() {
        "tmux" => Backend::Tmux,
        "herdr" => Backend::Herdr,
        "native" => Backend::Native,
        _ => return Err("Unknown terminal backend".into()),
    };
    let endpoint = match backend {
        Backend::Tmux => get("socket"),
        Backend::Herdr | Backend::Native => {
            let session = get("session");
            if session.is_empty() {
                get("server")
            } else {
                session
            }
        }
    };
    let id = TerminalId {
        backend,
        endpoint,
        pane: get("pane_id"),
        generation: get("terminal_id"),
    };
    id.validate().map_err(str::to_owned)?;
    Ok(id)
}
#[cfg(unix)]
pub fn process_start(proc_root: &Path, pid: &str) -> Option<String> {
    let pid = pid.parse::<u32>().ok()?;
    crate::platform::process_start(proc_root, pid).map(|start| start.to_string())
}
/// Shared saved reference producer; display aliases never replace endpoints.
pub fn reference(row: &Value) -> Result<crate::core::layouts::TerminalRef> {
    if super::pane_observations::conflicted(row) {
        return Err(super::pane_observations::WARNING.into());
    }
    let id = identity(row)?;
    let generation = match row.get("generation") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(value)) => value.clone(),
        _ => return Err("Invalid supplemental terminal generation".into()),
    };
    let reference = crate::core::layouts::TerminalRef {
        backend: id.backend,
        server: id.endpoint,
        pane_id: id.pane,
        terminal_id: id.generation,
        generation,
        label: String::new(),
        place: String::new(),
    };
    reference.validate()?;
    Ok(reference)
}
fn encoded(value: &str) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            output.push(byte as char);
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
    }
    output
}
pub fn parse_tmux(
    output: &str,
    socket: &str,
    start: impl Fn(&str) -> Option<String>,
) -> Result<Vec<Value>> {
    if output.len() > LIMIT {
        return Err("Inventory exceeds size limit".into());
    }
    let mut panes = Vec::new();
    for line in output.lines() {
        let fields: Vec<_> = line.split('\t').take(KEYS.len() + 1).collect();
        if fields.len() != KEYS.len() {
            return Err(
                "Malformed tmux inventory; labels must not contain tabs or newlines".into(),
            );
        }
        let mut row: Map<String, Value> = KEYS
            .iter()
            .zip(fields)
            .map(|(k, v)| ((*k).into(), v.into()))
            .collect();
        if row["dead"] == "1" {
            continue;
        }
        let Some(server_start) = start(row["server_pid"].as_str().unwrap_or("")) else {
            continue;
        };
        let Some(pane_start) = start(row["pid"].as_str().unwrap_or("")) else {
            continue;
        };
        let terminal = format!(
            "v2:{}:{}:{}:{}",
            row["server_pid"].as_str().unwrap_or(""),
            server_start,
            row["pid"].as_str().unwrap_or(""),
            pane_start
        );
        normalize_agent(&mut row);
        row.insert("backend".into(), "tmux".into());
        row.insert("socket".into(), socket.into());
        row.insert("server".into(), format!("tmux@{}", encoded(socket)).into());
        row.insert("terminal_id".into(), terminal.into());
        row.insert("identity_verified".into(), true.into());
        let row = Value::Object(row);
        identity(&row)?;
        panes.push(row);
    }
    Ok(panes)
}
pub fn normalize_herdr(mut data: Value, session: &str) -> Result<Value> {
    if let Some(snapshot) = data.get_mut("snapshot") {
        data = snapshot.take();
    }
    let Value::Object(mut data) = data else {
        return Err("Incomplete Herdr snapshot".into());
    };
    if !data.get("panes").is_some_and(Value::is_array) {
        return Err("Incomplete Herdr snapshot".into());
    }
    let mut snapshot = Map::new();
    for key in ["panes", "workspaces", "tabs", "clients", "layouts"] {
        let mut rows = Vec::new();
        if let Some(entries) = data.remove(key) {
            let Value::Array(entries) = entries else {
                return Err("Invalid Herdr collection".into());
            };
            for entry in entries {
                let Value::Object(mut row) = entry else {
                    return Err("Invalid Herdr record".into());
                };
                row.insert("backend".into(), "herdr".into());
                row.insert("session".into(), session.into());
                row.insert("server".into(), session.into());
                if key == "panes" {
                    normalize_agent(&mut row);
                }
                let row = Value::Object(row);
                if key == "panes" {
                    identity(&row)?;
                }
                rows.push(row);
            }
        }
        snapshot.insert(key.into(), rows.into());
    }
    snapshot.insert("herdr_sessions".into(), json!([session]));
    snapshot.insert("herdr_focus".into(),json!([{"session":session,"scope":"server","workspace_id":data.get("focused_workspace_id"),"tab_id":data.get("focused_tab_id"),"pane_id":data.get("focused_pane_id"),"per_client_focus":"unavailable"}]));
    let mut snapshot = Value::Object(snapshot);
    super::pane_observations::normalize(&mut snapshot);
    Ok(snapshot)
}
/// Candidate endpoints plus explicit incompleteness evidence. No socket is removed.
pub struct SocketDiscovery {
    pub paths: Vec<PathBuf>,
    pub errors: Vec<String>,
}
fn scan_socket_directory(
    directory: &Path,
    remaining: &mut usize,
    found: &mut BTreeSet<PathBuf>,
    errors: &mut BTreeSet<&'static str>,
    deadline: Option<std::time::Instant>,
    candidate: impl Fn(fs::DirEntry) -> std::io::Result<Option<PathBuf>>,
) -> bool {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => {
            errors.insert("Cannot enumerate automatic socket directory");
            return true;
        }
    };
    for entry in entries {
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            errors.insert("Automatic socket discovery deadline reached");
            return false;
        }
        if *remaining == 0 {
            errors.insert("Automatic socket scan entry limit reached; raise runtime.discovery_socket_scan_limit or reduce discovery scope");
            return false;
        }
        *remaining -= 1;
        match entry.and_then(&candidate) {
            Ok(Some(path)) => {
                found.insert(path);
            }
            Ok(None) => {}
            Err(_) => {
                errors.insert("Cannot inspect automatic socket directory entry");
            }
        }
    }
    true
}
fn socket_discovery(found: BTreeSet<PathBuf>, errors: BTreeSet<&str>) -> SocketDiscovery {
    SocketDiscovery {
        paths: found.into_iter().collect(),
        errors: errors.into_iter().map(str::to_owned).collect(),
    }
}
#[cfg(test)]
pub fn discover_sockets(
    configured: &[PathBuf],
    roots: &[PathBuf],
    uid: u32,
    scan_limit: usize,
) -> SocketDiscovery {
    discover_sockets_with(configured, roots, uid, scan_limit, None)
}
/// Only owned Unix sockets are discovered automatically. Explicit endpoints
/// remain the operator's configuration and are validated by the adapter.
#[cfg(unix)]
pub(crate) fn discover_sockets_with(
    configured: &[PathBuf],
    roots: &[PathBuf],
    uid: u32,
    scan_limit: usize,
    deadline: Option<std::time::Instant>,
) -> SocketDiscovery {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let mut found: BTreeSet<PathBuf> = configured
        .iter()
        .filter(|p| p.is_absolute())
        .cloned()
        .collect();
    let mut errors = BTreeSet::new();
    let mut remaining = scan_limit;
    for root in roots {
        if !scan_socket_directory(
            &root.join(format!("tmux-{uid}")),
            &mut remaining,
            &mut found,
            &mut errors,
            deadline,
            |entry| {
                let info = fs::symlink_metadata(entry.path())?;
                Ok((info.file_type().is_socket() && info.uid() == uid).then(|| entry.path()))
            },
        ) {
            break;
        }
    }
    socket_discovery(found, errors)
}
/// Native Windows uses Herdr; tmux endpoints belong to WSL instances.
#[cfg(windows)]
pub(crate) fn discover_sockets_with(
    configured: &[PathBuf],
    roots: &[PathBuf],
    _uid: u32,
    scan_limit: usize,
    deadline: Option<std::time::Instant>,
) -> SocketDiscovery {
    let mut endpoints: BTreeSet<PathBuf> = configured
        .iter()
        .filter(|p| p.is_absolute())
        .cloned()
        .collect();
    let mut errors = BTreeSet::new();
    if !roots.is_empty()
        && default_mux_binary()
            .file_stem()
            .is_some_and(|n| n.eq_ignore_ascii_case("psmux"))
    {
        let root = std::env::var_os("PSMUX_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join(".psmux")));
        if let Some(root) = root {
            if root.is_absolute() && crate::platform::check_path(&root).is_ok() {
                let mut remaining = scan_limit;
                scan_socket_directory(
                    &root,
                    &mut remaining,
                    &mut endpoints,
                    &mut errors,
                    deadline,
                    |entry| {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        let Some(stem) = name.strip_suffix(".port") else {
                            return Ok(None);
                        };
                        let namespace =
                            stem.split_once("__").map(|(ns, _)| ns).unwrap_or("default");
                        Ok((!namespace.is_empty()
                            && namespace.len() <= 80
                            && namespace
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)))
                        .then(|| root.join(namespace)))
                    },
                );
            } else {
                errors.insert("Invalid automatic psmux socket directory");
            }
        }
    }
    socket_discovery(endpoints, errors)
}
#[cfg(windows)]
pub fn process_start(_proc_root: &Path, pid: &str) -> Option<String> {
    let process = crate::platform::process_metadata(pid.parse().ok()?).ok()?;
    (process.owner == crate::platform::current_identity().ok()?)
        .then(|| process.creation_time.to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let mut bytes = [0; 16];
            crate::platform::random_bytes(&mut bytes).unwrap();
            let path = std::env::temp_dir()
                .join(format!("dd-terminal-scan-{:x}", u128::from_le_bytes(bytes)));
            crate::core::layouts::write_private(&path.join("fixture"), b"fixture").unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn automatic_scan_counts_all_entries_across_roots_and_preserves_explicit_paths() {
        use std::os::unix::{fs::symlink, net::UnixListener};
        let f = Fixture::new();
        let uid = unsafe { libc::geteuid() };
        let first = f.0.join("first");
        let second = f.0.join("second");
        let directory = first.join(format!("tmux-{uid}"));
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("unrelated-a"), b"unused").unwrap();
        fs::write(directory.join("unrelated-b"), b"unused").unwrap();
        let socket = directory.join("s, a");
        let _listener = UnixListener::bind(&socket).unwrap();
        symlink(&socket, directory.join("linked-socket")).unwrap();
        let other_directory = second.join(format!("tmux-{uid}"));
        fs::create_dir_all(&other_directory).unwrap();
        let other = other_directory.join("s-b");
        let _other_listener = UnixListener::bind(&other).unwrap();
        let explicit = f.0.join("configured offline");
        let roots = [first, second];
        let bounded = discover_sockets(std::slice::from_ref(&explicit), &roots, uid, 4);
        assert_eq!(bounded.paths, [explicit.clone(), socket.clone()]);
        assert_eq!(bounded.errors.len(), 1);
        assert!(bounded.errors[0].contains("entry limit reached"));
        let complete = discover_sockets(std::slice::from_ref(&explicit), &roots, uid, 5);
        assert_eq!(complete.paths, [explicit, socket, other]);
        assert!(complete.errors.is_empty());
        assert!(
            discover_sockets(&[], &[f.0.join("absent")], uid, 1)
                .errors
                .is_empty()
        );
    }
    #[test]
    fn saved_terminal_output_limit_counts_both_streams_and_session_names_are_bounded() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        let executable = f.0.join("query-fixture");
        fs::write(
            &executable,
            "#!/bin/sh\nprintf '%2048s' ''; printf '%1024s' '' >&2\n",
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let mut adapter = Terminals::default();
        let settings = crate::app::runtime_settings::Settings::default()
            .set("terminal_query_limit_bytes", "2048")
            .unwrap();
        adapter.apply_settings(&settings).unwrap();
        assert_eq!(adapter.output_limit, 2048);
        assert!(
            adapter
                .query(&executable, &[])
                .unwrap_err()
                .contains("InvalidData")
        );
        adapter
            .apply_settings(&settings.set("terminal_query_limit_bytes", "3072").unwrap())
            .unwrap();
        assert_eq!(adapter.query(&executable, &[]).unwrap().len(), 2048);
        fs::write(&executable, "#!/bin/sh\nprintf '%s\\n' 'b running' 'a running' 'a running' 'c running' 'd stopped' 'e running'\n").unwrap();
        adapter.herdr = executable;
        assert_eq!(adapter.herdr_sessions(2).unwrap(), ["a", "b", "c"]);
        assert_eq!(adapter.herdr_sessions(4).unwrap(), ["a", "b", "c", "e"]);
    }
    #[test]
    fn saved_terminal_query_deadline_limits_the_owned_subprocess() {
        use std::os::unix::fs::PermissionsExt;
        let mut random = [0; 16];
        crate::platform::random_bytes(&mut random).unwrap();
        let directory = std::env::temp_dir().join(format!(
            "daedalus-query-budget-{:x}",
            u128::from_le_bytes(random)
        ));
        fs::create_dir(&directory).unwrap();
        let executable = directory.join("query");
        fs::write(
            &executable,
            "#!/bin/sh\n/bin/sleep 0.25\nprintf fixture-result\n",
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let mut adapter = Terminals::default();
        let mut settings = crate::app::runtime_settings::Settings {
            terminal_query_timeout_ms: 100,
            ..Default::default()
        };
        adapter.apply_settings(&settings).unwrap();
        assert!(adapter.query(&executable, &[]).is_err());
        settings.terminal_query_timeout_ms = 2000;
        adapter.apply_settings(&settings).unwrap();
        assert_eq!(adapter.query(&executable, &[]).unwrap(), "fixture-result");
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn verify_rejects_reused_herdr_pane_using_mock_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("daedalus-terminal-mock-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let executable = dir.join("mock-herdr");
        fs::write(&executable, "#!/bin/sh\nprintf '%s\\n' '{\"result\":{\"pane\":{\"pane_id\":\"pane\",\"terminal_id\":\"new\"}}}'\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = Terminals {
            herdr: executable,
            ..Terminals::default()
        };
        let mut saved =
            json!({"backend":"herdr","session":"work","pane_id":"pane","terminal_id":"old"});
        assert!(adapter.verify(&saved).is_err());
        saved["terminal_id"] = "new".into();
        assert_eq!(adapter.verify(&saved).unwrap()["terminal_id"], "new");
        assert!(adapter.verify(&saved).unwrap().get("pid").is_none());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn conflicting_fresh_tmux_rows_cannot_submit_input_but_exact_duplicates_recover() {
        use crate::adapters::terminal_actions::{Action, ReceiptState};
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        let proc_root = f.0.join("proc");
        for (pid, start) in [("11", "110"), ("22", "220")] {
            fs::create_dir_all(proc_root.join(pid)).unwrap();
            let mut fields = vec!["0"; 20];
            fields[0] = "S";
            fields[19] = start;
            fs::write(
                proc_root.join(pid).join("stat"),
                format!("{pid} (fixture) {}", fields.join(" ")),
            )
            .unwrap();
        }
        let socket = f.0.join("fixture.sock");
        let socket = socket.to_str().unwrap();
        let original = fixture();
        let mut changed: Vec<_> = original.split('\t').map(str::to_owned).collect();
        changed[15] = "81".into();
        let payload = f.0.join("panes");
        fs::write(&payload, format!("{original}\n{}\n", changed.join("\t"))).unwrap();
        let effects = f.0.join("effects");
        let executable = f.0.join("inert-tmux");
        fs::write(&executable,format!("#!/bin/sh\ncase \"$3\" in\nlist-panes) exec /bin/cat '{}' ;;\nlist-clients) exit 0 ;;\nsend-keys) printf '%s\\n' \"$*\" >> '{}' ;;\n*) exit 1 ;;\nesac\n",payload.display(),effects.display())).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = Terminals {
            tmux: executable,
            proc_root,
            ..Terminals::default()
        };
        let saved = parse_tmux(&original, socket, |pid| Some(format!("{pid}0")))
            .unwrap()
            .remove(0);
        assert!(
            adapter
                .action(&saved, Action::Send("fixture".into()))
                .err()
                .unwrap()
                .contains("Conflicting terminal observations")
        );
        assert!(!effects.exists());
        let mut marked = saved.clone();
        marked["observation_conflict"] = json!(true);
        assert!(adapter.verify(&marked).is_err());
        assert!(reference(&marked).is_err());
        fs::write(&payload, format!("{original}\n{original}\n")).unwrap();
        assert_eq!(
            adapter
                .action(&saved, Action::Send("fixture".into()))
                .unwrap()
                .state,
            ReceiptState::Submitted
        );
        // One submission consists of literal text followed by one Enter command.
        let effects = fs::read_to_string(effects).unwrap();
        assert_eq!(effects.lines().count(), 2);
        assert_eq!(
            effects
                .lines()
                .filter(|line| line.contains(" -l -- fixture"))
                .count(),
            1
        );
        assert_eq!(
            effects
                .lines()
                .filter(|line| line.ends_with(" Enter"))
                .count(),
            1
        );
    }
    #[test]
    fn verify_reads_herdr_shell_pid_from_process_info() {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("daedalus-terminal-pid-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let executable = dir.join("mock-herdr");
        fs::write(&executable, "#!/bin/sh\ncase \"$4\" in\nget) printf '%s\\n' '{\"result\":{\"pane\":{\"pane_id\":\"w1:p2\",\"terminal_id\":\"term\"}}}' ;;\nprocess-info) if [ \"$6\" = w1:p2 ]; then printf '%s\\n' '{\"result\":{\"process_info\":{\"pane_id\":\"w1:p2\",\"shell_pid\":4242}}}'; else printf '%s\\n' '{\"result\":{\"process_info\":{\"pane_id\":\"w1:p9\",\"shell_pid\":7}}}'; fi ;;\nesac\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = Terminals {
            herdr: executable,
            ..Terminals::default()
        };
        let saved =
            json!({"backend":"herdr","session":"work","pane_id":"w1:p2","terminal_id":"term"});
        assert_eq!(adapter.verify(&saved).unwrap()["pid"], 4242);
        fs::remove_dir_all(dir).unwrap();
    }
    fn fixture() -> String {
        [
            "work",
            "$1",
            "shell",
            "@1",
            "%1",
            "22",
            "cursor-agent",
            "1",
            crate::test_support::path("/tmp"),
            "11",
            "Title",
            "/dev/pts/1",
            "",
            "0",
            "0",
            "80",
            "24",
            "0",
            "0",
            "0",
            "",
        ]
        .join("\t")
    }
    #[test]
    fn tmux_generations_are_exact() {
        let socket = crate::test_support::path("/tmp/test.sock");
        let rows = parse_tmux(&fixture(), socket, |pid| Some(format!("{pid}0"))).unwrap();
        assert_eq!(rows[0]["terminal_id"], "v2:11:110:22:220");
        assert_eq!(rows[0]["agent"], "cursor");
        assert_eq!(rows[0]["server"], format!("tmux@{}", encoded(socket)));
        assert!(parse_tmux(&fixture(), socket, |_| None).unwrap().is_empty());
    }
    #[test]
    fn malformed_tmux_is_rejected() {
        assert!(
            parse_tmux(
                "wrong\trow",
                crate::test_support::path("/tmp/socket"),
                |_| Some("1".into())
            )
            .is_err()
        );
    }
    #[test]
    fn herdr_retains_layouts_and_focus() {
        let data = json!({"snapshot":{"panes":[{"pane_id":"pane","terminal_id":"uuid"}],"layouts":[{"tab_id":"tab","root":{}}],"focused_pane_id":"pane"}});
        let result = normalize_herdr(data, "work").unwrap();
        assert_eq!(result["panes"][0]["server"], "work");
        assert_eq!(result["layouts"][0]["tab_id"], "tab");
        assert_eq!(result["herdr_focus"][0]["per_client_focus"], "unavailable");
    }
    #[test]
    fn incomplete_herdr_is_rejected() {
        assert!(normalize_herdr(json!({}), "work").is_err());
        assert!(normalize_herdr(json!({"panes":[{"pane_id":"pane"}]}), "work").is_err());
    }
    #[test]
    fn traversal_pid_is_rejected() {
        assert!(process_start(Path::new(crate::test_support::path("/tmp")), "../etc").is_none());
    }
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(windows)]
    {
        true
    }
}
fn binary_in(name: &str, paths: &[PathBuf], home: Option<&Path>) -> Option<PathBuf> {
    let filename = if cfg!(windows) && Path::new(name).extension().is_none() {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    let known = home.into_iter().filter(|p| p.is_absolute()).flat_map(|p| {
        #[allow(unused_mut)]
        let mut dirs = vec![p.join(".local/bin"), p.join(".cargo/bin")];
        #[cfg(windows)]
        dirs.extend([
            p.join("AppData/Local/Microsoft/WinGet/Links"),
            p.join("AppData/Local/Programs/Herdr"),
            p.join("AppData/Local/Programs/Herdr/bin"),
        ]);
        dirs
    });
    paths
        .iter()
        .filter(|p| p.is_absolute())
        .cloned()
        .chain(known)
        .map(|p| p.join(&filename))
        .find(|p| executable_file(p))
}
fn search_paths() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default()
}
fn user_home() -> Option<PathBuf> {
    let home = if cfg!(windows) {
        std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
    } else {
        std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
    };
    home.map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_dir())
}
fn default_herdr_binary() -> PathBuf {
    if let Some(binary) = std::env::var_os("DAEDALUS_HERDR_BIN")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && executable_file(p))
    {
        return binary;
    }
    let home = user_home();
    binary_in("herdr", &search_paths(), home.as_deref()).unwrap_or_else(|| "herdr".into())
}
/// Optional backends are not probed when absent. Explicit endpoints still query
/// the configured executable so unavailable integrations remain visible errors.
pub(crate) fn binary_available(binary: &Path) -> bool {
    if binary.is_absolute() {
        return executable_file(binary);
    }
    if binary.components().count() != 1 {
        return false;
    }
    binary
        .to_str()
        .and_then(|name| binary_in(name, &search_paths(), None))
        .is_some()
}

fn default_mux_binary() -> PathBuf {
    if let Some(binary) = std::env::var_os("DAEDALUS_TMUX_BIN")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return binary;
    }
    #[cfg(windows)]
    {
        let home = user_home();
        for name in ["tmux", "psmux"] {
            if let Some(path) = binary_in(name, &search_paths(), home.as_deref()) {
                return path;
            }
        }
    }
    "tmux".into()
}

/// Display-only enrichment for native mux wrappers. Routing retains the exact
/// pane/root incarnation; a friendly harness label never grants hook authority.
#[cfg(windows)]
fn windows_harness_labels(mut panes: Vec<Value>, adapter: &Terminals) -> Vec<Value> {
    let progress = || {
        adapter
            .discovery_budget
            .as_ref()
            .map_or(Ok(()), |budget| budget.time_left().map(|_| ()))
    };
    let Ok(guard) = crate::core::native_metadata::Guard::new(adapter.native_metadata_policy) else {
        return panes;
    };
    let mut guard = guard.with_progress(&progress).with_deadline(
        adapter
            .discovery_budget
            .as_ref()
            .map(|budget| budget.deadline),
    );
    for pane in &mut panes {
        let coarse = pane["agent"].as_str().unwrap_or("").to_ascii_lowercase();
        if !["powershell", "pwsh", "cmd", "node", "bash", "sh"].contains(&coarse.as_str()) {
            continue;
        }
        let Some(pid) = pane["pid"].as_str().and_then(|p| p.parse().ok()) else {
            continue;
        };
        let Ok(children) = crate::platform::descendant_processes_budgeted(pid, &mut guard) else {
            continue;
        };
        let candidates = children
            .iter()
            .filter_map(|p| {
                let name = p.executable.file_stem()?.to_str()?.to_ascii_lowercase();
                [
                    "claude",
                    "codex",
                    "grok",
                    "hermes",
                    "opencode",
                    "cursor-agent",
                    "pi",
                    "agy",
                ]
                .contains(&name.as_str())
                .then_some(name)
            })
            .collect::<Vec<_>>();
        if let [name] = candidates.as_slice() {
            pane["agent"] = if name == "cursor-agent" {
                "cursor".into()
            } else {
                name.clone().into()
            };
            pane["agent_detection"] = "windows_process_tree".into();
        }
    }
    panes
}

#[cfg(test)]
mod binary_tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn adapter_queries_use_a_stable_home_instead_of_launch_directory() {
        let Some(home) = user_home() else { return };
        let terminals = Terminals::default();
        let observed = terminals.query(Path::new("/bin/pwd"), &[]).unwrap();
        assert_eq!(Path::new(observed.trim()), home);
    }
    #[test]
    fn resolves_known_user_install_when_path_is_missing() {
        let root = std::env::temp_dir().join(format!(
            "dd-bin-{}-{}",
            std::process::id(),
            crate::test_support::unique_nanos()
        ));
        let directory = root.join(".cargo/bin");
        fs::create_dir_all(&directory).unwrap();
        let name = if cfg!(windows) { "herdr.exe" } else { "herdr" };
        let binary = directory.join(name);
        fs::write(&binary, "fixture; never executed").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(binary_in("herdr", &[], Some(&root)), None);
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert_eq!(binary_in("herdr", &[], Some(&root)), Some(binary.clone()));
        assert!(binary_available(&binary));
        assert_eq!(
            binary_in("herdr", std::slice::from_ref(&directory), None),
            Some(binary)
        );
        assert_eq!(
            binary_in(
                "herdr",
                &[PathBuf::from("relative")],
                Some(Path::new("relative"))
            ),
            None
        );
        assert!(!binary_available(Path::new("./untrusted-herdr")));
        fs::remove_dir_all(root).unwrap();
    }
}
