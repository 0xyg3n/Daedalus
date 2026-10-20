//! Private native hook profile installation. Applying writes the selected profile;
//! providers may load it on restart or reload it immediately (Cursor).
//! Schema: https://code.claude.com/docs/en/hooks (onFailure requires >=2.1.295).
use super::{
    hook_runtime::{self, Options, ProcessIdentity, Registration},
    hooks::{EventKind, Harness},
};
use crate::{
    adapters::terminals::Terminals,
    core::{goal_store::Scope, identity::TerminalId, layouts},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::{
    fd::AsRawFd,
    unix::fs::{MetadataExt, OpenOptionsExt},
};
use std::{
    fs::{self},
    path::{Path, PathBuf},
};
pub type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    #[serde(default = "default_harness")]
    pub harness: Harness,
    pub instance: String,
    pub terminal: TerminalId,
    pub conversation: String,
    pub process: ProcessIdentity,
    #[serde(default)]
    pub server_emitter: Option<hook_runtime::ServerEmitter>,
    pub directory: PathBuf,
    pub profile: PathBuf,
    pub binary: PathBuf,
    pub transcript: Option<PathBuf>,
    pub registration_id: String,
    pub options: Options,
    pub goal_id: Option<String>,
    /// Version already observed by the dependency adapter, not a guessed default.
    #[serde(alias = "claude_version")]
    pub harness_version: String,
}
/// Operator-selected installation destinations; identities are discovered from
/// the verified foreground process and native session metadata by preview_live.
#[derive(Clone, Debug)]
pub struct LiveRequest {
    pub instance: String,
    pub directory: PathBuf,
    pub profile: PathBuf,
    pub binary: PathBuf,
    pub transcript: Option<PathBuf>,
    pub registration_id: String,
    pub options: Options,
    pub goal_id: Option<String>,
    pub harness_version: String,
}

/// Prefer the updater's stable launcher for installed hooks. Codex keeps hook
/// commands in memory for the lifetime of a session; a content-addressed
/// release path makes those commands stale after an update.
pub fn installed_launcher(current: PathBuf) -> PathBuf {
    #[cfg(unix)]
    {
        let Some(home) = std::env::var_os("HOME") else {
            return current;
        };
        installed_launcher_in(current, &PathBuf::from(home))
    }
    #[cfg(not(unix))]
    current
}

#[cfg(unix)]
fn installed_launcher_in(current: PathBuf, home: &Path) -> PathBuf {
    let launcher = home.join(".local/bin/daedalus");
    if layouts::check_path(launcher.parent().expect("launcher parent")).is_ok()
        && std::fs::canonicalize(&launcher).is_ok_and(|target| layouts::check_path(&target).is_ok())
        && std::fs::symlink_metadata(&launcher).is_ok_and(|meta| meta.file_type().is_symlink())
        && std::fs::canonicalize(&current)
            .ok()
            .is_some_and(|running| std::fs::canonicalize(&launcher).ok() == Some(running))
    {
        launcher
    } else {
        current
    }
}
pub fn preview_live(
    request: &LiveRequest,
    pane: &Value,
    config: &super::config::ControlConfig,
    environment: &std::collections::BTreeMap<String, String>,
    terminals: &Terminals,
) -> Result<Preview> {
    let sessions = super::session_actions::SessionStore {
        directory: request.directory.clone(),
    };
    let (checkpoint, frozen) =
        sessions.freeze_live_source(terminals, config, environment, pane, "")?;
    if checkpoint.sid.is_empty() || checkpoint.origin != "Native metadata" {
        if checkpoint.harness == "opencode" {
            return Err("OpenCode has not reported its selected conversation ID. Check the terminal integration, open or reselect the conversation, and refresh before connecting Telegram. A running process alone cannot identify the recipient session.".into());
        }
        return Err(
            "Harness has not reported a native conversation ID; wait for session registration"
                .into(),
        );
    }
    let process = &frozen["_daedalus_handoff_process"];
    let numeric = |key: &str| {
        process[key]
            .as_str()
            .ok_or("Foreground process evidence is missing")
    };
    let harness = serde_json::from_value::<Harness>(json!(checkpoint.harness))
        .map_err(|_| "Native hook adapter is unavailable")?;
    let process = ProcessIdentity {
        pid: numeric("pid")?
            .parse()
            .map_err(|_| "Invalid foreground PID")?,
        start_ticks: numeric("start")?
            .parse()
            .map_err(|_| "Invalid foreground generation")?,
    };
    let server_emitter = if harness == Harness::Opencode {
        Some(
            match local_opencode_server(&process, &terminals.proc_root) {
                Ok(server) => server,
                Err(error) if error == NO_LOCAL_SERVER => reconnect_opencode_server(
                    &request.directory,
                    &crate::adapters::terminals::identity(&frozen)?,
                    &process,
                    &terminals.proc_root,
                )?,
                Err(error) => return Err(error),
            },
        )
    } else {
        None
    };
    let request = Request {
        harness,
        instance: request.instance.clone(),
        terminal: crate::adapters::terminals::identity(&frozen)?,
        conversation: checkpoint.sid,
        process,
        server_emitter,
        directory: request.directory.clone(),
        profile: request.profile.clone(),
        binary: request.binary.clone(),
        transcript: request.transcript.clone(),
        registration_id: request.registration_id.clone(),
        options: request.options.clone(),
        goal_id: request.goal_id.clone(),
        harness_version: request.harness_version.clone(),
    };
    preview(&request, terminals)
}

// A shared/remote server cannot be inferred from a terminal label. Guided
// enrollment supports one native local server directly owned by the selected UI.
// The review pins both process incarnations and the executable's file identity.
#[cfg(windows)]
fn local_opencode_server(
    ui: &ProcessIdentity,
    proc_root: &Path,
) -> Result<hook_runtime::ServerEmitter> {
    hook_runtime::verify_process(ui, proc_root).map_err(|e| e.to_string())?;
    let root = crate::platform::process_identity(ui.pid).map_err(|_| "OpenCode UI disappeared")?;
    let executable = crate::platform::open_regular(&root.executable).map_err(|e| e.to_string())?;
    let identity = crate::platform::file_identity(&executable).map_err(|e| e.to_string())?;
    let children = crate::platform::descendant_processes(ui.pid)
        .map_err(|_| "Cannot verify the OpenCode server process")?;
    let mut servers = Vec::new();
    for child in children.into_iter().filter(|p| p.parent_pid == ui.pid) {
        let current = crate::platform::process_identity(child.pid)
            .map_err(|_| "OpenCode server changed during discovery")?;
        if current.parent_pid != ui.pid || current.creation_time != child.creation_time {
            return Err("OpenCode server ancestry changed during discovery".into());
        }
        let Ok(file) = crate::platform::open_regular(&child.executable) else {
            continue;
        };
        if crate::platform::file_identity(&file).ok() != Some(identity) {
            continue;
        }
        servers.push(
            hook_runtime::capture_server_emitter(
                ProcessIdentity {
                    pid: child.pid,
                    start_ticks: child.creation_time,
                },
                &child.executable,
                proc_root,
            )
            .map_err(|e| e.to_string())?,
        );
    }
    hook_runtime::verify_process(ui, proc_root).map_err(|e| e.to_string())?;
    select_local_server(servers)
}

#[cfg(unix)]
fn local_opencode_server(
    ui: &ProcessIdentity,
    proc_root: &Path,
) -> Result<hook_runtime::ServerEmitter> {
    hook_runtime::verify_process(ui, proc_root).map_err(|e| e.to_string())?;
    let ui_dir = proc_root.join(ui.pid.to_string());
    let executable =
        fs::canonicalize(ui_dir.join("exe")).map_err(|_| "OpenCode UI executable disappeared")?;
    let children = fs::read_to_string(ui_dir.join(format!("task/{}/children", ui.pid)))
        .map_err(|_| "Cannot discover the local OpenCode server")?;
    let mut servers = Vec::new();
    let children: Vec<_> = children.split_whitespace().collect();
    if children.len() > 128 {
        return Err("OpenCode server process tree exceeds limit".into());
    }
    for pid in children {
        let Ok(pid) = pid.parse::<u32>() else {
            return Err("Invalid OpenCode child PID".into());
        };
        let directory = proc_root.join(pid.to_string());
        if fs::canonicalize(directory.join("exe")).ok().as_ref() != Some(&executable) {
            continue;
        }
        let argv =
            fs::read(directory.join("cmdline")).map_err(|_| "OpenCode server disappeared")?;
        if argv.len() > 65536 {
            return Err("OpenCode server arguments exceed limit".into());
        }
        let args: Vec<_> = argv.split(|b| *b == 0).filter(|s| !s.is_empty()).collect();
        if !args
            .windows(2)
            .any(|a| a == [b"serve".as_slice(), b"--service".as_slice()])
        {
            continue;
        }
        let stat = fs::read_to_string(directory.join("stat"))
            .map_err(|_| "OpenCode server disappeared")?;
        if stat.len() > 8192
            || stat
                .rsplit_once(')')
                .and_then(|(_, tail)| tail.split_whitespace().nth(1))
                .and_then(|p| p.parse::<u32>().ok())
                != Some(ui.pid)
        {
            return Err("OpenCode server ancestry changed during discovery".into());
        }
        let start = crate::adapters::terminals::process_start(proc_root, &pid.to_string())
            .and_then(|s| s.parse().ok())
            .ok_or("OpenCode server generation unavailable")?;
        servers.push(
            hook_runtime::capture_server_emitter(
                ProcessIdentity {
                    pid,
                    start_ticks: start,
                },
                &executable,
                proc_root,
            )
            .map_err(|e| e.to_string())?,
        );
    }
    hook_runtime::verify_process(ui, proc_root).map_err(|e| e.to_string())?;
    select_local_server(servers)
}
fn select_local_server(
    mut servers: Vec<hook_runtime::ServerEmitter>,
) -> Result<hook_runtime::ServerEmitter> {
    if servers.is_empty() {
        return Err(NO_LOCAL_SERVER.into());
    }
    if servers.len() != 1 {
        return Err("Cannot identify one local OpenCode server for this terminal. Shared or remote servers need an explicit server binding through hook profile-preview; no hook was installed.".into());
    }
    Ok(servers.pop().unwrap())
}
const NO_LOCAL_SERVER: &str = "No directly owned OpenCode server was found. Open a conversation and reconnect its hooks; an existing server needs a verified saved binding.";

/// A background server can outlive the UI that first started it. Reconnect only
/// a previously reviewed emitter for this exact terminal generation, never an
/// arbitrary process with the same executable name or the latest saved session.
fn reconnect_opencode_server(
    directory: &Path,
    terminal: &TerminalId,
    ui: &ProcessIdentity,
    proc_root: &Path,
) -> Result<hook_runtime::ServerEmitter> {
    hook_runtime::verify_process(ui, proc_root).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    let executable = crate::platform::process_identity(ui.pid)
        .map_err(|_| "OpenCode UI disappeared")?
        .executable;
    #[cfg(unix)]
    let executable = fs::read_link(proc_root.join(ui.pid.to_string()).join("exe"))
        .map_err(|_| "OpenCode UI disappeared")?;
    let entries = fs::read_dir(directory.join("hooks")).map_err(|_| NO_LOCAL_SERVER)?;
    let mut servers = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= 4096 {
            return Err("Hook registration limit exceeded".into());
        }
        let path = entry
            .map_err(|_| "Cannot inspect saved hook binding")?
            .path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let r = hook_runtime::inspect(&path).map_err(|e| e.to_string())?;
        if r.directory != directory
            || r.scope.harness != Harness::Opencode
            || &r.scope.terminal != terminal
        {
            continue;
        }
        let Some(server) = r.server_emitter else {
            continue;
        };
        if hook_runtime::verify_server_emitter(&server, proc_root).is_err() {
            continue;
        }
        // Pin the current UI to the very same native executable as the emitter.
        #[cfg(unix)]
        let matches = fs::metadata(&executable)
            .is_ok_and(|m| m.dev() == server.device && m.ino() == server.inode);
        #[cfg(windows)]
        let matches = crate::platform::open_regular(&executable)
            .ok()
            .and_then(|f| crate::platform::file_identity(&f).ok())
            == Some((server.device, server.inode));
        if matches && !servers.contains(&server) {
            servers.push(server);
        }
    }
    hook_runtime::verify_process(ui, proc_root).map_err(|e| e.to_string())?;
    select_local_server(servers)
}

