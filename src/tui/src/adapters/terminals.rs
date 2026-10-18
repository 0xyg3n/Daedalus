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
}
impl Default for Terminals {
    fn default() -> Self {
        Self {
            tmux: default_mux_binary(),
            herdr: "herdr".into(),
            proc_root: "/proc".into(),
            timeout: Duration::from_secs(5),
        }
    }
}
impl Terminals {
    pub(crate) fn query(&self, executable: &Path, args: &[&str]) -> Result<String> {
        let output =
            process::run_terminal(Command::new(executable).args(args), self.timeout, LIMIT)
                .map_err(|e| format!("Terminal query failed ({:?})", e.kind()))?;
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
        let panes = parse_tmux(&output, socket, |pid| process_start(&self.proc_root, pid))?;
        #[cfg(windows)]
        let panes = windows_harness_labels(panes);
        let format = "#{client_name}\t#{client_pid}\t#{session_id}\t#{window_id}\t#{pane_id}\t#{client_tty}\t#{client_flags}";
        let output = self.query(&self.tmux, &["-S", socket, "list-clients", "-F", format])?;
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
            let fields: Vec<_> = line.split('\t').collect();
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
        Ok(json!({"panes":panes,"clients":clients}))
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
    pub fn herdr_sessions(&self) -> Result<Vec<String>> {
        let text = self.query(&self.herdr, &["session", "list"])?;
        Ok(text
            .lines()
            .filter_map(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                (fields.len() > 1 && fields[1] == "running").then(|| fields[0].to_owned())
            })
            .collect())
    }
    /// Caller revalidates immediately before a mutation. Linked tmux views must
    /// agree on the same terminal generation; a session hint selects the view.
    pub fn verify(&self, saved: &Value) -> Result<Value> {
        let expected = identity(saved)?;
        let candidates = match expected.backend {
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
        _ => return Err("Unknown terminal backend".into()),
    };
    let endpoint = match backend {
        Backend::Tmux => get("socket"),
        Backend::Herdr => {
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
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value = fs::read_to_string(proc_root.join(pid).join("stat")).ok()?;
    let (_, fields) = value.rsplit_once(')')?;
    let start = fields.split_whitespace().nth(19)?;
    (!start.is_empty() && start.bytes().all(|b| b.is_ascii_digit())).then(|| start.to_owned())
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
        let fields: Vec<_> = line.split('\t').collect();
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
        let agent = match row["agent"].as_str().unwrap_or("") {
            "opencode.exe" => "opencode",
            "cursor-agent" => "cursor",
            name => name,
        }
        .to_owned();
        #[cfg(windows)]
        let agent = agent.strip_suffix(".exe").unwrap_or(&agent).to_owned();
        row.insert("agent".into(), agent.into());
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
pub fn normalize_herdr(data: Value, session: &str) -> Result<Value> {
    let data = data.get("snapshot").unwrap_or(&data);
    if !data.get("panes").is_some_and(Value::is_array) {
        return Err("Incomplete Herdr snapshot".into());
    }
    let mut snapshot = Map::new();
    for key in ["panes", "workspaces", "tabs", "clients", "layouts"] {
        let mut rows = Vec::new();
        if let Some(entries) = data.get(key) {
            for entry in entries.as_array().ok_or("Invalid Herdr collection")? {
                let mut row = entry.as_object().cloned().ok_or("Invalid Herdr record")?;
                row.insert("backend".into(), "herdr".into());
                row.insert("session".into(), session.into());
                row.insert("server".into(), session.into());
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
    Ok(Value::Object(snapshot))
}
/// Only owned Unix sockets are discovered automatically. Explicit endpoints
/// remain the operator's configuration and are validated by the adapter.
#[cfg(unix)]
pub fn discover_sockets(configured: &[PathBuf], roots: &[PathBuf], uid: u32) -> Vec<PathBuf> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let mut found: BTreeSet<PathBuf> = configured
        .iter()
        .filter(|p| p.is_absolute())
        .cloned()
        .collect();
    for root in roots {
        if let Ok(entries) = fs::read_dir(root.join(format!("tmux-{uid}"))) {
            for entry in entries.flatten() {
                if let Ok(info) = fs::symlink_metadata(entry.path())
                    && info.file_type().is_socket()
                    && info.uid() == uid
                {
                    found.insert(entry.path());
                }
            }
        }
    }
    found.into_iter().collect()
}
/// Native Windows uses Herdr; tmux endpoints belong to WSL instances.
#[cfg(windows)]
pub fn discover_sockets(configured: &[PathBuf], roots: &[PathBuf], _uid: u32) -> Vec<PathBuf> {
    let mut endpoints: BTreeSet<PathBuf> = configured
        .iter()
        .filter(|p| p.is_absolute())
        .cloned()
        .collect();
    if !roots.is_empty()
        && default_mux_binary()
            .file_stem()
            .is_some_and(|n| n.eq_ignore_ascii_case("psmux"))
    {
        let root = std::env::var_os("PSMUX_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join(".psmux")));
        if let Some(root) =
            root.filter(|p| p.is_absolute() && crate::platform::check_path(p).is_ok())
            && let Ok(entries) = fs::read_dir(&root)
        {
            for entry in entries.take(256).flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Some(stem) = name.strip_suffix(".port") else {
                    continue;
                };
                let namespace = stem.split_once("__").map(|(ns, _)| ns).unwrap_or("default");
                if !namespace.is_empty()
                    && namespace.len() <= 80
                    && namespace
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
                {
                    endpoints.insert(root.join(namespace));
                }
            }
        }
    }
    endpoints.into_iter().collect()
}
#[cfg(windows)]
pub fn process_start(_proc_root: &Path, pid: &str) -> Option<String> {
    let process = crate::platform::process_identity(pid.parse().ok()?).ok()?;
    (process.owner == crate::platform::current_identity().ok()?)
        .then(|| process.creation_time.to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
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
        let rows = parse_tmux(
            &fixture(),
            crate::test_support::path("/tmp/test.sock"),
            |pid| Some(format!("{pid}0")),
        )
        .unwrap();
        assert_eq!(rows[0]["terminal_id"], "v2:11:110:22:220");
        assert_eq!(rows[0]["agent"], "cursor");
        assert_eq!(rows[0]["server"], "tmux@%2Ftmp%2Ftest.sock");
        assert!(
            parse_tmux(
                &fixture(),
                crate::test_support::path("/tmp/test.sock"),
                |_| None
            )
            .unwrap()
            .is_empty()
        );
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

fn default_mux_binary() -> PathBuf {
    if let Some(binary) = std::env::var_os("DAEDALUS_TMUX_BIN")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return binary;
    }
    #[cfg(windows)]
    {
        for name in ["tmux.exe", "psmux.exe"] {
            if let Some(path) = std::env::var_os("PATH").and_then(|paths| {
                std::env::split_paths(&paths)
                    .filter(|p| p.is_absolute())
                    .map(|p| p.join(name))
                    .find(|p| p.is_file())
            }) {
                return path;
            }
        }
    }
    "tmux".into()
}

/// Display-only enrichment for native mux wrappers. Routing retains the exact
/// pane/root incarnation; a friendly harness label never grants hook authority.
#[cfg(windows)]
fn windows_harness_labels(mut panes: Vec<Value>) -> Vec<Value> {
    for pane in &mut panes {
        let coarse = pane["agent"].as_str().unwrap_or("").to_ascii_lowercase();
        if !["powershell", "pwsh", "cmd", "node", "bash", "sh"].contains(&coarse.as_str()) {
            continue;
        }
        let Some(pid) = pane["pid"].as_str().and_then(|p| p.parse().ok()) else {
            continue;
        };
        let Ok(children) = crate::platform::descendant_processes(pid) else {
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