pub(crate) fn check_binary_path(path: &Path) -> Result<()> {
    if let Err(error) = layouts::check_path(path) {
        // The installer owns this one update-stable launcher. State/profile symlinks
        // remain forbidden, and the launcher must resolve to this running binary.
        let current = std::env::current_exe().map_err(|_| "Cannot locate hook executable")?;
        if installed_launcher(current) != path {
            return Err(error);
        }
        layouts::check_path(
            &std::fs::canonicalize(path).map_err(|_| "Hook launcher target disappeared")?,
        )?;
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
pub struct Preview {
    pub profile: PathBuf,
    pub executable: PathBuf,
    pub registration: Registration,
    pub before_sha: Option<String>,
    pub after_sha: String,
    #[serde(skip_serializing)]
    before: Option<Vec<u8>>,
    #[serde(skip_serializing)]
    after: Vec<u8>,
    #[serde(skip_serializing)]
    previous_registration: Option<Registration>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub profile: PathBuf,
    pub backup: PathBuf,
    pub after_sha: String,
    pub registration: PathBuf,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
fn hermes_quote(value: &str) -> Result<String> {
    #[cfg(windows)]
    {
        // Hermes uses shlex(posix=False) and removes matching outer quotes on
        // Windows. POSIX apostrophe escaping does not describe that grammar.
        if value.contains('"') || value.chars().any(char::is_control) {
            return Err("Invalid Windows Hermes hook argument".into());
        }
        Ok(format!("\"{value}\""))
    }
    #[cfg(unix)]
    Ok(quote(value))
}
/// Claude's documented command+args form invokes the native executable directly.
/// Keep registration paths as argv even when they contain shell metacharacters.
fn direct_hook_command(handler: &mut Value, binary: &str, registration: &str, event: &str) {
    handler["command"] = binary.into();
    handler["args"] = json!([
        "hook",
        "run",
        "--registration",
        registration,
        "--event-kind",
        event
    ]);
}
fn default_harness() -> Harness {
    Harness::Claude
}
fn version_triple(version: &str) -> Option<(u32, u32, u32)> {
    let parts = version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .ok()?;
    if parts.len() != 3 {
        return None;
    }
    Some((parts[0], parts[1], parts[2]))
}
pub(crate) fn grok_readiness_supported(version: &str) -> bool {
    version_triple(version)
        .is_some_and(|(major, minor, patch)| major == 1 && minor == 0 && patch >= 52)
}
fn supported(version: &str) -> bool {
    let numbers = version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>();
    numbers.is_ok_and(|n| n.len() == 3 && (n[0], n[1], n[2]) >= (2, 1, 295))
}
/// Explicit terminal/process/conversation supplied by trusted discovery. Neither
/// labels nor a profile name can grant authority to another running session.
pub fn preview(request: &Request, terminals: &Terminals) -> Result<Preview> {
    #[cfg(windows)]
    if !matches!(
        request.harness,
        Harness::Claude | Harness::Pi | Harness::Opencode | Harness::Hermes
    ) {
        return Err("Native Windows hook enrollment currently supports direct-argv Claude, Pi, OpenCode and Hermes bridges; this provider's Windows shell hook contract is not verified".into());
    }
    #[cfg(windows)]
    if request.harness == Harness::Claude
        && version_triple(&request.harness_version).is_none_or(|v| v < (2, 1, 296))
    {
        return Err(
            "Windows Claude direct-argv hooks require an observed version >=2.1.296".into(),
        );
    }
    if request.harness == Harness::Grok
        && request
            .transcript
            .as_ref()
            .is_some_and(|path| path.file_name().is_none_or(|name| name != "updates.jsonl"))
    {
        return Err("Grok hook transcripts must point to this session's updates.jsonl, not events.jsonl. Select updates.jsonl or omit the transcript; event evidence is resolved separately.".into());
    }
    if request.harness == Harness::Pi
        && !request
            .profile
            .extension()
            .and_then(|v| v.to_str())
            .is_some_and(|v| matches!(v, "js" | "ts"))
    {
        return Err("Pi extensions require a .js or .ts file".into());
    }
    if request.harness == Harness::Opencode {
        let version = request
            .harness_version
            .trim()
            .trim_start_matches('v')
            .split('.')
            .map(str::parse::<u32>)
            .collect::<std::result::Result<Vec<_>, _>>();
        if !version.is_ok_and(|v| v.len() == 3 && v[0] == 2 && (v[1], v[2]) >= (0, 28)) {
            return Err("OpenCode bridge requires an observed 2.x version >=2.0.28".into());
        }
        if !request
            .profile
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|s| matches!(s, "js" | "ts" | "mjs"))
            || request.transcript.is_some()
        {
            return Err("OpenCode requires a dedicated native plugin file and uses canonical context, not a transcript path".into());
        }
        if request.options.notifications {
            return Err("OpenCode notification hooks are not implemented".into());
        }
    }
    // Pi 0.87.0 introduced the actionable settle boundary. Older versions may
    // accept an unknown event handler without ever invoking it.
    if request.harness == Harness::Pi {
        let parts = request
            .harness_version
            .trim()
            .trim_start_matches('v')
            .split('.')
            .map(str::parse::<u32>)
            .collect::<std::result::Result<Vec<_>, _>>();
        if !parts.is_ok_and(|v| {
            v.len() == 3 && ((v[0] == 0 && (v[1], v[2]) >= (87, 0)) || (v[0] == 1 && v[1] == 1))
        }) {
            return Err("Pi hook bridge requires verified 0.x >=0.87.0 or 1.1.x API".into());
        }
    }
    if request.harness != Harness::Claude && request.harness_version.trim().is_empty() {
        return Err("An observed harness version is required".into());
    }
    if request.harness == Harness::Grok
        && request.goal_id.is_some()
        && version_triple(&request.harness_version) != Some((1, 0, 46))
        && !grok_readiness_supported(&request.harness_version)
    {
        return Err(
            "Grok goal hooks require verified version 1.0.46 or 1.0.52+ in the 1.0 series".into(),
        );
    }
    if request.harness == Harness::Claude && !supported(&request.harness_version) {
        return Err("Claude hook profiles require an observed version >=2.1.295".into());
    }
    for path in [&request.directory, &request.profile] {
        layouts::check_path(path)?;
    }
    check_binary_path(&request.binary)?;
    if let Some(path) = &request.transcript {
        layouts::check_path(path)?;
    }
    if request.profile == request.binary || request.transcript.as_ref() == Some(&request.profile) {
        return Err("Profile path conflicts with a runtime input".into());
    }
    #[cfg(unix)]
    {
        let meta = fs::metadata(&request.binary).map_err(|_| "Native binary does not exist")?;
        if !meta.is_file() || meta.mode() & 0o111 == 0 || meta.mode() & 0o022 != 0 {
            return Err("Native binary must be a non-writable executable".into());
        }
    }
    #[cfg(windows)]
    {
        use std::io::Read;
        let mut file = crate::platform::open_regular(&request.binary)
            .map_err(|_| "Native binary does not exist or has an unsafe path")?;
        let mut magic = [0; 2];
        file.read_exact(&mut magic)
            .map_err(|_| "Cannot read native binary")?;
        if magic != *b"MZ"
            || !request
                .binary
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("exe"))
        {
            return Err("Windows hook handlers require the native Daedalus .exe".into());
        }
    }
    let mut registration = Registration {
        version: 1,
        id: request.registration_id.clone(),
        directory: request.directory.clone(),
        scope: Scope {
            instance: request.instance.clone(),
            terminal: request.terminal.clone(),
            conversation: request.conversation.clone(),
            harness: request.harness,
        },
        goal_id: request.goal_id.clone(),
        harness_process: Some(request.process.clone()),
        server_emitter: request.server_emitter.clone(),
        transcript: request.transcript.clone(),
        events: vec![
            EventKind::SessionStart,
            EventKind::UserPromptSubmit,
            EventKind::PostToolUse,
            EventKind::Stop,
            EventKind::SessionEnd,
        ],
        options: request.options.clone(),
        activity_lease_ms: 30000,
        pending_timeout_ms: 30000,
        hook_timeout_ms: 12000,
    };
    if matches!(
        request.harness,
        Harness::Cursor | Harness::Pi | Harness::Opencode
    ) {
        registration.events.push(EventKind::AssistantResponse);
    }
    if request.harness == Harness::Pi {
        registration
            .events
            .extend([EventKind::Activity, EventKind::Settled]);
        if request.options.notifications {
            return Err("Pi subagent notifications are not implemented".into());
        }
    }
    if request.options.hindsight && !matches!(request.harness, Harness::Hermes | Harness::Pi) {
        registration.events.push(EventKind::PreCompact);
    }
    if request.harness == Harness::Grok
        && grok_readiness_supported(&request.harness_version)
        && (request.options.telegram_delivery || request.options.telegram_progress)
    {
        registration.events.push(EventKind::Settled);
    }
    if request.options.notifications {
        registration.events.push(EventKind::SubagentStop);
        if matches!(request.harness, Harness::Claude | Harness::Grok) {
            registration.events.push(EventKind::Notification);
        }
    }
    registration.validate().map_err(|e| e.to_string())?;
    hook_runtime::verify_live_session(&registration, terminals)
        .map_err(|_| "Selected session/process is not live in the foreground")?;
    let parent = request.profile.parent().ok_or("Profile has no parent")?;
    #[cfg(unix)]
    {
        let parent_meta = fs::metadata(parent)
            .map_err(|_| "Create the private profile directory before preview")?;
        if !parent_meta.is_dir()
            || parent_meta.uid() != unsafe { libc::geteuid() }
            || parent_meta.mode() & 0o077 != 0
        {
            return Err("Profile directory must be private and owned".into());
        }
    }
    #[cfg(windows)]
    crate::platform::validate_native_profile_dir(parent)
        .map_err(|_| "Profile directory must be private and owned")?;
    let before = read_profile(&request.profile)?;
    let after = merge(before.as_deref(), &registration, &request.binary)?;
    let previous_registration = if registration.path().exists() {
        Some(hook_runtime::inspect(&registration.path()).map_err(|e| e.to_string())?)
    } else {
        None
    };
    Ok(Preview {
        previous_registration,
        profile: request.profile.clone(),
        executable: request.binary.clone(),
        registration,
        before_sha: before.as_deref().map(hash),
        after_sha: hash(&after),
        before,
        after,
    })
}

/// Build a dedicated additive Claude plugin without inventing a live process.
pub(crate) fn launch_hooks(
    registration: &Registration,
    binary: &Path,
    version: &str,
) -> Result<Vec<u8>> {
    registration.validate().map_err(|e| e.to_string())?;
    let supported_provider = match registration.scope.harness {
        Harness::Claude => supported(version),
        Harness::Grok => {
            version_triple(version) == Some((1, 0, 46)) || grok_readiness_supported(version)
        }
        Harness::Pi => {
            version_triple(version).is_some_and(|(major, minor, _)| major == 1 && minor == 1)
        }
        _ => false,
    };
    if registration.harness_process.is_some() || !supported_provider {
        return Err("First-launch hooks require Claude >=2.1.295, Pi 1.1.x or Grok 1.0.46 and an unbound template".into());
    }
    if registration.scope.harness == Harness::Grok
        && registration.events.contains(&EventKind::Settled)
        && !grok_readiness_supported(version)
    {
        return Err("Grok idle readiness requires verified 1.0.52+ in the 1.0 series".into());
    }
    let bytes = merge(None, registration, binary)?;
    if registration.scope.harness != Harness::Grok {
        return Ok(bytes);
    }
    // Native runtime overwrites GROK_SESSION_ID after handler env. Other
    // conversations never invoke this registration or touch its databases.
    let mut value: Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid generated hooks")?;
    for groups in value["hooks"]
        .as_object_mut()
        .ok_or("Invalid hooks")?
        .values_mut()
    {
        for group in groups.as_array_mut().ok_or("Invalid groups")? {
            for handler in group["hooks"].as_array_mut().ok_or("Invalid handlers")? {
                let command = handler["command"].as_str().ok_or("Invalid command")?;
                handler["command"] = format!(
                    "[ \"${{GROK_SESSION_ID:-}}\" = {} ] || exit 0; {command}",
                    quote(&registration.scope.conversation)
                )
                .into();
            }
        }
    }
    serde_json::to_vec_pretty(&value).map_err(|_| "Cannot encode Grok hooks".into())
}

pub(crate) fn merge(
    before: Option<&[u8]>,
    registration: &Registration,
    binary: &Path,
) -> Result<Vec<u8>> {
    if registration.scope.harness == Harness::Opencode {
        let marker = format!(
            "// Daedalus managed OpenCode bridge v2: {}\n",
            registration.id
        );
        if before.is_some_and(|bytes| !bytes.starts_with(marker.as_bytes())) {
            return Err("Choose a new plugin file; unmanaged code is preserved".into());
        }
        return super::hook_bridges::opencode(registration, binary);
    }
    if registration.scope.harness == Harness::Pi {
        let marker = format!("// Daedalus managed Pi bridge v1: {}\n", registration.id);
        if before.is_some_and(|bytes| !bytes.starts_with(marker.as_bytes())) {
            return Err("Choose a new extension file; existing unmanaged code is preserved".into());
        }
        return super::hook_bridges::pi(registration, binary);
    }
    if registration.scope.harness == Harness::Cursor {
        return merge_cursor(before, registration, binary);
    }
    if registration.scope.harness == Harness::Hermes {
        return merge_hermes(before, registration, binary);
    }
    let mut settings = match before {
        Some(bytes) => {
            serde_json::from_slice::<Value>(bytes).map_err(|_| "Profile is not valid JSON")?
        }
        None => json!({}),
    };
    let root = settings
        .as_object_mut()
        .ok_or("Profile must be a JSON object")?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("Profile hooks must be an object")?;
    let marker = format!("Daedalus managed hook: {}", registration.id);
    // Remove only marked handlers, preserving other handlers in their matcher.
    for groups in hooks.values_mut() {
        for group in groups
            .as_array_mut()
            .ok_or("Hook event must contain matcher groups")?
        {
            let handlers = group
                .get_mut("hooks")
                .and_then(Value::as_array_mut)
                .ok_or("Matcher group lacks hooks")?;
            handlers.retain(|h| {
                h.get("statusMessage").and_then(Value::as_str) != Some(&marker)
                    && !(registration.scope.harness == Harness::Grok
                        && h.get("command")
                            .and_then(Value::as_str)
                            .is_some_and(|c| c.ends_with(&format!(" # {marker}"))))
            });
        }
        groups
            .as_array_mut()
            .unwrap()
            .retain(|g| g["hooks"].as_array().is_none_or(|a| !a.is_empty()));
    }
    let binary = binary.to_str().ok_or("Binary path is not UTF-8")?;
    let path = registration.path();
    let path = path.to_str().ok_or("Registration path is not UTF-8")?;
    for event in &registration.events {
        let name = serde_json::to_value(event)
            .map_err(|_| "Cannot encode hook event")?
            .as_str()
            .ok_or("Invalid hook event")?
            .to_owned();
        let command = format!(
            "{} hook run --registration {} --event-kind {}",
            quote(binary),
            quote(path),
            quote(&name)
        );
        let timeout =
            if registration.scope.harness == Harness::Codex && *event == EventKind::SessionEnd {
                3
            } else {
                15
            };
        let mut handler =
            json!({"type":"command","command":command,"timeout":timeout,"statusMessage":marker});
        if cfg!(windows) && registration.scope.harness == Harness::Claude {
            direct_hook_command(&mut handler, binary, path, &name);
        }
        if registration.scope.harness == Harness::Grok {
            // Grok documents command/type/timeout, not statusMessage. A shell
            // comment identifies managed commands without an unknown JSON key.
            handler.as_object_mut().unwrap().remove("statusMessage");
            handler["command"] = format!("{command} # {marker}").into();
        }
        if registration.scope.harness == Harness::Claude
            && matches!(event, EventKind::UserPromptSubmit)
        {
            handler["onFailure"] = "block".into();
        }
        let grok_idle = registration.scope.harness == Harness::Grok && *event == EventKind::Settled;
        let native_event = if grok_idle {
            "Notification".to_owned()
        } else {
            name
        };
        let group = if grok_idle {
            json!({"matcher":"idle_prompt","hooks":[handler]})
        } else {
            json!({"hooks":[handler]})
        };
        hooks
            .entry(native_event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("Invalid event hooks")?
            .push(group);
    }
    serde_json::to_vec_pretty(&settings).map_err(|_| "Cannot encode profile".into())
}
/// Uses YAML Value, not a JSON conversion, to preserve tags, merge mappings and
/// scalar types. Formatting/comments may change; rollback restores exact bytes.
fn merge_hermes(
    before: Option<&[u8]>,
    registration: &Registration,
    binary: &Path,
) -> Result<Vec<u8>> {
    use yaml_serde::{Mapping, Value as Y};
    let mut value: Y = match before {
        Some(bytes) => yaml_serde::from_slice(bytes).map_err(|_| "Profile is not valid YAML")?,
        None => Y::Mapping(Mapping::new()),
    };
    let root = value
        .as_mapping_mut()
        .ok_or("Hermes profile must be a mapping")?;
    let hooks = root
        .entry(Y::String("hooks".into()))
        .or_insert_with(|| Y::Mapping(Mapping::new()))
        .as_mapping_mut()
        .ok_or("Hermes hooks must be a mapping")?;
    let marker = Y::String("daedalus_managed".into());
    for handlers in hooks.values_mut() {
        // Outbound webhook configuration is distinct from executable hook lists.
        if let Some(rows) = handlers.as_sequence_mut() {
            rows.retain(|row| row.get(&marker).and_then(Y::as_str) != Some(&registration.id));
        }
    }
    for event in &registration.events {
        let native = match event {
            EventKind::SessionStart => "on_session_start",
            EventKind::SessionEnd => "on_session_finalize",
            EventKind::UserPromptSubmit => "pre_llm_call",
            EventKind::PostToolUse => "post_tool_call",
            EventKind::Stop => "post_llm_call",
            EventKind::SubagentStop => "subagent_stop",
            _ => return Err("Unsupported Hermes shell event".into()),
        };
        let canonical = serde_json::to_value(event).map_err(|_| "Cannot encode event")?;
        // Use the native Hermes argv grammar; hooks never run through a shell.
        let command = format!(
            "{} hook run --registration {} --event-kind {}",
            hermes_quote(binary.to_str().ok_or("Binary path is not UTF-8")?)?,
            hermes_quote(
                registration
                    .path()
                    .to_str()
                    .ok_or("Registration path is not UTF-8")?
            )?,
            hermes_quote(canonical.as_str().ok_or("Invalid event")?)?
        );
        let handler = yaml_serde::to_value(
            json!({"command":command,"timeout":15,"daedalus_managed":registration.id}),
        )
        .map_err(|_| "Cannot encode Hermes handler")?;
        hooks
            .entry(Y::String(native.into()))
            .or_insert_with(|| Y::Sequence(vec![]))
            .as_sequence_mut()
            .ok_or("Hermes hook event must contain handlers")?
            .push(handler);
    }
    yaml_serde::to_string(&value)
        .map(String::into_bytes)
        .map_err(|_| "Cannot encode Hermes profile".into())
}

fn merge_cursor(
    before: Option<&[u8]>,
    registration: &Registration,
    binary: &Path,
) -> Result<Vec<u8>> {
    let mut value: Value = match before {
        Some(bytes) => serde_json::from_slice(bytes).map_err(|_| "Profile is not valid JSON")?,
        None => json!({"version":1}),
    };
    if value.get("version") != Some(&json!(1)) {
        return Err("Cursor hook profile must use version 1".into());
    }
    let root = value.as_object_mut().ok_or("Invalid Cursor profile")?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("Invalid Cursor hooks")?;
    let marker = format!(" # Daedalus managed hook: {}", registration.id);
    for handlers in hooks.values_mut() {
        handlers
            .as_array_mut()
            .ok_or("Cursor event must contain handlers")?
            .retain(|h| {
                !h.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|c| c.ends_with(&marker))
            });
    }
    for event in &registration.events {
        let native = match event {
            EventKind::SessionStart => "sessionStart",
            EventKind::SessionEnd => "sessionEnd",
            EventKind::UserPromptSubmit => "beforeSubmitPrompt",
            EventKind::PreToolUse => "preToolUse",
            EventKind::PostToolUse => "postToolUse",
            EventKind::Stop => "stop",
            EventKind::PreCompact => "preCompact",
            EventKind::SubagentStop => "subagentStop",
            EventKind::AssistantResponse => "afterAgentResponse",
            _ => return Err("Unsupported Cursor event".into()),
        };
        let canonical = serde_json::to_value(event).map_err(|_| "Cannot encode event")?;
        let command = format!(
            "{} hook run --registration {} --event-kind {}{marker}",
            quote(binary.to_str().ok_or("Binary path is not UTF-8")?),
            quote(
                registration
                    .path()
                    .to_str()
                    .ok_or("Registration path is not UTF-8")?
            ),
            quote(canonical.as_str().ok_or("Invalid event")?)
        );
        let mut handler = json!({"type":"command","command":command,"timeout":15});
        if *event == EventKind::UserPromptSubmit {
            handler["failClosed"] = true.into();
        }
        // Preserve Cursor's native default continuation cap; do not disable it.
        hooks
            .entry(native)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("Invalid Cursor handlers")?
            .push(handler);
    }
    serde_json::to_vec_pretty(&value).map_err(|_| "Cannot encode Cursor profile".into())
}

pub(crate) struct InstallationLock(std::fs::File);
impl Drop for InstallationLock {
    fn drop(&mut self) {
        #[cfg(windows)]
        let _ = crate::platform::unlock(&self.0);
        // Explicit release also releases inherited duplicate descriptors.
        #[cfg(unix)]
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
#[cfg(unix)]
pub(crate) fn lock(directory: &Path) -> Result<InstallationLock> {
    layouts::check_path(directory)?;
    let meta = fs::metadata(directory).map_err(|_| "Private installation directory is missing")?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err("Installation directory must be private and owned".into());
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.join("hook-install.lock"))
        .map_err(|_| "Cannot lock installation")?;
    let meta = file
        .metadata()
        .map_err(|_| "Cannot inspect installation lock")?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
    {
        return Err("Unsafe installation lock".into());
    }
    // Brief concurrent writers may finish within this bounded wait.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        if !matches!(
            error.raw_os_error(),
            Some(libc::EWOULDBLOCK) | Some(libc::EINTR)
        ) || std::time::Instant::now() >= deadline
        {
            return Err("Another hook installation is active".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    Ok(InstallationLock(file))
}
#[cfg(windows)]
pub(crate) fn lock(directory: &Path) -> Result<InstallationLock> {
    crate::platform::validate_private_dir(directory)
        .map_err(|_| "Installation directory must be private and owned")?;
    let file = crate::platform::open_private(&directory.join("hook-install.lock"), true, true)
        .map_err(|_| "Cannot open installation lock")?;
    crate::platform::lock_exclusive(&file, true)
        .map_err(|_| "Another hook installation is active")?;
    Ok(InstallationLock(file))
}
// Native provider files may also grant access to Windows SYSTEM and
// Administrators. This boundary never applies to Daedalus registrations,
// credentials, receipts or backups, which remain strictly owner-only.
#[cfg(unix)]
pub(crate) fn lock_profile(directory: &Path) -> Result<InstallationLock> {
    lock(directory)
}
#[cfg(windows)]
pub(crate) fn lock_profile(directory: &Path) -> Result<InstallationLock> {
    crate::platform::validate_native_profile_dir(directory)
        .map_err(|_| "Unsafe native profile directory")?;
    let file =
        crate::platform::open_native_profile(&directory.join("hook-install.lock"), true, true)
            .map_err(|_| "Cannot open profile lock")?;
    crate::platform::lock_exclusive(&file, true)
        .map_err(|_| "Another hook installation is active")?;
    Ok(InstallationLock(file))
}
#[cfg(unix)]
pub(crate) fn read_profile(path: &Path) -> Result<Option<Vec<u8>>> {
    layouts::read_private(path)
}
#[cfg(windows)]
pub(crate) fn read_profile(path: &Path) -> Result<Option<Vec<u8>>> {
    use std::io::Read;
    let file =
        match crate::platform::open_native_profile(path, false, false) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(
                "Native profile must belong to the current user and exclude ordinary shared access"
                    .into(),
            ),
        };
    const LIMIT: u64 = 8 * 1024 * 1024;
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read native profile")?;
    if bytes.len() as u64 > LIMIT {
        return Err("Native profile exceeds size limit".into());
    }
    Ok(Some(bytes))
}
#[cfg(unix)]
pub(crate) fn write_profile(path: &Path, bytes: &[u8]) -> Result<()> {
    layouts::write_private(path, bytes)
}
#[cfg(windows)]
pub(crate) fn write_profile(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::{
        io::Write,
        sync::atomic::{AtomicU64, Ordering},
    };
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("Native profile exceeds size limit".into());
    }
    let parent = path.parent().ok_or("Profile has no parent")?;
    crate::platform::validate_native_profile_dir(parent)
        .map_err(|_| "Unsafe native profile directory")?;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let temporary = parent.join(format!(
        ".daedalus-hook-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = crate::platform::create_native_profile_new(&temporary)
        .map_err(|_| "Cannot create native profile replacement")?;
    let result = file.write_all(bytes).and_then(|_| file.sync_all());
    drop(file);
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
        return Err("Cannot save native profile replacement".into());
    }
    let result = crate::platform::replace_native_profile(&temporary, path)
        .map_err(|_| "Cannot replace native profile".into());
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
/// Apply the reviewed bytes only if the profile and exact live session still
/// match. Backup precedes mutation. A provider may reload the written profile
/// immediately; only registration creation by itself is inert.
pub fn apply(plan: &Preview, terminals: &Terminals) -> Result<Receipt> {
    check_binary_path(&plan.executable)?;
    let _active =
        hook_runtime::active_guard(&plan.registration.directory).map_err(|e| e.to_string())?;
    let _lock = lock(&plan.registration.directory)?;
    hook_runtime::verify_live_session(&plan.registration, terminals)
        .map_err(|_| "Session changed since preview")?;
    let parent = plan.profile.parent().ok_or("Profile has no parent")?;
    let _profile_lock = if parent != plan.registration.directory {
        Some(lock_profile(parent)?)
    } else {
        None
    };
    let current = read_profile(&plan.profile)?;
    let already_applied = current.as_deref() == Some(plan.after.as_slice());
    if current != plan.before && !already_applied {
        return Err("Profile changed since preview; review again".into());
    }
    let backup = plan
        .registration
        .directory
        .join("hook-backups")
        .join(format!("{}-{}.json", plan.registration.id, plan.after_sha));
    if already_applied && let Some(saved) = layouts::read_private(&backup)? {
        let saved: Value =
            serde_json::from_slice(&saved).map_err(|_| "Invalid installation backup")?;
        if saved["profile"] != json!(plan.profile) || saved["after_sha"] != plan.after_sha {
            return Err("Backup does not match installation".into());
        }
        let registration = install_reviewed_registration(plan, terminals)?;
        let receipt = Receipt {
            profile: plan.profile.clone(),
            backup,
            after_sha: plan.after_sha.clone(),
            registration,
        };
        persist_receipt(&receipt)?;
        return Ok(receipt);
    }
    let record = json!({"profile":plan.profile,"before":plan.before,"after_sha":plan.after_sha});
    let bytes = serde_json::to_vec(&record).map_err(|_| "Cannot encode backup")?;
    if let Some(old) = layouts::read_private(&backup)? {
        if old != bytes {
            return Err("Backup conflicts with an existing installation".into());
        }
    } else {
        layouts::write_private(&backup, &bytes)?;
    }
    let registration = install_reviewed_registration(plan, terminals)?;
    write_profile(&plan.profile, &plan.after)?;
    let receipt = Receipt {
        profile: plan.profile.clone(),
        backup,
        after_sha: plan.after_sha.clone(),
        registration,
    };
    persist_receipt(&receipt)?;
    Ok(receipt)
}
fn install_reviewed_registration(plan: &Preview, terminals: &Terminals) -> Result<PathBuf> {
    if let Some(previous) = &plan.previous_registration
        && serde_json::to_value(previous).map_err(|_| "Invalid saved binding")?
            != serde_json::to_value(&plan.registration).map_err(|_| "Invalid new binding")?
    {
        return hook_runtime::rebind_reviewed_process(previous, &plan.registration, terminals)
            .map_err(|e| e.to_string());
    }
    hook_runtime::install(&plan.registration).map_err(|e| e.to_string())
}
fn persist_receipt(receipt: &Receipt) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(receipt)
        .map_err(|_| "Cannot encode managed installation receipt")?;
    layouts::write_private(&receipt.backup.with_extension("receipt.json"),&bytes)
        .map_err(|_|"Profile applied but managed receipt could not be saved; inspect installation before retrying".into())
}
/// Restore only an unchanged managed profile. The inert registration remains for
/// audit and can be reused; no process or session is stopped or started.
pub fn rollback(receipt: &Receipt) -> Result<()> {
    let registration = hook_runtime::inspect(&receipt.registration).map_err(|e| e.to_string())?;
    let _active = hook_runtime::active_guard(&registration.directory).map_err(|e| e.to_string())?;
    let _lock = lock(&registration.directory)?;
    let parent = receipt.profile.parent().ok_or("Profile has no parent")?;
    let _profile_lock = if parent != registration.directory {
        Some(lock_profile(parent)?)
    } else {
        None
    };
    let expected = registration
        .directory
        .join("hook-backups")
        .join(format!("{}-{}.json", registration.id, receipt.after_sha));
    if receipt.backup != expected {
        return Err("Backup path does not match installation".into());
    }
    let bytes = layouts::read_private(&receipt.backup)?.ok_or("Installation backup is missing")?;
    let backup: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid installation backup")?;
    if backup["profile"] != json!(receipt.profile) || backup["after_sha"] != receipt.after_sha {
        return Err("Backup does not match installed profile".into());
    }
    let current = read_profile(&receipt.profile)?.ok_or("Installed profile is missing")?;
    if hash(&current) != receipt.after_sha {
        return Err("Profile changed after install; preserve edits and review manually".into());
    }
    let before: Option<Vec<u8>> =
        serde_json::from_value(backup["before"].clone()).map_err(|_| "Invalid saved profile")?;
    match before {
        Some(bytes) => write_profile(&receipt.profile, &bytes)?,
        None => fs::remove_file(&receipt.profile).map_err(|_| "Cannot remove managed profile")?,
    };
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::core::identity::Backend;
    use std::{
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    #[test]
    fn guided_opencode_server_rejects_missing_and_ambiguous_bindings() {
        let server = hook_runtime::ServerEmitter {
            process: ProcessIdentity {
                pid: 7,
                start_ticks: 90,
            },
            executable: "/fixture/opencode".into(),
            device: 1,
            inode: 2,
        };
        assert!(select_local_server(vec![]).is_err());
        assert!(select_local_server(vec![server.clone(), server.clone()]).is_err());
        assert_eq!(select_local_server(vec![server.clone()]).unwrap(), server);
    }
    #[test]
    fn guided_opencode_server_pins_native_local_child_and_rejects_other_commands() {
        let f = Fixture::new();
        let proc = &f.terminals.proc_root;
        let executable = f.dir.join("opencode");
        fs::write(&executable, b"\x7fELFfixture").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir_all(proc.join("42/task/42")).unwrap();
        fs::write(proc.join("42/task/42/children"), b"43").unwrap();
        fs::create_dir(proc.join("43")).unwrap();
        std::os::unix::fs::symlink(&executable, proc.join("42/exe")).unwrap();
        std::os::unix::fs::symlink(&executable, proc.join("43/exe")).unwrap();
        let mut fields = vec!["0"; 20];
        fields[0] = "S";
        fields[1] = "42";
        fields[19] = "600";
        fs::write(
            proc.join("43/stat"),
            format!("43 (opencode) {}", fields.join(" ")),
        )
        .unwrap();
        fs::write(proc.join("43/cmdline"), b"opencode\0serve\0--service\0").unwrap();
        let server = local_opencode_server(&f.request.process, proc).unwrap();
        assert_eq!(
            server.process,
            ProcessIdentity {
                pid: 43,
                start_ticks: 600
            }
        );
        let mut registration = preview(&f.request, &f.terminals).unwrap().registration;
        registration.scope.harness = Harness::Opencode;
        registration.server_emitter = Some(server.clone());
        registration.transcript = None;
        registration.goal_id = None;
        let path = hook_runtime::install(&registration).unwrap();
        hook_runtime::ready_snapshot(&path, proc).unwrap();
        let con = rusqlite::Connection::open(f.dir.join("hook-observations.sqlite3")).unwrap();
        let scope = serde_json::to_string(&registration.scope).unwrap();
        con.execute("INSERT INTO hook_observations(scope,event_id,payload,state,observed_at,finalized) VALUES(?,'old-idle','fixture','\"idle\"',0,1)", [&scope]).unwrap();
        con.execute(
            "INSERT INTO hook_reservations(scope,correlation) VALUES(?,'uncertain-old-input')",
            [&scope],
        )
        .unwrap();
        // The old UI exits; a new incarnation reconnects to its surviving server.
        let stat = fs::read_to_string(proc.join("42/stat")).unwrap();
        fs::write(proc.join("42/stat"), stat.replace("500", "501")).unwrap();
        fs::write(proc.join("42/task/42/children"), b"").unwrap();
        let next_ui = ProcessIdentity {
            pid: 42,
            start_ticks: 501,
        };
        let rebound_server =
            reconnect_opencode_server(&f.dir, &registration.scope.terminal, &next_ui, proc)
                .unwrap();
        assert_eq!(rebound_server, server);
        let mut foreign = registration.scope.terminal.clone();
        foreign.generation = "different".into();
        assert!(reconnect_opencode_server(&f.dir, &foreign, &next_ui, proc).is_err());
        let mut next = registration.clone();
        next.harness_process = Some(next_ui.clone());
        let mut changed_policy = next.clone();
        changed_policy.scope.conversation = "another-session".into();
        assert!(
            hook_runtime::rebind_reviewed_process(&registration, &changed_policy, &f.terminals)
                .is_err()
        );
        hook_runtime::rebind_reviewed_process(&registration, &next, &f.terminals).unwrap();
        assert_eq!(
            hook_runtime::ready_snapshot(&path, proc).unwrap().state,
            hook_runtime::Readiness::Unknown
        );
        assert_eq!(
            con.query_row(
                "SELECT correlation FROM hook_reservations WHERE scope=?",
                [&scope],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "uncertain-old-input"
        );
        assert!(hook_runtime::rebind_reviewed_process(&registration, &next, &f.terminals).is_err());
        // A recycled server PID cannot be recovered from a saved binding.
        let stat = fs::read_to_string(proc.join("43/stat")).unwrap();
        fs::write(proc.join("43/stat"), stat.replace("600", "601")).unwrap();
        assert!(reconnect_opencode_server(&f.dir, &next.scope.terminal, &next_ui, proc).is_err());
        fs::write(proc.join("43/cmdline"), b"opencode\0run\0").unwrap();
        assert!(local_opencode_server(&f.request.process, proc).is_err());
        assert!(
            local_opencode_server(
                &ProcessIdentity {
                    pid: 42,
                    start_ticks: 501
                },
                proc
            )
            .is_err()
        );
    }
    struct Fixture {
        dir: PathBuf,
        request: Request,
        terminals: Terminals,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "daedalus-hook-install-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&dir).unwrap();
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
            let proc = dir.join("proc");
            fs::create_dir_all(proc.join("42")).unwrap();
            let mut fields = vec!["0"; 20];
            fields[0] = "S";
            fields[2] = "42";
            fields[4] = "99";
            fields[5] = "42";
            fields[19] = "500";
            fs::write(
                proc.join("42/stat"),
                format!("42 (harness) {}", fields.join(" ")),
            )
            .unwrap();
            let executable = dir.join("herdr");
            fs::write(&executable,"#!/bin/sh\nprintf '%s\\n' '{\"result\":{\"pane\":{\"pane_id\":\"pane\",\"terminal_id\":\"terminal\",\"pid\":42}}}'\n").unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
            let request = Request {
                harness: Harness::Claude,
                instance: "example".into(),
                terminal: TerminalId {
                    backend: Backend::Herdr,
                    endpoint: "example".into(),
                    pane: "pane".into(),
                    generation: "terminal".into(),
                },
                conversation: "session".into(),
                process: ProcessIdentity {
                    pid: 42,
                    start_ticks: 500,
                },
                server_emitter: None,
                directory: dir.clone(),
                profile: dir.join("settings.local.json"),
                binary: executable.clone(),
                transcript: Some(dir.join("transcript.jsonl")),
                registration_id: "session-hook".into(),
                options: Options::default(),
                goal_id: None,
                harness_version: "2.1.295".into(),
            };
            Self {
                dir,
                request,
                terminals: Terminals {
                    herdr: executable,
                    proc_root: proc,
                    ..Terminals::default()
                },
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
    #[test]
    fn updater_launcher_keeps_hook_command_stable() {
        let f = Fixture::new();
        let bin = f.dir.join("release/daedalus");
        let launcher = f.dir.join(".local/bin/daedalus");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        fs::write(&bin, b"fixture").unwrap();
        std::os::unix::fs::symlink(&bin, &launcher).unwrap();
        assert_eq!(installed_launcher_in(bin.clone(), &f.dir), launcher);
        assert_eq!(
            installed_launcher_in(f.request.binary.clone(), &f.dir),
            f.request.binary
        );
    }
    #[test]
    fn direct_windows_hook_preserves_paths_as_argv() {
        let mut handler = json!({"type":"command","timeout":15,"statusMessage":"managed"});
        let binary = r"C:\Program Files\Daedalus\daedalus.exe";
        let registration = r"C:\Users\A & B\%private%\hooks.json";
        direct_hook_command(&mut handler, binary, registration, "Stop");
        assert_eq!(handler["command"], binary);
        assert_eq!(handler["args"][3], registration);
        assert_eq!(handler["args"].as_array().unwrap().len(), 6);
        assert!(handler.get("shell").is_none());
        assert_eq!(handler["timeout"], 15);
    }
    #[test]
    fn preview_preserves_existing_hooks_apply_backups_and_rollback_restores_exact_bytes() {
        let f = Fixture::new();
        let original=br#"{"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"user-script"}]}]}}"#;
        layouts::write_private(&f.request.profile, original).unwrap();
        let plan = preview(&f.request, &f.terminals).unwrap();
        assert_eq!(fs::read(&f.request.profile).unwrap(), original);
        let value: Value = serde_json::from_slice(&plan.after).unwrap();
        let public = serde_json::to_string(&plan).unwrap();
        assert!(!public.contains("user-script"));
        assert!(!public.contains("\"before\""));
        assert_eq!(value["theme"], "dark");
        assert_eq!(
            value["hooks"]["Stop"][0]["hooks"][0]["command"],
            "user-script"
        );
        assert!(
            value["hooks"]["Stop"][1]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("--event-kind 'Stop'")
        );
        assert_eq!(
            value["hooks"]["UserPromptSubmit"][0]["hooks"][0]["onFailure"],
            "block"
        );
        assert!(value["hooks"].get("PreToolUse").is_none());
        let receipt = apply(&plan, &f.terminals).unwrap();
        assert!(receipt.backup.exists());
        assert_eq!(apply(&plan, &f.terminals).unwrap().backup, receipt.backup);
        let repeated = preview(&f.request, &f.terminals).unwrap();
        assert_eq!(
            apply(&repeated, &f.terminals).unwrap().backup,
            receipt.backup
        );
        rollback(&receipt).unwrap();
        assert_eq!(fs::read(&f.request.profile).unwrap(), original);
    }
    #[test]
    fn changed_profile_or_process_refuses_and_rollback_preserves_later_user_edits() {
        let f = Fixture::new();
        let plan = preview(&f.request, &f.terminals).unwrap();
        layouts::write_private(&f.request.profile, b"{}").unwrap();
        assert!(apply(&plan, &f.terminals).is_err());
        let plan = preview(&f.request, &f.terminals).unwrap();
        let receipt = apply(&plan, &f.terminals).unwrap();
        layouts::write_private(&f.request.profile, b"{\"new\":true}").unwrap();
        assert!(rollback(&receipt).is_err());
        assert_eq!(fs::read(&f.request.profile).unwrap(), b"{\"new\":true}");
        fs::write(f.terminals.proc_root.join("42/stat"), "42 (gone) X 0").unwrap();
        assert!(preview(&f.request, &f.terminals).is_err());
    }
    #[test]
    fn profile_cli_preview_apply_and_private_receipt_rollback_work() {
        let f = Fixture::new();
        let r = &f.request;
        let mut args = vec!["profile-preview".to_owned()];
        for (k, v) in [
            ("--harness", "claude".to_owned()),
            ("--instance", r.instance.clone()),
            ("--backend", "herdr".into()),
            ("--endpoint", r.terminal.endpoint.clone()),
            ("--pane", r.terminal.pane.clone()),
            ("--generation", r.terminal.generation.clone()),
            ("--conversation", r.conversation.clone()),
            ("--pid", "42".into()),
            ("--process-start", "500".into()),
            ("--directory", r.directory.display().to_string()),
            ("--profile", r.profile.display().to_string()),
            ("--binary", r.binary.display().to_string()),
            (
                "--transcript",
                r.transcript.as_ref().unwrap().display().to_string(),
            ),
            ("--registration-id", r.registration_id.clone()),
            ("--version", r.harness_version.clone()),
        ] {
            args.push(k.into());
            args.push(v);
        }
        let mut output = Vec::new();
        super::super::hook_cli::profile(&args, &mut output, &f.terminals).unwrap();
        let preview: Value = serde_json::from_slice(&output).unwrap();
        assert!(preview.get("before").is_none());
        assert!(preview.get("after").is_none());
        args[0] = "profile-apply".into();
        assert!(super::super::hook_cli::profile(&args, &mut Vec::new(), &f.terminals).is_err());
        args.extend(["--expect-sha".into(), "changed".into()]);
        assert!(super::super::hook_cli::profile(&args, &mut Vec::new(), &f.terminals).is_err());
        *args.last_mut().unwrap() = preview["after_sha"].as_str().unwrap().into();
        output.clear();
        super::super::hook_cli::profile(&args, &mut output, &f.terminals).unwrap();
        let receipt: Value = serde_json::from_slice(&output).unwrap();
        let path = PathBuf::from(receipt["receipt"].as_str().unwrap());
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        super::super::hook_cli::profile(
            &[
                "profile-rollback".into(),
                "--from".into(),
                path.display().to_string(),
            ],
            &mut Vec::new(),
            &f.terminals,
        )
        .unwrap();
        assert!(!r.profile.exists());
    }
    #[test]
    fn live_bootstrap_derives_exact_process_and_native_conversation() {
        let f = Fixture::new();
        let proc = f.terminals.proc_root.join("42");
        fs::write(proc.join("cmdline"), b"claude\0--resume\0session\0").unwrap();
        fs::write(
            proc.join("environ"),
            format!("CLAUDE_CONFIG_DIR={}\0", f.dir.display()),
        )
        .unwrap();
        std::os::unix::fs::symlink(&f.dir, proc.join("cwd")).unwrap();
        let pane = json!({"backend":"herdr","server":"example","pane_id":"pane","terminal_id":"terminal","pid":42,"agent":"claude","agent_session":{"kind":"id","agent":"claude","value":"session"},"cwd":f.dir});
        let script = format!(
            "#!/bin/sh\ncase \"$4\" in\nprocess-info) printf '%s\\n' '{{\"result\":{{\"process_info\":{{\"foreground_processes\":[{{\"pid\":42}}]}}}}}}' ;;\n*) printf '%s\\n' '{}' ;;\nesac\n",
            json!({"result":{"pane":pane}})
        );
        fs::write(&f.terminals.herdr, script).unwrap();
        let request = LiveRequest {
            instance: f.request.instance.clone(),
            directory: f.dir.clone(),
            profile: f.request.profile.clone(),
            binary: f.request.binary.clone(),
            transcript: f.request.transcript.clone(),
            registration_id: "live".into(),
            options: Options::default(),
            goal_id: None,
            harness_version: "2.1.295".into(),
        };
        let config = super::super::config::ControlConfig::fresh(&f.dir).unwrap();
        let p = preview_live(
            &request,
            &pane,
            &config,
            &std::collections::BTreeMap::new(),
            &f.terminals,
        )
        .unwrap();
        assert_eq!(p.registration.scope.conversation, "session");
        assert_eq!(
            p.registration.harness_process,
            Some(ProcessIdentity {
                pid: 42,
                start_ticks: 500
            })
        );
        fs::write(proc.join("cmdline"), b"sh\0").unwrap();
        assert!(
            preview_live(
                &request,
                &pane,
                &config,
                &std::collections::BTreeMap::new(),
                &f.terminals
            )
            .is_err()
        );
    }
    #[test]
    fn pi_bridge_uses_native_settlement_and_stdin_transport_without_shell() {
        let mut f = Fixture::new();
        f.request.harness = Harness::Pi;
        f.request.goal_id = None;
        f.request.options = Options::default();
        f.request.harness_version = "0.87.0".into();
        f.request.profile = f.dir.join("daedalus.js");
        f.request.harness_version = "0.86.9".into();
        assert!(preview(&f.request, &f.terminals).is_err());
        f.request.harness_version = "0.87.0".into();
        let fake = f.dir.join("fake-daedalus");
        let log = f.dir.join("events.jsonl");
        fs::write(&fake,format!("#!/usr/bin/env node\nconst fs=require('node:fs'); const p=JSON.parse(fs.readFileSync(0,'utf8'));fs.appendFileSync({},JSON.stringify(p)+'\\n');process.stdout.write(JSON.stringify(p.hook_event_name==='Stop'?{{continue:true,entries:[]}}:p.hook_event_name==='SessionStart'?{{additional_context:'reference memory'}}:{{}}));\n",json!(log))).unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
        f.request.binary = fake;
        let plan = preview(&f.request, &f.terminals).unwrap();
        let generated = String::from_utf8(plan.after.clone()).unwrap();
        assert!(!generated.contains("pi.on('agent_end'"));
        let receipt = apply(&plan, &f.terminals).unwrap();
        let runner = f.dir.join("fixture.mjs");
        fs::write(
            &runner,
            format!(
                r#"import bridge from {};
const handlers={{}};bridge({{on:(name,handler)=>{{handlers[name]=handler;}}}});
const ctx={{sessionManager:{{getSessionId:()=> 'session',getSessionFile:()=>undefined}}}};
handlers.session_start({{}},ctx);
const foreign=handlers.before_agent_start({{messageType:'user',prompt:'foreign'}},{{sessionManager:{{getSessionId:()=> 'other'}}}});
if(foreign!==undefined)throw new Error('cross-session memory');
const started=handlers.before_agent_start({{messageType:'user',prompt:'$(touch should-not-exist)'}},ctx);
if(started.message.content!=='reference memory')throw new Error('missing reference memory');
handlers.turn_start({{}},ctx);
handlers.tool_execution_end({{toolName:'read'}},ctx);
handlers.message_end({{message:{{role:'assistant',content:[{{type:'text',text:'answer'}}]}}}},ctx);
const response=handlers.agent_before_settle({{outcome:'completed',entries:[{{type:'custom_message',customType:'existing',content:'preserve',display:false}}]}},ctx);
if(response.continue!==true)throw new Error('missing continuation');
if(response.entries[0].customType!=='existing')throw new Error('discarded prior extension entries');
handlers.agent_settled({{aborted:false}},ctx);
handlers.session_shutdown({{}},ctx);
handlers.session_start({{}},{{sessionManager:{{getSessionId:()=> 'other'}}}});
"#,
                json!(f.request.profile)
            ),
        )
        .unwrap();
        let result = std::process::Command::new("node")
            .arg(&runner)
            .current_dir(&f.dir)
            .output()
            .expect("Node is required for the native Pi bridge fixture");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let events = fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(events.len(), 8);
        assert_eq!(events[1]["prompt"], "$(touch should-not-exist)");
        assert_eq!(events[6]["hook_event_name"], "Settled");
        assert_eq!(events[1]["turn_id"], events[6]["turn_id"]);
        assert!(!f.dir.join("should-not-exist").exists());
        rollback(&receipt).unwrap();
        assert!(!f.request.profile.exists());
        layouts::write_private(&f.request.profile, b"export default function custom() {};")
            .unwrap();
        assert!(preview(&f.request, &f.terminals).is_err());
    }
    #[test]
    fn installation_guard_unlocks_inherited_descriptor() {
        use std::os::fd::FromRawFd;
        let f = Fixture::new();
        let held = lock(&f.request.directory).unwrap();
        let descriptor = unsafe { libc::fcntl(held.0.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        assert!(descriptor >= 0);
        let inherited = unsafe { std::fs::File::from_raw_fd(descriptor) };
        drop(held);
        let next = lock(&f.request.directory).unwrap();
        drop(inherited);
        drop(next);
    }
    #[test]
    fn installation_waits_for_brief_descriptor_retention_and_rollback_obeys_cutover() {
        let f = Fixture::new();
        let held = lock(&f.request.directory).unwrap();
        let child = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            drop(held);
        });
        drop(lock(&f.request.directory).unwrap());
        child.join().unwrap();
        let plan = preview(&f.request, &f.terminals).unwrap();
        let receipt = apply(&plan, &f.terminals).unwrap();
        let cutover = OpenOptions::new()
            .read(true)
            .write(true)
            .open(f.request.directory.join(".cutover.lock"))
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while unsafe { libc::flock(cutover.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "fixture lock retained"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(rollback(&receipt).is_err());
        assert_eq!(fs::read(&receipt.profile).unwrap(), plan.after);
        #[cfg(unix)]
        unsafe {
            libc::flock(cutover.as_raw_fd(), libc::LOCK_UN);
        }
        drop(cutover);
        rollback(&receipt).unwrap();
    }
    #[test]
    fn opencode_server_emitter_is_distinct_from_ui_and_never_fakes_idle() {
        use hook_runtime::{NativeVerifier, Readiness, Verifier};
        let f = Fixture::new();
        let mut registration = preview(&f.request, &f.terminals).unwrap().registration;
        registration.goal_id = None;
        registration.scope.harness = Harness::Opencode;
        registration.events = vec![
            EventKind::SessionStart,
            EventKind::UserPromptSubmit,
            EventKind::AssistantResponse,
            EventKind::Stop,
        ];
        registration.transcript = None;
        let server_file = f.dir.join("opencode-server");
        fs::write(&server_file, b"\x7fELF fixture only").unwrap();
        fs::set_permissions(&server_file, fs::Permissions::from_mode(0o700)).unwrap();
        let server_dir = f.terminals.proc_root.join("333");
        fs::create_dir(&server_dir).unwrap();
        let mut fields = vec!["0"; 20];
        fields[0] = "S";
        fields[1] = "1";
        fields[19] = "800";
        fs::write(
            server_dir.join("stat"),
            format!("333 (server) {}", fields.join(" ")),
        )
        .unwrap();
        std::os::unix::fs::symlink(&server_file, server_dir.join("exe")).unwrap();
        registration.server_emitter = Some(
            hook_runtime::capture_server_emitter(
                ProcessIdentity {
                    pid: 333,
                    start_ticks: 800,
                },
                &server_file,
                &f.terminals.proc_root,
            )
            .unwrap(),
        );
        let hook_dir = f.terminals.proc_root.join(std::process::id().to_string());
        fs::create_dir(&hook_dir).unwrap();
        fields[1] = "333";
        fields[19] = "900";
        fs::write(
            hook_dir.join("stat"),
            format!("{} (hook) {}", std::process::id(), fields.join(" ")),
        )
        .unwrap();
        let mut verifier = NativeVerifier {
            terminals: f.terminals.clone(),
        };
        verifier.verify(&registration).unwrap();
        let mut request = f.request.clone();
        request.registration_id = "opencode-profile-test".into();
        request.harness = Harness::Opencode;
        request.harness_version = "2.0.28".into();
        request.profile = f.dir.join("daedalus-opencode.mjs");
        request.transcript = None;
        request.goal_id = None;
        request.options = Options::default();
        request.server_emitter = registration.server_emitter.clone();
        // The receiver must drain stdin before exiting. Reusing the Herdr
        // discovery fixture races spawnSync's pipe writer and causes EPIPE.
        let events_path = f.dir.join("opencode-events.jsonl");
        request.binary = f.dir.join("fake-hook-receiver");
        fs::write(&request.binary, format!("#!/usr/bin/env node\nconst fs=require('node:fs');const value=JSON.parse(fs.readFileSync(0,'utf8'));fs.appendFileSync({},JSON.stringify(value)+'\\n');process.stdout.write(JSON.stringify(value.hook_event_name==='SessionStart'?{{additional_context:'reference memory'}}:{{}}));\n", json!(events_path))).unwrap();
        fs::set_permissions(&request.binary, fs::Permissions::from_mode(0o700)).unwrap();
        let plan = preview(&request, &f.terminals).unwrap();
        assert!(
            String::from_utf8(plan.after.clone())
                .unwrap()
                .contains("ctx.tool.hook('execute.after'")
        );
        assert!(
            plan.registration
                .events
                .contains(&EventKind::AssistantResponse)
        );
        let bridge = f.dir.join("fixture-opencode.mjs");
        let generated = String::from_utf8(plan.after.clone()).unwrap().replace(
            "import { Plugin } from '@opencode/plugin';",
            "const Plugin={define:value=>value};",
        );
        fs::write(&bridge, generated).unwrap();
        let runner = f.dir.join("opencode-test.mjs");
        fs::write(&runner, format!(r#"import plugin from {};
const handlers={{}};let disposed=0;
const ctx={{session:{{get:async()=>({{id:'session'}}),context:async()=>[],hook:async(n,h)=>{{handlers[n]=h;return{{dispose:async()=>{{disposed++}}}};}}}},
tool:{{hook:async(n,h)=>{{handlers[n]=h;return{{dispose:async()=>{{disposed++}}}};}}}},
event:{{async *subscribe(){{yield{{id:'evt_other',type:'session.text.ended',data:{{sessionID:'other',text:'ignored'}}}};yield{{id:'evt_answer',type:'session.text.ended',data:{{sessionID:'session',text:'answer'}}}};}}}}}};
const cleanup=await plugin.setup(ctx);
const current={{sessionID:'session',system:[{{type:'text',text:'existing'}}]}};
const foreign={{sessionID:'other',system:[]}};
await handlers.context(current);await handlers.context(foreign);
if(current.system.length!==2||current.system[1].text!=='reference memory'||foreign.system.length!==0)throw new Error('memory context scope or preservation');
await handlers.prompt({{sessionID:'session',messageID:'msg_1',prompt:{{text:'test'}}}});
await handlers['execute.after']({{sessionID:'session',id:'call_1',tool:'read'}});
await new Promise(resolve=>setTimeout(resolve,10));await cleanup();
if(disposed!==3)throw new Error('hook cleanup missing');
"#,json!(bridge))).unwrap();
        let output = std::process::Command::new("node")
            .arg(&runner)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let events: Vec<Value> = fs::read_to_string(&events_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events.len(), 6);
        assert!(events.iter().all(|event| event["session_id"] == "session"));
        for expected in [
            "SessionStart",
            "UserPromptSubmit",
            "PostToolUse",
            "AssistantResponse",
            "SessionEnd",
        ] {
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event["hook_event_name"] == expected)
                    .count(),
                if expected == "SessionEnd" { 2 } else { 1 }
            );
        }
        assert_eq!(
            events
                .iter()
                .find(|event| event["hook_event_name"] == "AssistantResponse")
                .unwrap()["text"],
            "answer"
        );
        let receipt = apply(&plan, &f.terminals).unwrap();
        assert_eq!(preview(&request, &f.terminals).unwrap().after, plan.after);
        rollback(&receipt).unwrap();
        assert!(!request.profile.exists());
        request.harness_version = "1.9.0".into();
        assert!(preview(&request, &f.terminals).is_err());
        let path = hook_runtime::install(&registration).unwrap();
        for event in ["UserPromptSubmit", "Stop"] {
            let payload=serde_json::to_vec(&json!({"session_id":"session","hook_event_name":event,"bridge_version":"daedalus.opencode.v2","event_id":event})).unwrap();
            hook_runtime::execute(
                payload.as_slice(),
                &registration,
                None,
                &mut verifier,
                &mut hook_runtime::SystemClock,
            )
            .unwrap();
        }
        assert_eq!(
            hook_runtime::ready_snapshot(&path, &f.terminals.proc_root)
                .unwrap()
                .state,
            Readiness::Unknown
        );
        let prompt = json!({"session_id":"session","hook_event_name":"UserPromptSubmit","bridge_version":"daedalus.opencode.v2",
            "event_id":"prompt-native","turn_id":"msg_prompt","prompt":"verified prompt"});
        let completed = json!({"session_id":"session","hook_event_name":"Stop","bridge_version":"daedalus.opencode.v2",
            "event_id":"evt_terminal","native_event":{"id":"evt_terminal","type":"session.execution.succeeded",
                "data":{"sessionID":"session"},"durable":{"aggregateID":"session","seq":5,"version":1}},
            "context":[{"id":"msg_prompt","type":"user","text":"verified prompt"},
                {"id":"msg_terminal","type":"idle","outcome":"succeeded"}]});
        for payload in [prompt, completed] {
            hook_runtime::execute(
                serde_json::to_vec(&payload).unwrap().as_slice(),
                &registration,
                None,
                &mut verifier,
                &mut hook_runtime::SystemClock,
            )
            .unwrap();
        }
        let ready = hook_runtime::ready_snapshot(&path, &f.terminals.proc_root).unwrap();
        assert_eq!(ready.state, Readiness::Idle);
        assert!(
            hook_runtime::reserve_ready_for(
                &path,
                &f.terminals.proc_root,
                ready.sequence,
                "next exact incoming prompt"
            )
            .unwrap()
        );
        assert_eq!(
            hook_runtime::ready_snapshot(&path, &f.terminals.proc_root)
                .unwrap()
                .state,
            Readiness::Busy
        );
        fields[1] = "42";
        fs::write(
            hook_dir.join("stat"),
            format!("{} (hook) {}", std::process::id(), fields.join(" ")),
        )
        .unwrap();
        assert!(
            verifier.verify(&registration).is_err(),
            "UI ancestry is not server authorization"
        );
        let other = f.dir.join("other-server");
        fs::write(&other, b"\x7fELF other").unwrap();
        fs::remove_file(server_dir.join("exe")).unwrap();
        std::os::unix::fs::symlink(other, server_dir.join("exe")).unwrap();
        assert!(hook_runtime::verify_live_session(&registration, &f.terminals).is_err());
    }
    #[test]
    fn existing_registration_json_needs_no_server_field() {
        let f = Fixture::new();
        let reg = preview(&f.request, &f.terminals).unwrap().registration;
        let mut value = serde_json::to_value(reg).unwrap();
        value.as_object_mut().unwrap().remove("server_emitter");
        let reg: Registration = serde_json::from_value(value).unwrap();
        assert!(reg.server_emitter.is_none());
        reg.validate().unwrap();
    }
    fn launch_fixture(f: &Fixture) -> super::super::hook_launch::Spec {
        let mut registration = preview(&f.request, &f.terminals).unwrap().registration;
        registration.harness_process = None;
        registration.scope.conversation = "12345678-abcd-4000-8000-123456789abc".into();
        registration.transcript = Some(
            f.dir
                .join(format!("{}.jsonl", registration.scope.conversation)),
        );
        let native = f.dir.join("native-provider");
        fs::write(&native, b"\x7fELF isolated fixture, never executed").unwrap();
        fs::set_permissions(&native, fs::Permissions::from_mode(0o700)).unwrap();
        let pid = std::process::id();
        let process_dir = f.terminals.proc_root.join(pid.to_string());
        fs::create_dir_all(&process_dir).unwrap();
        let mut fields = vec!["0"; 20];
        fields[0] = "S";
        fields[1] = "42";
        fields[2] = "42";
        fields[4] = "99";
        fields[5] = "42";
        fields[19] = "900";
        fs::write(
            process_dir.join("stat"),
            format!("{pid} (launch) {}", fields.join(" ")),
        )
        .unwrap();
        super::super::hook_launch::Spec {
            registration,
            native_binary: native.clone(),
            entrypoint: None,
            hook_profile: None,
            daedalus_binary: native,
            working_directory: f.dir.clone(),
            observed_version: "2.1.295".into(),
            expires_ms: 5000,
            model: None,
        }
    }
    #[test]
    fn grok_first_launch_binds_local_process_and_filters_other_sessions() {
        use super::super::hook_launch as launch;
        let f = Fixture::new();
        let mut spec = launch_fixture(&f);
        spec.registration.scope.harness = Harness::Grok;
        spec.registration.goal_id = None;
        spec.registration.transcript = None;
        spec.registration.events = vec![EventKind::SessionStart, EventKind::Stop];
        spec.hook_profile = Some(f.dir.clone());
        spec.observed_version = "1.0.46".into();
        let plan = launch::prepare(spec, 1000).unwrap();
        assert_eq!(
            &plan.command()[..3],
            ["--no-leader", "--no-auto-update", "--session-id"]
        );
        let target = f
            .dir
            .join("hooks")
            .join(format!("daedalus-{}.json", plan.spec.registration.id));
        assert!(!target.exists());
        let path = launch::save(&plan, &plan.fingerprint().unwrap(), 1000).unwrap();
        assert!(!target.exists());
        let active = launch::activate(&path, &f.terminals, 1001).unwrap();
        let registered = hook_runtime::inspect(&active.registration).unwrap();
        assert_eq!(
            registered.scope.conversation,
            plan.spec.registration.scope.conversation
        );
        assert_eq!(registered.harness_process.unwrap().pid, std::process::id());
        assert_eq!(active.hook_profile.as_ref(), Some(&f.dir));
        let hooks: Value = serde_json::from_slice(&fs::read(&target).unwrap()).unwrap();
        assert_eq!(fs::metadata(&target).unwrap().mode() & 0o777, 0o600);
        let command = hooks["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        // The selected fixture executable is deliberately not executable ELF:
        // other sessions must return before ever reaching it.
        let status = std::process::Command::new(crate::test_support::path("/bin/sh"))
            .args(["-c", command])
            .env("GROK_SESSION_ID", "unrelated")
            .status()
            .unwrap();
        assert!(status.success());
        let selected = std::process::Command::new(crate::test_support::path("/bin/sh"))
            .args(["-c", command])
            .env("GROK_SESSION_ID", &registered.scope.conversation)
            .output()
            .unwrap();
        assert!(!selected.status.success());
        assert!(launch::activate(&path, &f.terminals, 1002).is_err());
        assert_eq!(
            fs::read(&target).unwrap(),
            serde_json::to_vec_pretty(&hooks).unwrap()
        );

        assert!(launch::cleanup_preview(&path, &f.terminals.proc_root).is_err());
        let stat_path = f
            .terminals
            .proc_root
            .join(std::process::id().to_string())
            .join("stat");
        let stat = fs::read_to_string(&stat_path).unwrap();
        fs::write(&stat_path, b"malformed").unwrap();
        assert!(launch::cleanup_preview(&path, &f.terminals.proc_root).is_err());
        // Change only starttime. A global replacement also corrupts the PID
        // whenever the test runner's PID happens to contain the digits "900".
        let (identity, fields) = stat.rsplit_once(')').unwrap();
        let mut fields = fields.split_whitespace().collect::<Vec<_>>();
        assert_eq!(fields[19], "900");
        fields[19] = "901";
        fs::write(&stat_path, format!("{identity}) {}", fields.join(" "))).unwrap();
        let cleanup = launch::cleanup_preview(&path, &f.terminals.proc_root).unwrap();
        assert_eq!(cleanup.hook_file, target);
        assert!(target.exists());
        let approved = cleanup.fingerprint().unwrap();
        assert!(launch::cleanup_apply(&path, "wrong", &f.terminals.proc_root).is_err());
        let original = fs::read(&target).unwrap();
        layouts::write_private(&target, b"{}").unwrap();
        assert!(launch::cleanup_apply(&path, &approved, &f.terminals.proc_root).is_err());
        layouts::write_private(&target, &original).unwrap();
        // A changed registration invalidates the approval even with the same scope.
        let registration_bytes = fs::read(&active.registration).unwrap();
        let mut changed: Value = serde_json::from_slice(&registration_bytes).unwrap();
        changed["options"]["telegram_delivery"] = json!(true);
        layouts::write_private(&active.registration, &serde_json::to_vec(&changed).unwrap())
            .unwrap();
        assert!(launch::cleanup_apply(&path, &approved, &f.terminals.proc_root).is_err());
        layouts::write_private(&active.registration, &registration_bytes).unwrap();
        launch::cleanup_apply(&path, &approved, &f.terminals.proc_root).unwrap();
        assert!(!target.exists());
        assert!(path.exists());
        assert!(path.with_extension("claimed").exists());
        assert!(active.registration.exists());
        assert!(launch::cleanup_apply(&path, &approved, &f.terminals.proc_root).is_err());
    }
    #[test]
    fn grok_launch_refuses_unreviewed_version_and_existing_hook_target() {
        use super::super::hook_launch as launch;
        let f = Fixture::new();
        let mut spec = launch_fixture(&f);
        spec.registration.scope.harness = Harness::Grok;
        spec.registration.goal_id = None;
        spec.registration.transcript = None;
        spec.hook_profile = Some(f.dir.clone());
        spec.observed_version = "1.0.45".into();
        assert!(launch::prepare(spec.clone(), 1000).is_err());
        spec.observed_version = "1.0.46".into();
        fs::create_dir(f.dir.join("hooks")).unwrap();
        let target = f
            .dir
            .join("hooks")
            .join(format!("daedalus-{}.json", spec.registration.id));
        fs::write(&target, b"existing").unwrap();
        assert!(launch::prepare(spec, 1000).is_err());
        assert_eq!(fs::read(target).unwrap(), b"existing");
    }
    #[test]
    fn first_launch_registration_precedes_exec_and_is_single_use() {
        use super::super::hook_launch as launch;
        let f = Fixture::new();
        let spec = launch_fixture(&f);
        let plan = launch::prepare(spec, 1000).unwrap();
        let path = launch::save(&plan, &plan.fingerprint().unwrap(), 1000).unwrap();
        assert!(!plan.spec.registration.path().exists());
        let active = launch::activate(&path, &f.terminals, 1001).unwrap();
        assert_eq!(active.command_name, Some("claude"));
        assert_eq!(active.arguments[0], "--session-id");
        assert_eq!(active.arguments[2], "--plugin-dir");
        let registered = hook_runtime::inspect(&active.registration).unwrap();
        assert_eq!(
            registered.harness_process,
            Some(ProcessIdentity {
                pid: std::process::id(),
                start_ticks: 900
            })
        );
        let hooks: Value =
            serde_json::from_slice(&fs::read(plan.plugin().join("hooks/hooks.json")).unwrap())
                .unwrap();
        assert!(
            hooks["hooks"]["SessionStart"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("--event-kind 'SessionStart'")
        );
        assert!(launch::activate(&path, &f.terminals, 1002).is_err());
        assert!(!f.request.profile.exists());
    }
    #[test]
    fn pi_first_launch_pins_script_and_creates_exact_native_session_arguments() {
        use super::super::hook_launch as launch;
        let f = Fixture::new();
        let mut spec = launch_fixture(&f);
        spec.registration.scope.harness = Harness::Pi;
        spec.registration.goal_id = None;
        spec.registration.events = vec![
            EventKind::SessionStart,
            EventKind::UserPromptSubmit,
            EventKind::Activity,
            EventKind::PostToolUse,
            EventKind::AssistantResponse,
            EventKind::Stop,
            EventKind::Settled,
            EventKind::SessionEnd,
        ];
        spec.registration.transcript = None;
        spec.observed_version = "1.1.0".into();
        let script = f.dir.join("pi-cli.js");
        fs::write(&script, b"#!/usr/bin/env node\n// fixture only\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        spec.entrypoint = Some(script.clone());
        let plan = launch::prepare(spec.clone(), 1000).unwrap();
        let path = launch::save(&plan, &plan.fingerprint().unwrap(), 1000).unwrap();
        fs::write(&script, b"#!/usr/bin/env node\n// changed\n").unwrap();
        assert!(launch::activate(&path, &f.terminals, 1001).is_err());
        assert!(!path.with_extension("claimed").exists());
        // New reviewed specification is needed after an entrypoint change.
        let mut fresh = spec;
        fresh.registration.id = "pi-fresh".into();
        let plan = launch::prepare(fresh, 1000).unwrap();
        let path = launch::save(&plan, &plan.fingerprint().unwrap(), 1000).unwrap();
        let activated = launch::activate(&path, &f.terminals, 1001).unwrap();
        assert_eq!(activated.command_name, None);
        assert_eq!(activated.arguments[0], script.to_string_lossy());
        assert_eq!(activated.arguments[1], "--session-id");
        assert_eq!(
            activated.arguments[2],
            plan.spec.registration.scope.conversation
        );
        assert!(plan.plugin().join("sessions").is_dir());
        assert!(plan.plugin().join("daedalus.mjs").is_file());
        let registered = hook_runtime::inspect(&activated.registration).unwrap();
        let payload = json!({"session_id":registered.scope.conversation,"bridge_version":"daedalus.pi.v1","hook_event_name":"SessionStart","event_id":"first-native-session-start","transcript_path":null});
        hook_runtime::execute(
            serde_json::to_vec(&payload).unwrap().as_slice(),
            &registered,
            None,
            &mut hook_runtime::NativeVerifier {
                terminals: f.terminals.clone(),
            },
            &mut hook_runtime::SystemClock,
        )
        .unwrap();
        assert!(
            hook_runtime::inspect(&activated.registration)
                .unwrap()
                .harness_process
                .is_some()
        );
        assert!(launch::activate(&path, &f.terminals, 1002).is_err());
    }
    #[test]
    fn launch_cli_preview_is_inert_and_publish_requires_exact_current_approval() {
        let f = Fixture::new();
        let mut spec = launch_fixture(&f);
        spec.expires_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 60_000;
        let source = f.dir.join("launch-spec.json");
        layouts::write_private(&source, &serde_json::to_vec(&spec).unwrap()).unwrap();
        let mut output = Vec::new();
        super::super::hook_cli::run(
            "hook",
            &[
                "launch-preview".into(),
                "--from".into(),
                source.display().to_string(),
            ],
            std::io::empty(),
            &mut output,
        )
        .unwrap();
        let review: Value = serde_json::from_slice(&output).unwrap();
        assert!(!Path::new(review["launch_plan"].as_str().unwrap()).exists());
        assert!(!spec.registration.path().exists());
        let args = vec![
            "launch-apply".into(),
            "--from".into(),
            source.display().to_string(),
            "--expect-sha".into(),
            review["fingerprint"].as_str().unwrap().to_owned(),
        ];
        spec.model = Some("changed".into());
        layouts::write_private(&source, &serde_json::to_vec(&spec).unwrap()).unwrap();
        assert!(
            super::super::hook_cli::run("hook", &args, std::io::empty(), &mut Vec::new()).is_err()
        );
        spec.model = None;
        layouts::write_private(&source, &serde_json::to_vec(&spec).unwrap()).unwrap();
        super::super::hook_cli::run("hook", &args, std::io::empty(), &mut Vec::new()).unwrap();
        assert!(Path::new(review["launch_plan"].as_str().unwrap()).exists());
        assert!(!spec.registration.path().exists());
    }
    #[test]
    fn guided_new_pane_publishes_exact_binding_once_and_registers_before_exec() {
        use super::super::{hook_launch as launch, pane_launch};
        let f = Fixture::new();
        let spec = launch_fixture(&f);
        let hook = launch::prepare(spec, 1000).unwrap();
        let mut config = super::super::config::ControlConfig::fresh(&f.dir).unwrap();
        config.agent_name = "example".into();
        layouts::write_private(
            &f.dir.join("control.json"),
            &serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let parent = json!({"backend":"herdr","session":"example","pane_id":"pane","terminal_id":"terminal","cwd":f.dir,"debug_secret":"PRIVATE-DISCOVERY"});
        let plan:pane_launch::Plan=serde_json::from_value(json!({"request":{"harness":"claude","parent":parent,"cwd":f.dir,"model":null},"configuration":config,"hook":hook,"environment":{"HOME":f.dir,"PATH":crate::test_support::path("/usr/bin:/bin")},"profile":f.dir})).unwrap();
        fs::write(&f.terminals.herdr,r#"#!/bin/sh
case "$*" in
 *'pane split'*) printf '%s\n' '{"result":{"pane":{"pane_id":"child","terminal_id":"child-generation","pid":42}}}';;
 *'pane process-info'*) printf '%s\n' '{"result":{"process_info":{"shell_pid":42,"foreground_processes":[{"pid":42}]}}}';;
 *'pane get child'*) printf '%s\n' '{"result":{"pane":{"pane_id":"child","terminal_id":"child-generation","pid":42}}}';;
 *'pane get pane'*) printf '%s\n' '{"result":{"pane":{"pane_id":"pane","terminal_id":"terminal","pid":42}}}';;
 *'pane run'*) printf '%s\n' '{"result":{}}';;
 *) exit 2;;
esac
"#).unwrap();
        assert!(!plan.review().to_string().contains("PRIVATE-DISCOVERY"));
        let approval = plan.fingerprint().unwrap();
        let mut changed = config.clone();
        changed.agent_name = "changed".into();
        layouts::write_private(
            &f.dir.join("control.json"),
            &serde_json::to_vec(&changed).unwrap(),
        )
        .unwrap();
        assert!(pane_launch::apply(&plan, &config, &f.terminals, &approval, 1001).is_err());
        layouts::write_private(
            &f.dir.join("control.json"),
            &serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let receipt = pane_launch::apply(&plan, &config, &f.terminals, &approval, 1001).unwrap();
        assert_eq!(
            receipt.state,
            crate::adapters::terminal_actions::ReceiptState::Submitted
        );
        assert!(!plan.hook.spec.registration.path().exists());
        let ticket = f
            .dir
            .join(format!("launch-ticket-{}", plan.hook.spec.registration.id));
        let ready = layouts::read_private(&ticket.join("ready.json"))
            .unwrap()
            .unwrap();
        fs::remove_file(ticket.join("ready.json")).unwrap();
        assert!(pane_launch::bound_plan(&ticket).unwrap().is_none());
        layouts::write_private(
            &ticket.join("ready.json"),
            &serde_json::to_vec(&f.dir.join("unreviewed.json")).unwrap(),
        )
        .unwrap();
        assert!(pane_launch::bound_plan(&ticket).is_err());
        layouts::write_private(&ticket.join("ready.json"), &ready).unwrap();
        let published = pane_launch::bound_plan(&ticket).unwrap().unwrap();
        let original = layouts::read_private(&published).unwrap().unwrap();
        let mut tampered: Value = serde_json::from_slice(&original).unwrap();
        tampered["spec"]["model"] = "unreviewed-model".into();
        layouts::write_private(&published, &serde_json::to_vec(&tampered).unwrap()).unwrap();
        assert!(pane_launch::bound_plan(&ticket).is_err());
        layouts::write_private(&published, &original).unwrap();
        let activated = launch::activate(&published, &f.terminals, 1002).unwrap();
        let registered = hook_runtime::inspect(&activated.registration).unwrap();
        assert_eq!(registered.scope.terminal.generation, "child-generation");
        assert!(pane_launch::apply(&plan, &config, &f.terminals, &approval, 1003).is_err());
    }
    #[test]
    fn launch_refuses_expired_changed_and_foreign_process_before_mutation() {
        use super::super::hook_launch as launch;
        let f = Fixture::new();
        let spec = launch_fixture(&f);
        let plan = launch::prepare(spec, 1000).unwrap();
        let path = launch::save(&plan, &plan.fingerprint().unwrap(), 1000).unwrap();
        assert!(launch::activate(&path, &f.terminals, 6000).is_err());
        let process = f
            .terminals
            .proc_root
            .join(std::process::id().to_string())
            .join("stat");
        fs::write(&process, "malformed").unwrap();
        assert!(launch::activate(&path, &f.terminals, 1001).is_err());
        assert!(!path.with_extension("claimed").exists());
        fs::write(&plan.spec.native_binary, b"\x7fELF changed native binary").unwrap();
        assert!(launch::activate(&path, &f.terminals, 1001).is_err());
        assert!(!plan.spec.registration.path().exists());
        assert!(!plan.plugin().exists());
    }
    #[test]
    fn hermes_yaml_preserves_configuration_hooks_and_exact_rollback() {
        let mut f = Fixture::new();
        f.request.harness = Harness::Hermes;
        f.request.goal_id = None;
        f.request.harness_version = "0.9.0".into();
        f.request.profile = f.dir.join("config.yaml");
        let original=b"# original formatting\nmodel: configured\nquoted_boolean: 'yes'\nquoted_number: '001'\nnested:\n  seq: [a, false, 4]\nhooks_auto_accept: false\nhooks:\n  pre_llm_call:\n    - command: existing-command\n  outbound:\n    - url: https://example.invalid/hooks\n";
        layouts::write_private(&f.request.profile, original).unwrap();
        let p = preview(&f.request, &f.terminals).unwrap();
        let value: yaml_serde::Value = yaml_serde::from_slice(&p.after).unwrap();
        let before: yaml_serde::Value = yaml_serde::from_slice(original).unwrap();
        for key in [
            "model",
            "quoted_boolean",
            "quoted_number",
            "nested",
            "hooks_auto_accept",
        ] {
            assert_eq!(value[key], before[key]);
        }
        assert_eq!(value["hooks"]["outbound"], before["hooks"]["outbound"]);
        assert_eq!(
            value["hooks"]["pre_llm_call"].as_sequence().unwrap().len(),
            2
        );
        assert!(value["hooks"].get("PreCompact").is_none());
        assert_eq!(
            merge(Some(&p.after), &p.registration, &f.request.binary).unwrap(),
            p.after
        );
        let receipt = apply(&p, &f.terminals).unwrap();
        rollback(&receipt).unwrap();
        assert_eq!(fs::read(&f.request.profile).unwrap(), original);
    }
    #[test]
    fn hermes_process_rebind_fences_readiness_and_preserves_uncertain_input() {
        let mut f = Fixture::new();
        f.request.harness = Harness::Hermes;
        f.request.profile = f.dir.join("config.yaml");
        f.request.transcript = None;
        let first = preview(&f.request, &f.terminals).unwrap();
        let receipt = apply(&first, &f.terminals).unwrap();
        hook_runtime::ready_snapshot(&receipt.registration, &f.terminals.proc_root).unwrap();
        let connection =
            rusqlite::Connection::open(f.dir.join("hook-observations.sqlite3")).unwrap();
        connection
            .execute(
                "INSERT INTO hook_reservations(scope,correlation) VALUES(?,?)",
                rusqlite::params![
                    serde_json::to_string(&first.registration.scope).unwrap(),
                    "unconfirmed-input"
                ],
            )
            .unwrap();
        let stat = f.terminals.proc_root.join("42/stat");
        fs::write(
            &stat,
            fs::read_to_string(&stat).unwrap().replace("500", "600"),
        )
        .unwrap();
        f.request.process.start_ticks = 600;
        let next = preview(&f.request, &f.terminals).unwrap();
        apply(&next, &f.terminals).unwrap();
        assert_eq!(
            hook_runtime::inspect(&receipt.registration)
                .unwrap()
                .harness_process
                .unwrap()
                .start_ticks,
            600
        );
        assert_eq!(
            hook_runtime::ready_snapshot(&receipt.registration, &f.terminals.proc_root)
                .unwrap()
                .state,
            hook_runtime::Readiness::Unknown
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM hook_reservations", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn cursor_native_profile_preserves_unmanaged_handlers_and_native_loop_cap() {
        let mut f = Fixture::new();
        f.request.harness = Harness::Cursor;
        f.request.harness_version = "1.7.2".into();
        f.request.profile = f.dir.join("cursor-hooks.json");
        layouts::write_private(
            &f.request.profile,
            br#"{"version":1,"hooks":{"stop":[{"command":"existing"}]}}"#,
        )
        .unwrap();
        let p = preview(&f.request, &f.terminals).unwrap();
        let value: Value = serde_json::from_slice(&p.after).unwrap();
        assert_eq!(value["hooks"]["stop"][0]["command"], "existing");
        assert!(value["hooks"]["stop"][1].get("loop_limit").is_none());
        assert_eq!(value["hooks"]["beforeSubmitPrompt"][0]["failClosed"], true);
        assert!(value["hooks"].get("afterAgentResponse").is_some());
        assert_eq!(
            merge(Some(&p.after), &p.registration, &f.request.binary).unwrap(),
            p.after
        );
        let receipt = apply(&p, &f.terminals).unwrap();
        rollback(&receipt).unwrap();
        let original: Value =
            serde_json::from_slice(&fs::read(&f.request.profile).unwrap()).unwrap();
        assert_eq!(original["hooks"]["stop"].as_array().unwrap().len(), 1);
    }
    #[test]
    fn grok_profile_rejects_event_log_as_hook_transcript() {
        let mut f = Fixture::new();
        f.request.harness = Harness::Grok;
        f.request.harness_version = "1.0.52".into();
        f.request.goal_id = None;
        f.request.transcript = Some(f.dir.join("events.jsonl"));
        assert!(
            preview(&f.request, &f.terminals)
                .err()
                .unwrap()
                .contains("updates.jsonl")
        );
        assert!(!f.request.profile.exists());
        f.request.transcript = Some(f.dir.join("chat_history.jsonl"));
        assert!(preview(&f.request, &f.terminals).is_err());
        f.request.transcript = Some(f.dir.join("updates.jsonl"));
        assert!(preview(&f.request, &f.terminals).is_ok());
        f.request.transcript = None;
        assert!(preview(&f.request, &f.terminals).is_ok());
    }
    #[test]
    fn grok_readiness_installs_native_idle_matcher_only_for_verified_versions() {
        let mut f = Fixture::new();
        f.request.harness = Harness::Grok;
        f.request.transcript = None;
        f.request.goal_id = None;
        f.request.options.telegram_progress = true;
        f.request.options.notifications = false;
        f.request.harness_version = "1.0.52".into();
        f.request.profile = f.dir.join("grok-hooks.json");
        let plan = preview(&f.request, &f.terminals).unwrap();
        assert!(plan.registration.events.contains(&EventKind::Settled));
        let profile: Value = serde_json::from_slice(&plan.after).unwrap();
        assert!(profile["hooks"].get("Settled").is_none());
        assert_eq!(
            profile["hooks"]["Notification"][0]["matcher"],
            "idle_prompt"
        );
        assert!(
            profile["hooks"]["Notification"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("--event-kind 'Settled'")
        );
        assert_eq!(
            merge(Some(&plan.after), &plan.registration, &f.request.binary).unwrap(),
            plan.after
        );
        f.request.harness_version = "1.0.51".into();
        assert!(
            !preview(&f.request, &f.terminals)
                .unwrap()
                .registration
                .events
                .contains(&EventKind::Settled)
        );
        assert!(!grok_readiness_supported("2.0.0"));
    }
    #[test]
    fn grok_native_profile_is_passive_private_and_idempotent() {
        let mut f = Fixture::new();
        f.request.harness = Harness::Grok;
        f.request.transcript = None;
        f.request.goal_id = None;
        f.request.harness_version = "1.0.0".into();
        f.request.profile = f.dir.join("grok-hooks.json");
        let plan = preview(&f.request, &f.terminals).unwrap();
        let value: Value = serde_json::from_slice(&plan.after).unwrap();
        let handler = &value["hooks"]["Stop"][0]["hooks"][0];
        assert!(handler.get("statusMessage").is_none());
        assert!(handler.get("onFailure").is_none());
        assert!(
            handler["command"]
                .as_str()
                .unwrap()
                .contains("--event-kind 'Stop'")
        );
        assert_eq!(
            merge(Some(&plan.after), &plan.registration, &f.request.binary).unwrap(),
            plan.after
        );
        let receipt = apply(&plan, &f.terminals).unwrap();
        rollback(&receipt).unwrap();
        assert!(!f.request.profile.exists());
        f.request.goal_id = Some("goal".into());
        assert!(preview(&f.request, &f.terminals).is_err());
    }
    #[test]
    fn codex_profile_uses_native_events_and_retains_trust_review() {
        let mut f = Fixture::new();
        f.request.harness = Harness::Codex;
        f.request.harness_version = "0.130.0".into();
        f.request.profile = f.dir.join("hooks.json");
        let plan = preview(&f.request, &f.terminals).unwrap();
        let value: Value = serde_json::from_slice(&plan.after).unwrap();
        assert!(value["hooks"].get("Notification").is_none());
        assert_eq!(value["hooks"]["SessionEnd"][0]["hooks"][0]["timeout"], 3);
        assert!(
            value["hooks"]["UserPromptSubmit"][0]["hooks"][0]
                .get("onFailure")
                .is_none()
        );
        assert!(
            !String::from_utf8(plan.after.clone())
                .unwrap()
                .contains("bypass")
        );
        let receipt = apply(&plan, &f.terminals).unwrap();
        assert_eq!(
            hook_runtime::inspect(&receipt.registration)
                .unwrap()
                .scope
                .harness,
            Harness::Codex
        );
        rollback(&receipt).unwrap();
        assert!(!f.request.profile.exists());
    }
    #[test]
    fn matching_managed_handlers_are_replaced_without_duplicate_or_foreign_hook_removal() {
        let f = Fixture::new();
        let p = preview(&f.request, &f.terminals).unwrap();
        assert_eq!(
            merge(Some(&p.after), &p.registration, &f.request.binary).unwrap(),
            p.after
        );
        let mut old = f.request.clone();
        old.harness_version = "2.1.100".into();
        assert!(preview(&old, &f.terminals).is_err());
    }
}

/// Installation support is independent from launch/session adapter support.
/// Every roster entry is reported, including adapters still awaiting payload and
/// reply-contract tests. Unsupported entries must not receive Claude settings.
#[derive(Clone, Debug, Serialize)]
pub struct Capability {
    pub harness: &'static str,
    pub installable: bool,
    pub integration: &'static str,
    pub reference: &'static str,
}
pub fn capabilities() -> [Capability; 7] {
    [
        Capability {
            harness: "claude",
            installable: true,
            integration: "Private JSON matcher-group profile; observed version >=2.1.295",
            reference: "https://code.claude.com/docs/en/hooks",
        },
        Capability {
            harness: "codex",
            installable: true,
            integration: "Private native hooks.json matcher groups; requires explicit Codex /hooks trust review",
            reference: "https://learn.chatgpt.com/docs/hooks",
        },
        Capability {
            harness: "grok",
            installable: true,
            integration: "Native lifecycle profile; Grok 1.0.46 turn-gated goal continuation; feeder readiness unavailable; project hooks retain native trust",
            reference: "https://docs.x.ai/build/features/hooks",
        },
        Capability {
            harness: "hermes",
            installable: true,
            integration: "Private YAML shell hooks, native turn-correlated feeding and final output; provider first-use consent retained; generic goal continuation unavailable",
            reference: "https://github.com/NousResearch/hermes-agent/blob/main/website/docs/user-guide/features/hooks.md",
        },
        Capability {
            harness: "cursor",
            installable: true,
            integration: "Version 1 hooks.json; native lifecycle, generation-correlated feeder readiness, assistant output and bounded goal followups",
            reference: "https://cursor.com/docs/hooks",
        },
        Capability {
            harness: "pi",
            installable: true,
            integration: "Pi 0.87+ native extension bridge; actionable agent_before_settle goal boundary and final agent_settled readiness",
            reference: "https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/extensions.md",
        },
        Capability {
            harness: "opencode",
            installable: true,
            integration: "OpenCode 2.0.28+ local server plugin: exact server and foreground UI identity, canonical turn completion, assistant output, progress and memory; goal continuation remains unavailable",
            reference: "https://opencode.ai/v2/docs/build/plugins",
        },
    ]
}

#[cfg(all(test, windows))]
mod windows_server_tests {
    use super::*;
    #[test]
    #[ignore = "Private child process for the native server identity test"]
    fn server_child() {
        use std::io::Read;
        let _ = std::io::stdin().read_exact(&mut [0u8; 1]);
    }
    #[test]
    fn guided_server_pins_one_native_child_and_rejects_replaced_ui() {
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let observed = crate::platform::process_identity(std::process::id()).unwrap();
        let ui = ProcessIdentity {
            pid: observed.pid,
            start_ticks: observed.creation_time,
        };
        let proc = Path::new("unused");
        assert!(local_opencode_server(&ui, proc).is_err());
        let child = Child(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "app::hook_install::windows_server_tests::server_child",
                    "--ignored",
                ])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let pin = local_opencode_server(&ui, proc).unwrap();
        assert_eq!(pin.process.pid, child.0.id());
        hook_runtime::verify_server_emitter(&pin, proc).unwrap();
        assert!(
            local_opencode_server(
                &ProcessIdentity {
                    pid: ui.pid,
                    start_ticks: ui.start_ticks + 1
                },
                proc
            )
            .is_err()
        );
        drop(child);
        assert!(hook_runtime::verify_server_emitter(&pin, proc).is_err());
    }
}
